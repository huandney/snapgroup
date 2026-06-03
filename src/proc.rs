use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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

/// Roda `cmd` que não emite progresso (ex.: `btrfs subvolume snapshot`, que
/// bloqueia mudo no kernel fazendo a cópia de metadata), chamando `on_tick` a
/// cada `interval` enquanto o processo roda — o caller anima um spinner pra não
/// parecer travado. Mesmo contrato de erro do `run_streamed`.
pub fn run_ticking(mut cmd: Command, interval: Duration, mut on_tick: impl FnMut()) -> Result<()> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().context("spawn falhou")?;
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    // Drena os dois pipes em threads pra não travar caso encham; só o stderr
    // interessa pra mensagem de erro.
    let h_out = thread::spawn(move || {
        let mut sink = String::new();
        let _ = BufReader::new(stdout).read_to_string(&mut sink);
    });
    let h_err = thread::spawn(move || {
        let mut buf = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut buf);
        buf
    });

    loop {
        if let Some(status) = child.try_wait().context("try_wait falhou")? {
            let _ = h_out.join();
            let stderr_buf = h_err.join().unwrap_or_default();
            if !status.success() {
                bail!("{}", stderr_buf.trim_end());
            }
            return Ok(());
        }
        on_tick();
        thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn ticking_fires_while_running_and_succeeds() {
        let mut ticks = 0;
        run_ticking(sh("sleep 0.25"), Duration::from_millis(40), || ticks += 1).unwrap();
        assert!(ticks >= 1, "esperava ao menos um tick, teve {ticks}");
    }

    #[test]
    fn ticking_fails_with_stderr_text() {
        let err = run_ticking(sh("echo nope >&2; exit 1"), Duration::from_millis(40), || {})
            .unwrap_err();
        assert!(format!("{err:#}").contains("nope"));
    }
}
