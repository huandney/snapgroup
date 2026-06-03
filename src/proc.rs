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
}
