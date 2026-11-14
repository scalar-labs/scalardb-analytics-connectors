//! The PostgreSQL connector.
//!
//! It reads PostgreSQL through the Apache Arrow ADBC PostgreSQL driver. The
//! connector declares what is specific to PostgreSQL: its `pg_catalog`
//! reading and type mapping, which offered work PostgreSQL runs with the same
//! meaning, its SQL spelling, the conversion of the driver's output, and its
//! handling of the driver's errors. The shared ADBC and SQL layers implement
//! the connector lifecycle, planning, and execution from these declarations.

#![forbid(unsafe_code)]

mod catalog;
mod dialect;
mod driver_output;
mod error_handler;
mod pushdown;

pub use catalog::{
    PostgresCheck, PostgresColumn, PostgresDomainCheck, PostgresForeignKey,
    PostgresGeneratedStorage, PostgresIdentity, PostgresKey, PostgresNotNull, PostgresPrimaryKey,
    PostgresReferencedTable, PostgresTable, PostgresTableKind, PostgresType, PostgresTypeForm,
    PostgresUniqueKey, PostgresUniqueKind, PostgresValueSource, ServerVersion,
};
pub use error_handler::PostgresErrorHandler;
pub use pushdown::PlanningFacts;

use std::time::Duration;

use adbc_core::options::{OptionDatabase, OptionValue as AdbcOptionValue};
use analytics_types::AnalyticsType;
use arrow_array::ArrayRef;
use connector_adbc::{Adbc, AdbcConnector, DriverRequirement, DriverVersion};
use connector_sdk::{
    Access, ConnectionOptions, Connector, ConnectorDescriptor, ConnectorError, Listed,
    LogicalExpression, LogicalPlan, NameFilter, NamespacePath, OptionDescriptor, OptionValue,
    OptionValueType, PlanningError, SecretString, Session, TableEntry, TableRecord, TableReference,
};
use connector_sql::{
    SqlCatalog, SqlConnector, SqlExecutor, SqlPlanner, SqlRead, catalog::CatalogReads,
};

/// The Connector name callers pass when opening a Data Source.
pub const CONNECTOR: &str = "postgresql";

/// The ADBC driver and the versions this connector was tested with.
const DRIVER: DriverRequirement = DriverRequirement {
    library: "adbc_driver_postgresql",
    minimum: DriverVersion {
        major: 1,
        minor: 13,
        patch: 0,
    },
    below: DriverVersion {
        major: 1,
        minor: 14,
        patch: 0,
    },
};

/// Session settings fixed at connection: timestamps are produced in UTC, and
/// string literals in generated SQL treat backslashes literally.
const FIXED_SESSION_OPTIONS: &str = "-c TimeZone=UTC -c standard_conforming_strings=on";

/// Driver options fixed by the connector: results are read through binary
/// `COPY`, and domains and enums are resolved from the server's catalog.
const FIXED_DRIVER_OPTIONS: [(&str, &str); 2] = [
    ("adbc.postgresql.use_copy", "true"),
    ("adbc.postgresql.type_resolver_mode", "server"),
];

/// The PostgreSQL Connector.
pub struct PostgresConnector;

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

impl Connector for PostgresConnector {
    type Access = Adbc<Self>;
    type Catalog = SqlCatalog<Self>;
    type Planner = SqlPlanner<Self>;
    type Executor = SqlExecutor<Self>;
    type ErrorHandler = PostgresErrorHandler;
    type Query = SqlRead;

