use anyhow::{Context, Result, bail};
use std::fs::File;
use std::os::fd::AsRawFd;

const LOCK_PATH: &str = "/run/snapgroup.lock";

/// Lock global exclusivo do snapg. Enquanto o guard vive, nenhuma outra
/// instância adquire. O kernel libera automaticamente quando o fd fecha —
/// no Drop do guard ou no término/crash do processo. Não removemos o arquivo:
/// o lock é no inode (flock), e apagar abriria uma janela pra outra instância
/// criar e travar um arquivo diferente.
pub struct Lock {
    _file: File,
}

/// Adquire o lock global, não-bloqueante. Falha imediata se outra instância já
/// o detém: duas operações mutantes simultâneas colidiriam nos nomes fixos de
/// subvolume (`*_snapg_regret`, `*.snapgroup_prep`, aside) e no mount em
/// `/run/snapgroup/{uuid}`.
pub fn acquire() -> Result<Lock> {
    let file =
        File::create(LOCK_PATH).with_context(|| format!("abrir lockfile {LOCK_PATH}"))?;

    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            bail!(
                "outra instância do snapg está em execução (lock {LOCK_PATH}).\n  \
                 Aguarde ela concluir antes de rodar de novo."
            );
        }
        return Err(err).with_context(|| format!("flock {LOCK_PATH}"));
    }

    Ok(Lock { _file: file })
}
