use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::fs;
use std::process::Command;

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // alguns campos só serão consumidos na lista fzf
pub struct Snapshot {
    pub number: u32,
    #[serde(rename = "type")]
    pub kind: String,
    pub date: String,
    pub user: String,
    pub description: String,
    pub cleanup: String,
    pub userdata: Option<serde_json::Value>,
}

/// Auto-discover: snap-tools opera em todas as configs snapper que existirem.
/// Adicionar suporte pra um novo subvolume = só rodar `snapper -c <nome> create-config`.
pub fn list_configs() -> Result<Vec<String>> {
    let out = Command::new("snapper")
        .args(["--jsonout", "list-configs"])
        .output()
        .context("snapper list-configs falhou")?;
    if !out.status.success() {
        bail!(
            "snapper list-configs: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("JSON inválido de list-configs")?;
    parsed["configs"]
        .as_array()
        .context("estrutura inesperada de list-configs (sem chave 'configs')")?
        .iter()
        .map(|c| {
            c["config"]
                .as_str()
                .map(String::from)
                .context("config sem nome")
        })
        .collect()
}

pub fn list(config: &str) -> Result<Vec<Snapshot>> {
    let out = Command::new("snapper")
        .args(["--jsonout", "-c", config, "list"])
        .output()
        .context("snapper list falhou")?;
    if !out.status.success() {
        bail!(
            "snapper list -c {config}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut parsed: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&out.stdout).context("JSON inválido")?;
    let arr = parsed
        .remove(config)
        .ok_or_else(|| anyhow!("config '{config}' ausente do JSON"))?;
    serde_json::from_value(arr).context("schema de snapshot inesperado")
}

pub fn create(config: &str, description: &str, group_id: i64) -> Result<u32> {
    let userdata = format!("snap-tools-id={group_id}");
    let out = Command::new("snapper")
        .args([
            "-c",
            config,
            "create",
            "--description",
            description,
            "--userdata",
            &userdata,
            "--print-number",
        ])
        .output()
        .context("snapper create falhou")?;
    if !out.status.success() {
        bail!(
            "snapper create -c {config}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .context("snapper create não devolveu número válido")
}

pub fn delete(config: &str, number: u32) -> Result<()> {
    let n = number.to_string();
    let out = Command::new("snapper")
        .args(["-c", config, "delete", &n])
        .output()
        .context("snapper delete falhou")?;
    if !out.status.success() {
        bail!(
            "snapper delete -c {config} {n}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Lê SUBVOLUME diretamente do arquivo de config — formato é shell-like
/// (`SUBVOLUME="/home"`), parsing trivial sem depender da formatação tabular
/// do `snapper get-config` que pode mudar entre versões.
pub fn config_subvolume(config: &str) -> Result<String> {
    let path = format!("/etc/snapper/configs/{config}");
    let content = fs::read_to_string(&path).with_context(|| format!("ler {path}"))?;
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("SUBVOLUME=") {
            let val = rest.trim().trim_matches('"');
            if !val.is_empty() {
                return Ok(val.to_string());
            }
        }
    }
    bail!("SUBVOLUME não encontrado em {path}");
}
