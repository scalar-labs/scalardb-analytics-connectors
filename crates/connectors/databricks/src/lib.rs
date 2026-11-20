//! The Databricks connector.
//!
//! It reads Databricks through a SQL warehouse with an ADBC driver. The
//! connector declares what is specific to Databricks: its connection options
//! and their driver options, its Unity Catalog reading and type mapping,
//! which offered work Databricks runs with the same meaning, its SQL spelling,
//! and its handling of the driver's errors. The shared ADBC and SQL layers
//! implement the connector lifecycle, planning, and execution from these
//! declarations.

#![forbid(unsafe_code)]

mod catalog;
mod dialect;
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

use adbc_core::options::{OptionDatabase, OptionValue as AdbcOptionValue};
use analytics_types::AnalyticsType;
use arrow_array::ArrayRef;
use connector_adbc::{
    Adbc, AdbcConnection, AdbcConnector, AdbcSession, DriverRequirement, DriverVersion, query,
};
use connector_sdk::{
    Access, ConnectionOptions, Connector, ConnectorDescriptor, ConnectorError, ExecutionOutput,
    Listed, LogicalExpression, LogicalPlan, NameFilter, NamespacePath, PlanningError, Session,
    TableEntry, TableMetadata, TableRecord, TableReference,
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
/// Generated literals depend on neither; the expressions of the views read
/// are evaluated under them.
#[expect(
    dead_code,
    reason = "run on each new connection once the driver is chosen and can run them"
)]
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

    /// Returns the driver's output unchanged.
    ///
    /// Which Arrow types the driver returns for decimals, timestamps, and
    /// nested types, and so which conversions reach the Analytics storage,
    /// is checked against a workspace once the driver is chosen; until then a
    /// column that arrives in another form fails the read as `internal`.
    fn convert_column(
        column: &ArrayRef,
        _target: &AnalyticsType,
    ) -> Result<ArrayRef, ConnectorError> {
        Ok(Arc::clone(column))
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
        let cancel = Arc::new(statement.cancel_handle());
        ExecutionOutput::new(Box::pin(statement), cancel)
    }

    fn session_context(_catalog: &mut CatalogReads<'_, Self>) -> Result<(), ConnectorError> {
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
