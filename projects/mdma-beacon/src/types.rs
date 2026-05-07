//! Beacon domain types - Single Source of Truth
//!
//! All types defined here are the canonical definitions.
//! Other modules re-export these types but NEVER redefine them.
//!
//! ## Type Safety Philosophy
//!
//! - All newtypes have PRIVATE fields (cannot be constructed unsafely)
//! - Validation happens at construction time
//! - Invalid states are impossible to represent
//! - Display implementations are human-readable

use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf, str::FromStr};
use thiserror::Error;

// ============================================================================
// Re-exports from shared components
// ============================================================================

pub use storage_primitives::{ByteSize, PartitionSize};

// ============================================================================
// Validation Errors
// ============================================================================

#[derive(Error, Debug, Clone, PartialEq)]
pub enum ValidationError {
    #[error("hostname must be 1-253 characters, got {0}")]
    HostnameTooLong(usize),

    #[error("hostname contains invalid characters (allowed: alphanumeric, hyphen, dot)")]
    HostnameInvalidChars,

    #[error("hostname cannot start with hyphen or dot")]
    HostnameInvalidStart,

    #[error("SSH key must start with ssh-rsa, ssh-ed25519, or ecdsa-sha2-")]
    SshKeyInvalidPrefix,

    #[error("SSH key format invalid (must have at least type and key)")]
    SshKeyInvalidFormat,

    #[error("device path must start with /dev/, got: {0}")]
    DevicePathInvalidPrefix(String),

    #[error("device path cannot be empty")]
    DevicePathEmpty,

    #[error("Drive too small: {0}")]
    DriveToSmall(String),

    #[error("unknown unit type: {0}")]
    InvalidUnitType(String),
}

// ============================================================================
// Hostname (validated at construction)
// ============================================================================

/// Validated hostname
///
/// A hostname that has been validated to comply with DNS naming standards.
/// The inner field is private to ensure all construction goes through validation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Hostname(String);

impl Hostname {
    /// Create a new validated hostname
    ///
    /// # Validation Rules
    ///
    /// - Length: 1-253 characters
    /// - Characters: alphanumeric, hyphen, dot
    /// - Cannot start with hyphen or dot
    pub fn new(s: String) -> Result<Self, ValidationError> {
        // Validate length
        if s.is_empty() || s.len() > 253 {
            return Err(ValidationError::HostnameTooLong(s.len()));
        }

        // Validate characters
        if !s
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '.')
        {
            return Err(ValidationError::HostnameInvalidChars);
        }

        // Validate start character
        if s.starts_with('-') || s.starts_with('.') {
            return Err(ValidationError::HostnameInvalidStart);
        }

        Ok(Hostname(s))
    }

    /// Get the hostname as a string slice
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Hostname {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ============================================================================
// SSH Public Key (validated at construction)
// ============================================================================

/// Validated SSH public key
///
/// An SSH public key that has been validated for basic format correctness.
/// The inner field is private to ensure validation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SshPublicKey(String);

impl SshPublicKey {
    /// Create a new validated SSH public key
    ///
    /// # Validation Rules
    ///
    /// - Must start with ssh-rsa, ssh-ed25519, or ecdsa-sha2-
    /// - Must have at least 2 space-separated parts (type and key)
    pub fn new(s: String) -> Result<Self, ValidationError> {
        let trimmed = s.trim();

        // Validate key type prefix
        if !trimmed.starts_with("ssh-rsa ")
            && !trimmed.starts_with("ssh-ed25519 ")
            && !trimmed.starts_with("ecdsa-sha2-")
        {
            return Err(ValidationError::SshKeyInvalidPrefix);
        }

        // Must have at least 2 space-separated parts
        if trimmed.split_whitespace().count() < 2 {
            return Err(ValidationError::SshKeyInvalidFormat);
        }

        Ok(SshPublicKey(trimmed.to_string()))
    }

    /// Get the SSH key as a string slice
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SshPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Don't print the whole key for security/readability
        let parts: Vec<&str> = self.0.split_whitespace().collect();
        if parts.len() >= 2 {
            write!(f, "{} {}...", parts[0], &parts[1][..20.min(parts[1].len())])
        } else {
            write!(f, "{}", self.0)
        }
    }
}

// ============================================================================
// Device Path (validated at construction)
// ============================================================================

