pub mod ipc;
pub mod service;

pub use ipc::IpcServer;
pub use service::{LibraryService, ServiceError};
