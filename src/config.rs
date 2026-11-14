use std::fs;

pub const PATH: &str = "/etc/snapgroup.conf";

/// Teto da carência: 10 anos. Acima disso, `dias * 86_400` no cálculo do cutoff
/// estouraria `i64` e, no release (wrap silencioso), viraria negativo —
/// purgando a lixeira inteira. Também pega typo (um valor absurdo).
pub const MAX_PRUNE_DAYS: u64 = 3650;

/// Config persistente do snapgroup. Arquivo ausente ou chave inválida cai no
/// default — o snapgroup nasceu zero-config e instalar a feature não pode
/// mudar comportamento de ninguém sem opt-in.
pub struct Config {
    /// Quantos grupos automáticos (nome padrão) manter vivos; fixos não contam.
    /// `0` = ilimitado (comportamento legado).
    pub keep_groups: usize,
    /// Quantos grupos do hook do pacman manter vivos, numa fila própria: um
    /// dia de muitas transações não empurra os automáticos do `save` para a
    /// lixeira. `0` = ilimitado.
    pub keep_pacman: usize,
    /// Dias que um grupo fica na lixeira antes do purge apagar de vez.
    /// `u64`: dias negativos não existem, e um valor negativo poria o cutoff
    /// no futuro e purgaria a lixeira inteira na hora.
    pub trash_prune_days: u64,
    /// O que o seletor do restore mostra ao abrir (Tab troca).
    pub restore_filter: RestoreFilter,
}

/// Recorte do seletor do restore. O Regret conta como manual.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RestoreFilter {
    All,
    Manual,
    Pacman,
}

impl RestoreFilter {
    pub const ORDER: [RestoreFilter; 3] = [RestoreFilter::All, RestoreFilter::Manual, RestoreFilter::Pacman];

    pub fn label(self) -> &'static str {
        match self {
            RestoreFilter::All => "Todos",
            RestoreFilter::Manual => "Manuais",
            RestoreFilter::Pacman => "Pacman",
        }
    }

    pub fn shows(self, pacman: bool) -> bool {
        match self {
            RestoreFilter::All => true,
            RestoreFilter::Manual => !pacman,
            RestoreFilter::Pacman => pacman,
        }
    }

    pub fn next(self) -> RestoreFilter {
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        Self::ORDER[(i + 1) % Self::ORDER.len()]
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            keep_groups: 0,
            // Diferente de 0 de propósito: o hook cria grupos sozinhos, e sem
            // teto eles cresceriam para sempre (o snap-pac parava em 50).
            keep_pacman: 20,
            trash_prune_days: 15,
            restore_filter: RestoreFilter::All,
        }
    }
}

/// Grava `KEY=valor` no `/etc/snapgroup.conf` (`snapg setup`). Troca só as
/// linhas da chave e deixa os comentários; a chave ausente vai para o fim. O
/// arquivo é um `backup=` do pacote: alterado, um upgrade gera `.pacnew` em
/// vez de sobrescrever. Grava num temporário e renomeia, para um corte no meio
/// não deixar o arquivo pela metade.
pub fn set(key: &str, value: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    let current = fs::read_to_string(PATH).unwrap_or_default();
    let tmp = format!("{PATH}.snapg-tmp");
    fs::write(&tmp, update_key(&current, key, value)).with_context(|| format!("gravar {tmp}"))?;
    fs::rename(&tmp, PATH).with_context(|| format!("substituir {PATH}"))
}

fn update_key(content: &str, key: &str, value: &str) -> String {
    let mut found = false;
    let mut out: Vec<String> = content
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            let is_key = !trimmed.starts_with('#')
                && trimmed.split_once('=').is_some_and(|(k, _)| k.trim() == key);
            if !is_key {
                return line.to_string();
            }
            // Todas as ocorrências: o parser fica com a última.
            found = true;
            format!("{key}={value}")
        })
        .collect();
    if !found {
        out.push(format!("{key}={value}"));
    }
    out.join("\n") + "\n"
}

pub fn load() -> Config {
    let Ok(content) = fs::read_to_string(PATH) else {
        return Config::default();
    };
    parse(&content)
}

