//! Pure `/etc/fstab` rewrite used by the boot-partition migration (issue #65).
//!
//! Already-provisioned units mount the NVMe boot partition (p1) at
//! `/boot/firmware`, but Void's `rpi-kernel` installs into `/boot`. The
//! migration re-points that single fstab line at `/boot`.

/// Mount point the old provisioning used for p1.
pub const OLD_BOOT_MOUNT: &str = "/boot/firmware";
/// Mount point Void's `rpi-kernel` installs into.
pub const NEW_BOOT_MOUNT: &str = "/boot";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FstabError {
    #[error("invalid PARTUUID {0:?}: expected only hex digits and '-'")]
    InvalidPartuuid(String),
    #[error("fstab has no line mounting {OLD_BOOT_MOUNT} and none mounting {NEW_BOOT_MOUNT}")]
    NoBootEntry,
    #[error("fstab mounts both {OLD_BOOT_MOUNT} and {NEW_BOOT_MOUNT}; refusing to guess")]
    Ambiguous,
    #[error("fstab has more than one line mounting {OLD_BOOT_MOUNT}")]
    DuplicateEntry,
}

/// The fstab line that mounts p1 at `/boot`.
fn boot_line(p1_partuuid: &str) -> String {
    format!("PARTUUID={p1_partuuid} {NEW_BOOT_MOUNT} vfat defaults,nofail 0 2")
}

/// The mount point (second field) of an fstab line, ignoring blanks/comments.
fn mount_point(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    trimmed.split_whitespace().nth(1)
}

/// Replace ONLY the line mounting `/boot/firmware` with a `/boot` vfat entry
/// for `p1_partuuid`. Every other line is returned byte-for-byte.
///
/// Idempotent: if the text already mounts `/boot` (and not `/boot/firmware`)
/// it is returned unchanged.
pub fn migrate_fstab(fstab: &str, p1_partuuid: &str) -> Result<String, FstabError> {
    if p1_partuuid.is_empty()
        || !p1_partuuid
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-')
    {
        return Err(FstabError::InvalidPartuuid(p1_partuuid.to_string()));
    }

    let count = |wanted: &str| {
        fstab
            .lines()
            .filter(|l| mount_point(l) == Some(wanted))
            .count()
    };

    match (count(OLD_BOOT_MOUNT), count(NEW_BOOT_MOUNT)) {
        (0, 0) => Err(FstabError::NoBootEntry),
        (0, _) => Ok(fstab.to_string()),
        (1, 0) => Ok(fstab
            .split_inclusive('\n')
            .map(|raw| {
                let body = raw.trim_end_matches(['\n', '\r']);
                if mount_point(body) == Some(OLD_BOOT_MOUNT) {
                    let ending = &raw[body.len()..];
                    format!("{}{ending}", boot_line(p1_partuuid))
                } else {
                    raw.to_string()
                }
            })
            .collect()),
        (1, _) => Err(FstabError::Ambiguous),
        (_, _) => Err(FstabError::DuplicateEntry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const UUID: &str = "1a2b3c4d-01";

    const BEFORE: &str = "\
# /etc/fstab
PARTUUID=aaaa-02 /              ext4 defaults,noatime 0 1
PARTUUID=0ld-01  /boot/firmware vfat defaults         0 2
/dev/nvme0n1p3   /music         ext4 defaults,nofail  0 2
tmpfs            /tmp           tmpfs defaults,nosuid 0 0
";

    const AFTER: &str = "\
# /etc/fstab
PARTUUID=aaaa-02 /              ext4 defaults,noatime 0 1
PARTUUID=1a2b3c4d-01 /boot vfat defaults,nofail 0 2
/dev/nvme0n1p3   /music         ext4 defaults,nofail  0 2
tmpfs            /tmp           tmpfs defaults,nosuid 0 0
";

    #[test]
    fn rewrites_only_the_boot_firmware_line() {
        assert_eq!(migrate_fstab(BEFORE, UUID).unwrap(), AFTER);
    }

    #[test]
    fn music_and_other_lines_are_untouched() {
        let out = migrate_fstab(BEFORE, UUID).unwrap();
        for line in BEFORE.lines().filter(|l| !l.contains("/boot/firmware")) {
            assert!(out.lines().any(|o| o == line), "lost line: {line}");
        }
    }

    #[test]
    fn already_migrated_is_returned_unchanged() {
        assert_eq!(migrate_fstab(AFTER, UUID).unwrap(), AFTER);
    }

    #[test]
    fn migration_is_idempotent() {
        let once = migrate_fstab(BEFORE, UUID).unwrap();
        assert_eq!(migrate_fstab(&once, UUID).unwrap(), once);
    }

    #[test]
    fn missing_trailing_newline_is_preserved() {
        let input = "PARTUUID=a /boot/firmware vfat defaults 0 2";
        assert_eq!(
            migrate_fstab(input, UUID).unwrap(),
            "PARTUUID=1a2b3c4d-01 /boot vfat defaults,nofail 0 2"
        );
    }

    #[test]
    fn commented_out_boot_firmware_line_is_ignored() {
        let input = "#PARTUUID=a /boot/firmware vfat defaults 0 2\n\
                     PARTUUID=b /boot/firmware vfat defaults 0 2\n";
        assert_eq!(
            migrate_fstab(input, UUID).unwrap(),
            "#PARTUUID=a /boot/firmware vfat defaults 0 2\n\
             PARTUUID=1a2b3c4d-01 /boot vfat defaults,nofail 0 2\n"
        );
    }

    #[test]
    fn no_boot_entry_is_an_error() {
        let input = "PARTUUID=a / ext4 defaults 0 1\n";
        assert_eq!(migrate_fstab(input, UUID), Err(FstabError::NoBootEntry));
    }

    #[test]
    fn both_entries_is_ambiguous() {
        let input =
            "PARTUUID=a /boot vfat defaults 0 2\nPARTUUID=a /boot/firmware vfat defaults 0 2\n";
        assert_eq!(migrate_fstab(input, UUID), Err(FstabError::Ambiguous));
    }

    #[test]
    fn duplicate_boot_firmware_lines_is_an_error() {
        let input = "PARTUUID=a /boot/firmware vfat d 0 2\nPARTUUID=b /boot/firmware vfat d 0 2\n";
        assert_eq!(migrate_fstab(input, UUID), Err(FstabError::DuplicateEntry));
    }

    #[test]
    fn invalid_partuuid_is_rejected() {
        for bad in ["", "abc def", "x;rm -rf /", "zzzz"] {
            assert_eq!(
                migrate_fstab(BEFORE, bad),
                Err(FstabError::InvalidPartuuid(bad.to_string()))
            );
        }
    }
}
