//! The Connector: the library's entry point, the components it is built from,
//! and the flows the SDK runs for every Connector.
//!
//! A Connector interprets one kind of Data Source. It names its components
//! through associated types and declares what only it knows; the SDK runs the
//! flows that apply with the same meaning to every Connector:
//!
//! - pushdown planning: facts, the Connector's rewrite rules, query generation,
//!   and the decomposition checks ([`Connector::plan`]);
//! - a planned read: the schema check against the table's current metadata,
//!   the source read, value conversion, and one cancel across the steps
//!   ([`Connector::read`]);
//! - error classification: redaction, the Connector's error table, and the
//!   access's generic classes ([`Connector::classify`]).
//!
//! Shared layers such as ADBC access and SQL processing provide components
//! that implement [`Access`], [`Catalog`], [`Planner`], and [`Executor`] for
//! the Connectors that declare what those layers need.

mod error;
mod flow;
mod unit;

pub use error::{ErrorClass, ErrorHandler, GenericErrorHandler};
pub use flow::PlannedRead;
pub use unit::{DataSource, Session};

pub(crate) use unit::ConnectorState;

use std::time::Duration;

use analytics_types::AnalyticsType;
use arrow_array::ArrayRef;

use crate::{
    Component, ConnectionOptions, ConnectorDescriptor, ConnectorError, DataSourceAccess,
    ExecutableQueryResult, ExecutionOutput, LibrarySettings, Listed, LogicalPlan, NameFilter,
    NamespacePath, PlanningError, PortableQuery, PushdownRequest, PushdownResult, RemoteExecution,
    RewriteRuleSet, SourceTableMetadata, TableEntry, TableRecord, TableReference,
};

/// The diagnostic a Connector's access reports for a Data Source error.
pub type Diagnostic<C> = <<C as Connector>::Access as Access<C>>::Diagnostic;

/// The per-session state of a Connector's access, such as one connection.
pub type AccessSession<C> = <<C as Connector>::Access as Access<C>>::Session;

/// The interpreter of one kind of Data Source, and the entry point of the
/// library.
///
/// The associated types select the components the Connector is built from.
/// The required functions declare what only this Connector knows. The
/// provided functions are the flows every Connector shares.
pub trait Connector: Sized + Send + Sync + 'static {
    /// How the Data Source is reached.
    type Access: Access<Self>;
    /// How the Data Source's catalog answers metadata requests.
    type Catalog: Catalog<Self>;
    /// How offered plans are decomposed and the Data Source query generated.
    type Planner: Planner<Self>;
    /// How a generated query is sent to the Data Source.
    type Executor: Executor<Self>;
    /// The Connector's error table and redaction of the access's diagnostics.
    type ErrorHandler: ErrorHandler<Diagnostic = Diagnostic<Self>>;
    /// The Data Source query generated in planning and run by the executor.
    ///
    /// It may be planned in one process and run in another.
    type Query: PortableQuery + Send + Sync + 'static;

    /// Describes this Connector for static information and option validation.
    ///
    /// It is available before the library is initialized.
    fn descriptor() -> ConnectorDescriptor;

    /// Converts one column of source output to `target`'s Analytics storage,
    /// or returns it unchanged when the source already produced that storage.
    ///
    /// Only conversions that change no value are allowed; a value the target
    /// type cannot carry is a `data` error.
    fn convert_column(
        column: &ArrayRef,
        target: &AnalyticsType,
    ) -> Result<ArrayRef, ConnectorError>;

    /// Plans one offered read on `session`.
    ///
    /// The offered plan is validated, the planner's facts are read, its rewrite
    /// rules move work into the remote plan, a local plan that only returns
    /// the remote result is dropped, and the planner generates the query.
    fn plan(
        session: &Session<Self>,
        request: PushdownRequest,
    ) -> Result<PushdownResult<PlannedRead<Self::Query>>, PlanningError> {
        flow::plan(session, request)
    }

    /// Starts one planned read on `session`.
    ///
    /// Nothing is sent before the first pull. The first pull reads the
    /// table's current metadata through the catalog and checks the registered
    /// schema; a mismatch is `stale registration` and the query is never
    /// sent. Each batch the executor returns is converted with
    /// [`Connector::convert_column`] into the planned output schema. The
    /// returned cancel stops whichever step is running.
    fn read(
        session: &Session<Self>,
        remote: &RemoteExecution<PlannedRead<Self::Query>>,
    ) -> Result<ExecutionOutput, ConnectorError> {
        flow::read(session, remote)
    }

    /// Classifies one Data Source error reported by the access.
    ///
    /// The error handler first redacts the diagnostic; its error table then
    /// takes precedence over the access's generic classes.
    fn classify(diagnostic: Diagnostic<Self>) -> ConnectorError {
        let diagnostic = Self::ErrorHandler::redact(diagnostic);
        let class = Self::ErrorHandler::classify(&diagnostic);
        Self::Access::error(diagnostic, class)
    }
}

