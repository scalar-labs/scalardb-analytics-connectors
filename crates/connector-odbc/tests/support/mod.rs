//! A PostgreSQL server in Docker reached through the PostgreSQL ODBC driver,
//! and a SQL-speaking test Connector that uses ODBC access.
//!
//! The server container is the one the PostgreSQL connector's tests use,
//! named after the image and left running between runs.
//! `POSTGRESQL_TEST_IMAGE` selects the server image (default `postgres:18`),
//! and `ODBC_DRIVER_DIRECTORY`, which the Nix development shell sets, holds
//! the PostgreSQL ODBC driver as `psqlodbcw`.

#![allow(dead_code)]

use std::{
    env,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, OnceLock},
    thread,
    time::{Duration, Instant},
};

use analytics_types::AnalyticsType;
use arrow_array::ArrayRef;
use connector_odbc::{ConnectionString, Odbc, OdbcConnection, OdbcConnector, OdbcDiagnostic};
use connector_sdk::{
    Access, ConnectionOptions, Connector, ConnectorDescriptor, ConnectorError, DataSourceAccess,
    ExecutionOutput, GenericErrorHandler, LibrarySettings, Listed, LiteralValue, LogicalExpression,
    LogicalPlan, NameFilter, NamespacePath, OptionDescriptor, OptionValue, OptionValueType,
    PlanningError, SecretString, Session, TableEntry, TableMetadata, TableRecord, TableReference,
    test_connector::TestTable,
};
use connector_sql::{
    SqlCatalog, SqlConnector, SqlDialect, SqlExecutor, SqlPlanner, SqlRead, catalog::CatalogReads,
};

pub const PASSWORD: &str = "connector-test-secret";

/// A SQL-speaking Connector that reaches PostgreSQL through ODBC.
///
/// Its options are the ODBC connection string without credentials, and the
/// user and password. It returns ODBC output unchanged, and its catalog and
/// planning functions are never requested: the tests run catalog statements
/// and planned reads directly.
pub struct OdbcTestConnector;

impl Connector for OdbcTestConnector {
    type Access = Odbc<Self>;
    type Catalog = SqlCatalog<Self>;
    type Planner = SqlPlanner<Self>;
    type Executor = SqlExecutor<Self>;
    type ErrorHandler = GenericErrorHandler<OdbcDiagnostic>;
    type Query = SqlRead;
    type Settings = ();

    fn settings(_options: &ConnectionOptions) -> Result<(), ConnectorError> {
        Ok(())
    }

    fn descriptor() -> ConnectorDescriptor {
        let option = |key: &str, secret: bool, required: bool| OptionDescriptor {
            key: key.to_owned(),
            secret,
            required,
            value_type: OptionValueType::String,
            default: None,
            description: String::new(),
        };
        ConnectorDescriptor {
            name: "odbc-test".to_owned(),
            version: "0.0.0".to_owned(),
            connectors: vec!["odbc-test".to_owned()],
            options: vec![
                option("connection_string", false, true),
                option("user", false, false),
                option("password", true, false),
            ],
            access: DataSourceAccess::Direct,
        }
    }

    fn convert_column(
        column: &ArrayRef,
        _target: &AnalyticsType,
    ) -> Result<ArrayRef, ConnectorError> {
        Ok(Arc::clone(column))
    }
}

impl OdbcConnector for OdbcTestConnector {
    const DRIVER: &'static str = "psqlodbcw";

    fn connection_string(options: ConnectionOptions) -> Result<ConnectionString, ConnectorError> {
        let value = |key| match options.get(key) {
            Some(OptionValue::Plain(value)) => Some(SecretString::new(value.clone())),
            Some(OptionValue::Secret(value)) => Some(SecretString::new(value.expose().to_owned())),
            None => None,
        };
        Ok(ConnectionString {
            attributes: value("connection_string")
                .map(|value| value.expose().to_owned())
                .unwrap_or_default(),
            user: value("user"),
            password: value("password"),
        })
    }
}

impl SqlDialect for OdbcTestConnector {
    fn quote_identifier(identifier: &str) -> String {
        format!("\"{}\"", identifier.replace('"', "\"\""))
    }

    fn write_literal(_literal: &LiteralValue, sql: &mut String) {
        sql.push_str("NULL");
    }
}

impl SqlConnector for OdbcTestConnector {
    type Connection = OdbcConnection<Self>;
    type Table = TestTable;
    type Context = ();
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
        _catalog: &mut CatalogReads<'_, Self>,
        _filter: &NameFilter,
    ) -> Result<Vec<Listed<NamespacePath>>, ConnectorError> {
        unreachable!("the tests list no namespaces")
    }

    fn table_list(
        _catalog: &mut CatalogReads<'_, Self>,
        _namespaces: &NameFilter,
        _tables: &NameFilter,
    ) -> Result<Vec<Listed<TableEntry>>, ConnectorError> {
        unreachable!("the tests list no tables")
    }

    fn table_metadata(
        _catalog: &mut CatalogReads<'_, Self>,
        _tables: &[TableReference],
        _context: &(),
    ) -> Result<Vec<TableRecord<TestTable>>, ConnectorError> {
        unreachable!("the tests read no table metadata")
    }

    fn derive_table_metadata(_table: &TestTable, _settings: &()) -> TableMetadata {
        unreachable!("the tests read no table metadata")
    }

    fn planning_facts(
        _session: &Session<Self>,
        _offered_plan: &LogicalPlan,
    ) -> Result<(TableReference, ()), PlanningError> {
        unreachable!("the tests plan no reads")
    }

    fn runs_remotely(_expression: &LogicalExpression, _facts: &()) -> bool {
        false
    }
}

