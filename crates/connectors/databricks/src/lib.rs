//! The Databricks connector.
//!
//! It reads Databricks through a SQL warehouse with the ADBC Driver Foundry
//! Databricks driver. The connector declares what is specific to Databricks:
//! its connection options and their driver options, its fixed session
//! settings, its Unity Catalog reading and type mapping, which offered work
//! Databricks runs with the same meaning, its SQL spelling, the conversion of
//! the driver's output, and its handling of the driver's errors. The shared
//! ADBC and SQL layers implement the connector lifecycle, planning, and
//! execution from these declarations.

#![forbid(unsafe_code)]

mod catalog;
mod dialect;
mod driver_output;
mod error_handler;
mod options;
mod pushdown;

pub use catalog::{
    DatabricksColumn, DatabricksForeignKey, DatabricksPrimaryKey, DatabricksReferencedTable,
    DatabricksTable,
};
pub use error_handler::DatabricksErrorHandler;
pub use pushdown::PlanningFacts;

use std::{sync::Arc, time::Duration};

use adbc_core::{
    error::{Error as AdbcError, Status},
    options::{OptionDatabase, OptionValue as AdbcOptionValue},
};
use analytics_types::AnalyticsType;
use arrow_array::ArrayRef;
use connector_adbc::{
    Adbc, AdbcConnection, AdbcConnector, AdbcQueryCancelHandle, AdbcSession, DriverRequirement,
    DriverVersion, query,
};
use connector_sdk::{
    Access, ConnectionOptions, Connector, ConnectorDescriptor, ConnectorError, ExecutionCancel,
    ExecutionOutput, Listed, LogicalExpression, LogicalPlan, NameFilter, NamespacePath,
    PlanningError, Session, TableEntry, TableMetadata, TableRecord, TableReference,
};
use connector_sql::{
    SqlCatalog, SqlConnector, SqlExecutor, SqlPlanner, SqlRead, catalog::CatalogReads,
};

use options::DatabricksOptions;

/// The Connector name callers pass when opening a Data Source.
pub const CONNECTOR: &str = "databricks";

/// The ADBC driver: the ADBC Driver Foundry Databricks driver, of the
/// release this connector is written against.
const DRIVER: DriverRequirement = DriverRequirement {
    library: "adbc_driver_databricks",
    minimum: DriverVersion {
        major: 0,
        minor: 4,
        patch: 0,
    },
    below: DriverVersion {
        major: 0,
        minor: 5,
        patch: 0,
    },
};

/// Session settings fixed on each connection: timestamps are shown in UTC,
/// and arithmetic and casts follow ANSI SQL, raising errors instead of
/// returning NULL.
///
/// The driver has no option for session settings, so they run as
/// statements when a session opens. Each ADBC connection of the driver holds
/// one Databricks session, which keeps them for every later statement.
/// Generated literals depend on neither; the expressions of the views read
/// are evaluated under them.
const FIXED_SESSION_STATEMENTS: [&str; 2] = ["SET TIME ZONE 'UTC'", "SET ANSI_MODE = true"];

/// The Databricks Connector.
pub struct DatabricksConnector;

/// The Connector's settings of one Databricks Data Source, from its
/// connection options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DatabricksSettings {
    /// The one catalog the namespace and table lists hold, when the
    /// `catalog` option names one.
    pub catalog: Option<String>,
}

impl Connector for DatabricksConnector {
    type Access = Adbc<Self>;
    type Catalog = SqlCatalog<Self>;
    type Planner = SqlPlanner<Self>;
    type Executor = SqlExecutor<Self>;
    type ErrorHandler = DatabricksErrorHandler;
    type Query = SqlRead;
    type Settings = DatabricksSettings;

    /// Validates the options and keeps the listing catalog.
    fn settings(options: &ConnectionOptions) -> Result<DatabricksSettings, ConnectorError> {
        Ok(DatabricksSettings {
            catalog: DatabricksOptions::parse(options)?.catalog,
        })
    }

    fn descriptor() -> ConnectorDescriptor {
        ConnectorDescriptor {
            name: CONNECTOR.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            connectors: vec![CONNECTOR.to_owned()],
            options: options::descriptors(),
            access: Self::Access::description(),
        }
    }

    fn convert_column(
        column: &ArrayRef,
        target: &AnalyticsType,
    ) -> Result<ArrayRef, ConnectorError> {
        driver_output::convert_column(column, target)
    }
}

impl AdbcConnector for DatabricksConnector {
    const DRIVER: DriverRequirement = DRIVER;

