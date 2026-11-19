//! The Amazon DynamoDB connector.
//!
//! It reads DynamoDB directly through the AWS SDK for Rust, which holds the
//! protocol: request signing, the credential chain, endpoints, retries, and
//! the JSON forms of attribute values. The connector holds what the data
//! means: the catalog read from `ListTables` and `DescribeTable` and its type
//! mapping, the `Scan` or `Query` that implements a read, and the
//! classification of DynamoDB's errors.
//!
//! DynamoDB declares the types of key attributes only, so a table's columns
//! are its key attributes and one `item` column that holds each whole item
//! as DynamoDB JSON.

#![forbid(unsafe_code)]

mod access;
mod catalog;
mod error_handler;
mod options;
mod pushdown;
mod query;

pub use access::{DynamoDbAccess, DynamoDbSession};
pub use catalog::{
    AttributeDefinition, DynamoDbCatalog, DynamoDbTable, ITEM, KeyElement, KeyType, NAMESPACE,
    Projection, SecondaryIndex, StreamSpecification,
};
pub use error_handler::{DynamoDbErrorHandler, Error, TransportKind};
pub use pushdown::{DynamoDbPlanner, PlanningFacts};
pub use query::{Comparison, DynamoDbExecutor, KeyComparison, KeyValue, Projected, TableRead};

use std::sync::Arc;

use analytics_types::AnalyticsType;
use arrow_array::ArrayRef;
use connector_sdk::{Access, ConnectionOptions, Connector, ConnectorDescriptor, ConnectorError};

/// The Connector name callers pass when opening a Data Source.
pub const CONNECTOR: &str = "dynamodb";

/// The DynamoDB Connector.
pub struct DynamoDbConnector;

impl Connector for DynamoDbConnector {
    type Access = DynamoDbAccess;
    type Catalog = DynamoDbCatalog;
    type Planner = DynamoDbPlanner;
    type Executor = DynamoDbExecutor;
    type ErrorHandler = DynamoDbErrorHandler;
    type Query = TableRead;
    /// No connection option changes how types map.
    type Settings = ();

    fn descriptor() -> ConnectorDescriptor {
        ConnectorDescriptor {
            name: "dynamodb".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            connectors: vec![CONNECTOR.to_owned()],
            options: options::descriptors(),
            access: Self::Access::description(),
        }
    }

    fn settings(_options: &ConnectionOptions) -> Result<(), ConnectorError> {
        Ok(())
    }

    /// The executor already decodes each attribute into its Analytics
    /// storage, so every column passes through unchanged.
    fn convert_column(
        column: &ArrayRef,
        _target: &AnalyticsType,
    ) -> Result<ArrayRef, ConnectorError> {
        Ok(Arc::clone(column))
    }
}
