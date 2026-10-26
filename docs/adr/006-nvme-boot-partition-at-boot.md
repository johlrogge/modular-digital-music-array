# ADR-006: Mount the NVMe boot partition at /boot (Void-native bootfs)

## Status
Accepted — 2026-10-26. Supersedes ADR-002 Decision §2 (Population).

## Context

On 2026-10-24 `xbps-install -Su` on mdma-johlyroger installed a new kernel into
rootfs `/boot` and removed the old `/lib/modules`, while the NVMe boot
partition (p1), which the firmware reads, kept the old `kernel8.img`. The unit
hung on reboot. See the incident note in ADR-002; tracked in #65.

Facts:

- The firmware reads p1. Provisioning mounted it at `/boot/firmware`.
- Void's `rpi-kernel` and `rpi-firmware` own all of `/boot/*` (72 MB; p1 is
  512 MiB). `rpi-kernel` has no triggers or kernel hooks.
- `rpi-firmware` lists `/boot/config.txt` and `/boot/cmdline.txt` as
  conf_files, so locally modified copies survive updates (xbps writes
  `.new-<ver>` beside them).
- The beacon SD image already uses vfat at `/boot` (void-mklive layout).

## Decision

1. **Mount p1 at `/boot`.** Package updates write directly to the boot device.
   No sync step.
2. **fstab uses PARTUUID**, with `nofail` for `/boot` and the data partitions.
3. **Provisioning re-extracts the `/boot` owners** (`rpi-kernel`,
   `rpi-firmware`) if p1 lacks `kernel8.img`.
4. **stage6 verifies** that the kernel image and modules match before
   finishing.

### Guard: `boot_health` component

mdma-admin `Reboot` refuses unless the verdict is Healthy or RebootPending
(`force` overrides). Verdicts: Healthy, RebootPending, BootfsNotMounted,
KernelImageStale, ModulesMissing.

### Migration

`/usr/libexec/mdma/migrate-bootfs` (in mdma-admin) is idempotent and dry-run by
default. It backs up the fstab and bootfs, rewrites only the p1 fstab line,
remounts, re-extracts `rpi-kernel`/`rpi-firmware`, and verifies. It does not
reboot, and documents its rollback.

## Consequences

### Positive
- One mount, one copy. No sync invariant to violate.
- EEPROM updates stage onto the real boot device.
- Unsafe reboots are refused by default.

### Negative
- Existing units need the migration.
- A missing or unmounted p1 is no longer hidden; `nofail` keeps the unit
  booting, and `boot_health` reports it.

## Alternatives rejected

- **Keep `/boot/firmware` plus an xbps hook.** `rpi-kernel` has no hook
  mechanism, it leaves two copies and a sync invariant, and that is what
  failed in May and October.
- **Keep `/boot/firmware` plus a periodic sync.** Same problems.

## Related

- ADR-002 — redundant NVMe boot partition (Decision §2 superseded)
- Issue #65
