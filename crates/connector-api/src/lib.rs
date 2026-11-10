//! Connector C API and source-connector assembly.
//!
//! The C functions and types are declared in `include/scalardb_analytics_connector.h`,
//! which is generated from the [`abi`] module. The contract's units and calling
//! conventions are implemented by `connector_sdk::runtime`; this crate adds
//! the C representation, the process-wide library, and the connectors built
//! into this library (`registry`).

#![deny(unsafe_code)]

#[allow(unsafe_code)]
pub mod abi;
mod library;
mod registry;
