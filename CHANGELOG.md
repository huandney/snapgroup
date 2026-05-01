# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-beta] - 2026-04-30
> Commits: `0efaf56`

### Architecture & Core
- **Refactor**: Remove residual 'else' blocks to enforce 100% Zero-Else policy across CLI commands, eliminating nested control flow.
- **Refine**: Condense multi-line `println!` and `eprintln!` macros into single lines to improve horizontal scannability and procedural reading style.
- **Fix**: Arm boot-cleanup service correctly in the restored rootfs using `systemctl --root` during the `redo` execution.
