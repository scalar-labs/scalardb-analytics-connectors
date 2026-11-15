//! The SQL Server through ODBC connector.
//!
//! It reads SQL Server through the Microsoft ODBC Driver 18 for SQL Server
//! (direct access through `connector-odbc`). It reads the `sys` catalog views
//! itself and maps types by its type mapping. The connector declares what is
//! specific to SQL Server: its catalog reading and type mapping, how ODBC
//! reads each column and the conversion of what it returns, which offered
//! work SQL Server runs with the same meaning, its SQL spelling, and its
//! handling of SQL Server's errors. The shared ODBC and SQL layers implement
//! the connector lifecycle, planning, and execution from these declarations.

#![forbid(unsafe_code)]

mod catalog;
mod dialect;
mod error_handler;
mod odbc_output;
mod pushdown;

pub use catalog::{
    MetadataVisibility, SqlServerCheck, SqlServerColumn, SqlServerForeignKey, SqlServerKey,
    SqlServerReferencedTable, SqlServerTable, SqlServerTableKind, SqlServerType,
    SqlServerUniqueKey, SqlServerUniqueKind, SqlServerValueSource,
};
pub use error_handler::SqlServerErrorHandler;
pub use pushdown::PlanningFacts;

use analytics_types::AnalyticsType;
use arrow_array::ArrayRef;
use arrow_schema::DataType;
use connector_odbc::{
    ConnectionString, Odbc, OdbcConnection, OdbcConnector, OdbcDataType, attribute_value,
};
use connector_sdk::{
    Access, ConnectionOptions, Connector, ConnectorDescriptor, ConnectorError, ErrorKind,
    ExecutionOutput, Listed, LogicalExpression, LogicalPlan, NameFilter, NamespacePath,
    OptionDescriptor, OptionValue, OptionValueType, PlanningError, SecretString, Session,
    TableEntry, TableMetadata, TableRecord, TableReference,
};
use connector_sql::{
    SqlCatalog, SqlConnector, SqlExecutor, SqlPlanner, SqlRead, catalog::CatalogReads,
};

/// The Connector name callers pass when opening a Data Source.
pub const CONNECTOR: &str = "sqlserver-odbc";

/// The ODBC driver the connector connects with, by the name it is registered
/// under with the ODBC driver manager.
const DRIVER: &str = "ODBC Driver 18 for SQL Server";

/// Driver attributes fixed by the connector: a broken connection is not
/// reconnected. The driver's connection resiliency would otherwise reopen it
/// and run the next statement on a new server session, so that a session
/// would outlive its connection.
const FIXED_ATTRIBUTES: &str = "ConnectRetryCount=0";

/// The values of `encrypt`, which the driver's `Encrypt` keyword accepts.
const ENCRYPT_VALUES: [&str; 3] = ["yes", "no", "strict"];

/// The values of `trust_server_certificate`.
const TRUST_VALUES: [&str; 2] = ["yes", "no"];

/// The SQL Server through ODBC Connector.
pub struct SqlServerConnector;

/// Describes one accepted connection option.
fn option(
    key: &str,
    secret: bool,
    required: bool,
    default: Option<&str>,
    description: &str,
) -> OptionDescriptor {
    OptionDescriptor {
        key: key.to_owned(),
        secret,
        required,
        value_type: OptionValueType::String,
        default: default.map(str::to_owned),
        description: description.to_owned(),
    }
}

impl Connector for SqlServerConnector {
    type Access = Odbc<Self>;
    type Catalog = SqlCatalog<Self>;
    type Planner = SqlPlanner<Self>;
    type Executor = SqlExecutor<Self>;
    type ErrorHandler = SqlServerErrorHandler;
    type Query = SqlRead;
    /// Every option reaches the ODBC driver; none changes how the connector
    /// reads.
    type Settings = ();

    fn settings(_options: &ConnectionOptions) -> Result<(), ConnectorError> {
        Ok(())
    }

    fn descriptor() -> ConnectorDescriptor {
        ConnectorDescriptor {
            name: CONNECTOR.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            connectors: vec![CONNECTOR.to_owned()],
            options: vec![
                option(
                    "server",
                    false,
                    true,
                    None,
                    "SQL Server to connect to: `host`, `host,port`, or `host\\instance`.",
                ),
                option("database", false, true, None, "Database to connect to."),
                option(
                    "user",
                    false,
                    true,
                    None,
                    "SQL Server authentication login.",
                ),
                option("password", true, false, None, "Password of the login."),
                option(
                    "encrypt",
                    false,
                    false,
                    Some("yes"),
                    "Connection encryption: yes, no, or strict.",
                ),
                option(
                    "trust_server_certificate",
                    false,
                    false,
                    Some("no"),
                    "Whether the server's certificate is trusted without validation: yes or no.",
                ),
            ],
            access: Self::Access::description(),
        }
    }

