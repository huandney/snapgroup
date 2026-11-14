use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::fs;
use std::process::Command;

#[derive(Clone, Debug, Deserialize)]
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

/// Auto-discover: snapgroup opera em todas as configs snapper que existirem.
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

/// O que só os checkpoints do hook do pacman gravam.
pub struct PacmanMarks<'a> {
    /// Onde a transação começa no `pacman.log`.
    pub anchor: Option<crate::pacman_log::Anchor>,
    /// Operação da primeira transação (`-Syu`, `-U`): diz a fonte dela, cuja
    /// linha `Running` fica antes da âncora.
    pub op: Option<&'a str>,
    /// Execução do ajudante (paru, makepkg) que o checkpoint cobre.
    pub session: Option<&'a str>,
}

/// Cria um membro de grupo. `auto` grava se o checkpoint saiu com o nome
/// padrão: decide se ele entra na retenção. `pacman` marca o checkpoint do
/// hook, que tem fila própria de retenção e é achado no `pacman.log`.
pub fn create(
    config: &str,
    description: &str,
    group_id: i64,
    auto: bool,
    pacman: Option<&PacmanMarks>,
) -> Result<u32> {
    use crate::group::{
        AUTO_KEY, PACMAN_LOG_INO_KEY, PACMAN_LOG_KEY, PACMAN_OP_KEY, SESSION_KEY, TRIGGER_KEY,
        TRIGGER_PACMAN,
    };
    let mut userdata = format!("snapgroup-id={group_id},{AUTO_KEY}={}", u8::from(auto));
    if let Some(marks) = pacman {
        userdata.push_str(&format!(",{TRIGGER_KEY}={TRIGGER_PACMAN}"));
        if let Some(anchor) = marks.anchor {
            userdata.push_str(&format!(",{PACMAN_LOG_KEY}={}", anchor.offset));
            if let Some(ino) = anchor.ino {
                userdata.push_str(&format!(",{PACMAN_LOG_INO_KEY}={ino}"));
            }
        }
        if let Some(op) = marks.op {
            userdata.push_str(&format!(",{PACMAN_OP_KEY}={op}"));
        }
        if let Some(session) = marks.session {
            userdata.push_str(&format!(",{SESSION_KEY}={session}"));
        }
    }
    create_with_userdata(config, description, &userdata)
}

pub fn create_with_userdata(config: &str, description: &str, userdata: &str) -> Result<u32> {
    let out = Command::new("snapper")
        .args([
            "-c",
            config,
            "create",
            "--description",
            description,
            "--userdata",
            userdata,
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

/// Marca um snapshot como lixeira. Re-afirmar `snapgroup-id` é idempotente e
/// blinda contra a dúvida merge-vs-replace do `snapper modify --userdata`: se
/// ele substituir, o id sobrevive; se mesclar, fica igual.
pub fn trash(
    config: &str,
    number: u32,
    group_id: i64,
    trash_epoch: i64,
    reason: &str,
) -> Result<()> {
    modify_userdata(
        config,
        number,
        &format!("snapgroup-id={group_id},snapgroup-trash={trash_epoch},snapgroup-trash-reason={reason}"),
    )
}

/// Tira um snapshot da lixeira: regrava o id e zera as marcas de trash. Valor
/// vazio remove a chave no snapper; e mesmo que persista vazio, `extract_trash`
/// não parseia "" — o grupo deixa de contar como lixeira de qualquer forma.
pub fn untrash(config: &str, number: u32, group_id: i64) -> Result<()> {
    modify_userdata(
        config,
        number,
        &format!("snapgroup-id={group_id},snapgroup-trash=,snapgroup-trash-reason="),
    )
}

/// Fixa um membro: tira-o da retenção. Re-afirma o id pelo mesmo motivo do
/// `trash` (merge-vs-replace do `modify --userdata`).
pub fn pin(config: &str, number: u32, group_id: i64) -> Result<()> {
    modify_userdata(
        config,
        number,
        &format!("snapgroup-id={group_id},{}=0", crate::group::AUTO_KEY),
    )
}

/// Mais uma transação da execução que o checkpoint cobre. Re-afirma o id pelo
/// mesmo motivo do `trash`.
pub fn set_session_transactions(config: &str, number: u32, group_id: i64, count: usize) -> Result<()> {
    modify_userdata(
        config,
        number,
        &format!("snapgroup-id={group_id},{}={count}", crate::group::SESSION_TX_KEY),
    )
}

fn modify_userdata(config: &str, number: u32, userdata: &str) -> Result<()> {
    let n = number.to_string();
    let out = Command::new("snapper")
        .args(["-c", config, "modify", "--userdata", userdata, &n])
        .output()
        .context("snapper modify falhou")?;
    if !out.status.success() {
        bail!(
            "snapper modify -c {config} {n}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

pub fn modify_description(config: &str, number: u32, description: &str) -> Result<()> {
    let n = number.to_string();
    let out = Command::new("snapper")
        .args(["-c", config, "modify", "--description", description, &n])
        .output()
        .context("snapper modify falhou")?;
    if !out.status.success() {
        bail!(
            "snapper modify -c {config} {n}: {}",
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
