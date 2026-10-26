# ADR-007: Provisioning never destroys data partitions by accident

## Status
Accepted — 2026-10-26

## Context

A provisioning audit found these paths to data loss:

- stage3 formatted a partition whenever blkid failed or the label mismatched.
- stage2 rewrote the whole GPT with recomputed offsets whenever any entry was
  missing.
- A failed lsblk was treated as an empty disk.
- `force_wipe_partitions` bypassed the layout check.

Together these could shift and reformat /music.

The rule is "never destroy data by accident", not "never": a user must still
be able to deliberately wipe a unit, music and metadata included.

## Decision

1. **A non-empty partition table is never rewritten** unless the user chose
   `WipeAll` (see 10). The on-disk table is the truth; its labels are matched
   by role, not by recomputed sizes.
2. **An empty disk gets the full layout created.** "Empty" means no partitions
   AND no whole-disk signature (ext4, LUKS, LVM, ZFS ...); a disk with a
   signature but no table is refused.
3. **A mismatched layout makes provisioning refuse**, with a typed
   `LayoutError` stating that no data was changed.
4. **Failed reads abort.** A failed sfdisk, wipefs probe or blkid is never
   treated as empty or unlabeled. The table is re-read right before writing.
5. **Data partitions (music, metadata, cache) are formatted only if created in
   this run.** An OS partition carrying a data label aborts.
6. **The format function only accepts OS partitions or newly created data
   partitions**, enforced by type.
7. **`force_wipe_partitions` is replaced by
   `ExistingDataPolicy { Preserve (default), ReinstallOs, WipeAll }`.**
   `ReinstallOs` reformats only boot, root and var.
8. **Data partitions on a different NVMe than the target abort provisioning**,
   also under `WipeAll`: that protects against NVMe enumeration swaps and is
   not a user choice.
9. **Wiping never happens implicitly.** Only `WipeAll` destroys data.
10. **`WipeAll` is a deliberate, confirmed wipe.** It destroys ALL data on the
    planned target drive(s), music and metadata included, and repartitions with
    the full fresh layout. Safeguards:
    - Offered on the beacon main form ("Wipe drive and provision fresh") and
      on the layout-refused page, but not when the refusal is an unreadable
      disk or data on another drive, where a wipe would not help.
    - The UI lists the existing partitions that will be destroyed and the user
      must type `WIPE`. The route validates the confirmation server-side and
      rejects a wipe request without it.
    - Stage 2 reads the table first (aborting if unreadable), logs every
      partition it will destroy, runs `wipefs -a` on them and the disk, and
      writes with `sfdisk --wipe always --wipe-partitions always`.
    - All partitions are then "created this run", so stage 3 formats them
      through the normal path; `FormatTarget` still refuses pre-existing data
      partitions under every policy.

## Consequences

### Positive
- No accidental path can shift or reformat /music.
- A unit can still be wiped on purpose, from the beacon, without SSH.
- Size constant changes no longer affect existing units.

### Negative
- Old layouts without a boot partition can't be reprovisioned without a wipe
  until an "OS-span relayout" exists. Future work: rewrite only the OS entries
  and keep the data entries byte-identical.
- `WipeAll` is irreversible; the typed confirmation is the only guard.

## Related

- ADR-002 — NVMe boot partition layout
- ADR-006 — `/boot` mount and `ReinstallOs` scope
