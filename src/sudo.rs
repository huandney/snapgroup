use anyhow::{Result, bail};
use std::os::unix::process::CommandExt;
use std::process::Command;

pub fn ensure_root() -> Result<()> {
    if current_uid() == 0 {
        return Ok(());
    }
    let args: Vec<String> = std::env::args().collect();
    // exec substitui o processo atual — sem PID extra, sinais propagam direto.
    let err = Command::new("sudo").args(&args).exec();
    bail!("falha ao re-executar com sudo: {err}");
}

fn current_uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or(u32::MAX)
}