    /// Passes the validated options as the driver's options. The driver
    /// cannot limit connecting, so `connection_timeout` is not passed.
    fn database_options(
        options: ConnectionOptions,
        _connection_timeout: Option<Duration>,
    ) -> Result<Vec<(OptionDatabase, AdbcOptionValue)>, ConnectorError> {
        Ok(DatabricksOptions::parse(&options)?.driver_options())
    }
}

/// Stops one statement the driver runs.
///
/// The driver cancels a statement only while `ExecuteQuery` waits for it: it
/// then sends `CancelOperation` to the warehouse. `ExecuteQuery` returns only
/// once the statement has finished at the warehouse, so afterwards only its
/// result remains to be fetched, and the driver answers a cancel with
/// `invalid state`, having no statement to stop. The read is stopped all the
/// same: its next pull returns the stop, and releasing its stream closes the
/// operation and aborts the result's downloads in flight.
struct QueryCancel(AdbcQueryCancelHandle<DatabricksConnector>);

impl ExecutionCancel for QueryCancel {
    fn cancel(&self) -> Result<(), ConnectorError> {
        cancel_outcome(self.0.try_cancel())
    }
}

/// The outcome of a cancel the driver answered with `result`: `invalid
/// state` means that no statement was running at the warehouse.
fn cancel_outcome(result: Result<(), AdbcError>) -> Result<(), ConnectorError> {
    match result {
        Err(error) if error.status == Status::InvalidState => Ok(()),
        result => result.map_err(|error| DatabricksConnector::classify(error.into())),
    }
}

impl SqlConnector for DatabricksConnector {
    type Connection = AdbcConnection<Self>;
    type Table = DatabricksTable;
    type Context = ();
    type PlanningFacts = PlanningFacts;

    fn connection(session: &AdbcSession<Self>) -> AdbcConnection<Self> {
        session.connection().clone()
    }

    /// Runs the statement through the ADBC driver.
    fn query(connection: &AdbcConnection<Self>, sql: String) -> ExecutionOutput {
        let statement = query(connection, sql);
        let cancel = Arc::new(QueryCancel(statement.cancel_handle()));
        ExecutionOutput::new(Box::pin(statement), cancel)
    }

    /// Runs the fixed session settings on the session's connection.
    fn session_context(catalog: &mut CatalogReads<'_, Self>) -> Result<(), ConnectorError> {
        for statement in FIXED_SESSION_STATEMENTS {
            catalog.query(statement)?;
        }
        Ok(())
    }

    fn namespace_list(
        catalog: &mut CatalogReads<'_, Self>,
        settings: &DatabricksSettings,
        filter: &NameFilter,
    ) -> Result<Vec<Listed<NamespacePath>>, ConnectorError> {
        catalog::namespace_list(catalog, settings, filter)
    }

    fn table_list(
        catalog: &mut CatalogReads<'_, Self>,
        settings: &DatabricksSettings,
        namespaces: &NameFilter,
        tables: &NameFilter,
    ) -> Result<Vec<Listed<TableEntry>>, ConnectorError> {
        catalog::table_list(catalog, settings, namespaces, tables)
    }

    fn table_metadata(
        catalog: &mut CatalogReads<'_, Self>,
        tables: &[TableReference],
        _context: &(),
    ) -> Result<Vec<TableRecord<DatabricksTable>>, ConnectorError> {
        catalog::table_metadata(catalog, tables)
    }

    fn derive_table_metadata(
        table: &DatabricksTable,
        _settings: &DatabricksSettings,
    ) -> TableMetadata {
        table.to_table_metadata()
    }

    fn planning_facts(
        session: &Session<Self>,
        offered_plan: &LogicalPlan,
    ) -> Result<(TableReference, PlanningFacts), PlanningError> {
        pushdown::planning_facts(session, offered_plan)
    }

    fn runs_remotely(expression: &LogicalExpression, facts: &PlanningFacts) -> bool {
        pushdown::runs_remotely(expression, facts)
    }
}

#[cfg(test)]
mod tests {
    use connector_sdk::ErrorKind;

    use super::*;

    #[test]
    fn a_cancel_that_finds_no_running_statement_succeeds() {
        let no_query = AdbcError::with_message_and_status(
            "AdbcStatementCancel: no active query to cancel",
            Status::InvalidState,
        );
        assert!(cancel_outcome(Err(no_query)).is_ok());
        assert!(cancel_outcome(Ok(())).is_ok());
    }

    #[test]
    fn other_cancel_failures_are_classified() {
        let failed =
            AdbcError::with_message_and_status("query state is poisoned", Status::Internal);
        let error = cancel_outcome(Err(failed)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Internal);
    }
}
