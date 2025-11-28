// bases/beacon/src/types.rs
use serde::{Deserialize, Serialize};
use std::fmt;

/// Newtype for hostname to ensure validity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hostname(String);

impl Hostname {
    pub fn new(s: String) -> Result<Self, String> {
        // Validate hostname: alphanumeric, hyphens, dots
        // Must start with alphanumeric, max 253 chars
        if s.is_empty() || s.len() > 253 {
            return Err("hostname must be 1-253 characters".to_string());
        }
        
        if !s.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '.') {
            return Err("hostname contains invalid characters".to_string());
        }
        
        if s.starts_with('-') || s.starts_with('.') {
            return Err("hostname cannot start with hyphen or dot".to_string());
        }
        
        Ok(Hostname(s))
    }
    
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Hostname {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Newtype for SSH public key
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshPublicKey(String);

impl SshPublicKey {
    pub fn new(s: String) -> Result<Self, String> {
        // Basic validation: must start with ssh-rsa, ssh-ed25519, etc.
        let trimmed = s.trim();
        
        if !trimmed.starts_with("ssh-rsa ")
            && !trimmed.starts_with("ssh-ed25519 ")
            && !trimmed.starts_with("ecdsa-sha2-") 
        {
            return Err("SSH key must start with ssh-rsa, ssh-ed25519, or ecdsa-sha2-".to_string());
        }
        
        // Must have at least 2 space-separated parts (type and key)
        if trimmed.split_whitespace().count() < 2 {
            return Err("SSH key format invalid".to_string());
        }
        
        Ok(SshPublicKey(trimmed.to_string()))
    }
    
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SshPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unit type selection
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UnitType {
    Mdma909,
    Mdma101,
    Mdma303,
}

impl UnitType {
    pub fn as_str(&self) -> &'static str {
        match self {
            UnitType::Mdma909 => "mdma-909",
            UnitType::Mdma101 => "mdma-101",
            UnitType::Mdma303 => "mdma-303",
        }
    }
    
    pub fn requires_dual_nvme(&self) -> bool {
        matches!(self, UnitType::Mdma909)
    }
}

impl fmt::Display for UnitType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Device path newtype
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePath(String);

impl DevicePath {
    pub fn new(s: String) -> Self {
        DevicePath(s)
    }
    
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DevicePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Storage capacity in bytes
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StorageBytes(u64);

impl StorageBytes {
    pub fn new(bytes: u64) -> Self {
        StorageBytes(bytes)
    }
    
    pub fn bytes(&self) -> u64 {
        self.0
    }
    
    pub fn gigabytes(&self) -> f64 {
        self.0 as f64 / 1_000_000_000.0
    }
}

impl fmt::Display for StorageBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.1} GB", self.gigabytes())
    }
}

/// Provisioning configuration submitted by user
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionConfig {
    pub unit_type: UnitType,
    pub hostname: Hostname,
    pub ssh_key: SshPublicKey,
}
