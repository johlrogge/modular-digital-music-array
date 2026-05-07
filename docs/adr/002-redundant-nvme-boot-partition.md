# ADR-002: Redundant NVMe boot partition for SD-failure resilience

## Status
Accepted — 2026-05-07

## Context

The MDMA provisioning pipeline currently boots the Pi 5 exclusively from the SD
card. The SD holds the kernel, device trees, `cmdline.txt`, and `config.txt`;
the rootfs and `/lib/modules` live on the NVMe. This couples two physical
devices into a single boot path with no fallback.

Today (2026-05-07) the SD card kernel went out of sync with the NVMe rootfs
`/lib/modules`. The kernel loaded but could not find matching modules, and the
unit failed to boot. Recovery required out-of-band intervention.

The failure mode this eliminates: any divergence or hardware failure that
takes the SD out — bit rot, wear-out, corruption from an interrupted write,
or kernel/module drift between SD and NVMe — bricks the unit. SD cards are
the least reliable component in the stack and the one we depend on most.

## Decision

Add a redundant boot path on the NVMe so the firmware can fall through when
the SD fails.

1. **Partition layout.** New NVMe layout includes a ~512 MB FAT32 boot
   partition ahead of the rootfs. 512 MB is sufficient for current and
   plausible future kernel/dtb/initramfs payloads with comfortable headroom.
2. **Population.** Provisioning copies the kernel, device trees,
   `cmdline.txt`, and `config.txt` to the NVMe boot partition during
   `stage4_install` / `stage5_configure`. Every kernel update must write to
   both the SD boot partition and the NVMe boot partition; this is a hard
   invariant of the update path, not a best-effort step.
3. **Boot order.** Pi 5 firmware `BOOT_ORDER` is set to USB → SD → NVMe.
   The exact encoded value is **TBD** pending empirical verification on
   hardware. USB remains first to preserve recovery via USB stick. SD
   remains primary so the existing boot path is unchanged on healthy units.
   NVMe is the fallback.
4. **Scope.** New installs only. Already-provisioned Pis do not gain the
   fallback retroactively; they pick it up on re-provision.

## Consequences

### Positive
- SD failure or SD/NVMe kernel drift no longer bricks the unit. Firmware
  falls through to NVMe boot automatically.
- Recovery path is in-band: the unit boots itself instead of needing a
  human with a card reader.

### Negative
- **Kernel-drift hazard moves, it does not vanish.** The NVMe boot
  partition can now go out of sync with `/lib/modules` in exactly the way
  the SD did. Every kernel update must atomically update both boot
  partitions. This is mandatory sync logic on the update path, owned by
  the package post-install hooks.
- **Boot time cost.** A few seconds of additional firmware probe time per
  boot when SD is healthy (firmware tries SD first, succeeds, never reaches
  NVMe). Negligible.
- **Misconfigured `BOOT_ORDER` could brick the unit unrecoverably** if the
  firmware refuses to fall through. Mitigated by read-back verification of
  the written `BOOT_ORDER` and idempotent skip when already correct;
  provisioning aborts if read-back disagrees.
- **No migration path for already-provisioned Pis.** They keep the
  single-point-of-failure boot path until re-provisioned. Deliberate: an
  online repartition of a live rootfs is the riskier option (see B below).

## Alternatives considered

- **Option A (chosen).** Apply the new layout to new installs only; existing
  Pis stay as-is until re-provisioned.
- **Option B — online shrink and reformat of existing rootfs** to carve out
  a boot partition in place. Rejected: shrinking a live rootfs is high-risk
  and a failure mid-operation can lose data.
- **Option C — use unallocated disk space.** Rejected: the current layout
  has no unallocated space; the rootfs occupies the disk.
- **Option D — provision a fresh Pi with the new layout and migrate
  music/metadata via rsync** from the existing unit. This is the actual
  recovery path for `mdma-johlyroger`. Not a beacon feature; it is how the
  project moves the existing fleet onto the new layout operationally.

## Related

- Issue #22 — redundant boot partition
- Project task #74 — implement NVMe boot partition in provisioning
- Project task #57, #56 — provisioning pipeline groundwork
- Project task #91 — kernel/module sync invariant on updates
- Lessons from the 2026-05-07 boot incident: SD/NVMe coupling is the
  fragility, not the kernel update mechanism itself.

## Follow-up

Update this ADR with the verified `BOOT_ORDER` value once devops confirms
on hardware.
