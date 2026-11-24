//! The Oracle connector.
//!
//! It reads Oracle through the Oracle Instant Client ODBC driver (direct
//! access through `connector-odbc`). It reads the `ALL_*` data dictionary
//! views itself and maps types by its type mapping. The connector declares
//! what is specific to Oracle: its catalog reading and type mapping, its fixed
//! session settings, how ODBC reads each column and the conversion of what it
//! returns, which offered work Oracle runs with the same meaning, its SQL
//! spelling, and its handling of Oracle's errors. The shared ODBC and SQL
//! layers implement the connector lifecycle, planning, and execution from
//! these declarations.

#![forbid(unsafe_code)]

mod catalog;
mod dialect;
mod error_handler;
mod odbc_output;
mod pushdown;

pub use catalog::{
    OracleCheck, OracleColumn, OracleForeignKey, OracleKey, OracleLongText, OracleReferencedTable,
    OracleTable, OracleTableKind, OracleUniqueKey, OracleUniqueKind, OracleValueSource,
};
pub use error_handler::OracleErrorHandler;

use analytics_types::AnalyticsType;
use arrow_array::ArrayRef;
use arrow_schema::{DataType, TimeUnit};
use connector_odbc::{ConnectionString, Odbc, OdbcConnection, OdbcConnector, OdbcDataType};
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
pub const CONNECTOR: &str = "oracle";

/// The port of an Oracle listener when the options name none.
const DEFAULT_PORT: &str = "1521";

/// Driver attributes fixed by the connector: the driver's failover is
/// disabled, so that a broken connection is not reopened behind the session
/// and the next statement does not run on a new server session.
const FIXED_ATTRIBUTES: &str = "FEN=F";

/// Session settings fixed by the connector, run right after connecting:
///
/// - `TIME_ZONE` to UTC, so that the driver, which converts `TIMESTAMP WITH
///   TIME ZONE` and `WITH LOCAL TIME ZONE` values to the session time zone
///   and drops their offset, returns their UTC instant;
/// - `NLS_NUMERIC_CHARACTERS` to `.,`, so that `NUMBER` values, which the
///   connector reads as text, have a `.` decimal separator;
/// - `NLS_COMP` and `NLS_SORT` to `BINARY`, so that catalog names are matched
///   exactly and ordered by code point.
const SESSION_SETTINGS: &str = "ALTER SESSION SET TIME_ZONE = '+00:00' \
     NLS_NUMERIC_CHARACTERS = '.,' NLS_COMP = BINARY NLS_SORT = BINARY";

/// The Oracle Connector.
pub struct OracleConnector;

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

impl Connector for OracleConnector {
    type Access = Odbc<Self>;
    type Catalog = SqlCatalog<Self>;
    type Planner = SqlPlanner<Self>;
    type Executor = SqlExecutor<Self>;
    type ErrorHandler = OracleErrorHandler;
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
                    "host",
                    false,
                    true,
                    None,
                    "Host name or address of the Oracle listener; an IPv6 address may be given \
                     with or without brackets.",
                ),
                option(
                    "port",
                    false,
                    false,
                    Some(DEFAULT_PORT),
                    "Port of the Oracle listener.",
                ),
                option(
                    "service_name",
                    false,
                    true,
                    None,
                    "Service name of the database to connect to, such as a pluggable database's.",
                ),
                option("user", false, true, None, "Database user."),
                option("password", true, false, None, "Password of the user."),
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

impl OdbcConnector for OracleConnector {
    /// The Oracle Instant Client ODBC driver, `libsqora`, beside the Instant
    /// Client Basic libraries it loads relative to itself.
    const DRIVER: &'static str = "instantclient/libsqora";

