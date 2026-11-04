//! A node named by the user: `host` or `host:port`.

use std::fmt;
use std::str::FromStr;

/// A node named on the command line or in `MDMA_NODE`. Parsed strictly: a port
/// suffix that is not a valid port is an error, never part of the host.
///
/// IPv6 literals must be bracketed (`[::1]` or `[::1]:5555`); the brackets are
/// kept in `host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSpec {
    host: String,
    port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NodeSpecError {
    #[error("node is empty, expected host[:port]")]
    Empty,
    #[error("node '{0}' has no host, expected host[:port]")]
    NoHost(String),
    #[error("invalid port '{port}' in node '{spec}', expected 1-65535")]
    BadPort { spec: String, port: String },
    #[error("invalid node '{0}', IPv6 addresses must be bracketed, e.g. [::1]:5555")]
    BadHost(String),
}

impl NodeSpec {
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port, when one was given.
    pub fn port(&self) -> Option<u16> {
        self.port
    }
}

fn parse_port(spec: &str, port: &str) -> Result<u16, NodeSpecError> {
    match port.parse::<u16>() {
        Ok(p) if p != 0 => Ok(p),
        _ => Err(NodeSpecError::BadPort {
            spec: spec.to_string(),
            port: port.to_string(),
        }),
    }
}

impl FromStr for NodeSpec {
    type Err = NodeSpecError;

    fn from_str(spec: &str) -> Result<Self, Self::Err> {
        if spec.is_empty() {
            return Err(NodeSpecError::Empty);
        }
        let bad_host = || NodeSpecError::BadHost(spec.to_string());
        let (host, port) = if spec.starts_with('[') {
            let end = spec.find(']').ok_or_else(bad_host)?;
            let (host, rest) = spec.split_at(end + 1);
            let port = match rest {
                "" => None,
                _ => Some(parse_port(
                    spec,
                    rest.strip_prefix(':').ok_or_else(bad_host)?,
                )?),
            };
            if host.len() <= 2 {
                return Err(NodeSpecError::NoHost(spec.to_string()));
            }
            (host, port)
        } else {
            match spec.split_once(':') {
                None => (spec, None),
                Some((host, port)) => {
                    if port.contains(':') {
                        return Err(bad_host());
                    }
                    (host, Some(parse_port(spec, port)?))
                }
            }
        };
        if host.is_empty() {
            return Err(NodeSpecError::NoHost(spec.to_string()));
        }
        Ok(NodeSpec {
            host: host.to_string(),
            port,
        })
    }
}

impl fmt::Display for NodeSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.port {
            Some(port) => write!(f, "{}:{port}", self.host),
            None => f.write_str(&self.host),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(s: &str) -> NodeSpec {
        s.parse().unwrap()
    }

    #[test]
    fn host_alone_has_no_port() {
        let s = spec("mdma-909.local");
        assert_eq!((s.host(), s.port()), ("mdma-909.local", None));
    }

    #[test]
    fn host_with_port() {
        let s = spec("a.local:6000");
        assert_eq!((s.host(), s.port()), ("a.local", Some(6000)));
        assert_eq!(s.to_string(), "a.local:6000");
    }

    #[test]
    fn bracketed_ipv6_with_and_without_port() {
        let s = spec("[::1]:5555");
        assert_eq!((s.host(), s.port()), ("[::1]", Some(5555)));
        let s = spec("[::1]");
        assert_eq!((s.host(), s.port()), ("[::1]", None));
        assert_eq!(s.to_string(), "[::1]");
    }

    #[test]
    fn bad_ports_are_errors() {
        for bad in ["h:notaport", "h:", "h:0", "h:70000", "h:-1", "[::1]:x"] {
            assert!(
                matches!(bad.parse::<NodeSpec>(), Err(NodeSpecError::BadPort { .. })),
                "{bad}"
            );
        }
        assert_eq!(
            "h:notaport".parse::<NodeSpec>().unwrap_err().to_string(),
            "invalid port 'notaport' in node 'h:notaport', expected 1-65535"
        );
    }

    #[test]
    fn empty_and_hostless_are_errors() {
        assert_eq!("".parse::<NodeSpec>(), Err(NodeSpecError::Empty));
        for bad in [":5555", "[]", "[]:5555"] {
            assert!(
                matches!(bad.parse::<NodeSpec>(), Err(NodeSpecError::NoHost(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn malformed_hosts_are_errors() {
        for bad in ["::1", "a:b:5555", "[::1", "[::1]x", "[::1]5555"] {
            assert!(
                matches!(bad.parse::<NodeSpec>(), Err(NodeSpecError::BadHost(_))),
                "{bad}"
            );
        }
    }
}
