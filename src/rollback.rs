use crate::boot;
use crate::btrfs;
use crate::group::{Group, Member};
use crate::snapper;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

/// Registro de cada rollback bem-sucedido — usado pra reversão em falha parcial.
pub struct Done {
    pub config: String,
    pub mountpoint: String,
    pub current_subvol: String, // ex: "@home" — agora aponta pro novo RW
    pub backup_subvol: String,  // ex: "@home_snapg_regret" — o ativo anterior
}

pub struct RollbackError {
    pub done: Vec<Done>,
    pub failed_config: String,
    pub error: anyhow::Error,
}

/// Resultado da Fase 1 (preparação) — descreve um membro pronto pra commit.
/// Ainda nada foi tocado no sistema vivo nesse ponto.
struct Prep {
    config: String,
    mountpoint: String,
    current_subvol: String,
}

#[derive(Clone, Copy)]
enum CommitMode<'a> {
    Regret,
    PreserveRegret { label: &'a str },
}

impl CommitMode<'_> {
    fn phase2_context(self) -> &'static str {
        match self {
            CommitMode::Regret => "fase 2 (commit)",
            CommitMode::PreserveRegret { .. } => "fase 2 (commit recovery)",
        }
    }
}

/// Nome fixo do regret pra um dado subvolume ativo.
/// Ex: "@home" → "@home_snapg_regret"
pub fn regret_name(current_subvol: &str) -> String {
    format!("{current_subvol}_snapg_regret")
}

pub fn discard_name(current_subvol: &str, label: &str) -> String {
    format!("{}{label}", discard_prefix(current_subvol))
}

pub fn discard_prefix(current_subvol: &str) -> String {
    format!("{current_subvol}_snapg_discard_")
}

fn prep_intermediate_name(current_subvol: &str) -> String {
    format!("{current_subvol}.snapgroup_prep")
}

/// Preflight: toda config Snapper precisa viver no mesmo filesystem BTRFS que
/// `/`. O snapg monta só o top-level do FS de `/` e opera nos subvolumes por
/// path relativo sob esse top-level. Uma config em outro BTRFS não existiria
/// ali — ou, pior, um subvol homônimo de outro layout seria deletado/renomeado
/// por engano. Aborta antes de montar ou tocar qualquer coisa.
///
/// Suporte multi-filesystem é trabalho futuro (agrupar configs por UUID e
/// montar um top-level por FS); até lá, fail fast é a única opção segura.
pub fn ensure_single_filesystem(configs: &[String]) -> Result<()> {
    let root_uuid = btrfs::fs_uuid("/")?;
    for cfg in configs {
        let mp = snapper::config_subvolume(cfg)?;
        let uuid = btrfs::fs_uuid(&mp)
            .with_context(|| format!("descobrir UUID do filesystem de '{cfg}' ({mp})"))?;
        if uuid != root_uuid {
            bail!(
                "config snapper '{cfg}' (montada em {mp}) vive no filesystem {uuid}, \
                 diferente do filesystem de / ({root_uuid}).\n  \
                 O snapg ainda não suporta configs em múltiplos filesystems BTRFS.\n  \
                 Operação abortada antes de qualquer alteração."
            );
        }
    }
    Ok(())
}

/// Deleta regrets existentes de todas as configs no toplevel.
/// Idempotente: se não existir regret, é no-op silencioso.
pub fn delete_existing_regrets(toplevel: &Path, configs: &[String]) -> Result<()> {
    for cfg in configs {
        let mp = snapper::config_subvolume(cfg)?;
        let current = btrfs::subvol_relative_path(Path::new(&mp))
            .with_context(|| format!("descobrir subvol ativo de '{cfg}'"))?;
        let rname = regret_name(&current);
        let regret_path = toplevel.join(&rname);
        if !regret_path.exists() {
            continue;
        }
        btrfs::delete_subvolume(&regret_path)
            .with_context(|| format!("deletar regret {rname}"))?;
        crate::ui::rollback::print_deleted_regret(&rname);
    }
    Ok(())
}

