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

/// Grupos a mover pra lixeira: a cauda além dos `keep` mais novos. `keep == 0`
/// (ilimitado) ou grupos a menos que `keep` → nada. `groups` deve vir
/// newest-first, como `list_groups` entrega.
pub fn groups_to_prune(groups: &[Group], keep: usize) -> &[Group] {
    if keep == 0 || groups.len() <= keep {
        return &[];
    }
    &groups[keep..]
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
        Group, GroupId, Member, date, groups_to_prune, is_regret_origin, origin_date,
    };
    use crate::snapper::Snapshot;
    use serde_json::json;

    fn groups(ids: &[GroupId]) -> Vec<Group> {
        ids.iter()
            .map(|&id| Group { id, members: Vec::new() })
            .collect()
    }

    fn pruned_ids(groups: &[Group], keep: usize) -> Vec<GroupId> {
        groups_to_prune(groups, keep).iter().map(|g| g.id).collect()
    }

    #[test]
    fn keep_zero_prunes_nothing() {
        let g = groups(&[5, 4, 3, 2, 1]);
        assert!(pruned_ids(&g, 0).is_empty());
    }

    #[test]
    fn fewer_or_equal_than_keep_prunes_nothing() {
        let g = groups(&[3, 2, 1]);
        assert!(pruned_ids(&g, 3).is_empty());
        assert!(pruned_ids(&g, 5).is_empty());
    }

    #[test]
    fn prunes_tail_beyond_keep() {
        // newest-first: mantém os 2 mais novos (5, 4), poda o resto.
        let g = groups(&[5, 4, 3, 2, 1]);
        assert_eq!(pruned_ids(&g, 2), vec![3, 2, 1]);
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
