//! Console-specific newtypes for security validation

use thiserror::Error;

/// Error when constructing a PackageName
#[derive(Debug, Error)]
pub enum PackageNameError {
    #[error("not an MDMA package (must start with 'mdma-' or be 'beacon')")]
    NotMdmaPackage,
    #[error("invalid characters (only alphanumeric and hyphens allowed)")]
    InvalidCharacters,
}

/// Validated MDMA package name - prevents shell injection.
///
/// Only allows packages that:
/// - Start with "mdma-" or are exactly "beacon"
/// - Contain only alphanumeric characters and hyphens
#[derive(Debug, Clone)]
pub struct PackageName(String);

impl PackageName {
    /// Create a new PackageName after validation.
    pub fn new(name: &str) -> Result<Self, PackageNameError> {
        // Must start with "mdma-" or be "beacon"
        if !name.starts_with("mdma-") && name != "beacon" {
            return Err(PackageNameError::NotMdmaPackage);
        }
        // Must be alphanumeric + hyphens only
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(PackageNameError::InvalidCharacters);
        }
        Ok(Self(name.to_string()))
    }

    /// Get the package name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PackageName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Error when constructing a ServiceName
#[derive(Debug, Error)]
pub enum ServiceNameError {
    #[error("not an MDMA service (must start with 'mdma-' or be 'beacon')")]
    NotMdmaService,
    #[error("invalid characters (only alphanumeric and hyphens allowed)")]
    InvalidCharacters,
}

/// Validated runit service name - prevents shell injection.
///
/// Only allows services that:
/// - Start with "mdma-" or are exactly "beacon"
/// - Contain only alphanumeric characters and hyphens
#[derive(Debug, Clone)]
pub struct ServiceName(String);

impl ServiceName {
    /// Create a new ServiceName after validation.
    pub fn new(name: &str) -> Result<Self, ServiceNameError> {
        // Must start with "mdma-" or be "beacon"
        if !name.starts_with("mdma-") && name != "beacon" {
            return Err(ServiceNameError::NotMdmaService);
        }
        // Must be alphanumeric + hyphens only
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(ServiceNameError::InvalidCharacters);
        }
        Ok(Self(name.to_string()))
    }

    /// Get the service name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ServiceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_valid() {
        assert!(PackageName::new("mdma-library").is_ok());
        assert!(PackageName::new("mdma-console").is_ok());
        assert!(PackageName::new("beacon").is_ok());
    }

    #[test]
    fn package_name_rejects_non_mdma() {
        assert!(PackageName::new("vim").is_err());
        assert!(PackageName::new("nginx").is_err());
        assert!(PackageName::new("some-package").is_err());
    }

    #[test]
    fn package_name_rejects_injection() {
        assert!(PackageName::new("mdma-library; rm -rf /").is_err());
        assert!(PackageName::new("mdma-library$(whoami)").is_err());
        assert!(PackageName::new("mdma-library`id`").is_err());
    }

    #[test]
    fn service_name_valid() {
        assert!(ServiceName::new("mdma-library").is_ok());
        assert!(ServiceName::new("mdma-console").is_ok());
        assert!(ServiceName::new("beacon").is_ok());
    }

    #[test]
    fn service_name_rejects_non_mdma() {
        assert!(ServiceName::new("sshd").is_err());
        assert!(ServiceName::new("nginx").is_err());
    }
}