/// Two-phase rollback de um grupo.
///
/// Fase 1 (preparação, IO-pesada): cria `<subvol>.snapgroup_prep` a partir
/// do snapshot RO de cada membro. Falha aqui (ENOSPC, IO error, etc) é
/// frequente o suficiente pra justificar a separação. Se qualquer membro
/// falhar nessa fase, todos os preps criados são deletados e o sistema
/// vivo permanece **100% intocado**.
///
/// Fase 2 (commit, metadata-only): para cada membro, faz live→regret,
/// prep→live, fix `.snapshots`. São apenas renames, atômicos por membro,
/// extremamente improváveis de falhar. Se ainda assim falhar no meio de
/// um grupo, retorna `RollbackError` com os membros já commitados pra que
/// o caller decida se reverte (`revert_partial`).
///
/// INVARIANTE: o caller DEVE garantir que não há restore pendente antes de chamar.
pub fn rollback_group(group: &Group, toplevel: &Path) -> Result<Vec<Done>, RollbackError> {
    rollback_group_with_commit(group, toplevel, CommitMode::Regret)
}

/// Variante de recuperação: restaura os membros sem promover o estado atual a
/// `_snapg_regret`. O estado substituído vai para `_snapg_discard_<label>`,
/// mantendo qualquer Regret anterior intacto para a próxima decisão manual.
pub fn rollback_group_preserving_regret(
    group: &Group,
    toplevel: &Path,
    label: &str,
) -> Result<Vec<Done>, RollbackError> {
    rollback_group_with_commit(group, toplevel, CommitMode::PreserveRegret { label })
}

fn rollback_group_with_commit(
    group: &Group,
    toplevel: &Path,
    mode: CommitMode<'_>,
) -> Result<Vec<Done>, RollbackError> {
    let mut preps = Vec::new();
    for m in &group.members {
        match prepare_member(m, toplevel) {
            Ok(p) => preps.push(p),
            Err(e) => {
                cleanup_preps(&preps, toplevel);
                return Err(RollbackError {
                    done: Vec::new(),
                    failed_config: m.config.clone(),
                    error: e.context("fase 1 (prepare) — sistema vivo intacto"),
                });
            }
        }
    }

    let mut done = Vec::new();
    for p in &preps {
        let result = match mode {
            CommitMode::Regret => commit_prep(p, toplevel),
            CommitMode::PreserveRegret { label } => {
                commit_prep_preserving_regret(p, toplevel, label)
            }
        };
        match result {
            Ok(d) => done.push(d),
            Err(e) => {
                cleanup_preps(&preps[done.len()..], toplevel);
                return Err(RollbackError {
                    done,
                    failed_config: p.config.clone(),
                    error: e.context(mode.phase2_context()),
                });
            }
        }
    }
    Ok(done)
}

/// Path top-level absoluto do subvolume read-only de um membro — o conteúdo que
/// virará root no rollback. Disponível ANTES do commit, então serve tanto à
/// fase 1 do rollback quanto ao preflight que prevê se o sync de /boot mexerá
/// no /boot legado.
pub fn member_snapshot_path(m: &Member, toplevel: &Path) -> Result<PathBuf> {
    let mountpoint = snapper::config_subvolume(&m.config)?;
    let base_subvol = configured_subvol_for_mountpoint(&mountpoint)?;
    // Path top-level do snapshot read-only (pode ser nested ou top-level)
    let snap_live_path = format!(
        "{}/.snapshots/{}/snapshot",
        mountpoint.trim_end_matches('/'),
        m.snapshot.number
    );
    let snap_subvol_path = btrfs::subvol_relative_path(Path::new(&snap_live_path))
        .unwrap_or_else(|_| format!("{base_subvol}/.snapshots/{}/snapshot", m.snapshot.number));
    Ok(toplevel.join(snap_subvol_path))
}

fn configured_subvol_for_mountpoint(mountpoint: &str) -> Result<String> {
    if mountpoint == "/" && let Some(root_subvol) = boot::default_root_subvol() {
        return Ok(root_subvol);
    }
    base_subvol_of_mountpoint(mountpoint)
}

/// Subvol base de um mountpoint, descontando o sufixo de restore pendente.
/// Pré-reboot o subvol vivo é `<base>_snapg_regret` (o chão renomeado, ainda
/// montado por inode); o nome base é o que importa pra preparar/escanear.
pub fn base_subvol_of_mountpoint(mountpoint: &str) -> Result<String> {
    let current = btrfs::subvol_relative_path(Path::new(mountpoint))
        .with_context(|| format!("descobrir subvol ativo de {mountpoint}"))?;
    Ok(current
        .strip_suffix("_snapg_regret")
        .unwrap_or(&current)
        .to_string())
}

