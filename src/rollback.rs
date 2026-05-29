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
    backup_subvol: String, // ex: "@home_snapg_regret"
}

/// Nome fixo do regret pra um dado subvolume ativo.
/// Ex: "@home" → "@home_snapg_regret"
pub fn regret_name(current_subvol: &str) -> String {
    format!("{current_subvol}_snapg_regret")
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
        println!("  regret anterior deletado: {rname}");
    }
    Ok(())
}

/// Nome do aside temporário pro regret anterior durante um restore checkpoint.
/// Sufixo distinto de `_snapg_regret`, `.snapgroup_prep` e `_snapg_discard_*`
/// pra não colidir com nada que rollback_group/revert criem.
/// Ex: "@" → "@.snapgroup_regret_aside"
fn regret_aside_name(current_subvol: &str) -> String {
    format!("{current_subvol}.snapgroup_regret_aside")
}

/// Regret anterior movido pro lado durante um restore checkpoint, à espera do
/// resultado do rollback.
pub struct AsidedRegret {
    pub config: String,
    pub regret_subvol: String, // nome canônico: "@_snapg_regret"
    pub aside_subvol: String,  // "@.snapgroup_regret_aside"
}

/// Move os regrets existentes pra um nome aside, em vez de deletá-los.
/// Diferente de `delete_existing_regrets`: preserva o "botão de arrependimento"
/// anterior até o novo rollback commitar. Se o rollback falhar e o sistema
/// voltar a um estado limpo, o caller restaura o aside (`restore_asides`); em
/// estado ambíguo, preserva. Idempotente quanto a regrets ausentes.
pub fn aside_existing_regrets(toplevel: &Path, configs: &[String]) -> Result<Vec<AsidedRegret>> {
    let mut asides = Vec::new();
    for cfg in configs {
        let mp = snapper::config_subvolume(cfg)?;
        let current = btrfs::subvol_relative_path(Path::new(&mp))
            .with_context(|| format!("descobrir subvol ativo de '{cfg}'"))?;
        let rname = regret_name(&current);
        let regret_path = toplevel.join(&rname);
        if !regret_path.exists() {
            continue;
        }
        let aname = regret_aside_name(&current);
        let aside_path = toplevel.join(&aname);
        // Aside órfão = restou de uma tentativa anterior abortada. Estado
        // inesperado: não sobrescrever (perderia o regret antigo). Para.
        if aside_path.exists() {
            bail!(
                "aside órfão encontrado em {} — provável tentativa anterior \
                 interrompida. Resolva manualmente antes de novo restore.",
                aside_path.display()
            );
        }
        fs::rename(&regret_path, &aside_path)
            .with_context(|| format!("mover regret {rname} pra aside {aname}"))?;
        asides.push(AsidedRegret {
            config: cfg.clone(),
            regret_subvol: rname,
            aside_subvol: aname,
        });
    }
    Ok(asides)
}

/// Restaura os asides de volta ao nome canônico de regret. Usar SOMENTE quando
/// o sistema vivo está num estado limpo conhecido (slots `_snapg_regret`
/// livres): rollback falhou na Fase 1, ou `revert_partial` concluiu.
pub fn restore_asides(asides: &[AsidedRegret], toplevel: &Path) -> Result<()> {
    for a in asides {
        let aside_path = toplevel.join(&a.aside_subvol);
        let regret_path = toplevel.join(&a.regret_subvol);
        fs::rename(&aside_path, &regret_path).with_context(|| {
            format!("restaurar aside {} → {}", a.aside_subvol, a.regret_subvol)
        })?;
    }
    Ok(())
}

