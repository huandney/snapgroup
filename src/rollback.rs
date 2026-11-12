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
    pub backup_subvol: String,  // ex: "@home_snapg_regret", discard ou undo
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
    /// Subvolumes aninhados no subvol ativo, a levar para o novo no commit.
    nested: Vec<PathBuf>,
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

pub fn undo_name(current_subvol: &str, label: &str) -> String {
    format!("{}{label}", undo_prefix(current_subvol))
}

pub fn undo_prefix(current_subvol: &str) -> String {
    format!("{current_subvol}_snapg_undo_")
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
        btrfs::delete_subvolume_tree(toplevel, &rname)
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

    // Fase 2 inteira sob escudo: o grupo commita por completo ou falha por
    // erro tratado, nunca por um Ctrl-C no meio dos renames.
    let _shield = crate::proc::shield_termination_signals();
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
    let nested = nested_to_carry(toplevel, &current_subvol, &intermediate)?;

    Ok(Prep {
        config: m.config.clone(),
        mountpoint,
        current_subvol,
        nested,
    })
}

/// Lista os subvolumes aninhados do subvol ativo e confere, ainda na fase 1,
/// que cada um cabe no intermediate. Um snapshot btrfs não desce em subvolumes
/// aninhados (ficam como diretório vazio), então sem levá-los no commit eles
/// ficariam para trás no backup: o sistema subiria com os diretórios vazios e
/// o backup não poderia mais ser deletado ("Directory not empty").
/// Em conflito, apaga o intermediate e falha com o sistema vivo intacto.
fn nested_to_carry(toplevel: &Path, current_subvol: &str, intermediate: &Path) -> Result<Vec<PathBuf>> {
    let checked = btrfs::nested_subvolumes(toplevel, current_subvol).and_then(|nested| {
        for rel in &nested {
            check_nested_destination(intermediate, rel)?;
        }
        Ok(nested)
    });
    if checked.is_err() {
        let _ = btrfs::delete_subvolume(intermediate);
    }
    checked.with_context(|| format!("subvolumes aninhados de {current_subvol}"))
}

/// Sufixo do que estava no caminho de um subvolume aninhado e foi posto de lado.
const DISPLACED_SUFFIX: &str = ".snapg-displaced";

fn displaced_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(DISPLACED_SUFFIX);
    dest.with_file_name(name)
}

/// Confere o destino de um subvolume aninhado. `Ok(None)`: livre (ausente ou
/// diretório vazio — o stub que o snapshot deixa). `Ok(Some(aside))`: ocupado,
/// e o ocupante deve ser renomeado para `aside` antes do move. Acontece quando
/// o checkpoint tem um diretório comum onde hoje há um subvolume, ou quando o
/// sistema vivo recriou o caminho depois de o subvolume ter saído dali. Pôr de
/// lado preserva os dois lados; recusar tornaria o checkpoint irrestaurável e
/// a restauração pendente incancelável.
///
/// Erro só para o que não dá para contornar: um ancestral que não é diretório
/// (symlink absoluto resolveria contra o sistema vivo, fora do subvolume) ou
/// um `aside` que já existe.
fn check_nested_destination(to: &Path, rel: &Path) -> Result<Option<PathBuf>> {
    let mut cur = to.to_path_buf();
    let mut components = rel.components().peekable();
    while let Some(component) = components.next() {
        cur.push(component);
        let Ok(meta) = fs::symlink_metadata(&cur) else {
            return Ok(None); // ausente: os diretórios pais serão criados no move
        };
        if components.peek().is_some() {
            if !meta.is_dir() {
                bail!("{} existe no destino e não é um diretório", cur.display());
            }
            continue;
        }
        let free = meta.is_dir()
            && fs::read_dir(&cur).with_context(|| format!("ler {}", cur.display()))?.next().is_none();
        if free {
            return Ok(None);
        }
        let aside = displaced_path(&cur);
        if fs::symlink_metadata(&aside).is_ok() {
            bail!(
                "{} está ocupado e {} já existe; remova um dos dois e tente de novo",
                cur.display(),
                aside.display()
            );
        }
        return Ok(Some(aside));
    }
    Ok(None)
}