/// Fase 1: cria a cópia writable do snapshot RO num nome intermediário.
/// Operação cara (metadata copy) e propensa a ENOSPC. **Não toca em nada vivo.**
fn prepare_member(m: &Member, toplevel: &Path) -> Result<Prep> {
    let mountpoint = snapper::config_subvolume(&m.config)?;

    // Path top-level do subvol atualmente ativo (ex: "@home")
    let current_subvol = configured_subvol_for_mountpoint(&mountpoint)?;

    let intermediate_name = prep_intermediate_name(&current_subvol);

    let src = member_snapshot_path(m, toplevel)?;
    let intermediate = toplevel.join(&intermediate_name);

    // Limpa lixo de tentativa anterior abortada (defensivo).
    if intermediate.exists() {
        let _ = btrfs::delete_subvolume(&intermediate);
    }

    btrfs::create_snapshot(&src, &intermediate)
        .with_context(|| format!("criar cópia writable do snap #{}", m.snapshot.number))?;

    Ok(Prep {
        config: m.config.clone(),
        mountpoint,
        current_subvol,
    })
}

/// Best-effort: deleta todos os intermediates criados na fase 1.
/// Usado quando fase 1 ou fase 2 abortam.
fn cleanup_preps(preps: &[Prep], toplevel: &Path) {
    for p in preps {
        let intermediate = toplevel.join(prep_intermediate_name(&p.current_subvol));
        if intermediate.exists() {
            let _ = btrfs::delete_subvolume(&intermediate);
        }
    }
}

/// Recuperação do root usada pelo doctor: não atualiza o Regret. O `@` anterior
/// fica em discard e é limpo no boot seguinte; um undo antes do reboot ainda
/// consegue restaurá-lo com `revert_partial`.
pub fn rollback_root_explicit_preserving_regret(
    toplevel: &Path,
    root_subvol: &str,
    src: &Path,
    label: &str,
) -> Result<Done> {
    let current_subvol = root_subvol.to_string();
    let intermediate = toplevel.join(prep_intermediate_name(&current_subvol));
    if intermediate.exists() {
        let _ = btrfs::delete_subvolume(&intermediate);
    }

    btrfs::create_snapshot(src, &intermediate)
        .with_context(|| format!("criar cópia writable de {}", src.display()))?;

    let prep = Prep {
        config: "root".to_string(),
        mountpoint: "/".to_string(),
        current_subvol,
    };
    commit_prep_preserving_regret(&prep, toplevel, label)
}

fn commit_prep(p: &Prep, toplevel: &Path) -> Result<Done> {
    let backup_subvol = regret_name(&p.current_subvol);
    commit_prepared_subvol(p, toplevel, &backup_subvol, true)
}

fn commit_prep_preserving_regret(p: &Prep, toplevel: &Path, label: &str) -> Result<Done> {
    let discard_subvol = discard_name(&p.current_subvol, label);
    commit_prepared_subvol(p, toplevel, &discard_subvol, false)
}

struct StashedBackup {
    original_subvol: String,
    stashed_subvol: String,
}

fn stale_regret_name(current_subvol: &str, label: &str) -> String {
    format!(
        "{}old-regret_{}_{}",
        discard_prefix(current_subvol),
        label,
        std::process::id()
    )
}

fn stash_existing_backup(
    toplevel: &Path,
    original_subvol: &str,
    current_subvol: &str,
) -> Result<StashedBackup> {
    let label = btrfs::now_local_label().context("obter label de tempo")?;
    let stashed_subvol = stale_regret_name(current_subvol, &label);
    let original = toplevel.join(original_subvol);
    let stashed = toplevel.join(&stashed_subvol);
    if stashed.exists() {
        bail!("destino temporário de Regret antigo já existe: {}", stashed.display());
    }
    fs::rename(&original, &stashed).with_context(|| {
        format!("preservar Regret anterior {original_subvol} em {stashed_subvol}")
    })?;
    Ok(StashedBackup {
        original_subvol: original_subvol.to_string(),
        stashed_subvol,
    })
}

fn restore_stashed_backup(stashed_backup: &Option<StashedBackup>, toplevel: &Path) -> Result<()> {
    let Some(stashed_backup) = stashed_backup else {
        return Ok(());
    };
    fs::rename(
        toplevel.join(&stashed_backup.stashed_subvol),
        toplevel.join(&stashed_backup.original_subvol),
    )
    .with_context(|| {
        format!(
            "restaurar Regret anterior {} → {}",
            stashed_backup.stashed_subvol, stashed_backup.original_subvol
        )
    })
}

