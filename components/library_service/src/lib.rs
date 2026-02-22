//! Library service — core business logic for the MDMA music library.
//!
//! Extracted from the mdma-library binary so it can be imported by BDD tests
//! and other consumers.

pub mod fact_generator;
pub mod fact_writer;
pub mod ipc;
pub mod pipeline;
pub mod service;

pub use ipc::IpcServer;
pub use service::{LibraryService, ServiceError};
