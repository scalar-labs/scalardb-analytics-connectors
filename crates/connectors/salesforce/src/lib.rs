//! The Salesforce connector.
//!
//! It reads Salesforce directly through the REST API client in
//! `connector-salesforce-client`, which holds the protocol: authentication,
//! HTTP, paging, and the JSON forms of values. The connector holds what the
//! data means: the catalog read from describe results and its type mapping,
//! which offered filters SOQL evaluates with the same meaning, the SOQL it
//! generates, and the classification of Salesforce's errors.

#![forbid(unsafe_code)]

mod access;
mod catalog;
mod error_handler;
mod options;
mod pushdown;
mod query;

pub use access::{SalesforceAccess, SalesforceSession};
pub use catalog::{SalesforceCatalog, SalesforceField, SalesforceObject};
pub use error_handler::SalesforceErrorHandler;
pub use pushdown::{PlanningFacts, SalesforcePlanner};
pub use query::{SalesforceExecutor, SoqlRead};

use std::sync::Arc;

use analytics_types::AnalyticsType;
use arrow_array::ArrayRef;
use connector_sdk::{Access, ConnectionOptions, Connector, ConnectorDescriptor, ConnectorError};

/// The Connector name callers pass when opening a Data Source.
pub const CONNECTOR: &str = "salesforce";

/// The REST API version every request uses. It is a fixed setting: another
/// version can describe fields differently, so changing it raises the
/// connector's version.
pub const API_VERSION: &str = "62.0";

/// The Salesforce Connector.
pub struct SalesforceConnector;

impl Connector for SalesforceConnector {
    type Access = SalesforceAccess;
    type Catalog = SalesforceCatalog;
    type Planner = SalesforcePlanner;
    type Executor = SalesforceExecutor;
    type ErrorHandler = SalesforceErrorHandler;
    type Query = SoqlRead;
    /// No connection option changes how types map.
    type Settings = ();

    fn descriptor() -> ConnectorDescriptor {
        ConnectorDescriptor {
            name: "salesforce".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            connectors: vec![CONNECTOR.to_owned()],
            options: options::descriptors(),
            access: Self::Access::description(),
        }
    }

    fn settings(_options: &ConnectionOptions) -> Result<(), ConnectorError> {
        Ok(())
    }

    /// The client already decodes each field into its Analytics storage, so
    /// every column passes through unchanged.
    fn convert_column(
        column: &ArrayRef,
        _target: &AnalyticsType,
    ) -> Result<ArrayRef, ConnectorError> {
        Ok(Arc::clone(column))
    }
}