/// Validated device path
///
/// A Linux device path that has been validated to start with /dev/.
/// The inner field is private to ensure validation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DevicePath(String);

impl DevicePath {
    /// Create a new validated device path
    ///
    /// # Validation Rules
    ///
    /// - Cannot be empty
    /// - Must start with /dev/
    pub fn new(path: impl Into<String>) -> Result<Self, ValidationError> {
        let path = path.into();

        if path.is_empty() {
            return Err(ValidationError::DevicePathEmpty);
        }

        if !path.starts_with("/dev/") {
            return Err(ValidationError::DevicePathInvalidPrefix(path));
        }

        Ok(DevicePath(path))
    }

    /// Get the device path as a string slice
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn as_path(&self) -> PathBuf {
        PathBuf::from_str(self.as_str())
            .expect("conversion from str to pathbuf should be infallible")
    }
}

impl fmt::Display for DevicePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for DevicePath {
    fn from(s: &str) -> Self {
        DevicePath::new(s).expect("DevicePath::from requires valid device path")
    }
}

// ============================================================================
// Mount Point (validated enum)
// ============================================================================

/// Mount point path
///
/// All known mount points as enum variants for compile-time safety.
/// Adding a new mount point requires explicitly deciding its filesystem type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MountPoint {
    /// Boot partition (/boot/firmware) - FAT32, holds Pi firmware and bootloader files
    Boot,
    /// Root filesystem (/)
    Root,
    /// Variable data (/var) - logs, journals
    Var,
    /// Music library (/music) - Contains both FLAC library and CDJ export cache
    Music,
    /// ACID metadata (/metadata)
    Metadata,
    /// General cache (/cache) - Used by MDMA-303 satellite nodes
    Cache,
}

impl MountPoint {
    /// Get the mount point as a string slice
    pub const fn as_str(&self) -> &'static str {
        match self {
            MountPoint::Boot => "/boot/firmware",
            MountPoint::Root => "/",
            MountPoint::Var => "/var",
            MountPoint::Music => "/music",
            MountPoint::Metadata => "/metadata",
            MountPoint::Cache => "/cache",
        }
    }

    /// Get the mount point as a Path reference
    pub fn as_path(&self) -> &std::path::Path {
        std::path::Path::new(self.as_str())
    }

    /// Get the filesystem type for this mount point
    ///
    /// All MDMA partitions use ext4.
    ///
    /// This method is exhaustive - adding a new MountPoint variant
    /// will cause a compile error until filesystem_type is updated.
    pub const fn filesystem_type(&self) -> FilesystemType {
        match self {
            MountPoint::Boot => FilesystemType::Fat32,
            MountPoint::Root
            | MountPoint::Var
            | MountPoint::Music
            | MountPoint::Metadata
            | MountPoint::Cache => FilesystemType::Ext4,
        }
    }

    /// Get the partition label for this mount point
    ///
    /// Labels use kebab-case naming convention matching mount points.
    ///
    /// Labels are consistent across all MDMA hosts, making
    /// lsblk output instantly recognizable.
    pub fn label(&self) -> PartitionLabel {
        let label = match self {
            MountPoint::Boot => "boot",
            MountPoint::Root => "root",
            MountPoint::Var => "var",
            MountPoint::Music => "music",
            MountPoint::Metadata => "metadata",
            MountPoint::Cache => "cache",
        };
        PartitionLabel::new(label)
    }
}

impl fmt::Display for MountPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl AsRef<std::path::Path> for MountPoint {
    fn as_ref(&self) -> &std::path::Path {
        self.as_path()
    }
}

// ============================================================================
// Partition Label (static validated labels)
// ============================================================================

/// Partition label
///
/// A partition label. Inner field is private for consistency.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PartitionLabel(String);

impl PartitionLabel {
    /// Create a new partition label
    pub fn new(label: impl Into<String>) -> Self {
        PartitionLabel(label.into())
    }

    /// Get the label as a string slice
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PartitionLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ============================================================================
// Filesystem Type
// ============================================================================

/// Filesystem type for partitions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilesystemType {
    /// FAT32 filesystem (boot partition)
    Fat32,
    /// ext4 filesystem (all other partitions)
    Ext4,
}

