use crate::snapper::{self, Snapshot};
use crate::{boot, rollback};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

pub type GroupId = i64;

const USERDATA_KEY: &str = "snapgroup-id";
const TRASH_KEY: &str = "snapgroup-trash";
const ORIGIN_KIND_KEY: &str = "snapgroup-origin";
const ORIGIN_DATE_KEY: &str = "snapgroup-origin-date";
pub const AUTO_KEY: &str = "snapgroup-auto";
/// Quem criou o grupo sem o usuário pedir. Hoje só `pacman` (o hook).
pub const TRIGGER_KEY: &str = "snapgroup-trigger";
pub const TRIGGER_PACMAN: &str = "pacman";
/// Tamanho do `pacman.log` no instante do save: a transação do grupo começa ali.
pub const PACMAN_LOG_KEY: &str = "snapgroup-pacman-log";
/// Inode do `pacman.log` no save: um log trocado por outro não vale a posição.
pub const PACMAN_LOG_INO_KEY: &str = "snapgroup-pacman-log-ino";
/// Operação da primeira transação do checkpoint (`-Syu`, `-U`).
pub const PACMAN_OP_KEY: &str = "snapgroup-pacman-op";
/// Execução do ajudante (paru, makepkg) que o checkpoint cobre: `paru:<pid>-<início>`.
pub const SESSION_KEY: &str = "snapgroup-session";
/// Quantas transações a execução já fez (sem a chave: 1).
pub const SESSION_TX_KEY: &str = "snapgroup-session-tx";
const DEFAULT_NAME_PREFIX: &str = "Checkpoint ";
/// Nome padrão antes da 0.8.0: `snapg save <id>`.
const LEGACY_DEFAULT_NAME_PREFIX: &str = "snapg save ";

#[derive(Clone)]
pub struct Member {
    pub config: String,
    pub snapshot: Snapshot,
}

#[derive(Clone)]
pub struct Group {
    pub id: GroupId,
    pub members: Vec<Member>,
}

/// Data do grupo: todos os membros compartilham o instante do save, então o
/// primeiro membro representa o grupo. Fallback só dispara em grupo sem membros.
pub fn date(group: &Group) -> &str {
    group
        .members
        .first()
        .map(|m| snapshot_date(&m.snapshot))
        .unwrap_or("data desconhecida")
}

/// Descrição do grupo (do primeiro membro). Vazio é um valor legítimo.
pub fn description(group: &Group) -> &str {
    group
        .members
        .first()
        .map(|m| m.snapshot.description.as_str())
        .unwrap_or("")
}

pub fn extract_id(s: &Snapshot) -> Option<GroupId> {
    userdata_i64(s, USERDATA_KEY)
}

/// Epoch em que o snapshot foi marcado pra lixeira, se foi.
pub fn extract_trash(s: &Snapshot) -> Option<i64> {
    userdata_i64(s, TRASH_KEY)
}

pub fn snapshot_date(s: &Snapshot) -> &str {
    userdata_str(s, ORIGIN_DATE_KEY).unwrap_or(&s.date)
}

pub fn origin_date(group: &Group) -> Option<&str> {
    group
        .members
        .first()
        .and_then(|m| userdata_str(&m.snapshot, ORIGIN_DATE_KEY))
}

pub fn is_regret_origin(group: &Group) -> bool {
    group
        .members
        .first()
        .and_then(|m| userdata_str(&m.snapshot, ORIGIN_KIND_KEY))
        == Some("regret")
}

fn userdata_i64(s: &Snapshot, key: &str) -> Option<i64> {
    userdata_str(s, key)?.parse().ok()
}

fn userdata_str<'a>(s: &'a Snapshot, key: &str) -> Option<&'a str> {
    s.userdata.as_ref()?.as_object()?.get(key)?.as_str()
}

/// Grupo está na lixeira se QUALQUER membro carrega a marca. Um grupo
/// meio-marcado (falha best-effort no meio) ainda some do restore — não faz
/// sentido restaurar um grupo a caminho da saída — e o próximo purge termina.
pub fn is_trashed(group: &Group) -> bool {
    group.members.iter().any(|m| extract_trash(&m.snapshot).is_some())
}

/// Instante em que o grupo entrou na lixeira: o mais antigo entre os membros
/// marcados. O purge compara isto contra a janela de carência.
pub fn trash_epoch(group: &Group) -> Option<i64> {
    group.members.iter().filter_map(|m| extract_trash(&m.snapshot)).min()
}