type TestAccess = Odbc<OdbcTestConnector>;

/// The PostgreSQL server the tests use.
pub struct Server {
    container: String,
    pub port: u16,
}

/// Returns the running test server, starting its container if needed.
pub fn server() -> &'static Server {
    static SERVER: OnceLock<Server> = OnceLock::new();
    SERVER.get_or_init(start)
}

fn start() -> Server {
    let image = env::var("POSTGRESQL_TEST_IMAGE").unwrap_or_else(|_| "postgres:18".to_owned());
    let container = format!(
        "scalardb-analytics-connector-test-{}",
        image.replace([':', '/', '.'], "-")
    );
    let running = docker(&["inspect", "-f", "{{.State.Running}}", &container]);
    match running.as_deref().map(str::trim) {
        Ok("true") => {}
        Ok(_) => {
            docker(&["start", &container]).expect("the test container starts");
        }
        Err(_) => {
            // Another test binary may create the same container concurrently.
            if docker(&[
                "run",
                "-d",
                "--name",
                &container,
                "-e",
                &format!("POSTGRES_PASSWORD={PASSWORD}"),
                "-p",
                "127.0.0.1::5432",
                &image,
            ])
            .is_err()
            {
                docker(&["start", &container]).expect("the test container starts");
            }
        }
    }
    let port = docker(&["port", &container, "5432/tcp"]).expect("the container publishes a port");
    let port = port
        .lines()
        .next()
        .and_then(|line| line.rsplit(':').next())
        .and_then(|port| port.trim().parse().ok())
        .expect("the published port is a number");
    let server = Server { container, port };
    let deadline = Instant::now() + Duration::from_secs(60);
    while server.try_psql("SELECT 1").is_err() {
        assert!(Instant::now() < deadline, "PostgreSQL did not become ready");
        thread::sleep(Duration::from_millis(250));
    }
    server
}

fn docker(arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("docker")
        .args(arguments)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

impl Server {
    /// Runs SQL as the superuser, failing on the first error.
    pub fn psql(&self, sql: &str) -> String {
        self.try_psql(sql).unwrap_or_else(|error| panic!("{error}"))
    }

    fn try_psql(&self, sql: &str) -> Result<String, String> {
        let mut child = Command::new("docker")
            .args([
                "exec",
                "-i",
                &self.container,
                "psql",
                "-h",
                "127.0.0.1",
                "-U",
                "postgres",
                "-v",
                "ON_ERROR_STOP=1",
                "-qAt",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(sql.as_bytes())
            .map_err(|error| error.to_string())?;
        let output = child
            .wait_with_output()
            .map_err(|error| error.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).into_owned())
        }
    }

    /// The ODBC connection string of the server's `postgres` database,
    /// without the driver and credentials, followed by `extra` attributes.
    pub fn connection_string(&self, extra: &str) -> String {
        format!(
            "Server=127.0.0.1;Port={};Database=postgres;SSLmode=disable;{extra}",
            self.port
        )
    }
}

/// The library settings with the development shell's ODBC driver directory.
pub fn library_settings() -> LibrarySettings {
    let directory = env::var_os("ODBC_DRIVER_DIRECTORY")
        .expect("ODBC_DRIVER_DIRECTORY locates the ODBC drivers; run inside `nix develop`");
    LibrarySettings {
        odbc_driver_directory: Some(PathBuf::from(directory)),
        ..LibrarySettings::default()
    }
}

/// Opens a session as `user` with `password` and `extra` connection string
/// attributes.
pub fn session_as(
    user: &str,
    password: &str,
    extra: &str,
) -> Result<Session<OdbcTestConnector>, ConnectorError> {
    open_session(&library_settings(), user, password, extra)
}

/// Opens a session of a library with `settings` as `user` with `password`
/// and `extra` connection string attributes.
pub fn open_session(
    settings: &LibrarySettings,
    user: &str,
    password: &str,
    extra: &str,
) -> Result<Session<OdbcTestConnector>, ConnectorError> {
    let library = TestAccess::open_library(settings)?;
    let options = ConnectionOptions::parse(
        &OdbcTestConnector::descriptor(),
        [
            ("connection_string", server().connection_string(extra)),
            ("user", user.to_owned()),
            ("password", password.to_owned()),
        ]
        .map(|(key, value)| (key.to_owned(), SecretString::new(value))),
    )?;
    let data_source = TestAccess::open_data_source(
        &library,
        "odbc-test",
        options,
        Some(Duration::from_secs(10)),
    )?;
    Session::open(TestAccess::open_session(&data_source)?, ())
}

/// Opens a session as the superuser.
pub fn session(extra: &str) -> Session<OdbcTestConnector> {
    session_as("postgres", PASSWORD, extra).expect("the session opens")
}