/// Cria em `to` os diretórios pais que faltam para `rel_dir`, copiando dono e
/// modo dos correspondentes em `from` (um pai criado como root 0755 dentro de
/// um home quebraria o acesso do usuário).
fn create_parents_like(from: &Path, to: &Path, rel_dir: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let mut cur = PathBuf::new();
    for component in rel_dir.components() {
        cur.push(component);
        let dest = to.join(&cur);
        if fs::symlink_metadata(&dest).is_ok() {
            continue;
        }
        fs::create_dir(&dest).with_context(|| format!("criar {}", dest.display()))?;
        let meta = fs::metadata(from.join(&cur)).with_context(|| format!("ler {}", from.join(&cur).display()))?;
        std::os::unix::fs::chown(&dest, Some(meta.uid()), Some(meta.gid()))
            .with_context(|| format!("ajustar dono de {}", dest.display()))?;
        fs::set_permissions(&dest, meta.permissions())
            .with_context(|| format!("ajustar modo de {}", dest.display()))?;
    }
    Ok(())
}

/// Move cada subvolume aninhado de `from` para o mesmo caminho em `to`. Rename
/// é metadata-only e sobrevive a mounts/arquivos abertos. Em falha, devolve os
/// já movidos a `from` e repõe o que foi posto de lado — o caller enxerga
/// tudo-ou-nada.
fn move_nested(from: &Path, to: &Path, nested: &[PathBuf]) -> Result<()> {
    let mut displaced = Vec::with_capacity(nested.len());
    for rel in nested {
        match move_one_nested(from, to, rel) {
            Ok(was_displaced) => displaced.push(was_displaced),
            Err(e) => {
                for (back, was_displaced) in nested.iter().zip(displaced).rev() {
                    let dest = to.join(back);
                    let _ = fs::rename(&dest, from.join(back));
                    if was_displaced {
                        let _ = fs::rename(displaced_path(&dest), &dest);
                    }
                }
                return Err(e);
            }
        }
    }
    Ok(())
}

/// `Ok(true)` se o destino estava ocupado e o ocupante foi posto de lado.
fn move_one_nested(from: &Path, to: &Path, rel: &Path) -> Result<bool> {
    let aside = check_nested_destination(to, rel)?;
    if let Some(parent) = rel.parent() {
        create_parents_like(from, to, parent)?;
    }
    let dest = to.join(rel);
    if let Some(aside) = &aside {
        fs::rename(&dest, aside)
            .with_context(|| format!("pôr de lado {} em {}", dest.display(), aside.display()))?;
    }
    if let Err(e) = fs::rename(from.join(rel), &dest) {
        if let Some(aside) = &aside {
            let _ = fs::rename(aside, &dest);
        }
        return Err(e).with_context(|| {
            format!("mover {} de {} para {}", rel.display(), from.display(), to.display())
        });
    }
    if let Some(aside) = &aside {
        crate::ui::rollback::print_nested_displaced(&dest, aside);
    }
    Ok(aside.is_some())
}

