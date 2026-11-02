//! Reading the system's mount table. Shared by provisioning (stage 4) and the
//! network fallback, which must never mount a partition that is already mounted.

use std::path::PathBuf;

/// One line of the mount table: a source device and where it is mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MountEntry {
    pub device: String,
    pub target: PathBuf,
}

/// Parse `mount` output (`<source> on <target> type <fs> (<opts>)`) into
/// (device, target) pairs. Lines in any other shape are skipped. The target may
/// contain spaces: it is everything between the first ` on ` and the last
/// ` type `.
pub(crate) fn parse_mounts(mount_output: &str) -> Vec<MountEntry> {
    mount_output
        .lines()
        .filter_map(|line| {
            let (device, rest) = line.split_once(" on ")?;
            let (target, _) = rest.rsplit_once(" type ")?;
            Some(MountEntry {
                device: device.trim().to_string(),
                target: PathBuf::from(target),
            })
        })
        .collect()
}

/// Whether `device` appears as the source of a mount in `mount` output. Exact
/// match on the source field, so `/dev/nvme0n1p1` does not match
/// `/dev/nvme0n1p10`.
pub(crate) fn is_device_mounted(mount_output: &str, device: &str) -> bool {
    parse_mounts(mount_output)
        .iter()
        .any(|entry| entry.device == device)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_device_mounted_matches_exactly() {
        let out = "/dev/nvme0n1p10 on /mnt/x type ext4 (rw)\n\
                   /dev/nvme0n1p2 on /mnt/mdma-install type ext4 (rw)\n";
        assert!(!is_device_mounted(out, "/dev/nvme0n1p1"));
        assert!(is_device_mounted(out, "/dev/nvme0n1p10"));
        assert!(is_device_mounted(out, "/dev/nvme0n1p2"));
    }

    #[test]
    fn parse_mounts_returns_device_and_target_pairs() {
        let out = "/dev/mmcblk0p2 on / type ext4 (rw,relatime)\n\
                   tmpfs on /run type tmpfs (rw,nosuid)\n\
                   /dev/nvme0n1p2 on /run/beacon/installed-root type ext4 (ro,noload)\n";
        assert_eq!(
            parse_mounts(out),
            vec![
                MountEntry {
                    device: "/dev/mmcblk0p2".into(),
                    target: "/".into()
                },
                MountEntry {
                    device: "tmpfs".into(),
                    target: "/run".into()
                },
                MountEntry {
                    device: "/dev/nvme0n1p2".into(),
                    target: "/run/beacon/installed-root".into()
                },
            ]
        );
    }

    #[test]
    fn parse_mounts_keeps_spaces_in_targets_and_skips_junk() {
        let out = "\n\
                   garbage line\n\
                   /dev/sda1 on /mnt/my disk type vfat (rw)\n";
        assert_eq!(
            parse_mounts(out),
            vec![MountEntry {
                device: "/dev/sda1".into(),
                target: "/mnt/my disk".into()
            }]
        );
    }

    #[test]
    fn parse_mounts_of_nothing_is_empty() {
        assert!(parse_mounts("").is_empty());
    }
}
