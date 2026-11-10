//! Data Source-independent connector processing and Connector-facing traits.

#![forbid(unsafe_code)]

mod error;
mod execution;
mod metadata;
mod planning;

pub use error::{ConnectorError, DiagnosticEntry, DiagnosticValue, ErrorKind, RequestError};
pub use execution::{
    ConnectorExecutor, ExecutionCancel, ExecutionCancelHandle, ExecutionOutput, ExecutionStream,
    RecordBatchStream, execute_planned,
};
pub use metadata::{
    CatalogReader, ConnectorLibrary, DataSource, DataSourceSession, MetadataGap, MetadataState,
    TableMetadata, TableReference, TypeMapper, UnsupportedType,
};
pub use planning::{
    BOOLEAN_FUNCTIONS_URI, BinaryOperator, COMPARISON_FUNCTIONS_URI, ConnectorPlanner,
    ExecutableQueryResult, LOGICAL_TYPE_URI, LiteralValue, LogicalExpression,
    LogicalExpressionKind, LogicalPlan, LogicalRelation, PlannedPushdown, PlanningError,
    PortableQuery, PushdownRequest, PushdownResult, QueryPlan, REMOTE_RESULT_TYPE_URL,
    RemoteExecution, RewriteContext, RewriteRule, RewriteRuleConnectorPlanner, RewriteRuleSet,
    TableBinding, UnaryOperator,
};