/// Nome que `default_checkpoint_name` gera: `Checkpoint YYYY-MM-DD HH:MM`, ou
/// `Checkpoint <epoch>` quando o `date` falha; antes da 0.8.0, `snapg save
/// <id>`. Só decide grupos antigos, sem a marca `snapgroup-auto`; qualquer
/// outro texto conta como nome escolhido.
pub fn is_default_name(description: &str) -> bool {
    if let Some(id) = description.strip_prefix(LEGACY_DEFAULT_NAME_PREFIX) {
        return !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
    }
    let Some(rest) = description.strip_prefix(DEFAULT_NAME_PREFIX) else {
        return false;
    };
    let bytes = rest.as_bytes();
    if !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit) {
        return true;
    }
    const DATE_SHAPE: &[u8] = b"dddd-dd-dd dd:dd";
    bytes.len() == DATE_SHAPE.len()
        && bytes.iter().zip(DATE_SHAPE).all(|(b, shape)| match shape {
            b'd' => b.is_ascii_digit(),
            _ => b == shape,
        })
}

/// Membro automático: salvo com o nome padrão. A marca gravada pelo `save` (e
/// zerada pelo `rename`) decide; sem ela, grupo anterior à marca, vale o nome.
fn member_is_auto(s: &Snapshot) -> bool {
    match userdata_str(s, AUTO_KEY) {
        Some(value) => value == "1",
        None => is_default_name(&s.description),
    }
}

/// Grupo automático entra na retenção (`KEEP_GROUPS`); o resto é fixo e só sai
/// pelo `delete`. Exige todos os membros: um rename interrompido deixa o grupo
/// misto, e na dúvida ele fica fixo.
pub fn is_auto(group: &Group) -> bool {
    !group.members.is_empty() && group.members.iter().all(|m| member_is_auto(&m.snapshot))
}

/// Âncora no `pacman.log` gravada no save (`None` em grupos anteriores a ela).
pub fn pacman_log_anchor(group: &Group) -> Option<crate::pacman_log::Anchor> {
    Some(crate::pacman_log::Anchor {
        offset: first_userdata(group, PACMAN_LOG_KEY)?.parse().ok()?,
        ino: first_userdata(group, PACMAN_LOG_INO_KEY).and_then(|v| v.parse().ok()),
    })
}

fn first_userdata<'a>(group: &'a Group, key: &str) -> Option<&'a str> {
    userdata_str(&group.members.first()?.snapshot, key)
}

pub fn pacman_session(group: &Group) -> Option<&str> {
    first_userdata(group, SESSION_KEY)
}

pub fn pacman_op(group: &Group) -> Option<&str> {
    first_userdata(group, PACMAN_OP_KEY)
}

/// O ajudante da execução (`paru`, `makepkg`), do começo da sessão.
pub fn pacman_helper(group: &Group) -> Option<&str> {
    pacman_session(group)?.split_once(':').map(|(name, _)| name)
}

/// Transações que o checkpoint cobre: as de uma execução do paru, ou 1.
pub fn pacman_transactions(group: &Group) -> usize {
    first_userdata(group, SESSION_TX_KEY).and_then(|v| v.parse().ok()).unwrap_or(1)
}

/// Grupo criado pelo hook do pacman.
pub fn is_pacman(group: &Group) -> bool {
    first_userdata(group, TRIGGER_KEY) == Some(TRIGGER_PACMAN)
}

/// Grupos a mover pra lixeira (`KEEP_GROUPS`): entre os automáticos do `save`,
/// a cauda além dos `keep` mais novos. Fixos e grupos do pacman não entram na
/// conta. `keep == 0` (ilimitado) → nada. `groups` deve vir newest-first, como
/// `list_groups` entrega.
pub fn groups_to_prune(groups: &[Group], keep: usize) -> Vec<&Group> {
    if keep == 0 {
        return Vec::new();
    }
    groups.iter().filter(|g| is_auto(g) && !is_pacman(g)).skip(keep).collect()
}

/// Mesma regra para a fila do pacman (`KEEP_PACMAN`). Renomear fixa o grupo
/// (`snapgroup-auto=0`) e o tira desta fila também.
pub fn pacman_groups_to_prune(groups: &[Group], keep: usize) -> Vec<&Group> {
    if keep == 0 {
        return Vec::new();
    }
    groups.iter().filter(|g| is_auto(g) && is_pacman(g)).skip(keep).collect()
}

/// Ids em uso, vivos ou na lixeira.
pub fn all_group_ids() -> Result<Vec<GroupId>> {
    Ok(scan_all_groups()?.into_iter().map(|g| g.id).collect())
}

/// Grupos vivos (visíveis): exclui os que estão na lixeira.
pub fn list_groups() -> Result<Vec<Group>> {
    Ok(scan_all_groups()?.into_iter().filter(|g| !is_trashed(g)).collect())
}

