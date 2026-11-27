//! The MySQL connector.
//!
//! It reads MySQL through the ADBC Driver Foundry MySQL driver. The connector
//! declares what is specific to MySQL: its `information_schema` reading and
//! type mapping, which offered work MySQL runs with the same meaning, its SQL
//! spelling, the conversion of the driver's output, and its handling of the
//! driver's errors. The shared ADBC and SQL layers implement the connector
//! lifecycle, planning, and execution from these declarations.

#![forbid(unsafe_code)]

mod catalog;
mod dialect;
mod driver_output;
mod error_handler;
mod pushdown;

pub use catalog::{
    MysqlCheck, MysqlColumn, MysqlForeignKey, MysqlGeneration, MysqlKey, MysqlPrimaryKey,
    MysqlReferencedTable, MysqlTable, MysqlTableKind, MysqlUniqueKey, MysqlValueSource,
};
pub use error_handler::MysqlErrorHandler;
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
    Listed, LogicalExpression, LogicalPlan, NameFilter, NamespacePath, OptionDescriptor,
    OptionValue, OptionValueType, PlanningError, Session, TableEntry, TableMetadata, TableRecord,
    TableReference,
};
use connector_sql::{
    SqlCatalog, SqlConnector, SqlExecutor, SqlPlanner, SqlRead, catalog::CatalogReads,
};

/// The Connector name callers pass when opening a Data Source.
pub const CONNECTOR: &str = "mysql";

/// The ADBC driver and the versions this connector was tested with.
const DRIVER: DriverRequirement = DriverRequirement {
    library: "adbc_driver_mysql",
    minimum: DriverVersion {
        major: 0,
        minor: 8,
        patch: 0,
    },
    below: DriverVersion {
        major: 0,
        minor: 9,
        patch: 0,
    },
};

/// Session settings fixed at connection, as the driver's DSN parameters,
/// which it runs as `SET` statements right after connecting.
///
/// Results and literals are exchanged in `utf8mb4`, and string literals in
/// generated SQL treat backslashes literally. The driver itself fixes
/// `time_zone` to `+00:00`, so `TIMESTAMP` values arrive in UTC.
const FIXED_SESSION_PARAMETERS: &str = "charset=utf8mb4&sql_mode=%27NO_BACKSLASH_ESCAPES%27";

/// Driver options fixed by the connector: a zero date or datetime fails the
/// read instead of becoming NULL.
const FIXED_DRIVER_OPTIONS: [(&str, &str); 1] = [("mysql.query.zero_datetime_behavior", "error")];

/// The MySQL Connector.
pub struct MysqlConnector;

/// How one MySQL Data Source's types map, from its connection options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MysqlSettings {
    /// Whether `tinyint(1)` maps to `Boolean`, as MySQL's `BOOL` and
    /// `BOOLEAN` declare it, rather than to `Byte`. A value other than 0 and 1
    /// is then a `data` error.
    pub tinyint1_as_boolean: bool,
}

/// Describes one accepted connection option.
fn option(
    key: &str,
    secret: bool,
    required: bool,
    value_type: OptionValueType,
    default: Option<&str>,
    description: &str,
) -> OptionDescriptor {
    OptionDescriptor {
        key: key.to_owned(),
        secret,
        required,
        value_type,
        default: default.map(str::to_owned),
        description: description.to_owned(),
    }
}

impl Connector for MysqlConnector {
    type Access = Adbc<Self>;
    type Catalog = SqlCatalog<Self>;
    type Planner = SqlPlanner<Self>;
    type Executor = SqlExecutor<Self>;
    type ErrorHandler = MysqlErrorHandler;
    type Query = SqlRead;
    type Settings = MysqlSettings;

    fn settings(options: &ConnectionOptions) -> Result<MysqlSettings, ConnectorError> {
        Ok(MysqlSettings {
            tinyint1_as_boolean: matches!(
                options.get("tinyint1_as_boolean"),
                Some(OptionValue::Plain(value)) if value == "true"
            ),
        })
    }

