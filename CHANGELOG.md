# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