/// Grupos na lixeira: o purge consome esta lista.
pub fn list_trashed_groups() -> Result<Vec<Group>> {
    Ok(scan_all_groups()?.into_iter().filter(is_trashed).collect())
}

fn scan_all_groups() -> Result<Vec<Group>> {
    let configs = snapper::list_configs()?;
    let mut by_id: HashMap<GroupId, Vec<Member>> = HashMap::new();
    for cfg in &configs {
        for snap in snapper::list(cfg)? {
            let Some(id) = extract_id(&snap) else {
                continue;
            };
            by_id.entry(id).or_default().push(Member {
                config: cfg.clone(),
                snapshot: snap,
            });
        }
    }
    let mut groups: Vec<Group> = by_id
        .into_iter()
        .map(|(id, mut members)| {
            // Ordem estável dentro do grupo: alfabética por nome de config.
            members.sort_by(|a, b| a.config.cmp(&b.config));
            Group { id, members }
        })
        .collect();
    // Mais recente primeiro (epoch decrescente).
    groups.sort_by_key(|g| std::cmp::Reverse(g.id));
    Ok(groups)
}

pub fn kernel_labels(groups: &[Group], toplevel: &Path) -> HashMap<GroupId, String> {
    groups
        .iter()
        .map(|g| (g.id, kernel_label(g, toplevel)))
        .collect()
}

fn kernel_label(group: &Group, toplevel: &Path) -> String {
    let Some(root_m) = root_member(group).ok().flatten() else {
        return "?".to_string();
    };
    rollback::member_snapshot_path(root_m, toplevel)
        .map(|path| boot::kernel_label(&path))
        .unwrap_or_else(|_| "?".to_string())
}

