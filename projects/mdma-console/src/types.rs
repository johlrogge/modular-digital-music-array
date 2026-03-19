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
    use rstest::rstest;

    #[rstest]
    #[case("mdma-library")]
    #[case("mdma-console")]
    #[case("beacon")]
    fn package_name_valid(#[case] name: &str) {
        assert!(PackageName::new(name).is_ok());
    }

    #[rstest]
    #[case("vim")]
    #[case("nginx")]
    #[case("some-package")]
    fn package_name_rejects_non_mdma(#[case] name: &str) {
        assert!(PackageName::new(name).is_err());
    }

    #[rstest]
    #[case("mdma-library; rm -rf /")]
    #[case("mdma-library$(whoami)")]
    #[case("mdma-library`id`")]
    fn package_name_rejects_injection(#[case] name: &str) {
        assert!(PackageName::new(name).is_err());
    }

    #[rstest]
    #[case("mdma-library")]
    #[case("mdma-console")]
    #[case("beacon")]
    fn service_name_valid(#[case] name: &str) {
        assert!(ServiceName::new(name).is_ok());
    }

    #[rstest]
    #[case("sshd")]
    #[case("nginx")]
    fn service_name_rejects_non_mdma(#[case] name: &str) {
        assert!(ServiceName::new(name).is_err());
    }
}
