use crate::snapper::{self, Snapshot};
use anyhow::Result;
use std::collections::HashMap;

pub type GroupId = i64;

const USERDATA_KEY: &str = "snapgroup-id";

pub struct Member {
    pub config: String,
    pub snapshot: Snapshot,
}

pub struct Group {
    pub id: GroupId,
    pub members: Vec<Member>,
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
