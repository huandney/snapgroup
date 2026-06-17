# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.8.0-beta] - 2026-06-17
> Commits: `b29a1ab`

### Restore / TUI
- **Feature**: Selecting the Regret in `snapg restore` now asks whether to restore it or keep it as a normal checkpoint. Keeping it creates grouped Snapper snapshots from the `_snapg_regret` subvolumes, preserves the Regret, and records the Regret origin date in userdata so the UI shows the point being saved instead of the time the copy was made. While that Regret remains active, `list` and `restore` show `Regret guardado` and hide the duplicate checkpoint entry.
- **Refine**: Keeping a Regret reuses an equivalent checkpoint from the trash instead of creating another copy, and the trash groups/purges equivalent saved-Regret entries by origin date to avoid repeated identical rows and redundant disk usage.
- **Refine**: Automatic checkpoint names are now human-readable (`Checkpoint YYYY-MM-DD HH:MM` and `Regret YYYY-MM-DD HH:MM`) instead of epoch-based command echoes.
- **Refine**: The post-save confirmation line is no longer indented, matching the flush-left alignment of the other command results.

## [0.7.0-beta] - 2026-06-14
> Commits: `84138b2`, `c787fea`, `a0cc337`, `65d37e8`, `85bea69`

### Commands
- **Feature**: Group-aware automatic cleanup. With `KEEP_GROUPS=N` in `/etc/snapgroup.conf`, `snapg save` moves groups beyond the N newest to the trash; the default `0` keeps the previous unlimited behavior. Cleanup is group-aware on purpose — Snapper's own per-config retention would prune one subvolume's snapshot and leave its group half-broken.
- **Feature**: Recoverable trash pipeline. `snapg delete` now moves checkpoints to a trash (one review screen, no redundant "delete all" confirmation) instead of deleting outright; `snapg delete --purge` still deletes permanently. Trashed groups are hidden from `list`, `restore` and `rename`.
- **Feature**: New `snapg trash` to manage the trash — list trashed groups with a "purge in ~Nd" countdown, restore them to the live pool, or delete them permanently (the only confirmed action on that screen).
- **Feature**: Expired groups are purged automatically after `TRASH_PRUNE_DAYS` (default 15), piggybacking on `save`/`delete` without a new timer or daemon. The Regret is never touched — it is a renamed subvolume, not a Snapper snapshot.

### Config / Packaging
- **Feature**: New `/etc/snapgroup.conf` (`KEEP_GROUPS`, `TRASH_PRUNE_DAYS`), a shell-like file installed as a pacman backup so edits survive upgrades. Invalid or negative values fall back to the default; values above ten years are clamped, keeping the purge cutoff free of integer overflow.

## [0.6.0-beta] - 2026-06-12
> Commits: `fd686fa`, `9c2fbe2`, `14df553`, `960ab73`, `355ab4d`, `b0bfe17`, `32055ae`, `7b75ed2`, `e93d5ac`, `47b0dad`, `f59641c`, `e8160cc`, `16017c4`, `f0e6eb1`, `412dee8`, `084eb9b`, `6aafcda`

### Boot / Recovery
- **Fix**: Validate the BLAKE2B hashes recorded in `limine.conf` as part of the "boot ready" check. A vmlinuz byte-match alone could declare a `/boot` bootable that Limine would refuse at the bootloader stage after an interrupted sync.
- **Fix**: Always regenerate the initramfs on FAT32 restores. A DKMS driver update without a kernel change (e.g. nvidia) altered the initramfs while vmlinuz and hashes still matched, so the sync gate skipped regeneration and booted an initramfs incompatible with the restored root.
- **Fix**: Never offer a direct reboot for a pending restore on FAT32 `/boot`; the gate always resyncs first, covering interrupted syncs and `pacman`/DKMS drift while pending.
- **Fix**: Serialize `/boot` writes with the limine ecosystem's global mutex (`flock` on `/tmp/limine-global.lock`, 30s deadline), closing the race against pacman/mkinitcpio hooks.
- **Fix**: Doctor recovery no longer overwrites the legitimate Regret: the replaced state becomes a discard and the previous Regret is preserved.
- **Fix**: The rescue scan reads snapshot groups directly from the Btrfs top-level and only offers complete groups, instead of trusting a Snapper view that is blind during a rescue boot.

### Restore / Pending
- **Feature**: Pending restores are resolved through a gate shared by `restore`, `delete`, `save` and `rename`: finish (reboot, syncing `/boot` when needed) or cancel (return to the system still in use). Pending state is derived from the live mount, and the prompts preview destination subvol, expected kernel and what happens to `/boot`.
- **Fix**: Cancelling an interrupted Regret undo no longer deletes the Regret: the undo uses a dedicated `_snapg_undo_*` marker and cancel swaps everything back without deleting.
- **Fix**: Cancel syncs `/boot` before swapping subvols and gate reboots arm the boot cleanup, so an interruption can no longer leave a desynced `/boot` (or an orphaned subvol) invisible to the gate.

### Commands
- **Feature**: New `snapg rename` to edit a checkpoint's description across all members, interactively or via `snapg rename <id> [name...]`, with rollback on partial failure.
- **Feature**: `snapg save` without a name opens a small wizard showing members and kernel, with the auto-generated name as an accept-on-Enter placeholder. Non-TTY callers keep the silent auto-name.

### TUI
- **Refine**: Prompts run in the alternate screen and results append inline, app-wide — the terminal reads as a command/result transcript (doctor reports included). Esc hints now match where Esc actually goes, and the save confirmation uses the same mountpoint badges as `list`.

