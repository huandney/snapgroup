use std::fs;

const PATH: &str = "/etc/snapgroup.conf";

/// Config persistente do snapgroup. Arquivo ausente ou chave inválida cai no
/// default — o snapgroup nasceu zero-config e instalar a feature não pode
/// mudar comportamento de ninguém sem opt-in.
pub struct Config {
    /// Quantos grupos snapg manter vivos. `0` = ilimitado (comportamento legado).
    pub keep_groups: usize,
    /// Dias que um grupo fica na lixeira antes do purge apagar de vez.
    /// `u64`: dias negativos não existem, e um valor negativo poria o cutoff
    /// no futuro e purgaria a lixeira inteira na hora.
    pub trash_prune_days: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            keep_groups: 0,
            trash_prune_days: 15,
        }
    }
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
            "TRASH_PRUNE_DAYS" => {
                if let Ok(n) = val.parse() {
                    cfg.trash_prune_days = n;
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
        assert_eq!(cfg.trash_prune_days, 15);
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
    fn unknown_key_is_ignored() {
        let cfg = parse("FOO=bar\nKEEP_GROUPS=2\n");
        assert_eq!(cfg.keep_groups, 2);
    }
}