pub fn root_member(group: &Group) -> Result<Option<&Member>> {
    for member in &group.members {
        let mountpoint = snapper::config_subvolume(&member.config)?;
        if mountpoint == "/" {
            return Ok(Some(member));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{
        Group, GroupId, Member, date, groups_to_prune, is_auto, is_default_name, is_pacman,
        is_regret_origin, origin_date, pacman_groups_to_prune,
    };
    use crate::snapper::Snapshot;
    use serde_json::{Value, json};

    fn snapshot(description: &str, userdata: Option<Value>) -> Snapshot {
        Snapshot {
            number: 1,
            kind: "single".to_string(),
            date: "2026-11-12 10:00:00".to_string(),
            user: "root".to_string(),
            description: description.to_string(),
            cleanup: String::new(),
            userdata,
        }
    }

    fn named_group(id: GroupId, description: &str, userdata: Option<Value>) -> Group {
        Group {
            id,
            members: vec![Member {
                config: "root".to_string(),
                snapshot: snapshot(description, userdata),
            }],
        }
    }

    fn auto_groups(ids: &[GroupId]) -> Vec<Group> {
        ids.iter()
            .map(|&id| named_group(id, "Checkpoint 2026-11-12 10:00", None))
            .collect()
    }

    fn pruned_ids(groups: &[Group], keep: usize) -> Vec<GroupId> {
        groups_to_prune(groups, keep).iter().map(|g| g.id).collect()
    }

    #[test]
    fn keep_zero_prunes_nothing() {
        let g = auto_groups(&[5, 4, 3, 2, 1]);
        assert!(pruned_ids(&g, 0).is_empty());
    }

    #[test]
    fn fewer_or_equal_than_keep_prunes_nothing() {
        let g = auto_groups(&[3, 2, 1]);
        assert!(pruned_ids(&g, 3).is_empty());
        assert!(pruned_ids(&g, 5).is_empty());
    }

    #[test]
    fn prunes_tail_beyond_keep() {
        // newest-first: mantém os 2 mais novos (5, 4), poda o resto.
        let g = auto_groups(&[5, 4, 3, 2, 1]);
        assert_eq!(pruned_ids(&g, 2), vec![3, 2, 1]);
    }

    #[test]
    fn pinned_groups_neither_count_nor_get_pruned() {
        let g = vec![
            named_group(6, "Antes do driver", None),
            named_group(5, "Checkpoint 2026-11-12 10:00", None),
            named_group(4, "Certo", None),
            named_group(3, "Checkpoint 2026-11-11 10:00", None),
            named_group(2, "Checkpoint 2026-11-10 10:00", None),
            named_group(1, "Atual-1", None),
        ];
        // Os 2 automáticos mais novos (5, 3) ficam; só o 2 sai. Fixos ficam.
        assert_eq!(pruned_ids(&g, 2), vec![2]);
    }

    #[test]
    fn default_name_matches_only_what_save_generates() {
        assert!(is_default_name("Checkpoint 2026-11-12 00:06"));
        assert!(is_default_name("Checkpoint 1794456375"));
        assert!(!is_default_name("Checkpoint antes do driver"));
        assert!(!is_default_name("Checkpoint 2026-11-12 00:06 bom"));
        assert!(!is_default_name("Checkpoint 2026-11-12"));
        assert!(!is_default_name("Checkpoint "));
        assert!(!is_default_name("checkpoint 2026-11-12 00:06"));
        assert!(!is_default_name("Regret 2026-11-12 00:06"));
        assert!(is_default_name("snapg save 1781249891"));
        assert!(!is_default_name("snapg save teste"));
        assert!(!is_default_name("snapg save "));
    }

    #[test]
    fn auto_mark_wins_over_the_name() {
        let marked = json!({"snapgroup-id": "1", "snapgroup-auto": "1"});
        let pinned = json!({"snapgroup-id": "1", "snapgroup-auto": "0"});
        assert!(is_auto(&named_group(1, "Checkpoint 2026-11-12 10:00", Some(marked.clone()))));
        assert!(is_auto(&named_group(1, "qualquer", Some(marked))));
        assert!(!is_auto(&named_group(1, "Checkpoint 2026-11-12 10:00", Some(pinned))));
        // Sem a marca (grupo antigo), decide o nome.
        assert!(is_auto(&named_group(1, "Checkpoint 2026-11-12 10:00", Some(json!({"snapgroup-id": "1"})))));
        assert!(!is_auto(&named_group(1, "Teste restore", Some(json!({"snapgroup-id": "1"})))));
    }

    fn pacman_group(id: GroupId, auto: &str) -> Group {
        named_group(
            id,
            "pacman -Syu",
            Some(json!({"snapgroup-id": id.to_string(), "snapgroup-auto": auto, "snapgroup-trigger": "pacman"})),
        )
    }

    #[test]
    fn pacman_and_save_queues_do_not_touch_each_other() {
        // newest-first: 9..5 do pacman, depois 4..1 automáticos do save.
        let mut groups: Vec<Group> = (5..=9).rev().map(|id| pacman_group(id, "1")).collect();
        groups.extend(auto_groups(&[4, 3, 2, 1]));
        let ids = |v: Vec<&Group>| v.iter().map(|g| g.id).collect::<Vec<_>>();
        assert_eq!(ids(groups_to_prune(&groups, 2)), vec![2, 1]);
        assert_eq!(ids(pacman_groups_to_prune(&groups, 2)), vec![7, 6, 5]);
        assert!(pacman_groups_to_prune(&groups, 0).is_empty());
    }

    #[test]
    fn pinned_pacman_group_leaves_the_queue() {
        let groups = vec![pacman_group(3, "1"), pacman_group(2, "0"), pacman_group(1, "1")];
        assert!(is_pacman(&groups[1]));
        let pruned: Vec<_> = pacman_groups_to_prune(&groups, 1).iter().map(|g| g.id).collect();
        assert_eq!(pruned, vec![1]);
        assert!(groups_to_prune(&groups, 1).is_empty());
    }

    #[test]
    fn mixed_or_empty_groups_count_as_pinned() {
        let mut mixed = named_group(1, "Checkpoint 2026-11-12 10:00", None);
        mixed.members.push(Member {
            config: "home".to_string(),
            snapshot: snapshot("Checkpoint 2026-11-12 10:00", Some(json!({"snapgroup-auto": "0"}))),
        });
        assert!(!is_auto(&mixed));
        assert!(!is_auto(&Group { id: 1, members: Vec::new() }));
    }

    #[test]
    fn group_date_prefers_regret_origin_date() {
        let group = Group {
            id: 1,
            members: vec![Member {
                config: "root".to_string(),
                snapshot: Snapshot {
                    number: 10,
                    kind: "single".to_string(),
                    date: "2026-06-17 15:10:00".to_string(),
                    user: "root".to_string(),
                    description: "Regret 2026-06-17 14:34".to_string(),
                    cleanup: String::new(),
                    userdata: Some(json!({
                        "snapgroup-id": "1",
                        "snapgroup-origin": "regret",
                        "snapgroup-origin-date": "2026-06-17T14:34:12-0400"
                    })),
                },
            }],
        };

        assert_eq!(date(&group), "2026-06-17T14:34:12-0400");
        assert_eq!(origin_date(&group), Some("2026-06-17T14:34:12-0400"));
        assert!(is_regret_origin(&group));
    }
}