    fn descriptor() -> ConnectorDescriptor {
        use OptionValueType::{Integer, String};
        ConnectorDescriptor {
            name: "postgresql".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            connectors: vec![CONNECTOR.to_owned()],
            options: vec![
                option(
                    "host",
                    false,
                    true,
                    String,
                    None,
                    "Host name or address of the PostgreSQL server.",
                ),
                option(
                    "port",
                    false,
                    false,
                    Integer,
                    Some("5432"),
                    "TCP port of the PostgreSQL server.",
                ),
                option(
                    "database",
                    false,
                    true,
                    String,
                    None,
                    "Database to connect to.",
                ),
                option("user", false, true, String, None, "Role to connect as."),
                option(
                    "password",
                    true,
                    false,
                    String,
                    None,
                    "Password of the role.",
                ),
                option(
                    "sslmode",
                    false,
                    false,
                    String,
                    Some("prefer"),
                    "libpq SSL mode: disable, allow, prefer, require, verify-ca, or verify-full.",
                ),
                option(
                    "search_path",
                    false,
                    false,
                    String,
                    None,
                    "Schema search path of the sessions, such as `sales, public`. \
                     Data Source types of types outside pg_catalog that are not on \
                     the path are qualified with their schema. When omitted, the \
                     server's setting for the role and database applies.",
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

impl AdbcConnector for PostgresConnector {
    const DRIVER: DriverRequirement = DRIVER;

    /// Passes the connection string with the fixed session settings, and the
    /// fixed driver options.
    fn database_options(
        options: ConnectionOptions,
        connection_timeout: Option<Duration>,
    ) -> Result<Vec<(OptionDatabase, AdbcOptionValue)>, ConnectorError> {
        let conninfo = conninfo(&options, connection_timeout);
        let mut driver_options = vec![(
            OptionDatabase::Uri,
            AdbcOptionValue::String(conninfo.expose().to_owned()),
        )];
        driver_options.extend(FIXED_DRIVER_OPTIONS.iter().map(|(key, value)| {
            (
                OptionDatabase::Other((*key).to_owned()),
                AdbcOptionValue::String((*value).to_owned()),
            )
        }));
        Ok(driver_options)
    }
}

impl SqlConnector for PostgresConnector {
    type Table = PostgresTable;
    type Context = ServerVersion;
    type PlanningFacts = PlanningFacts;

    fn session_context(
        catalog: &mut CatalogReads<'_, Self>,
    ) -> Result<ServerVersion, ConnectorError> {
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
        server_version: &ServerVersion,
    ) -> Result<Vec<TableRecord<PostgresTable>>, ConnectorError> {
        catalog::table_metadata(catalog, tables, server_version)
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

/// Escapes one value of the startup `options` setting.
///
/// The server splits `options` at whitespace that is not escaped with a
/// backslash, so an unescaped tab or newline would start another setting,
/// such as one overriding `standard_conforming_strings`.
fn escape_startup_option(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        // C `isspace`: space, tab, newline, vertical tab, form feed, and
        // carriage return.
        if character == '\\' || character.is_ascii_whitespace() || character == '\u{0B}' {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Builds the libpq connection string from the validated options.
///
/// It is held as a secret because it contains the password. The client
/// encoding and fixed session settings are keys that callers cannot set; a
/// given `search_path` is added to the session settings.
fn conninfo(options: &ConnectionOptions, connection_timeout: Option<Duration>) -> SecretString {
    let mut conninfo = String::new();
    let mut push = |key: &str, value: &str| {
        let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
        conninfo.push_str(&format!("{key}='{escaped}' "));
    };
    for (option, keyword) in [
        ("host", "host"),
        ("port", "port"),
        ("database", "dbname"),
        ("user", "user"),
        ("password", "password"),
        ("sslmode", "sslmode"),
    ] {
        match options.get(option) {
            Some(OptionValue::Plain(value)) => push(keyword, value),
            Some(OptionValue::Secret(value)) => push(keyword, value.expose()),
            None => {}
        }
    }
    if let Some(timeout) = connection_timeout {
        let seconds = timeout.as_secs() + u64::from(timeout.subsec_nanos() > 0);
        push("connect_timeout", &seconds.max(1).to_string());
    }
    push("client_encoding", "UTF8");
    let mut session_options = FIXED_SESSION_OPTIONS.to_owned();
    if let Some(OptionValue::Plain(search_path)) = options.get("search_path") {
        let escaped = escape_startup_option(search_path);
        session_options.push_str(&format!(" -c search_path={escaped}"));
    }
    push("options", &session_options);
    SecretString::new(conninfo)
}

#[cfg(test)]
mod tests {
    use super::escape_startup_option;

    #[test]
    fn startup_option_values_cannot_start_another_setting() {
        assert_eq!(
            escape_startup_option("public\t-c\nstandard_conforming_strings=off"),
            "public\\\t-c\\\nstandard_conforming_strings=off"
        );
        assert_eq!(escape_startup_option("a b\\c\u{0B}"), "a\\ b\\\\c\\\u{0B}");
    }
}
