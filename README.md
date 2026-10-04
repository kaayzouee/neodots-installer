<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Copyright (C) 2026 kaayzouee -->
<!-- Author: https://github.com/kaayzouee -->

# neodots-installer

A Rust installer/preflight tool for the Neodots NixOS configuration.

## Current flow

The installer operates on the target NixOS tree instead of cloning the
Neodots repository into a disposable checkout.

1. Verify that the target is NixOS.
2. Acquire the installer lock and recover interrupted transactions.
3. Check for `hardware-configuration.nix`, `git`, `nix`, and a privilege helper (`sudo` or `doas`).
4. Detect the machine architecture, username, hostname, home directory, and existing Neodots settings.
5. Let the operator select the username, hostname, personal configuration, and persistence settings. Press Enter to keep detected values.
6. Stage the full `/etc/nixos` tree, optionally generate hardware configuration in disposable staging, and render the selected `machine.nix`.
7. Run `nix flake check --no-build --no-write-lock-file` against the staged `/etc/nixos` tree.
8. Transactionally install `machine.nix` and any newly generated hardware configuration, with recovery data stored outside `/etc/nixos`.

The installer does not run `nixos-rebuild switch` yet, and it does not yet provide
wallpaper selection or full persistence mount validation. Those remain part of the
machine-aware installer roadmap in
[Neodots issue #2](https://github.com/kaayzouee/neodots/issues/2).

## Target overrides

For an installed system, the installer defaults to `/` as the target root.
For staging or tests, set:

```bash
NEODOTS_NIXOS_ROOT=/mnt
NEODOTS_TARGET_USER=myuser
```
When selecting a different username, the installer derives the home directory as
/home/<username> unless the existing home directory is preserved by keeping the
detected username.

### Build

`cargo build --release`

### Test

`cargo test --locked`

### Run

`cargo run --release`

The code is split into focused modules for configuration, preflight checks, target
handling, staging, transactional installation, privilege helpers, and terminal input.
