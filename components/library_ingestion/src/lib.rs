//! Library ingestion pipeline — typestate stages for importing audio into the MDMA library.
//!
//! Both modules reference each other via `crate::` and must live in the same crate.

pub mod fact_generator;
pub(crate) mod filename_parser;
pub mod pipeline;

pub use pipeline::*;
