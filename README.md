<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Copyright (C) 2026 kaayzouee -->
<!-- Author: https://github.com/kaayzouee -->

# neodots-installer

A small Rust preflight scanner for the Neodots NixOS installer.

## Current checks

- `/etc/nixos/hardware-configuration.nix`
- `git` in `PATH`
- `sudo` or `doas` in `PATH`

When the hardware configuration is missing, the scanner prints the command that can generate it and asks whether it should run it.

## Build

```bash
cargo build --release
```

## Run

```bash
cargo run --release
```

The scanner is intentionally dependency-free at this stage. The TUI can be layered on top of these checks later without changing the detection logic.