    fn descriptor() -> ConnectorDescriptor {
        use OptionValueType::{Boolean, Integer, String};
        ConnectorDescriptor {
            name: "mysql".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            connectors: vec![CONNECTOR.to_owned()],
            options: vec![
                option(
                    "host",
                    false,
                    true,
                    String,
                    None,
                    "Host name or address of the MySQL server.",
                ),
                option(
                    "port",
                    false,
                    false,
                    Integer,
                    Some("3306"),
                    "TCP port of the MySQL server.",
                ),
                option("user", false, true, String, None, "Account to connect as."),
                option(
                    "password",
                    true,
                    false,
                    String,
                    None,
                    "Password of the account.",
                ),
                option(
                    "tls",
                    false,
                    false,
                    String,
                    Some("preferred"),
                    "TLS mode: true (verify the server), skip-verify, preferred, or false.",
                ),
                option(
                    "tinyint1_as_boolean",
                    false,
                    false,
                    Boolean,
                    Some("false"),
                    "Whether tinyint(1), as MySQL's BOOL and BOOLEAN declare it, maps to \
                     Boolean rather than Byte; a value other than 0 and 1 then fails the \
                     read as a data error.",
                ),
            ],
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

impl AdbcConnector for MysqlConnector {
    const DRIVER: DriverRequirement = DRIVER;

    /// Passes the DSN with the fixed session settings, the credentials as
    /// separate options, and the fixed driver options.
    fn database_options(
        options: ConnectionOptions,
        connection_timeout: Option<Duration>,
    ) -> Result<Vec<(OptionDatabase, AdbcOptionValue)>, ConnectorError> {
        let mut driver_options = vec![(
            OptionDatabase::Uri,
            AdbcOptionValue::String(dsn(&options, connection_timeout)),
        )];
        for (option, key) in [
            ("user", OptionDatabase::Username),
            ("password", OptionDatabase::Password),
        ] {
            let value = match options.get(option) {
                Some(OptionValue::Plain(value)) => value.clone(),
                Some(OptionValue::Secret(value)) => value.expose().to_owned(),
                None => continue,
            };
            driver_options.push((key, AdbcOptionValue::String(value)));
        }
        driver_options.extend(FIXED_DRIVER_OPTIONS.iter().map(|(key, value)| {
            (
                OptionDatabase::Other((*key).to_owned()),
                AdbcOptionValue::String((*value).to_owned()),
            )
        }));
        Ok(driver_options)
    }
}

impl SqlConnector for MysqlConnector {
    type Connection = AdbcConnection<Self>;
    type Table = MysqlTable;
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
        _settings: &Self::Settings,
        filter: &NameFilter,
    ) -> Result<Vec<Listed<NamespacePath>>, ConnectorError> {
        catalog::namespace_list(catalog, filter)
    }

    fn table_list(
        catalog: &mut CatalogReads<'_, Self>,
        _settings: &Self::Settings,
        namespaces: &NameFilter,
        tables: &NameFilter,
    ) -> Result<Vec<Listed<TableEntry>>, ConnectorError> {
        catalog::table_list(catalog, namespaces, tables)
    }

    fn table_metadata(
        catalog: &mut CatalogReads<'_, Self>,
        tables: &[TableReference],
        _context: &(),
    ) -> Result<Vec<TableRecord<MysqlTable>>, ConnectorError> {
        catalog::table_metadata(catalog, tables)
    }

    fn derive_table_metadata(table: &MysqlTable, settings: &MysqlSettings) -> TableMetadata {
        table.to_table_metadata(settings)
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

/// Builds the driver's DSN, `tcp(host:port)/?parameters`, from the validated
/// options.
///
/// It names no default database: generated SQL qualifies every table. The
/// fixed session settings follow the caller's TLS mode, and the credentials
/// travel as separate driver options.
fn dsn(options: &ConnectionOptions, connection_timeout: Option<Duration>) -> String {
    let plain = |key: &str| match options.get(key) {
        Some(OptionValue::Plain(value)) => Some(value.as_str()),
        _ => None,
    };
    let host = plain("host").unwrap_or_default();
    let port = plain("port").unwrap_or("3306");
    let tls = plain("tls").unwrap_or("preferred");
    let mut dsn = format!(
        "tcp({host}:{port})/?tls={}&{FIXED_SESSION_PARAMETERS}",
        query_value(tls)
    );
    if let Some(timeout) = connection_timeout {
        dsn.push_str(&format!("&timeout={}ms", timeout.as_millis().max(1)));
    }
    dsn
}

/// Percent-encodes a DSN query value, keeping only unreserved characters.
fn query_value(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            byte => format!("%{byte:02X}"),
        })
        .collect()
}