    const SESSION_STATEMENTS: &'static [&'static str] = &[SESSION_SETTINGS];

    /// Builds the connection string from the validated options: the Easy
    /// Connect string `//host:port/service_name` as the database, with the
    /// driver's fixed attributes. Easy Connect takes an IPv6 address only in
    /// brackets, so a host containing `:` is enclosed in them unless it
    /// already is.
    ///
    /// An option containing `;` is `invalid settings`: the driver ends a
    /// value at `;` however it is written.
    fn connection_string(options: ConnectionOptions) -> Result<ConnectionString, ConnectorError> {
        let plain = |key: &str| match options.get(key) {
            Some(OptionValue::Plain(value)) => Some(value.clone()),
            Some(OptionValue::Secret(value)) => Some(value.expose().to_owned()),
            None => None,
        };
        for key in ["host", "port", "service_name", "user", "password"] {
            if plain(key).is_some_and(|value| value.contains(';')) {
                return Err(ConnectorError::new(
                    ErrorKind::InvalidSettings,
                    false,
                    format!("{key} must not contain ';', which the Oracle ODBC driver cannot take"),
                ));
            }
        }
        let host = plain("host").unwrap_or_default();
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        };
        let database = format!(
            "//{host}:{}/{}",
            plain("port").unwrap_or_else(|| DEFAULT_PORT.to_owned()),
            plain("service_name").unwrap_or_default()
        );
        Ok(ConnectionString {
            attributes: format!(
                "{FIXED_ATTRIBUTES};DBQ={}",
                Self::attribute_value(&database)
            ),
            user: plain("user").map(SecretString::new),
            password: plain("password").map(SecretString::new),
        })
    }

    /// Encloses a value in double quotes where the connection string grammar
    /// needs it: the driver keeps braces as part of a value, but takes a
    /// value in double quotes without them. Oracle names and passwords cannot
    /// contain a double quote.
    fn attribute_value(value: &str) -> String {
        let needs_quotes = value.contains(['{', '}', '='])
            || value.starts_with(char::is_whitespace)
            || value.ends_with(char::is_whitespace);
        if needs_quotes {
            format!("\"{value}\"")
        } else {
            value.to_owned()
        }
    }

    /// Reads the columns whose default Arrow type cannot carry every value
    /// of the type mapping in another form, which the value conversion turns
    /// into the Analytics type ([`odbc_output`]):
    ///
    /// - `NUMBER(p, s)` as decimal text, because `arrow-odbc` reads decimals
    ///   only as `Decimal128`, while their Analytics storage narrows with the
    ///   precision;
    /// - every `DATE` and `TIMESTAMP` as microsecond timestamps, because the
    ///   driver describes them without fractional digits, which `arrow-odbc`
    ///   would read in seconds, dropping the fraction;
    /// - `BOOLEAN` as an 8-bit integer, because the driver does not return it
    ///   as an ODBC bit.
    fn read_type(data_type: &OdbcDataType) -> Option<DataType> {
        match data_type {
            OdbcDataType::Decimal { .. } | OdbcDataType::Numeric { .. } => Some(DataType::Utf8),
            OdbcDataType::Timestamp { .. } => {
                Some(DataType::Timestamp(TimeUnit::Microsecond, None))
            }
            OdbcDataType::Bit => Some(DataType::Int8),
            _ => None,
        }
    }
}

impl SqlConnector for OracleConnector {
    type Connection = OdbcConnection<Self>;
    type Table = OracleTable;
    /// A session reads nothing when it opens: no catalog read depends on a
    /// fact of the session.
    type Context = ();
    /// Whether Oracle evaluates a comparison with its offered meaning depends
    /// only on its operands' Analytics types, so planning needs no facts
    /// beyond the offered plan ([`pushdown`]).
    type PlanningFacts = ();

    fn connection(session: &OdbcConnection<Self>) -> OdbcConnection<Self> {
        session.clone()
    }

    fn query(connection: &OdbcConnection<Self>, sql: String) -> ExecutionOutput {
        connector_odbc::query(connection, sql)
    }

    fn session_context(_catalog: &mut CatalogReads<'_, Self>) -> Result<(), ConnectorError> {
        Ok(())
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
        _context: &(),
    ) -> Result<Vec<TableRecord<OracleTable>>, ConnectorError> {
        catalog::table_metadata(catalog, tables)
    }

    fn derive_table_metadata(table: &OracleTable, _settings: &()) -> TableMetadata {
        table.to_table_metadata()
    }

    fn planning_facts(
        _session: &Session<Self>,
        offered_plan: &LogicalPlan,
    ) -> Result<(TableReference, ()), PlanningError> {
        pushdown::planning_facts(offered_plan)
    }

    fn runs_remotely(expression: &LogicalExpression, _facts: &()) -> bool {
        pushdown::runs_remotely(expression)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Easy Connect string of the connection string built from `host`.
    fn database(host: &str) -> String {
        let options = ConnectionOptions::parse(
            &OracleConnector::descriptor(),
            [
                ("host", host),
                ("service_name", "FREEPDB1"),
                ("user", "reader"),
                ("password", "secret"),
            ]
            .map(|(key, value)| (key.to_owned(), SecretString::new(value.to_owned()))),
        )
        .unwrap();
        let attributes = OracleConnector::connection_string(options)
            .unwrap()
            .attributes;
        attributes
            .strip_prefix(&format!("{FIXED_ATTRIBUTES};DBQ="))
            .unwrap()
            .to_owned()
    }

    #[test]
    fn a_host_name_or_ipv4_address_is_written_as_given() {
        assert_eq!(database("db.example.com"), "//db.example.com:1521/FREEPDB1");
        assert_eq!(database("192.0.2.1"), "//192.0.2.1:1521/FREEPDB1");
    }

    #[test]
    fn an_ipv6_address_is_enclosed_in_brackets_once() {
        assert_eq!(database("::1"), "//[::1]:1521/FREEPDB1");
        assert_eq!(database("[::1]"), "//[::1]:1521/FREEPDB1");
        assert_eq!(database("2001:db8::5"), "//[2001:db8::5]:1521/FREEPDB1");
    }
}
