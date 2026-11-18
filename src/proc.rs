use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

/// Roda `cmd` transmitindo cada linha de stdout/stderr para `on_line`, sempre
/// chamado na thread corrente — toda escrita no terminal fica numa thread só,
/// sem risco de saída embaralhada. As leituras dos dois pipes vivem em threads
/// próprias só para não travar (pipe cheio) e para serializar via canal.
///
/// Bufferiza o stderr e mantém o MESMO contrato de erro do `.output()` +
/// checagem de status usado no resto do código: `Err` com o stderr acumulado
/// quando o processo sai com status != 0. A apresentação muda; o erro não.
pub fn run_streamed(mut cmd: Command, mut on_line: impl FnMut(&str)) -> Result<()> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().context("spawn falhou")?;
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let (tx, rx) = mpsc::channel::<(bool, String)>();
    let tx_err = tx.clone();
    let h_out = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send((false, line));
        }
    });
    let h_err = thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx_err.send((true, line));
        }
    });

    let mut stderr_buf = String::new();
    for (is_err, line) in rx {
        if is_err {
            stderr_buf.push_str(&line);
            stderr_buf.push('\n');
        }
        on_line(&line);
    }
    let _ = h_out.join();
    let _ = h_err.join();

    let status = child.wait().context("wait falhou")?;
    if !status.success() {
        bail!("{}", stderr_buf.trim_end());
    }
    Ok(())
}

