//! Storage size primitives for MDMA
//!
//! This component provides type-safe wrappers for storage sizes with:
//! - Human-readable display formatting
//! - Convenient constructors (from_gb, from_mb, etc.)
//! - Type safety to prevent mixing up bytes and other units
//!
//! # Examples
//!
//! ```
//! use storage_primitives::ByteSize;
//!
//! let size = ByteSize::from_gb(512);
//! assert_eq!(size.bytes(), 512_000_000_000);
//! assert_eq!(size.gigabytes(), 512);
//! println!("{}", size); // "512 GB"
//!
//! let small = ByteSize::from_mb(100);
//! println!("{}", small); // "100 MB"
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;

/// Size in bytes with smart display formatting
///
/// This type represents a size in bytes but provides human-readable
/// display and convenient constructors for common units (GB, MB, etc.).
///
/// The inner value is PRIVATE to ensure all construction goes through
/// validated methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ByteSize(u64);

impl ByteSize {
    // ========================================================================
    // Constructors
    // ========================================================================

    /// Create from a number of bytes
    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    /// Create from gigabytes (1 GB = 1,000,000,000 bytes, decimal not binary)
    pub const fn from_gb(gb: u64) -> Self {
        Self(gb * 1_000_000_000)
    }

    /// Create from megabytes (1 MB = 1,000,000 bytes)
    pub const fn from_mb(mb: u64) -> Self {
        Self(mb * 1_000_000)
    }

    /// Create from kilobytes (1 KB = 1,000 bytes)
    pub const fn from_kb(kb: u64) -> Self {
        Self(kb * 1_000)
    }

    /// Create from terabytes (1 TB = 1,000,000,000,000 bytes)
    pub const fn from_tb(tb: u64) -> Self {
        Self(tb * 1_000_000_000_000)
    }

    // ========================================================================
    // Conversions
    // ========================================================================

    /// Get the raw byte value
    pub const fn bytes(&self) -> u64 {
        self.0
    }

    /// Convert to gigabytes (truncating)
    pub const fn gigabytes(&self) -> u64 {
        self.0 / 1_000_000_000
    }

    /// Convert to megabytes (truncating)
    pub const fn megabytes(&self) -> u64 {
        self.0 / 1_000_000
    }

    /// Convert to kilobytes (truncating)
    pub const fn kilobytes(&self) -> u64 {
        self.0 / 1_000
    }

    /// Convert to terabytes (truncating)
    pub const fn terabytes(&self) -> u64 {
        self.0 / 1_000_000_000_000
    }

    /// Convert to gigabytes as a float (with fractional part)
    pub fn gigabytes_f64(&self) -> f64 {
        self.0 as f64 / 1_000_000_000.0
    }

    /// Convert to megabytes as a float (with fractional part)
    pub fn megabytes_f64(&self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }

    // ========================================================================
    // Arithmetic (saturating to prevent overflow)
    // ========================================================================

    /// Add two sizes (saturating at u64::MAX)
    pub const fn saturating_add(self, other: ByteSize) -> ByteSize {
        ByteSize(self.0.saturating_add(other.0))
    }

    /// Subtract two sizes (saturating at 0)
    pub const fn saturating_sub(self, other: ByteSize) -> ByteSize {
        ByteSize(self.0.saturating_sub(other.0))
    }

    /// Multiply by a factor (saturating at u64::MAX)
    pub const fn saturating_mul(self, factor: u64) -> ByteSize {
        ByteSize(self.0.saturating_mul(factor))
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const KB: u64 = 1_000;
        const MB: u64 = 1_000_000;
        const GB: u64 = 1_000_000_000;
        const TB: u64 = 1_000_000_000_000;

        if self.0 >= TB {
            let tb = self.0 as f64 / TB as f64;
            write!(f, "{:.1} TB", tb)
        } else if self.0 >= GB {
            // For GB, show whole numbers when >= 1 GB
            write!(f, "{} GB", self.0 / GB)
        } else if self.0 >= MB {
            write!(f, "{} MB", self.0 / MB)
        } else if self.0 >= KB {
            write!(f, "{} KB", self.0 / KB)
        } else {
            write!(f, "{} bytes", self.0)
        }
    }
}

impl From<u64> for ByteSize {
    fn from(bytes: u64) -> Self {
        ByteSize(bytes)
    }
}

// ============================================================================
// Type Aliases for Clarity
// ============================================================================

/// Alias for file sizes (makes intent clear in APIs)
pub type FileSize = ByteSize;

/// Alias for partition sizes
pub type PartitionSize = ByteSize;

/// Alias for storage capacity
pub type StorageCapacity = ByteSize;

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    #[rstest]
    #[case(ByteSize::from_gb(1), 1_000_000_000)]
    #[case(ByteSize::from_mb(1), 1_000_000)]
    #[case(ByteSize::from_kb(1), 1_000)]
    #[case(ByteSize::from_tb(1), 1_000_000_000_000)]
    fn constructors(#[case] size: ByteSize, #[case] expected_bytes: u64) {
        assert_eq!(size.bytes(), expected_bytes);
    }

    #[test]
    fn conversions() {
        let size = ByteSize::from_gb(512);
        assert_eq!(size.gigabytes(), 512);
        assert_eq!(size.megabytes(), 512_000);
        assert_eq!(size.bytes(), 512_000_000_000);
    }

    #[rstest]
    #[case(ByteSize::from_tb(2), "2.0 TB")]
    #[case(ByteSize::from_gb(512), "512 GB")]
    #[case(ByteSize::from_mb(100), "100 MB")]
    #[case(ByteSize::from_kb(50), "50 KB")]
    #[case(ByteSize::new(500), "500 bytes")]
    fn display_formatting(#[case] size: ByteSize, #[case] expected: &str) {
        assert_eq!(size.to_string(), expected);
    }

    #[test]
    fn fractional_gb() {
        let size = ByteSize::new(1_500_000_000);
        assert_eq!(size.gigabytes_f64(), 1.5);
    }

    #[test]
    fn saturating_arithmetic() {
        let size1 = ByteSize::from_gb(100);
        let size2 = ByteSize::from_gb(200);

        assert_eq!(size1.saturating_add(size2).gigabytes(), 300);
        assert_eq!(size2.saturating_sub(size1).gigabytes(), 100);
        assert_eq!(size1.saturating_sub(size2).bytes(), 0); // Saturates at 0
    }

    #[test]
    fn ordering_gb_less_than_gb() {
        assert!(ByteSize::from_gb(100) < ByteSize::from_gb(200));
    }

    #[test]
    fn ordering_mb_less_than_gb() {
        assert!(ByteSize::from_mb(100) < ByteSize::from_gb(1));
    }

    #[test]
    fn type_aliases() {
        let _file: FileSize = ByteSize::from_mb(50);
        let _partition: PartitionSize = ByteSize::from_gb(400);
        let _storage: StorageCapacity = ByteSize::from_tb(2);
        // Type aliases compile successfully
    }
}
