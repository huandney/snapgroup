use crate::btrfs;
use crate::group::{self, Group};
use crate::rollback;
use crate::snapper;
use crate::ui::rename as rename_ui;
use anyhow::{Context, Result, bail};

pub fn run(id: Option<group::GroupId>, description: Vec<String>) -> Result<()> {
    let configs = snapper::list_configs()?;
    let groups = group::list_groups()?;
    if groups.is_empty() {
        rename_ui::print_no_groups();
        return Ok(());
    }

    let plan = match id {
        Some(id) => direct_plan(id, description, &groups)?,
        None => interactive_plan(&groups)?,
    };
    let Some((group_index, new_description)) = plan else {
        rename_ui::print_cancelled();
        return Ok(());
    };

    rename_group(&groups[group_index], &new_description, configs.len())
}

fn direct_plan(
    id: group::GroupId,
    description: Vec<String>,
    groups: &[Group],
) -> Result<Option<(usize, String)>> {
    let Some(index) = groups.iter().position(|g| g.id == id) else {
        bail!("checkpoint #{id} não encontrado");
    };
    if !description.is_empty() {
        return Ok(Some((index, description.join(" "))));
    }
    Ok(rename_ui::prompt_description_screen(&groups[index])?.map(|v| (index, v)))
}

fn interactive_plan(groups: &[Group]) -> Result<Option<(usize, String)>> {
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;
    let result = {
        let kernel_labels = crate::commands::group_kernel_labels(groups, &mount_path);
        rename_ui::select_plan(groups, &kernel_labels)
    };
    let _ = btrfs::umount_toplevel(&mount_path);
    result
}

fn rename_group(group: &Group, description: &str, expected_members: usize) -> Result<()> {
    // Compara contra TODOS os membros, não só o primeiro: um rename anterior
    // interrompido (Ctrl-C no meio do loop) deixa descrições divergentes, e
    // re-rodar com o mesmo nome precisa reparar os membros que ficaram para trás.
    if group
        .members
        .iter()
        .all(|m| m.snapshot.description == description)
    {
        rename_ui::print_unchanged(group.id);
        return Ok(());
    }

    let mut changed: Vec<(String, u32, String)> = Vec::new();
    for member in &group.members {
        if let Err(e) =
            snapper::modify_description(&member.config, member.snapshot.number, description)
        {
            for (config, number, old_description) in changed.into_iter().rev() {
                let _ = snapper::modify_description(&config, number, &old_description);
            }
            return Err(e).with_context(|| {
                format!(
                    "renomear {} #{} para checkpoint #{}",
                    member.config, member.snapshot.number, group.id
                )
            });
        }
        changed.push((
            member.config.clone(),
            member.snapshot.number,
            member.snapshot.description.clone(),
        ));
    }

    rename_ui::print_done(group, description, expected_members);
    Ok(())
}

