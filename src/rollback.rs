use crate::btrfs;
use crate::group::{Group, Member};
use crate::snapper;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Registro de cada rollback bem-sucedido — usado pra reversão em falha parcial.
pub struct Done {
    pub config: String,
    pub mountpoint: String,
    pub current_subvol: String, // ex: "@home" — agora aponta pro novo RW
    pub backup_subvol: String,  // ex: "@home_backup_<epoch>" — o ativo anterior
}

pub struct RollbackError {
    pub done: Vec<Done>,
    pub failed_config: String,
    pub error: anyhow::Error,
}

/// Executa rollback em sequência. Retorna lista de membros já feitos
/// (em ordem) ou erro detalhado se algum falhar.
pub fn rollback_group(group: &Group, toplevel: &Path) -> Result<Vec<Done>, RollbackError> {
    let mut done = Vec::new();
    for m in &group.members {
        match rollback_member(m, toplevel) {
            Ok(d) => done.push(d),
            Err(e) => {
                return Err(RollbackError {
                    done,
                    failed_config: m.config.clone(),
                    error: e,
                });
            }
        }
    }
    Ok(done)
}

fn rollback_member(m: &Member, toplevel: &Path) -> Result<Done> {
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

    let label = btrfs::now_local_label()?;
    let backup_subvol = format!("{current_subvol}_backup_{label}");
    // Nome distintivo pra evitar colisão com qualquer subvol "real".
    let intermediate_name = format!("{current_subvol}.snapgroup_new");

    let src = toplevel.join(&snap_subvol_path);
    let intermediate = toplevel.join(&intermediate_name);
    let current = toplevel.join(&current_subvol);
    let backup = toplevel.join(&backup_subvol);

    // Limpa lixo de tentativa anterior abortada (defensivo).
    if intermediate.exists() {
        let _ = btrfs::delete_subvolume(&intermediate);
    }

    // Etapa 1: cria cópia writable do snapshot read-only.
    // Esta é a operação mais cara (cópia metadata) e a que mais pode falhar
    // (ENOSPC). Se falhar, NADA foi mudado ainda — abort limpo.
    btrfs::create_snapshot(&src, &intermediate)
        .with_context(|| format!("criar cópia writable do snap #{}", m.snapshot.number))?;

    // Etapa 2: arquiva o subvol ativo. rename é metadata-only, atômico,
    // sem I/O. Mount em /, /home etc. continua funcionando (kernel guarda
    // por inode, não path).
    if let Err(e) = fs::rename(&current, &backup) {
        let _ = btrfs::delete_subvolume(&intermediate);
        return Err(e).with_context(|| {
            format!(
                "renomear subvol ativo {current_subvol} → {backup_subvol}"
            )
        });
    }

    // Etapa 3: promove o intermediate ao nome ativo.
    // Se isso falhar (extremamente improvável — mesma fs, sem I/O), faz
    // best-effort de voltar pra estado anterior.
    if let Err(e) = fs::rename(&intermediate, &current) {
        let _ = fs::rename(&backup, &current);
        let _ = btrfs::delete_subvolume(&intermediate);
        return Err(e)
            .with_context(|| format!("promover intermediate → {current_subvol}"));
    }

    // Etapa 4: corrige nested .snapshots. Quando o subvol ativo tinha
    // .snapshots como subvol aninhado (caso CachyOS/openSUSE), ele foi pra
    // junto do backup no rename. O novo subvol ativo (criado de snapshot)
    // tem só placeholder vazio. Move o real de volta — rename é metadata-only,
    // funciona pra subvol aninhado entre subvols irmãos no mesmo fs.
    let backup_dotsnap = backup.join(".snapshots");
    let new_dotsnap = current.join(".snapshots");
    if btrfs::is_subvolume(&backup_dotsnap)
        && let Err(e) = fs::rename(&backup_dotsnap, &new_dotsnap)
    {
        // Reverte tudo: volta intermediate→current, backup→original.
        let _ = fs::rename(&current, &intermediate);
        let _ = fs::rename(&backup, &current);
        let _ = btrfs::delete_subvolume(&intermediate);
        return Err(e).with_context(|| {
            format!(
                "mover .snapshots de {backup_subvol} pro novo {current_subvol}"
            )
        });
    }

    Ok(Done {
        config: m.config.clone(),
        mountpoint,
        current_subvol,
        backup_subvol,
    })
}

/// Reverte rollbacks já feitos: restaura backup, descarta o subvol revertido.
/// SAFE: rename é atômico; delete só roda depois do rename bem-sucedido.
/// Operação em ordem reversa pra simetria, embora nessa fase os rollbacks
/// sejam independentes.
pub fn revert_done(done: &[Done], toplevel: &Path) -> Result<()> {
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

        // 3. Apaga o subvol revertido (best-effort — se falhar, log warning mas não aborta)
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

pub fn toplevel_mount_path(uuid: &str) -> PathBuf {
    PathBuf::from(format!("/run/snapgroup/{uuid}"))
}