## [0.5.0-beta] - 2026-06-04
> Commits: `bb8208e`, `9461dfa`, `75ae17a`, `cb25b05`, `2939975`, `5a66b57`, `175fb1d`, `862d98a`, `5f65c69`, `9dec3ee`

### Boot / Recovery
- **Feature**: Add an assisted rescue flow in `snapg doctor` for interrupted `/boot` syncs and rescue boots, including target-root detection, backup discovery, kernel-aware choices, and a scoped root-only restore path.
- **Feature**: Stream restore execution output in-place so long `/boot` backup and `mkinitcpio` operations show live progress instead of leaving the TUI apparently frozen.
- **Fix**: Detect leftover `/boot` backups and unmounted FAT32 boot partitions instead of reporting a false all-clear after interrupted recovery work.
- **Fix**: Gate doctor sync with the same short-circuit/lock rules as restore so recovery actions do not rewrite `/boot` unnecessarily or race another mutating `snapg` command.

### Restore / TUI
- **Feature**: Refresh the restore, doctor, and delete screens with a consistent branded layout, bounded checkpoint summaries, clearer confirmation prompts, and paginated delete confirmation for large checkpoint sets.
- **Refine**: Warn about FAT32 `/boot` only when the selected restore changes kernel artifacts, keeping same-kernel restores quiet and fast.

## [0.4.0-beta] - 2026-06-01
> Commits: `c7d2d73`, `97b7072`, `d883c41`

### Restore / TUI
- **Feature**: Rework `snapg restore` into a staged flow: choose a checkpoint or Regret, select the members to restore, review a summary, then continue, go back, or abort. Checkpoint and Regret restores now support partial member selection while keeping rollback/asides scoped to the selected members.
- **Feature**: Unify the interactive UI across `list`/`save`/`delete`/`restore`/`doctor`: arrow-key navigation everywhere with Esc to step back, a consistent tree layout (branch connectors, dot separators, arrows), colored status glyphs, screen-clearing between steps, and minute-precision timestamps.

## [0.3.0-beta] - 2026-05-31
> Commits: `5301695`, `ddb79e9`, `09fd379`

### Boot / Recovery
- **Feature**: Add `snapg doctor` as an assisted boot diagnosis flow. It checks whether `/boot` matches the target root, reports when no action is needed, and can apply the existing FAT32 boot sync after confirmation.
- **Fix**: Harden `/boot` scanning so symlinks are ignored and only real files/directories are considered as boot artifacts.
- **Fix**: Treat non-zero `systemctl reboot -i` exits as restore errors instead of reporting success after a rejected reboot request.
- **Fix**: Make `boot-clean` use the global non-blocking lock and skip cleanly on contention, leaving the systemd unit armed for the next boot instead of mutating subvolumes concurrently with another `snapg` command.

## [0.2.1-beta] - 2026-05-29
> Commits: `ef0c2ad`, `76c9cee`, `00ba8b8`

### Restore / Rollback
- **Fix**: Preflight a single-filesystem invariant — `save` and `restore` abort if any Snapper config lives on a different BTRFS, before mounting or deleting anything (the tool mounts only `/`'s top-level and operates by relative subvolume path).
- **Fix**: Preserve the previous "Regret" instead of deleting it up front — it is moved aside before the rollback, restored if the failure leaves the live system in a known-clean state, preserved (with per-config path) on an ambiguous state, and discarded only on success. Closes the window where a prep-phase failure destroyed the regret with nothing in return.
- **Fix**: Global exclusive lock (`flock` on `/run/snapgroup.lock`) around `save`/`restore`/`delete` — prevents two instances from colliding on the fixed subvolume names and the `/run/snapgroup/{uuid}` mount.

## [0.2.0-beta] - 2026-05-28
> Commits: `22e5451`, `e1bd095`, `8ee0bcd`, `2c579bc`, `96c065c`, `992e04e`, `4af27c3`, `6155353`, `6ab7e34`

### Restore / Rollback
- **Feature**: Restore-highlander — grouped restore of subvolumes (root + home + ...) to a single coherent point, selectable via a TUI. `undo`/`redo` act on the whole group.
- **Feature**: "Regret" (pre-restore state) is preserved and restorable; a new `save` discards it (Highlander semantics — only one exists at a time).
- **Fix**: Fix a TUI line-wrapping bug and remove the multi-select that caused inconsistent selections.

### Boot (FAT32 / Limine)
- **Feature**: Sync the kernel and initramfs in `/boot` (FAT32) with the restored snapshot on rollback — copy the snapshot's `vmlinuz`, regenerate the initramfs from the restored root, and re-inject the BLAKE2B hashes into `limine.conf`.
- **Fix**: Block the reboot when `/boot` sync fails or ends up desynced (`verify_synced` gate) instead of warning and rebooting into a system that won't boot (Emergency Mode); exit non-zero in that case.
- **Fix**: Atomic initramfs write (temp + `rename`) — an interrupted `mkinitcpio` no longer leaves a partial initramfs on the active path.
- **Refine**: Skip the sync when `/boot` already matches the snapshot (same-kernel restore) — instant, and removes the interruption window.

## [0.1.0-beta] - 2026-04-30
> Commits: `0efaf56`, `58ee52f`

### Architecture & Core
- **Refine**: Condense multi-line `println!` and `eprintln!` macros into single lines to improve horizontal scannability and procedural reading style.
- **Refine**: Restore idiomatic `else` blocks in CLI output functions, enforcing the principle that readability overrides procedural dogma.
- **Fix**: Arm boot-cleanup service correctly in the restored rootfs using `systemctl --root` during the `redo` execution.