/// How a Connector reaches its Data Source.
///
/// The SDK's units hold the state this returns: the library holds
/// [`Access::Library`], a Data Source [`Access::DataSource`], and a session
/// [`Access::Session`].
pub trait Access<C: Connector>: 'static {
    /// Process-level state, created when the library is initialized.
    type Library: Send + Sync + 'static;
    /// The state of one opened Data Source, including its credentials.
    type DataSource: Send + Sync + 'static;
    /// The state of one session, such as one connection.
    type Session: Send + Sync + 'static;
    /// What the access reports about a Data Source error.
    type Diagnostic;

    /// The form of Data Source access, for the Connector's descriptor.
    fn description() -> DataSourceAccess;

    /// Creates the process-level state from the library settings.
    fn open_library(settings: &LibrarySettings) -> Result<Self::Library, ConnectorError>;

    /// Opens a Data Source of the named Connector.
    ///
    /// `options` were already validated against the descriptor.
    /// `connection_timeout` limits connecting while this Data Source and its
    /// sessions open; `None` means no limit.
    fn open_data_source(
        library: &Self::Library,
        connector: &str,
        options: ConnectionOptions,
        connection_timeout: Option<Duration>,
    ) -> Result<Self::DataSource, ConnectorError>;

    /// Opens one session, connecting when the Data Source has connections.
    fn open_session(data_source: &Self::DataSource) -> Result<Self::Session, ConnectorError>;

    /// The driver the session reads through, for retrieval info; `None` when
    /// the access has no driver.
    fn component(session: &Self::Session) -> Option<Component>;

    /// Builds the error for a diagnostic.
    ///
    /// `class` is the Connector's classification, when its error table
    /// matched; otherwise the access applies its generic classes.
    fn error(diagnostic: Self::Diagnostic, class: Option<ErrorClass>) -> ConnectorError;
}

/// How a Connector's catalog answers the three metadata requests.
///
/// Each function starts one catalog read and returns its item stream with the
/// means of stopping it. Nothing may be sent to the Data Source before the
/// first pull.
pub trait Catalog<C: Connector>: 'static {
    /// The Connector's source metadata for one table.
    type Table: SourceTableMetadata;
    /// Session-level facts the catalog reads once when a session opens, such
    /// as the server version that decides which catalog columns exist.
    type Context: Send + Sync + 'static;

    /// Reads the session's context on a newly opened session.
    fn open_context(session: &AccessSession<C>) -> Result<Self::Context, ConnectorError>;

    /// Lists namespaces whose names exactly match `filter`, or all of them.
    fn namespace_list(
        session: &Session<C>,
        filter: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<NamespacePath>>, ConnectorError>;

    /// Lists tables across the matching namespaces whose names match `tables`.
    fn table_list(
        session: &Session<C>,
        namespaces: &NameFilter,
        tables: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<TableEntry>>, ConnectorError>;

    /// Reads one record per requested table, including tables that do not
    /// exist or could not be read.
    fn table_metadata(
        session: &Session<C>,
        tables: &[TableReference],
    ) -> Result<ExecutionOutput<TableRecord<Self::Table>>, ConnectorError>;
}

/// How a Connector decomposes an offered plan and generates its query.
pub trait Planner<C: Connector>: 'static {
    /// Facts about the read table that one planning call needs; their shape
    /// is the Connector's.
    type Facts;

    /// Resolves the offered plan's table binding and reads the current facts
    /// its planning needs.
    ///
    /// Returns the table as metadata requests name it; the schema check of
    /// the planned read reads its metadata.
    fn planning_facts(
        session: &Session<C>,
        offered_plan: &LogicalPlan,
    ) -> Result<(TableReference, Self::Facts), PlanningError>;

    /// The ordered rules that move work from the local plan into the remote
    /// plan. The SDK builds them once per session.
    fn rewrite_rules() -> RewriteRuleSet<Self::Facts>;

    /// Generates the Data Source query that implements the final remote plan.
    fn generate_query(
        remote_plan: &LogicalPlan,
        facts: &Self::Facts,
    ) -> Result<ExecutableQueryResult<C::Query>, PlanningError>;
}

/// How a Connector sends a generated query to its Data Source.
pub trait Executor<C: Connector>: 'static {
    /// Starts the query and returns the source's own output.
    ///
    /// The SDK checks the schema before it, and converts and checks each
    /// batch after it. Nothing may be sent before the first pull.
    fn run(session: &Session<C>, query: &C::Query) -> Result<ExecutionOutput, ConnectorError>;
}