fn restore_stashed_backup_error_note(
    stashed_backup: &Option<StashedBackup>,
    toplevel: &Path,
) -> String {
    match restore_stashed_backup(stashed_backup, toplevel) {
        Ok(()) => String::new(),
        Err(e) => format!("; restaurar Regret anterior falhou: {e:#}"),
    }
}

fn delete_stashed_backup(stashed_backup: Option<StashedBackup>, toplevel: &Path, config: &str) {
    let Some(stashed_backup) = stashed_backup else {
        return;
    };
    let stashed = toplevel.join(&stashed_backup.stashed_subvol);
    if let Err(e) = btrfs::delete_subvolume(&stashed) {
        crate::ui::rollback::print_stashed_regret_delete_failed(config, &stashed, &e);
    }
}

fn commit_prepared_subvol(
    p: &Prep,
    toplevel: &Path,
    backup_subvol: &str,
    replace_existing_backup: bool,
) -> Result<Done> {
    let intermediate = toplevel.join(prep_intermediate_name(&p.current_subvol));
    let current = toplevel.join(&p.current_subvol);
    let backup = toplevel.join(backup_subvol);

    let stashed_backup = if backup.exists() && replace_existing_backup {
        match stash_existing_backup(toplevel, backup_subvol, &p.current_subvol) {
            Ok(stashed_backup) => Some(stashed_backup),
            Err(e) => {
                let _ = btrfs::delete_subvolume(&intermediate);
                return Err(e);
            }
        }
    } else {
        None
    };

    if backup.exists() {
        let _ = btrfs::delete_subvolume(&intermediate);
        bail!("destino de rollback já existe: {}", backup.display());
    }

    // Etapa 1: arquiva o subvol ativo. Rename é metadata-only; mount
    // sobrevive (kernel referencia por inode, não path).
    if let Err(e) = fs::rename(&current, &backup) {
        let _ = btrfs::delete_subvolume(&intermediate);
        let restore_note = restore_stashed_backup_error_note(&stashed_backup, toplevel);
        return Err(e).with_context(|| {
            format!(
                "renomear subvol ativo {} → {}{}",
                p.current_subvol, backup_subvol, restore_note
            )
        });
    }

    // Etapa 2: promove o intermediate ao nome ativo.
    if let Err(e) = fs::rename(&intermediate, &current) {
        let _ = fs::rename(&backup, &current);
        let _ = btrfs::delete_subvolume(&intermediate);
        let restore_note = restore_stashed_backup_error_note(&stashed_backup, toplevel);
        return Err(e).with_context(|| {
            format!(
                "promover intermediate → {}{}",
                p.current_subvol, restore_note
            )
        });
    }

    // Etapa 3: corrige `.snapshots` aninhado (foi junto do backup no rename).
    let backup_dotsnap = backup.join(".snapshots");
    let new_dotsnap = current.join(".snapshots");
    if btrfs::is_subvolume(&backup_dotsnap)
        && let Err(e) = fs::rename(&backup_dotsnap, &new_dotsnap)
    {
        let _ = fs::rename(&current, &intermediate);
        let _ = fs::rename(&backup, &current);
        let _ = btrfs::delete_subvolume(&intermediate);
        let restore_note = restore_stashed_backup_error_note(&stashed_backup, toplevel);
        return Err(e).with_context(|| {
            format!(
                "mover .snapshots de {} pro novo {}{}",
                backup_subvol, p.current_subvol, restore_note
            )
        });
    }

    delete_stashed_backup(stashed_backup, toplevel, &p.config);

    Ok(Done {
        config: p.config.clone(),
        mountpoint: p.mountpoint.clone(),
        current_subvol: p.current_subvol.clone(),
        backup_subvol: backup_subvol.to_string(),
    })
}

