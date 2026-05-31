use anyhow::{Context, Result, anyhow};
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
    try_acquire()?.ok_or_else(|| {
        anyhow!(
            "outra instância do snapg está em execução (lock {LOCK_PATH}).\n  \
             Aguarde ela concluir antes de rodar de novo."
        )
    })
}

/// Tenta adquirir o lock global. `Ok(None)` significa contenção benigna:
/// outro snapg já segura o lock. Callers interativos transformam isso em erro;
/// `boot-clean` usa como sinal para adiar a limpeza para o próximo boot.
pub fn try_acquire() -> Result<Option<Lock>> {
    try_acquire_path(LOCK_PATH)
}

fn try_acquire_path(path: &str) -> Result<Option<Lock>> {
    let file = File::create(path).with_context(|| format!("abrir lockfile {path}"))?;

    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(Lock { _file: file }));
    }

    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Ok(None);
    }

    Err(err).with_context(|| format!("flock {path}"))
}

#[cfg(test)]
mod tests {
    use super::try_acquire_path;
    use std::fs::File;
    use std::os::fd::AsRawFd;

    #[test]
    fn lock_rejects_second_holder() {
        let path = format!("/tmp/snapgroup-test-lock-{}", std::process::id());
        let file = File::create(&path).expect("open lockfile");
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0);

        assert!(try_acquire_path(&path).expect("second attempt").is_none());
        let _ = std::fs::remove_file(path);
    }
}