/// Aninhados de `subvol`, ou nada se ele não existe (recuperação manual ou
/// interrompida pode já ter tirado o subvol do nome ativo).
fn nested_if_present(toplevel: &Path, subvol: &str) -> Result<Vec<PathBuf>> {
    if !toplevel.join(subvol).exists() {
        return Ok(Vec::new());
    }
    btrfs::nested_subvolumes(toplevel, subvol)
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
    let nested = nested_to_carry(toplevel, &current_subvol, &intermediate)?;

    let prep = Prep {
        config: "root".to_string(),
        mountpoint: "/".to_string(),
        current_subvol,
        nested,
    };
    let _shield = crate::proc::shield_termination_signals();
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
    if let Err(e) = btrfs::delete_subvolume_tree(toplevel, &stashed_backup.stashed_subvol) {
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

    // Etapa 3: leva os subvolumes aninhados (`.snapshots` e os demais), que
    // foram junto do backup no rename, para o novo subvol ativo.
    if let Err(e) = move_nested(&backup, &current, &p.nested) {
        let _ = fs::rename(&current, &intermediate);
        let _ = fs::rename(&backup, &current);
        let _ = btrfs::delete_subvolume(&intermediate);
        let restore_note = restore_stashed_backup_error_note(&stashed_backup, toplevel);
        return Err(e).with_context(|| {
            format!(
                "mover subvolumes aninhados de {} pro novo {}{}",
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
    let _shield = crate::proc::shield_termination_signals();
    for d in done.iter().rev() {
        let current = toplevel.join(&d.current_subvol);
        let backup = toplevel.join(&d.backup_subvol);
        let discard_name = format!("{}.snapgroup_discard", d.current_subvol);
        let discard = toplevel.join(&discard_name);

        // Sobra de um revert anterior cujo delete falhou: o rename da etapa 1
        // esbarraria nela.
        if discard.exists() {
            let _ = btrfs::delete_subvolume_tree(toplevel, &discard_name);
        }

        // 0. Move os aninhados de volta pro backup (simétrico ao commit).
        // Sem isso, cairiam no discard e seriam deletados junto.
        let nested = nested_if_present(toplevel, &d.current_subvol)
            .with_context(|| format!("revert {}: listar subvolumes aninhados", d.config))?;
        move_nested(&current, &backup, &nested).with_context(|| {
            format!("revert {}: mover subvolumes aninhados de volta pro backup", d.config)
        })?;

        // 1. Move o subvol revertido pra fora do nome ativo. Em recuperação
        // manual/interrompida ele pode já ter sumido; nesse caso o undo só
        // precisa restaurar o regret para o nome ativo.
        let moved_current_to_discard = current.exists();
        if moved_current_to_discard
            && let Err(e) = fs::rename(&current, &discard)
        {
            // Os aninhados já estão no backup; sem devolvê-los, o revertido
            // seguiria com o nome ativo e esses caminhos vazios.
            let _ = move_nested(&backup, &current, &nested);
            return Err(e).with_context(|| {
                format!("revert {}: tirar revertido de {}", d.config, d.current_subvol)
            });
        }

        // 2. Restaura o backup pro nome ativo (fstab volta a achar)
        if let Err(e) = fs::rename(&backup, &current) {
            // Tenta voltar o discard pro lugar (estado consistente com falha)
            if moved_current_to_discard {
                let _ = fs::rename(&discard, &current);
                let _ = move_nested(&backup, &current, &nested);
            }
            return Err(e).with_context(|| {
                format!("revert {}: restaurar backup {}", d.config, d.backup_subvol)
            });
        }

        // 3. Apaga o subvol revertido (SEGURO aqui — nunca foi montado).
        if moved_current_to_discard
            && let Err(e) = btrfs::delete_subvolume_tree(toplevel, &discard_name)
        {
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
/// Solução: deixa um `<subvol>_snapg_undo_<label>` no top-level. Antes do reboot,
/// cancelar faz o swap inverso e preserva o Regret; após reboot, o subvol fica
/// desmontado e pode ser limpo pelo boot-clean.
pub fn revert_regret(done: &[Done], toplevel: &Path, label: &str) -> Result<()> {
    let _shield = crate::proc::shield_termination_signals();
    for d in done.iter().rev() {
        let current = toplevel.join(&d.current_subvol);
        let backup = toplevel.join(&d.backup_subvol);
        let undo_subvol = undo_name(&d.current_subvol, label);
        let undo = toplevel.join(&undo_subvol);

        // 0. Move os aninhados pro regret, que vai voltar a ser o ativo
        // (simétrico ao commit). Inclui os criados depois do restore: o que
        // está fora dos snapshots acompanha o nome ativo.
        let nested = btrfs::nested_subvolumes(toplevel, &d.current_subvol)
            .with_context(|| format!("revert_regret {}: listar subvolumes aninhados", d.config))?;
        move_nested(&current, &backup, &nested).with_context(|| {
            format!("revert_regret {}: mover subvolumes aninhados pro regret", d.config)
        })?;

        // 1. Move o subvol revertido (= rootfs viva) pra fora do nome ativo.
        // Mount sobrevive — kernel referencia por inode, não path.
        if let Err(e) = fs::rename(&current, &undo) {
            let _ = move_nested(&backup, &current, &nested);
            return Err(e).with_context(|| {
                format!("revert_regret {}: tirar atual de {}", d.config, d.current_subvol)
            });
        }

        // 2. Restaura o regret pro nome ativo (fstab volta a achar no próximo boot).
        if let Err(e) = fs::rename(&backup, &current) {
            let _ = fs::rename(&undo, &current);
            let _ = move_nested(&backup, &current, &nested);
            return Err(e).with_context(|| {
                format!("revert_regret {}: restaurar regret {}", d.config, d.backup_subvol)
            });
        }

        // 3. NÃO DELETA. Undo fica como `<subvol>_snapg_undo_<label>`
        // até o próximo reboot. boot-clean limpa depois.
    }
    Ok(())
}

/// Cancela um `revert_regret` antes do reboot.
///
/// Aqui `current` é o conteúdo do Regret recém-promovido, portanto precioso. Ao
/// cancelar, ele volta para o slot fixo `_snapg_regret`, e o `_snapg_undo_*`
/// volta para o nome ativo. Nada é deletado.
pub fn cancel_regret_undo(done: &[Done], toplevel: &Path) -> Result<()> {
    cancel_regret_undo_with(done, toplevel, btrfs::nested_subvolumes)
}

/// A listagem de aninhados entra por parâmetro só para o teste unitário rodar
/// em diretórios comuns, onde `btrfs subvolume list` não funciona.
fn cancel_regret_undo_with(
    done: &[Done],
    toplevel: &Path,
    list_nested: fn(&Path, &str) -> Result<Vec<PathBuf>>,
) -> Result<()> {
    let _shield = crate::proc::shield_termination_signals();
    for d in done.iter().rev() {
        let current = toplevel.join(&d.current_subvol);
        let undo = toplevel.join(&d.backup_subvol);
        let regret_subvol = regret_name(&d.current_subvol);
        let regret = toplevel.join(&regret_subvol);

        if regret.exists() {
            bail!("destino de Regret já existe: {}", regret.display());
        }

        // Os aninhados precisam acompanhar o subvol que voltará a ser ativo.
        let nested = list_nested(toplevel, &d.current_subvol)
            .with_context(|| format!("cancel_regret_undo {}: listar subvolumes aninhados", d.config))?;
        move_nested(&current, &undo, &nested).with_context(|| {
            format!("cancel_regret_undo {}: mover subvolumes aninhados para {}", d.config, d.backup_subvol)
        })?;

        if let Err(e) = fs::rename(&current, &regret) {
            let _ = move_nested(&undo, &current, &nested);
            return Err(e).with_context(|| {
                format!("cancel_regret_undo {}: preservar Regret em {}", d.config, regret_subvol)
            });
        }

        if let Err(e) = fs::rename(&undo, &current) {
            let _ = fs::rename(&regret, &current);
            let _ = move_nested(&undo, &current, &nested);
            return Err(e).with_context(|| {
                format!("cancel_regret_undo {}: restaurar {}", d.config, d.backup_subvol)
            });
        }
    }
    Ok(())
}

pub fn toplevel_mount_path(uuid: &str) -> PathBuf {
    PathBuf::from(format!("/run/snapgroup/{uuid}"))
}

#[cfg(test)]
mod tests {
    use super::{
        Done, Prep, cancel_regret_undo, cancel_regret_undo_with, check_nested_destination,
        commit_prep, move_nested, prep_intermediate_name, regret_name, revert_partial,
        revert_regret, undo_name,
    };
    use crate::btrfs;
    use std::path::{Path, PathBuf};

    #[test]
    fn cancel_regret_undo_swaps_back_without_deleting_regret() {
        let base = std::env::temp_dir().join(format!(
            "snapg_cancel_regret_undo_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let current = "@";
        let undo = undo_name(current, "20260609-120000");
        let regret = regret_name(current);
        std::fs::create_dir(base.join(current)).unwrap();
        std::fs::write(base.join(current).join("marker"), "regret").unwrap();
        std::fs::create_dir(base.join(&undo)).unwrap();
        std::fs::write(base.join(&undo).join("marker"), "active").unwrap();

        let done = [Done {
            config: "root".to_string(),
            mountpoint: "/".to_string(),
            current_subvol: current.to_string(),
            backup_subvol: undo.clone(),
        }];

        cancel_regret_undo_with(&done, &base, |_, _| Ok(Vec::new())).unwrap();

        assert_eq!(
            std::fs::read_to_string(base.join(current).join("marker")).unwrap(),
            "active"
        );
        assert_eq!(
            std::fs::read_to_string(base.join(&regret).join("marker")).unwrap(),
            "regret"
        );
        assert!(!base.join(&undo).exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    fn temp_base(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("snapg_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn nested_destination_is_free_displaceable_or_refused() {
        let base = temp_base("nested_dest");
        std::fs::create_dir_all(base.join("var/lib/stub")).unwrap();
        std::fs::create_dir_all(base.join("var/lib/cheio")).unwrap();
        std::fs::write(base.join("var/lib/cheio/arquivo"), "x").unwrap();
        std::fs::write(base.join("var/arquivo"), "x").unwrap();
        std::os::unix::fs::symlink("/", base.join("link")).unwrap();

        // Stub vazio do snapshot e caminho que ainda não existe: livres.
        assert_eq!(check_nested_destination(&base, Path::new("var/lib/stub")).unwrap(), None);
        assert_eq!(check_nested_destination(&base, Path::new("data/novo")).unwrap(), None);
        // Ocupado no ponto final (diretório com conteúdo, arquivo): pôr de lado.
        assert_eq!(
            check_nested_destination(&base, Path::new("var/lib/cheio")).unwrap(),
            Some(base.join("var/lib/cheio.snapg-displaced"))
        );
        assert_eq!(
            check_nested_destination(&base, Path::new("var/arquivo")).unwrap(),
            Some(base.join("var/arquivo.snapg-displaced"))
        );
        // Ancestral que não é diretório (arquivo, symlink): recusado.
        assert!(check_nested_destination(&base, Path::new("var/arquivo/sub")).is_err());
        assert!(check_nested_destination(&base, Path::new("link/etc")).is_err());
        // O lugar do "de lado" já está tomado: recusado.
        std::fs::create_dir(base.join("var/lib/cheio.snapg-displaced")).unwrap();
        assert!(check_nested_destination(&base, Path::new("var/lib/cheio")).is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn move_nested_displaces_occupants_and_is_all_or_nothing() {
        let base = temp_base("move_nested");
        let (from, to) = (base.join("from"), base.join("to"));
        std::fs::create_dir_all(from.join("a/um")).unwrap();
        std::fs::create_dir_all(from.join("b/dois")).unwrap();
        std::fs::create_dir_all(to.join("a/um")).unwrap(); // stub
        let nested = [PathBuf::from("a/um"), PathBuf::from("b/dois")];

        // `b` não existe em `to`: o pai é criado e os dois são movidos.
        move_nested(&from, &to, &nested).unwrap();
        assert!(to.join("a/um").exists() && to.join("b/dois").exists());
        assert!(!from.join("a/um").exists() && !from.join("b/dois").exists());

        // De volta, com o segundo destino recriado e ocupado: o ocupante é
        // posto de lado e o move acontece.
        std::fs::create_dir_all(from.join("b/dois")).unwrap();
        std::fs::write(from.join("b/dois/recriado"), "x").unwrap();
        move_nested(&to, &from, &nested).unwrap();
        assert!(from.join("a/um").exists() && from.join("b/dois").exists());
        assert!(!from.join("b/dois/recriado").exists());
        assert!(from.join("b/dois.snapg-displaced/recriado").exists());

        // Segundo move impossível (ocupado e o "de lado" também): o primeiro
        // volta para a origem.
        std::fs::create_dir_all(to.join("b/dois")).unwrap();
        std::fs::write(to.join("b/dois/ocupado"), "x").unwrap();
        std::fs::create_dir_all(to.join("b/dois.snapg-displaced")).unwrap();
        assert!(move_nested(&from, &to, &nested).is_err());
        assert!(from.join("a/um").exists() && from.join("b/dois").exists());
        assert!(!to.join("a/um").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Exercita commit e as três reversões num btrfs de verdade. Rodar como
    /// root, com um btrfs vazio montado pelo top-level:
    ///   cargo test --no-run && sudo SNAPG_TEST_BTRFS=/mnt/x <binário> --ignored
    #[test]
    #[ignore = "precisa de root e de um btrfs vazio montado em SNAPG_TEST_BTRFS"]
    fn nested_subvolumes_follow_the_active_subvol_on_real_btrfs() {
        let top = PathBuf::from(std::env::var("SNAPG_TEST_BTRFS").expect("SNAPG_TEST_BTRFS"));
        let btrfs_cmd = |args: &[&str]| {
            let status = std::process::Command::new("btrfs")
                .args(args)
                .current_dir(&top)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "btrfs {args:?}");
        };
        let regret = regret_name("@");

        btrfs_cmd(&["subvolume", "create", "@"]);
        btrfs_cmd(&["subvolume", "create", "@/.snapshots"]);
        std::fs::create_dir_all(top.join("@/var/lib")).unwrap();
        btrfs_cmd(&["subvolume", "create", "@/var/lib/machines"]);
        std::fs::write(top.join("@/var/lib/machines/dado"), "x").unwrap();
        std::fs::create_dir(top.join("@/.snapshots/1")).unwrap();
        btrfs_cmd(&["subvolume", "snapshot", "-r", "@", "@/.snapshots/1/snapshot"]);
        // Depois do checkpoint: um aninhado sob um pai que o snapshot não tem,
        // e um arquivo que distingue o root antigo do restaurado.
        std::fs::create_dir(top.join("@/data")).unwrap();
        btrfs_cmd(&["subvolume", "create", "@/data/novo"]);
        std::fs::write(top.join("@/data/novo/arquivo"), "x").unwrap();
        std::fs::write(top.join("@/marker"), "antigo").unwrap();

        let prepare = || {
            let intermediate = top.join(prep_intermediate_name("@"));
            btrfs::create_snapshot(&top.join("@/.snapshots/1/snapshot"), &intermediate).unwrap();
            let nested = super::nested_to_carry(&top, "@", &intermediate).unwrap();
            assert_eq!(nested.len(), 3);
            Prep {
                config: "root".to_string(),
                mountpoint: "/".to_string(),
                current_subvol: "@".to_string(),
                nested,
            }
        };
        let assert_active_has_nested = || {
            assert!(top.join("@/var/lib/machines/dado").exists());
            assert!(top.join("@/data/novo/arquivo").exists());
            assert!(top.join("@/.snapshots/1/snapshot").exists());
            assert_eq!(btrfs::nested_subvolumes(&top, "@").unwrap().len(), 3);
        };
        let assert_bare = |subvol: &str| {
            assert!(btrfs::nested_subvolumes(&top, subvol).unwrap().is_empty(), "{subvol}");
        };

        // Commit: o restaurado assume o nome ativo e recebe os aninhados.
        let done = commit_prep(&prepare(), &top).unwrap();
        assert_active_has_nested();
        assert!(!top.join("@/marker").exists());
        assert_bare(&regret);

        // Enquanto pendente, o sistema vivo (o regret) recria um dos caminhos
        // como diretório comum, e o tmpfiles recria outro como subvolume vazio.
        std::fs::create_dir_all(top.join(&regret).join("var/lib/machines")).unwrap();
        std::fs::write(top.join(&regret).join("var/lib/machines/recriado"), "x").unwrap();

        // Desfazer antes do reboot: o antigo volta com os aninhados, e o que
        // foi recriado fica de lado em vez de travar o cancelamento.
        revert_partial(std::slice::from_ref(&done), &top).unwrap();
        assert_active_has_nested();
        assert!(top.join("@/marker").exists());
        assert!(top.join("@/var/lib/machines.snapg-displaced/recriado").exists());
        std::fs::remove_dir_all(top.join("@/var/lib/machines.snapg-displaced")).unwrap();
        assert!(!top.join(&regret).exists());
        assert!(!top.join("@.snapgroup_discard").exists());

        // Restaurar o Regret (após um reboot) e cancelar essa restauração.
        let done = commit_prep(&prepare(), &top).unwrap();
        revert_regret(std::slice::from_ref(&done), &top, "rotulo").unwrap();
        assert_active_has_nested();
        assert!(top.join("@/marker").exists());
        let undo = undo_name("@", "rotulo");
        assert_bare(&undo);

        let pending = Done {
            config: "root".to_string(),
            mountpoint: "/".to_string(),
            current_subvol: "@".to_string(),
            backup_subvol: undo.clone(),
        };
        cancel_regret_undo(&[pending], &top).unwrap();
        assert_active_has_nested();
        assert!(!top.join("@/marker").exists());
        assert!(!top.join(&undo).exists());
        assert_bare(&regret);

        // O Regret precisa continuar deletável, mesmo com um subvolume vazio
        // recriado dentro dele; um aninhado com dados é recusado, não apagado.
        btrfs_cmd(&["subvolume", "create", &format!("{regret}/vazio")]);
        btrfs_cmd(&["subvolume", "create", &format!("{regret}/cheio")]);
        std::fs::write(top.join(&regret).join("cheio/dado"), "x").unwrap();
        assert!(btrfs::delete_subvolume_tree(&top, &regret).is_err());
        assert!(top.join(&regret).join("cheio/dado").exists());
        std::fs::remove_file(top.join(&regret).join("cheio/dado")).unwrap();
        btrfs::delete_subvolume_tree(&top, &regret).unwrap();
        assert!(!top.join(&regret).exists());
    }
}
