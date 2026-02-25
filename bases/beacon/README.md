# beacon

Provisioning server for MDMA. Runs on an unprovisioned Raspberry Pi, serves a web UI, and walks through a 7-stage pipeline to partition, format, install, and configure the system.

Stages: `stage0_safety` → `stage1_validate` → `stage2_partition` → `stage3_format` → `stage4_install` → `stage5_configure` → `stage6_finalize`

Each stage implements the `Action` trait (`plan()` + `apply()`). The compiler enforces correct stage sequencing through the type system.

## Build

```bash
cargo build --package beacon
```

Cross-compile for Raspberry Pi (aarch64):

```bash
just beacon-cross
```

## Run

```bash
# Dry-run: show provisioning plan without applying
cargo run --package beacon -- --check

# Apply: actually provision (requires Raspberry Pi hardware)
cargo run --package beacon -- --apply
```

The web UI is available at `http://welcome-to-mdma.local` when beacon is running on an unprovisioned Pi.

## Back to workspace

[Workspace README](../../README.md)
