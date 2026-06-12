use crate::snapper::{self, Snapshot};
use crate::{boot, rollback};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

pub type GroupId = i64;

const USERDATA_KEY: &str = "snapgroup-id";

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
        .map(|m| m.snapshot.date.as_str())
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
    s.userdata
        .as_ref()?
        .as_object()?
        .get(USERDATA_KEY)?
        .as_str()?
        .parse()
        .ok()
}

pub fn list_groups() -> Result<Vec<Group>> {
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
