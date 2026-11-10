//! Data Source-independent connector processing and Connector-facing traits.
//!
//! Connector implementations use the items at the crate root. The connector
//! API drives connectors through [`runtime`], which implements the contract's
//! units and calling conventions on top of those traits.

#![forbid(unsafe_code)]

mod error;
mod execution;
mod lifecycle;
mod metadata;
mod planning;
pub mod runtime;
#[cfg(feature = "test-connector")]
pub mod test_connector;

pub use error::{ConnectorError, DiagnosticEntry, DiagnosticValue, ErrorKind, RequestError};
pub use execution::{
    ConnectorExecutor, ConnectorStream, ExecutionCancel, ExecutionCancelHandle, ExecutionOutput,
    ExecutionStream, RecordBatchStream, SourceStream, execute_planned, start_source_read,
};
pub use lifecycle::{
    Component, ConnectionOptions, ConnectorDescriptor, ConnectorLibrary, DataSource,
    DataSourceAccess, DataSourceSession, LibrarySettings, OptionDescriptor, OptionValue,
    OptionValueType, SecretString,
};
pub use metadata::{
    CatalogReader, Column, ColumnAttribute, ColumnChange, ColumnType, Completeness, Constraint,
    ConstraintChange, ConstraintChanges, ConstraintKind, Difference, Listed, NameFilter,
    NamespacePath, Reported, Side, SourceTableMetadata, TableAttribute, TableChanges,
    TableComparison, TableEntry, TableKind, TableMetadata, TableRecord, TableReference, TableState,
    UniqueKind, UnsupportedReason,
};
pub use planning::{
    BOOLEAN_FUNCTIONS_URI, BinaryOperator, COMPARISON_FUNCTIONS_URI, ConnectorPlanner,
    ExecutableQueryResult, LOGICAL_TYPE_URI, LiteralValue, LogicalExpression,
    LogicalExpressionKind, LogicalPlan, LogicalRelation, PlannedPushdown, PlanningError,
    PortableQuery, PushdownRequest, PushdownResult, QueryPlan, REMOTE_RESULT_TYPE_URL,
    RemoteExecution, RewriteContext, RewriteRule, RewriteRuleConnectorPlanner, RewriteRuleSet,
    TableBinding, UnaryOperator,
};