/// Reverte rollbacks já feitos durante uma falha PARCIAL.
///
/// INVARIANTE: usar SOMENTE quando o subvol "revertido" (current) ainda
/// não foi montado pelo kernel — i.e., antes do reboot. Nessa fase o
/// `current` é a cópia writable recém-promovida, criada do snapshot RO.
/// Ninguém depende dela; pode ser deletada sem risco.
///
/// **Não usar pra `revert_regret`**, onde `current` É a rootfs viva.
pub fn revert_partial(done: &[Done], toplevel: &Path) -> Result<()> {
    for d in done.iter().rev() {
        let current = toplevel.join(&d.current_subvol);
        let backup = toplevel.join(&d.backup_subvol);
        let discard_name = format!("{}.snapgroup_discard", d.current_subvol);
        let discard = toplevel.join(&discard_name);

        // 0. Move .snapshots de volta pro backup (simétrico ao rollback_member).
        // Sem isso, .snapshots cairia no discard e seria deletado junto.
        let current_dotsnap = current.join(".snapshots");
        let backup_dotsnap = backup.join(".snapshots");
        if btrfs::is_subvolume(&current_dotsnap) {
            fs::rename(&current_dotsnap, &backup_dotsnap).with_context(|| {
                format!("revert {}: mover .snapshots de volta pro backup", d.config)
            })?;
        }

        // 1. Move o subvol revertido pra fora do nome ativo. Em recuperação
        // manual/interrompida ele pode já ter sumido; nesse caso o undo só
        // precisa restaurar o regret para o nome ativo.
        let moved_current_to_discard = current.exists();
        if moved_current_to_discard {
            fs::rename(&current, &discard).with_context(|| {
                format!("revert {}: tirar revertido de {}", d.config, d.current_subvol)
            })?;
        }

        // 2. Restaura o backup pro nome ativo (fstab volta a achar)
        if let Err(e) = fs::rename(&backup, &current) {
            // Tenta voltar o discard pro lugar (estado consistente com falha)
            if moved_current_to_discard {
                let _ = fs::rename(&discard, &current);
            }
            return Err(e).with_context(|| {
                format!("revert {}: restaurar backup {}", d.config, d.backup_subvol)
            });
        }

        // 3. Apaga o subvol revertido (SEGURO aqui — nunca foi montado).
        if moved_current_to_discard && let Err(e) = btrfs::delete_subvolume(&discard) {
            crate::ui::rollback::print_discard_delete_failed(&d.config, &discard, &e);
        }
    }
    Ok(())
}

/// Restaura regret: troca current ↔ regret, sem deletar nada.
///
/// O subvol "revertido" (current pré-restore) é a rootfs/home/etc VIVA — o
/// kernel ainda o tem montado por inode mesmo depois do rename, e deletar
/// quebra o sistema rodando.
///
/// Solução: deixa um `<subvol>_snapg_discard_<label>` no top-level.
/// Após reboot, o subvol fica desmontado e pode ser limpo pelo boot-clean.
pub fn revert_regret(done: &[Done], toplevel: &Path, label: &str) -> Result<()> {
    for d in done.iter().rev() {
        let current = toplevel.join(&d.current_subvol);
        let backup = toplevel.join(&d.backup_subvol);
        let discard_subvol = discard_name(&d.current_subvol, label);
        let discard = toplevel.join(&discard_subvol);

        // 0. Move .snapshots de volta pro backup (simétrico ao rollback_member).
        let current_dotsnap = current.join(".snapshots");
        let backup_dotsnap = backup.join(".snapshots");
        if btrfs::is_subvolume(&current_dotsnap) {
            fs::rename(&current_dotsnap, &backup_dotsnap).with_context(|| {
                format!("revert_regret {}: mover .snapshots de volta pro backup", d.config)
            })?;
        }

        // 1. Move o subvol revertido (= rootfs viva) pra fora do nome ativo.
        // Mount sobrevive — kernel referencia por inode, não path.
        fs::rename(&current, &discard)
            .with_context(|| format!("revert_regret {}: tirar atual de {}", d.config, d.current_subvol))?;

        // 2. Restaura o regret pro nome ativo (fstab volta a achar no próximo boot).
        if let Err(e) = fs::rename(&backup, &current) {
            let _ = fs::rename(&discard, &current);
            return Err(e).with_context(|| {
                format!("revert_regret {}: restaurar regret {}", d.config, d.backup_subvol)
            });
        }

        // 3. NÃO DELETA. Discard fica como `<subvol>_snapg_discard_<label>`
        // até o próximo reboot. boot-clean limpa depois.
    }
    Ok(())
}

pub fn toplevel_mount_path(uuid: &str) -> PathBuf {
    PathBuf::from(format!("/run/snapgroup/{uuid}"))
}
