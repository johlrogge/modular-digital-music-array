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

pub use storage_primitives::{ByteSize, PartitionSize, StorageCapacity};

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

    pub(crate) fn join(&self, mount_point: MountPoint) -> PathBuf {
        PathBuf::from_str(self.as_str())
            .expect("conversion from str to pathbuf should be infallible")
            .join(mount_point.as_str().trim_start_matches("/"))
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
// Mount Point (static validated paths)
// ============================================================================

/// Mount point path
///
/// A static mount point path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MountPoint(&'static str);

impl MountPoint {
    /// Create a new mount point
    pub const fn new(path: &'static str) -> Self {
        MountPoint(path)
    }

    /// Get the mount point as a string slice
    pub const fn as_str(&self) -> &str {
        self.0
    }
}

impl fmt::Display for MountPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ============================================================================
// Partition Label (static validated labels)
// ============================================================================

/// Partition label
///
/// A static partition label. Inner field is private for consistency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PartitionLabel(&'static str);

impl PartitionLabel {
    /// Create a new partition label
    pub const fn new(label: &'static str) -> Self {
        PartitionLabel(label)
    }

    /// Get the label as a string slice
    pub const fn as_str(&self) -> &str {
        self.0
    }
}

impl fmt::Display for PartitionLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
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

    pub const fn requires_dual_nvme(&self) -> bool {
        matches!(self, UnitType::Mdma909)
    }
}

impl fmt::Display for UnitType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
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

    #[test]
    fn test_hostname_validation() {
        // Valid hostnames
        assert!(Hostname::new("mdma-909".to_string()).is_ok());
        assert!(Hostname::new("test.example.com".to_string()).is_ok());

        // Invalid hostnames
        assert!(matches!(
            Hostname::new("-invalid".to_string()),
            Err(ValidationError::HostnameInvalidStart)
        ));
        assert!(matches!(
            Hostname::new("inv@lid".to_string()),
            Err(ValidationError::HostnameInvalidChars)
        ));
        assert!(matches!(
            Hostname::new("".to_string()),
            Err(ValidationError::HostnameTooLong(_))
        ));
    }

    #[test]
    fn test_ssh_key_validation() {
        let valid_key = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ user@host";
        assert!(SshPublicKey::new(valid_key.to_string()).is_ok());

        assert!(matches!(
            SshPublicKey::new("invalid key".to_string()),
            Err(ValidationError::SshKeyInvalidPrefix)
        ));
    }

    #[test]
    fn test_device_path_validation() {
        assert!(DevicePath::new("/dev/nvme0n1").is_ok());
        assert!(DevicePath::new("/dev/sda1").is_ok());

        assert!(matches!(
            DevicePath::new("/not/a/device"),
            Err(ValidationError::DevicePathInvalidPrefix(_))
        ));
        assert!(matches!(
            DevicePath::new(""),
            Err(ValidationError::DevicePathEmpty)
        ));
    }

    #[test]
    fn test_mount_point() {
        let mp = MountPoint::new("/music");
        assert_eq!(mp.as_str(), "/music");
        assert_eq!(mp.to_string(), "/music");
    }

    #[test]
    fn test_unit_type_display() {
        assert_eq!(UnitType::Mdma909.to_string(), "mdma-909");
        assert_eq!(UnitType::Mdma101.as_str(), "mdma-101");
        assert!(UnitType::Mdma909.requires_dual_nvme());
        assert!(!UnitType::Mdma303.requires_dual_nvme());
    }
}
