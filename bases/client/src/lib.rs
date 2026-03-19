/// Configuration for an MDMA client project connecting to the gateway.
pub struct ClientConfig {
    /// Hostname or IP of the MDMA node (e.g. "mdma-909.local").
    /// None means connect directly via IPC (local dev mode).
    pub node: Option<String>,
    /// Gateway request/response port (default 5555).
    pub gateway_port: u16,
    /// Gateway event pub/sub port (default 5556).
    pub event_port: u16,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            node: None,
            gateway_port: 5555,
            event_port: 5556,
        }
    }
}

impl ClientConfig {
    /// Returns the gateway TCP address, or None if in direct IPC mode.
    pub fn gateway_addr(&self) -> Option<String> {
        self.node
            .as_deref()
            .map(|n| format!("tcp://{}:{}", n, self.gateway_port))
    }

    /// Returns the event subscription TCP address, or None if in direct IPC mode.
    pub fn event_addr(&self) -> Option<String> {
        self.node
            .as_deref()
            .map(|n| format!("tcp://{}:{}", n, self.event_port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_addr_with_node() {
        let cfg = ClientConfig {
            node: Some("mdma-909.local".into()),
            ..Default::default()
        };
        assert_eq!(cfg.gateway_addr(), Some("tcp://mdma-909.local:5555".into()));
    }

    #[test]
    fn gateway_addr_without_node_is_none() {
        let cfg = ClientConfig::default();
        assert_eq!(cfg.gateway_addr(), None);
    }

    #[test]
    fn event_addr_with_node() {
        let cfg = ClientConfig {
            node: Some("mdma-909.local".into()),
            ..Default::default()
        };
        assert_eq!(cfg.event_addr(), Some("tcp://mdma-909.local:5556".into()));
    }

    #[test]
    fn event_addr_without_node_is_none() {
        let cfg = ClientConfig::default();
        assert_eq!(cfg.event_addr(), None);
    }

    #[test]
    fn default_ports() {
        let cfg = ClientConfig::default();
        assert_eq!(cfg.gateway_port, 5555);
        assert_eq!(cfg.event_port, 5556);
    }
}
