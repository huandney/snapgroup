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
/// subvolume (`*_snapg_regret`, `*.snapgroup_prep`, `_snapg_discard_*`) e no mount em
/// `/run/snapgroup/{uuid}`.
pub fn acquire() -> Result<Lock> {
    try_acquire()?.ok_or_else(|| {
        anyhow!(
            "outra instância do snapg está em execução (lock {LOCK_PATH}).\n  \
             Aguarde ela concluir antes de rodar de novo."
        )
    })
}

/// Para o hook do pacman: espera até `limit` por outro snapg que esteja só
/// terminando (um save, um purge). Uma tela interativa aberta segura o lock
/// enquanto espera o usuário, então a espera tem fim e o erro diz o motivo.
pub fn acquire_waiting(limit: std::time::Duration) -> Result<Lock> {
    acquire_waiting_path(LOCK_PATH, limit)
}

fn acquire_waiting_path(path: &str, limit: std::time::Duration) -> Result<Lock> {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if let Some(lock) = try_acquire_path(path)? {
            return Ok(lock);
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "outro snapg está aberto (lock {path}): checkpoint do pacman não criado.\n  \
                 Feche a tela do snapg que estiver esperando (restore, delete, lixeira, rename) antes de atualizar."
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
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
    use super::{acquire_waiting_path, try_acquire_path};
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

    #[test]
    fn waiting_gives_up_after_the_limit_and_gets_a_freed_lock() {
        let path = format!("/tmp/snapgroup-test-lock-wait-{}", std::process::id());
        let held = try_acquire_path(&path).unwrap().unwrap();
        let started = std::time::Instant::now();
        let Err(err) = acquire_waiting_path(&path, std::time::Duration::from_millis(300)) else {
            panic!("pegou um lock que outro já segurava");
        };
        assert!(started.elapsed() >= std::time::Duration::from_millis(300));
        assert!(err.to_string().contains("checkpoint do pacman não criado"));
        drop(held);
        assert!(acquire_waiting_path(&path, std::time::Duration::from_millis(300)).is_ok());
        let _ = std::fs::remove_file(&path);
    }
}
