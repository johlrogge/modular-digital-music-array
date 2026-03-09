# beacon

Provisioning server for MDMA. Runs on an unprovisioned Raspberry Pi, serves a web UI at `http://welcome-to-mdma.local`, and walks through a 7-stage pipeline to partition the NVMe drive, install Void Linux, and configure all MDMA services.

Use beacon when setting up a fresh Pi. Once provisioned, the Pi boots directly into the MDMA service stack and beacon is no longer needed.

[Back to workspace README](../../README.md)

---

## Provisioning pipeline

```
stage0_safety     — checks you're on the right hardware (won't run on a desktop)
stage1_validate   — validates target disk and configuration
stage2_partition  — partitions the NVMe drive (/, /music, /metadata, /cdj-export)
stage3_format     — formats partitions
stage4_install    — installs Void Linux base system
stage5_configure  — installs and enables all 6 MDMA services
stage6_finalize   — sets hostname, SSH keys, cleans up
```

Each stage implements the `Action` trait with `plan()` and `apply()` methods. The compiler enforces correct stage sequencing through the type system — you cannot call `apply()` on stage N without a successful result from stage N-1.

## Build

```bash
cargo build --package beacon
```

Cross-compile for Raspberry Pi (aarch64):

```bash
just beacon-cross    # uses cargo-zigbuild (recommended)
just beacon-native   # uses aarch64-linux-gnu-gcc
```

## Deploy to unprovisioned Pi

```bash
just deploy-dev      # copies beacon binary to welcome-to-mdma.local and restarts
```

The unprovisioned Pi is accessible at `welcome-to-mdma.local` (password: `voidlinux`).

## Run

```bash
# Dry-run: show provisioning plan without applying anything
cargo run --package beacon -- --check

# Apply: actually provision (requires Raspberry Pi hardware)
cargo run --package beacon -- --apply
```

The web UI is at `http://welcome-to-mdma.local` while beacon is running. It walks through each stage interactively and shows progress.