/// Parser shell-like (`KEY=valor`), mesmo formato dos configs do snapper que o
/// projeto já lê. Sem crate de TOML — uma chave inteira não justifica a dep.
/// Linha malformada ou valor não-numérico mantém o default daquele campo.
fn parse(content: &str) -> Config {
    let mut cfg = Config::default();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let val = val.trim().trim_matches('"');
        match key.trim() {
            "KEEP_GROUPS" => {
                if let Ok(n) = val.parse() {
                    cfg.keep_groups = n;
                }
            }
            "KEEP_PACMAN" => {
                if let Ok(n) = val.parse() {
                    cfg.keep_pacman = n;
                }
            }
            "RESTORE_FILTER" => match val.to_ascii_lowercase().as_str() {
                "todos" => cfg.restore_filter = RestoreFilter::All,
                "manuais" => cfg.restore_filter = RestoreFilter::Manual,
                "pacman" => cfg.restore_filter = RestoreFilter::Pacman,
                _ => {}
            },
            "TRASH_PRUNE_DAYS" => {
                // Valor grande sinaliza "reter muito" → clampa no teto seguro,
                // em vez de cair no default e encolher a janela em silêncio.
                if let Ok(n) = val.parse::<u64>() {
                    cfg.trash_prune_days = n.min(MAX_PRUNE_DAYS);
                }
            }
            _ => {}
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn empty_content_yields_defaults() {
        let cfg = parse("");
        assert_eq!(cfg.keep_groups, 0);
        assert_eq!(cfg.keep_pacman, 20);
        assert_eq!(cfg.trash_prune_days, 15);
    }

    #[test]
    fn update_key_keeps_comments_and_replaces_only_the_key() {
        let conf = "# Quantos manter\nKEEP_GROUPS=0\n\n# Dias\nTRASH_PRUNE_DAYS = 15\n";
        let out = super::update_key(conf, "KEEP_GROUPS", "10");
        assert_eq!(out, "# Quantos manter\nKEEP_GROUPS=10\n\n# Dias\nTRASH_PRUNE_DAYS = 15\n");
        // Comentada não conta; ausente vai para o fim.
        let out = super::update_key("# KEEP_PACMAN=5\n", "KEEP_PACMAN", "20");
        assert_eq!(out, "# KEEP_PACMAN=5\nKEEP_PACMAN=20\n");
        assert_eq!(super::update_key("", "RESTORE_FILTER", "pacman"), "RESTORE_FILTER=pacman\n");
        assert_eq!(parse(&super::update_key(conf, "TRASH_PRUNE_DAYS", "30")).trash_prune_days, 30);
    }

    #[test]
    fn reads_restore_filter() {
        use super::RestoreFilter;
        assert_eq!(parse("").restore_filter, RestoreFilter::All);
        assert_eq!(parse("RESTORE_FILTER=manuais\n").restore_filter, RestoreFilter::Manual);
        assert_eq!(parse("RESTORE_FILTER=\"Pacman\"\n").restore_filter, RestoreFilter::Pacman);
        assert_eq!(parse("RESTORE_FILTER=outro\n").restore_filter, RestoreFilter::All);
    }

    #[test]
    fn reads_keep_pacman() {
        assert_eq!(parse("KEEP_PACMAN=5\n").keep_pacman, 5);
        assert_eq!(parse("KEEP_PACMAN=0\n").keep_pacman, 0);
        assert_eq!(parse("KEEP_PACMAN=-3\n").keep_pacman, 20);
    }

    #[test]
    fn reads_both_keys_ignoring_comments_and_quotes() {
        let cfg = parse("# comentário\nKEEP_GROUPS = 5\nTRASH_PRUNE_DAYS=\"30\"\n");
        assert_eq!(cfg.keep_groups, 5);
        assert_eq!(cfg.trash_prune_days, 30);
    }

    #[test]
    fn invalid_value_keeps_field_default() {
        let cfg = parse("KEEP_GROUPS=abc\nTRASH_PRUNE_DAYS=7\n");
        assert_eq!(cfg.keep_groups, 0);
        assert_eq!(cfg.trash_prune_days, 7);
    }

    #[test]
    fn negative_prune_days_keeps_default() {
        // Negativo poria o cutoff no futuro e purgaria tudo na hora; u64 rejeita.
        let cfg = parse("TRASH_PRUNE_DAYS=-1\n");
        assert_eq!(cfg.trash_prune_days, 15);
    }

    #[test]
    fn huge_prune_days_clamps_to_max() {
        // Sem teto, dias*86400 estoura i64 e o cutoff vira negativo no release.
        let cfg = parse("TRASH_PRUNE_DAYS=999999999999\n");
        assert_eq!(cfg.trash_prune_days, super::MAX_PRUNE_DAYS);
    }

    #[test]
    fn unknown_key_is_ignored() {
        let cfg = parse("FOO=bar\nKEEP_GROUPS=2\n");
        assert_eq!(cfg.keep_groups, 2);
    }
}
