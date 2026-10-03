<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Copyright (C) 2026 kaayzouee -->
<!-- Author: https://github.com/kaayzouee -->

# neodots-installer

A small Rust preflight scanner for the Neodots NixOS installer.

## Current checks

- `/etc/nixos/hardware-configuration.nix`
- `git` in `PATH`
- `sudo` or `doas` in `PATH`

After those checks pass, the scanner asks for permission to clone
[`kaayzouee/neodots`](https://github.com/kaayzouee/neodots) into a temporary
directory. It copies the local hardware configuration into that checkout and
runs `nix flake check --no-build`. The checkout is removed afterwards; the
scanner does not modify `/etc/nixos` or apply the configuration.

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

The machine-aware installer roadmap is tracked in
[Neodots issue #2](https://github.com/kaayzouee/neodots/issues/2).