/// Deleta os asides obsoletos após um rollback bem-sucedido (o regret anterior
/// foi substituído pelo novo). Best-effort: o rollback já commitou, então um
/// subvol-lixo remanescente não deve abortar a operação — só avisa.
pub fn delete_asides(asides: &[AsidedRegret], toplevel: &Path) {
    for a in asides {
        let aside_path = toplevel.join(&a.aside_subvol);
        if let Err(e) = btrfs::delete_subvolume(&aside_path) {
            eprintln!(
                "⚠ regret anterior (aside {}) não foi deletado: {e:#}",
                a.aside_subvol
            );
            eprintln!(
                "   limpe manualmente: sudo btrfs subvolume delete {}",
                aside_path.display()
            );
        }
    }
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
/// INVARIANTE: o caller DEVE ter deletado regrets existentes antes de chamar.
pub fn rollback_group(group: &Group, toplevel: &Path) -> Result<Vec<Done>, RollbackError> {
    // === Fase 1: preparação ===
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

    // === Fase 2: commit ===
    let mut done = Vec::new();
    for p in &preps {
        match commit_prep(p, toplevel) {
            Ok(d) => done.push(d),
            Err(e) => {
                // Limpa preps remanescentes (do membro que falhou em diante).
                cleanup_preps(&preps[done.len()..], toplevel);
                return Err(RollbackError {
                    done,
                    failed_config: p.config.clone(),
                    error: e.context("fase 2 (commit)"),
                });
            }
        }
    }
    Ok(done)
}

/// Fase 1: cria a cópia writable do snapshot RO num nome intermediário.
/// Operação cara (metadata copy) e propensa a ENOSPC. **Não toca em nada vivo.**
fn prepare_member(m: &Member, toplevel: &Path) -> Result<Prep> {
    let mountpoint = snapper::config_subvolume(&m.config)?;

    // Path top-level do subvol atualmente ativo (ex: "@home")
    let current_subvol = btrfs::subvol_relative_path(Path::new(&mountpoint))
        .with_context(|| format!("descobrir subvol ativo de {mountpoint}"))?;

    // Path top-level do snapshot read-only (pode ser nested ou top-level)
    let snap_live_path = format!(
        "{}/.snapshots/{}/snapshot",
        mountpoint.trim_end_matches('/'),
        m.snapshot.number
    );
    let snap_subvol_path = btrfs::subvol_relative_path(Path::new(&snap_live_path))
        .with_context(|| format!("descobrir path do snapshot #{}", m.snapshot.number))?;

    let backup_subvol = regret_name(&current_subvol);
    let intermediate_name = prep_intermediate_name(&current_subvol);

    let src = toplevel.join(&snap_subvol_path);
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
        backup_subvol,
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

/// Fase 2: faz os renames que efetivam o rollback. Apenas metadata, atômico
/// por syscall. Falha aqui é rara (mesma fs, sem IO).
fn commit_prep(p: &Prep, toplevel: &Path) -> Result<Done> {
    let intermediate = toplevel.join(prep_intermediate_name(&p.current_subvol));
    let current = toplevel.join(&p.current_subvol);
    let backup = toplevel.join(&p.backup_subvol);

    // Etapa 1: arquiva o subvol ativo. Rename é metadata-only; mount
    // sobrevive (kernel referencia por inode, não path).
    if let Err(e) = fs::rename(&current, &backup) {
        let _ = btrfs::delete_subvolume(&intermediate);
        return Err(e).with_context(|| {
            format!("renomear subvol ativo {} → {}", p.current_subvol, p.backup_subvol)
        });
    }

    // Etapa 2: promove o intermediate ao nome ativo.
    if let Err(e) = fs::rename(&intermediate, &current) {
        let _ = fs::rename(&backup, &current);
        let _ = btrfs::delete_subvolume(&intermediate);
        return Err(e).with_context(|| format!("promover intermediate → {}", p.current_subvol));
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
        return Err(e).with_context(|| {
            format!("mover .snapshots de {} pro novo {}", p.backup_subvol, p.current_subvol)
        });
    }

    Ok(Done {
        config: p.config.clone(),
        mountpoint: p.mountpoint.clone(),
        current_subvol: p.current_subvol.clone(),
        backup_subvol: p.backup_subvol.clone(),
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

        // 1. Move o subvol revertido pra fora do nome ativo
        fs::rename(&current, &discard)
            .with_context(|| format!("revert {}: tirar revertido de {}", d.config, d.current_subvol))?;

        // 2. Restaura o backup pro nome ativo (fstab volta a achar)
        if let Err(e) = fs::rename(&backup, &current) {
            // Tenta voltar o discard pro lugar (estado consistente com falha)
            let _ = fs::rename(&discard, &current);
            return Err(e).with_context(|| {
                format!("revert {}: restaurar backup {}", d.config, d.backup_subvol)
            });
        }

        // 3. Apaga o subvol revertido (SEGURO aqui — nunca foi montado).
        if let Err(e) = btrfs::delete_subvolume(&discard) {
            eprintln!(
                "⚠ revert {}: backup restaurado mas subvol descartado não foi deletado: {e:#}",
                d.config
            );
            eprintln!(
                "   limpe manualmente: sudo btrfs subvolume delete {}",
                discard.display()
            );
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
        let discard_name = format!("{}_snapg_discard_{label}", d.current_subvol);
        let discard = toplevel.join(&discard_name);

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