impl FilesystemType {
    /// Get the filesystem type string used by mkfs commands
    pub const fn as_str(&self) -> &'static str {
        match self {
            FilesystemType::Fat32 => "vfat",
            FilesystemType::Ext4 => "ext4",
        }
    }
}

impl fmt::Display for FilesystemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ============================================================================
// Unit Type
// ============================================================================

/// MDMA unit type selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitType {
    Mdma909,
    Mdma101,
    Mdma303,
}

impl UnitType {
    pub const fn as_str(&self) -> &'static str {
        match self {
            UnitType::Mdma909 => "mdma-909",
            UnitType::Mdma101 => "mdma-101",
            UnitType::Mdma303 => "mdma-303",
        }
    }

    #[allow(dead_code)] // Used in tests; intentionally available for future provisioning logic
    pub const fn requires_dual_nvme(&self) -> bool {
        matches!(self, UnitType::Mdma909)
    }
}

impl fmt::Display for UnitType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for UnitType {
    type Err = ValidationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mdma-909" => Ok(UnitType::Mdma909),
            "mdma-101" => Ok(UnitType::Mdma101),
            "mdma-303" => Ok(UnitType::Mdma303),
            _ => Err(ValidationError::InvalidUnitType(s.to_owned())),
        }
    }
}

// ============================================================================
// Provisioning Configuration
// ============================================================================

/// Provisioning configuration submitted by user
///
/// All fields use validated newtypes to ensure correctness.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionConfig {
    pub unit_type: UnitType,
    pub hostname: Hostname,
    pub ssh_key: SshPublicKey,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    #[rstest]
    #[case("mdma-909", true)]
    #[case("test.example.com", true)]
    #[case("-invalid", false)]
    #[case("inv@lid", false)]
    #[case("", false)]
    fn hostname_validation(#[case] input: &str, #[case] expected_ok: bool) {
        let result = Hostname::new(input.to_string());
        assert_eq!(result.is_ok(), expected_ok);
    }

    #[test]
    fn ssh_key_validation() {
        let valid_key = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ user@host";
        assert!(SshPublicKey::new(valid_key.to_string()).is_ok());

        assert!(matches!(
            SshPublicKey::new("invalid key".to_string()),
            Err(ValidationError::SshKeyInvalidPrefix)
        ));
    }

    #[rstest]
    #[case("/dev/nvme0n1", true)]
    #[case("/dev/sda1", true)]
    #[case("/not/a/device", false)]
    #[case("", false)]
    fn device_path_validation(#[case] path: &str, #[case] expected_ok: bool) {
        let result = DevicePath::new(path);
        assert_eq!(result.is_ok(), expected_ok);
    }

    #[rstest]
    #[case(MountPoint::Boot, "/boot/firmware", FilesystemType::Fat32, "boot")]
    #[case(MountPoint::Root, "/", FilesystemType::Ext4, "root")]
    #[case(MountPoint::Var, "/var", FilesystemType::Ext4, "var")]
    #[case(MountPoint::Music, "/music", FilesystemType::Ext4, "music")]
    #[case(MountPoint::Metadata, "/metadata", FilesystemType::Ext4, "metadata")]
    #[case(MountPoint::Cache, "/cache", FilesystemType::Ext4, "cache")]
    fn mount_point(
        #[case] mp: MountPoint,
        #[case] expected_path: &str,
        #[case] expected_fs: FilesystemType,
        #[case] expected_label: &str,
    ) {
        assert_eq!(mp.as_str(), expected_path);
        assert_eq!(mp.to_string(), expected_path);
        assert_eq!(mp.filesystem_type(), expected_fs);
        assert_eq!(mp.label().as_str(), expected_label);
    }

    #[rstest]
    #[case(UnitType::Mdma909, "mdma-909")]
    #[case(UnitType::Mdma101, "mdma-101")]
    #[case(UnitType::Mdma303, "mdma-303")]
    fn unit_type_display(#[case] unit: UnitType, #[case] expected: &str) {
        assert_eq!(unit.to_string(), expected);
        assert_eq!(unit.as_str(), expected);
    }

    #[rstest]
    #[case(UnitType::Mdma909, true)]
    #[case(UnitType::Mdma303, false)]
    fn unit_type_requires_dual_nvme(#[case] unit: UnitType, #[case] expected: bool) {
        assert_eq!(unit.requires_dual_nvme(), expected);
    }
}