const TERMINATION_SIGNALS: [libc::c_int; 4] =
    [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

/// Enquanto vive, o processo ignora Ctrl-C/kill/fechamento do terminal. Envolve
/// as sequências de rename de subvolume: morrer entre "ativo → backup" e
/// "novo → ativo" deixa o sistema sem o subvol que o fstab monta no boot.
///
/// Ignora (disposição, do processo inteiro) em vez de bloquear por máscara: a
/// máscara é por thread, e a thread do spinner receberia o sinal e mataria o
/// processo do mesmo jeito. O sinal recebido durante a seção é descartado, não
/// adiado — a seção dura milissegundos.
pub struct SignalShield {
    previous: [libc::sighandler_t; 4],
}

pub fn shield_termination_signals() -> SignalShield {
    let previous = TERMINATION_SIGNALS.map(|sig| unsafe { libc::signal(sig, libc::SIG_IGN) });
    SignalShield { previous }
}

impl Drop for SignalShield {
    fn drop(&mut self) {
        for (sig, handler) in TERMINATION_SIGNALS.into_iter().zip(self.previous) {
            unsafe { libc::signal(sig, handler) };
        }
    }
}

/// Janela em que o `/boot` está sendo reescrito (o sync depois de um restore:
/// vmlinuz, `mkinitcpio`, hashes do `limine.conf`, ~20–30 s). Morrer no meio
/// deixa o `/boot` pela metade, que é a origem dos incidentes de boot. Enquanto
/// vive:
///
/// - Ctrl-C, kill e fechamento do terminal ficam adiados, não descartados: um
///   aviso diz que o snapg termina o `/boot` primeiro, e o sinal é entregue
///   quando a janela fecha. Ignorar em silêncio, como o `SignalShield`, serve
///   para milissegundos; aqui o usuário apertaria Ctrl-C sem resposta.
/// - O terminal que fecha (SIGHUP) passa o stdin/stdout/stderr para
///   `/dev/null`: a próxima linha da tela falharia e o `println!` derrubaria o
///   processo no meio do sync.
/// - Um inibidor do logind segura desligar, reiniciar e suspender pelo menu ou
///   pela tampa. Ele vive num `cat` com o stdin ligado ao snapg: se o snapg
///   morrer de vez (SIGKILL), o pipe fecha e o inibidor some junto.
///
/// Janelas podem se aninhar (o cancelamento de um restore envolve o sync e a
/// troca de subvolumes numa só, e o sync abre a dele): só a de fora instala os
/// handlers, segura o inibidor e entrega o sinal ao fechar.
pub struct CriticalWindow {
    _signals: DeferredSignals,
    inhibitor: Option<std::process::Child>,
}

pub fn critical_window() -> CriticalWindow {
    let signals = defer_termination_signals();
    let inhibitor = if signals.previous.is_some() { hold_inhibitor() } else { None };
    CriticalWindow { _signals: signals, inhibitor }
}

fn hold_inhibitor() -> Option<std::process::Child> {
    // Grupo de processos próprio: o Ctrl-C do terminal vai para o grupo da
    // frente e soltaria o inibidor.
    use std::os::unix::process::CommandExt;
    Command::new("systemd-inhibit")
        .args([
            "--what=shutdown:sleep:handle-lid-switch:handle-power-key:handle-suspend-key",
            "--who=snapgroup",
            "--why=o /boot está sendo reescrito",
            "--mode=block",
            "cat",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()
}

impl Drop for CriticalWindow {
    fn drop(&mut self) {
        if let Some(mut child) = self.inhibitor.take() {
            drop(child.stdin.take());
            let _ = child.wait();
        }
    }
}

/// Os sinais de término ficam guardados em vez de agir; `Drop` da janela de
/// fora devolve os handlers de antes e entrega o primeiro que chegou. As de
/// dentro (`previous` vazio) só contam a profundidade: o sinal que chegou na
/// de fora continua guardado.
struct DeferredSignals {
    previous: Option<[libc::sighandler_t; 4]>,
}

static DEFERRED_SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static WINDOW_DEPTH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn defer_termination_signals() -> DeferredSignals {
    if WINDOW_DEPTH.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
        return DeferredSignals { previous: None };
    }
    let previous = TERMINATION_SIGNALS.map(|sig| unsafe { libc::signal(sig, on_deferred as *const () as libc::sighandler_t) });
    DeferredSignals { previous: Some(previous) }
}

const DEFERRED_NOTICE: &[u8] =
    "\n   ! o snapg sai assim que o /boot estiver pronto: interromper agora deixaria o boot pela metade\n".as_bytes();

/// Só chamadas async-signal-safe: atômicos, `write`, `open` e `dup2`.
extern "C" fn on_deferred(sig: libc::c_int) {
    use std::sync::atomic::Ordering;
    let first = DEFERRED_SIGNAL.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst).is_ok();
    if sig == libc::SIGHUP {
        unsafe {
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            if null >= 0 {
                libc::dup2(null, 0);
                libc::dup2(null, 1);
                libc::dup2(null, 2);
            }
        }
        return;
    }
    if first {
        unsafe { libc::write(2, DEFERRED_NOTICE.as_ptr().cast(), DEFERRED_NOTICE.len()) };
    }
}

impl Drop for DeferredSignals {
    fn drop(&mut self) {
        WINDOW_DEPTH.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        let Some(previous) = self.previous else {
            return;
        };
        for (sig, handler) in TERMINATION_SIGNALS.into_iter().zip(previous) {
            unsafe { libc::signal(sig, handler) };
        }
        let pending = DEFERRED_SIGNAL.swap(0, std::sync::atomic::Ordering::SeqCst);
        if pending != 0 {
            // O handler de antes faz a limpeza de sempre (desmontar o
            // top-level) e encerra com o status do sinal.
            unsafe { libc::raise(pending) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_signals_wait_for_the_window_to_close() {
        // Num filho, como o teste do escudo. Um byte no pipe prova que o filho
        // sobreviveu ao Ctrl-C dentro da janela; o status prova que o sinal
        // chegou quando ela fechou.
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
            let window = defer_termination_signals();
            unsafe { libc::raise(libc::SIGINT) };
            unsafe { libc::write(fds[1], b"x".as_ptr().cast(), 1) };
            drop(window);
            unsafe { libc::_exit(0) };
        }
        unsafe { libc::close(fds[1]) };
        let mut byte = 0u8;
        assert_eq!(unsafe { libc::read(fds[0], (&mut byte as *mut u8).cast(), 1) }, 1);
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGINT);
    }

    #[test]
    fn an_inner_window_keeps_the_signal_for_the_outer_one() {
        // O cancelamento: Ctrl-C no meio do sync (janela de dentro) só pode
        // valer depois da troca de subvolumes (janela de fora).
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
            let outer = defer_termination_signals();
            let inner = defer_termination_signals();
            unsafe { libc::raise(libc::SIGINT) };
            drop(inner);
            unsafe { libc::write(fds[1], b"x".as_ptr().cast(), 1) };
            drop(outer);
            unsafe { libc::_exit(0) };
        }
        unsafe { libc::close(fds[1]) };
        let mut byte = 0u8;
        assert_eq!(unsafe { libc::read(fds[0], (&mut byte as *mut u8).cast(), 1) }, 1);
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGINT);
    }

    #[test]
    fn shield_ignores_termination_signals() {
        // Num filho: a disposição de sinais é do processo inteiro, e outros
        // testes (threads do mesmo processo) armam e desarmam o escudo em
        // paralelo. O filho só usa chamadas async-signal-safe.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            let shield = shield_termination_signals();
            // Sem o escudo, este raise mataria o filho (status != exit 0).
            unsafe { libc::raise(libc::SIGHUP) };
            std::mem::forget(shield);
            unsafe { libc::_exit(0) };
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    }

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn streamed_delivers_stdout_and_stderr_lines() {
        let mut lines = Vec::new();
        run_streamed(sh("echo out1; echo err1 >&2; echo out2"), |l| {
            lines.push(l.to_string())
        })
        .unwrap();
        assert!(lines.iter().any(|l| l == "out1"));
        assert!(lines.iter().any(|l| l == "out2"));
        assert!(lines.iter().any(|l| l == "err1"));
    }

    #[test]
    fn streamed_fails_with_stderr_text() {
        let err = run_streamed(sh("echo boom >&2; exit 3"), |_| {}).unwrap_err();
        assert!(format!("{err:#}").contains("boom"));
    }
}
