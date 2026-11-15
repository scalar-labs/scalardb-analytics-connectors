//! Data Source-independent connector processing and the Connector entry point.
//!
//! A Connector implements [`Connector`] and the components it names; the
//! flows every Connector shares are provided by the SDK. The connector API
//! drives connectors through [`runtime`], which implements the contract's
//! units and calling conventions on top of [`Connector`].

#![forbid(unsafe_code)]

#[cfg(feature = "conformance")]
pub mod conformance;
mod connector;
pub mod convert;
mod error;
mod execution;
mod lifecycle;
mod metadata;
mod planning;
pub mod runtime;
#[cfg(feature = "test-connector")]
pub mod test_connector;

pub use connector::{
    Access, AccessSession, Catalog, Connector, DataSource, Diagnostic, ErrorClass, ErrorHandler,
    Executor, GenericErrorHandler, PlannedRead, Planner, Session,
};
pub use error::{ConnectorError, DiagnosticEntry, DiagnosticValue, ErrorKind, RequestError};
pub use execution::{
    ConnectorStream, ExecutionCancel, ExecutionCancelHandle, ExecutionOutput, ExecutionStream,
    RecordBatchStream, SourceStream, execute_planned, start_source_read,
};
pub use lifecycle::{
    Component, ConnectionOptions, ConnectorDescriptor, DataSourceAccess, LibrarySettings,
    OptionDescriptor, OptionValue, OptionValueType, SecretString,
};
pub use metadata::{
    Change, Column, ColumnAttribute, ColumnType, Constraint, ConstraintAttribute, ConstraintKind,
    JsonDifference, Listed, NameFilter, NamespacePath, Reported, SourceTableMetadata,
    TableAttribute, TableEntry, TableKind, TableMetadata, TableMetadataDifferences, TableRecord,
    TableReference, UniqueKind, UnsupportedReason, check_schema,
};
pub use planning::{
    BOOLEAN_FUNCTIONS_URI, BinaryOperator, COMPARISON_FUNCTIONS_URI, ConnectorPlanner,
    ExecutableQueryResult, LOGICAL_TYPE_URI, LiteralValue, LogicalExpression,
    LogicalExpressionKind, LogicalPlan, LogicalRelation, PlannedPushdown, PlanningError,
    PortableQuery, PushdownRequest, PushdownResult, QueryPlan, REMOTE_RESULT_TYPE_URL,
    RemoteExecution, RewriteContext, RewriteRule, RewriteRuleConnectorPlanner, RewriteRuleSet,
    TableBinding, UnaryOperator,
};
