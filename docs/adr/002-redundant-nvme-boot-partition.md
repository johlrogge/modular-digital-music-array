# ADR-002: NVMe as primary boot device with SD as self-contained rescue beacon

## Status
Accepted — 2026-05-07 (revised 2026-05-07)

## Context

The MDMA provisioning pipeline originally booted the Pi 5 exclusively from the SD
card. The SD held the kernel, device trees, `cmdline.txt`, and `config.txt`;
the rootfs and `/lib/modules` lived on the NVMe. This coupled two physical
devices into a single boot path.

Today (2026-05-07) the SD card kernel went out of sync with the NVMe rootfs
`/lib/modules`. The kernel loaded but could not find matching modules, and the
unit failed to boot. Recovery required out-of-band intervention.

The root cause was SD/NVMe coupling: the bootloader lived on one device and the
rootfs (including `/lib/modules`) lived on another. Any divergence between them
— kernel update applied to one but not both, bit rot, hardware failure — produces
an unbootable unit. The original design framed this as "drift mitigation." The
corrected design eliminates the coupling structurally by making NVMe the
production boot device, end to end.

Pi 5 `BOOT_ORDER` fallthrough is firmware-level only. Once the firmware hands
off to a kernel, a subsequent kernel panic cannot trigger firmware fallthrough to
the next device. This means SD/NVMe drift cannot self-recover via firmware
fallthrough — the fallthrough only helps if the device is unreadable at the
firmware level, not if the kernel loads but then panics due to missing modules.

## Decision

Restructure the boot path so NVMe is the production boot device. SD reverts to a
self-contained beacon image used only for rescue scenarios.

1. **Partition layout.** New NVMe layout includes a ~512 MB FAT32 boot
   partition ahead of the rootfs. 512 MB is sufficient for current and
   plausible future kernel/dtb/initramfs payloads with comfortable headroom.
2. **Population.** Provisioning copies the kernel, device trees,
   `cmdline.txt`, and `config.txt` to the NVMe boot partition during
   `stage4_install` / `stage5_configure`. `config.txt` is sourced from the
   rootfs `/boot/config.txt` (installed by the `rpi-firmware` package —
   verified via `xbps-query --files rpi-firmware`; it does NOT come from
   `rpi-base`). Kernel updates write to the NVMe boot partition and the NVMe
   rootfs `/lib/modules` together — same disk, handled natively by xbps's
   `rpi5-kernel` post-install hook. No cross-device sync invariant required.
3. **Boot order.** Pi 5 firmware `BOOT_ORDER` is set to **USB → NVMe → SD**.
   The encoded value is **`0xf164`** (right-to-left encoding: 4 = USB first,
   6 = NVMe second, 1 = SD third, f = loop). USB remains first to preserve
   recovery via USB stick. NVMe is primary production boot. SD is last.

   This is a structural fix: after provisioning, the Pi boots entirely from
   NVMe — the kernel comes from the NVMe boot partition, modules come from the
   NVMe rootfs, both on the same physical device. There is no cross-device
   coupling and no cross-device drift possible.
4. **Scope.** New installs only. Already-provisioned Pis do not gain the
   new boot order retroactively; they pick it up on re-provision.
5. **SD card post-provisioning.** SD remains a self-contained beacon image
   after provisioning. Stage 5 does NOT modify the SD's `cmdline.txt` to
   redirect to NVMe rootfs, and does NOT sync NVMe's kernel/dtbs back to the
   SD `/boot`. If firmware falls through to SD (NVMe physically unreadable),
   the user lands in the beacon's rescue UI — not a half-coupled production
   attempt. The SD stays clean and independently bootable as a recovery tool.

## Consequences

### Positive
- SD/NVMe kernel drift is structurally eliminated, not mitigated. Kernel and
  modules live on the same disk; xbps post-install hooks keep them in sync
  without any cross-device orchestration.
- NVMe failure falls through to SD, which lands in a rescue UI rather than a
  broken half-state.
- Recovery path is in-band: the unit boots itself into beacon rescue mode
  instead of needing a human with a card reader.

### Negative
- **Boot time cost.** A few seconds of additional firmware probe time at every
  boot when USB is empty (firmware checks USB first, finds nothing, moves to
  NVMe). Negligible.
- **Misconfigured `BOOT_ORDER` could brick the unit unrecoverably** if the
  firmware refuses to fall through. Mitigated by read-back verification of
  the written `BOOT_ORDER` and idempotent skip when already correct;
  provisioning aborts if read-back disagrees.
- **No migration path for already-provisioned Pis.** They keep the
  old SD-primary boot path until re-provisioned. Deliberate: an online
  repartition of a live rootfs is the riskier option (see B below).

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
- **USB → SD → NVMe (original design intent).** Reconsidered when boot-flow
  analysis showed it preserved the SD/NVMe coupling that caused the
  precipitating incident. SD-primary means the kernel and modules still live
  on different devices, leaving the drift class intact. Switching to
  USB → NVMe → SD eliminates the coupling architecturally.

## Related

- Issue #22 — redundant boot partition
- Project task #74 — implement NVMe boot partition in provisioning
- Project task #57, #56 — provisioning pipeline groundwork
- Lessons from the 2026-05-07 boot incident: SD/NVMe coupling is the
  fragility, not the kernel update mechanism itself. The fix is structural
  elimination, not mitigation.

## Follow-up

`BOOT_ORDER = 0xf164` is empirically verified. No outstanding verification
needed. The follow-up note from the original ADR is resolved.