    fn convert_column(
        column: &ArrayRef,
        target: &AnalyticsType,
    ) -> Result<ArrayRef, ConnectorError> {
        odbc_output::convert_column(column, target)
    }
}

impl OdbcConnector for SqlServerConnector {
    /// Builds the connection string from the validated options, with the
    /// driver and its fixed attributes.
    fn connection_string(options: ConnectionOptions) -> Result<ConnectionString, ConnectorError> {
        let plain = |key: &str| match options.get(key) {
            Some(OptionValue::Plain(value)) => Some(value.clone()),
            Some(OptionValue::Secret(value)) => Some(value.expose().to_owned()),
            None => None,
        };
        let choice = |key: &str, allowed: &[&str], default: &str| {
            let value = plain(key).unwrap_or_else(|| default.to_owned());
            if allowed.contains(&value.as_str()) {
                Ok(value)
            } else {
                Err(ConnectorError::new(
                    ErrorKind::InvalidSettings,
                    false,
                    format!("{key} must be one of {}", allowed.join(", ")),
                ))
            }
        };
        let encrypt = choice("encrypt", &ENCRYPT_VALUES, "yes")?;
        let trust = choice("trust_server_certificate", &TRUST_VALUES, "no")?;
        let mut attributes = format!("Driver={{{DRIVER}}};{FIXED_ATTRIBUTES}");
        for (keyword, value) in [
            ("Server", plain("server")),
            ("Database", plain("database")),
            ("Encrypt", Some(encrypt)),
            ("TrustServerCertificate", Some(trust)),
        ] {
            if let Some(value) = value {
                attributes.push(';');
                attributes.push_str(keyword);
                attributes.push('=');
                attributes.push_str(&attribute_value(&value));
            }
        }
        Ok(ConnectionString {
            attributes,
            user: plain("user").map(SecretString::new),
            password: plain("password").map(SecretString::new),
        })
    }

    /// Reads the columns whose default Arrow type cannot carry every value
    /// of the type mapping as text, which the value conversion parses
    /// ([`odbc_output`]):
    ///
    /// - timestamps with more than six fractional digits, `datetime2(7)`,
    ///   which `arrow-odbc` would read in nanoseconds;
    /// - decimals, which `arrow-odbc` reads only as `Decimal128`, while their
    ///   Analytics storage narrows with the precision;
    /// - fixed-length binary, which `arrow-odbc` reads as `FixedSizeBinary`
    ///   rather than the `Binary` storage of `Blob`.
    fn read_type(data_type: &OdbcDataType) -> Option<DataType> {
        match data_type {
            OdbcDataType::Timestamp { precision } if *precision > 6 => Some(DataType::Utf8),
            OdbcDataType::Decimal { .. } | OdbcDataType::Numeric { .. } => Some(DataType::Utf8),
            OdbcDataType::Binary { .. } => Some(DataType::Binary),
            _ => None,
        }
    }
}

impl SqlConnector for SqlServerConnector {
    type Connection = OdbcConnection<Self>;
    type Table = SqlServerTable;
    type Context = MetadataVisibility;
    type PlanningFacts = PlanningFacts;

    fn connection(session: &OdbcConnection<Self>) -> OdbcConnection<Self> {
        session.clone()
    }

    fn query(connection: &OdbcConnection<Self>, sql: String) -> ExecutionOutput {
        connector_odbc::query(connection, sql)
    }

    fn session_context(
        catalog: &mut CatalogReads<'_, Self>,
    ) -> Result<MetadataVisibility, ConnectorError> {
        catalog::session_context(catalog)
    }

    fn namespace_list(
        catalog: &mut CatalogReads<'_, Self>,
        filter: &NameFilter,
    ) -> Result<Vec<Listed<NamespacePath>>, ConnectorError> {
        catalog::namespace_list(catalog, filter)
    }

    fn table_list(
        catalog: &mut CatalogReads<'_, Self>,
        namespaces: &NameFilter,
        tables: &NameFilter,
    ) -> Result<Vec<Listed<TableEntry>>, ConnectorError> {
        catalog::table_list(catalog, namespaces, tables)
    }

    fn table_metadata(
        catalog: &mut CatalogReads<'_, Self>,
        tables: &[TableReference],
        visibility: &MetadataVisibility,
    ) -> Result<Vec<TableRecord<SqlServerTable>>, ConnectorError> {
        catalog::table_metadata(catalog, tables, *visibility)
    }

    fn derive_table_metadata(table: &SqlServerTable, _settings: &()) -> TableMetadata {
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
