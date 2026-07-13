use bevy::prelude::*;

/// Configuration for the DJ Workspace Bevy app.
///
/// Constructed by the project binary from CLI args or environment,
/// then inserted as a Bevy Resource before the app starts.
#[derive(Resource, Debug, Clone)]
pub struct DjWorkspaceConfig {
    /// Gateway TCP address, e.g. `tcp://mdma-909.local:5555`.
    /// None means connect directly via IPC.
    pub gateway: Option<String>,
    /// Library IPC socket path, e.g. `ipc:///run/mdma/library.sock`.
    pub library_socket: String,
}
