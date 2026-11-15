//! A PostgreSQL server in Docker and a connector opened on it.
//!
//! Tests share one container per image, named after the image and left
//! running between runs so that later runs start quickly. Each test file
//! works in its own schema. `POSTGRESQL_TEST_IMAGE` selects the server image
//! (default `postgres:18`), and `ADBC_DRIVER_DIRECTORY`, which the Nix
//! development shell sets, locates the ADBC driver.

#![allow(dead_code)]

use std::{
    io::Write,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::OnceLock,
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, Instant},
};

use connector_adbc::{Adbc, AdbcDatabase, AdbcLibrary};
use connector_postgresql::PostgresConnector;
use connector_sdk::{
    Access, ConnectionOptions, Connector, ConnectorError, LibrarySettings, SecretString, Session,
};

/// An opened PostgreSQL Data Source.
pub type PostgresDataSource = AdbcDatabase<PostgresConnector>;

/// A PostgreSQL session.
pub type PostgresSession = Session<PostgresConnector>;

type PostgresAccess = Adbc<PostgresConnector>;
use futures_core::Stream;

pub const PASSWORD: &str = "connector-test-secret";

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
    let image = std::env::var("POSTGRESQL_TEST_IMAGE").unwrap_or_else(|_| "postgres:18".to_owned());
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
    /// Runs SQL as the superuser over TCP, failing on the first error.
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

    /// Starts `sql` as the superuser in a `psql` process that keeps running,
    /// identified at the server by `application_name`.
    pub fn spawn_psql(&self, application_name: &str, sql: &str) -> Child {
        let mut child = Command::new("docker")
            .args([
                "exec",
                "-i",
                "-e",
                &format!("PGAPPNAME={application_name}"),
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
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("psql starts");
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(sql.as_bytes())
            .expect("psql reads the SQL");
        child
    }

    /// Recreates `schema` empty and runs `sql` in it.
    pub fn reset_schema(&self, schema: &str, sql: &str) {
        self.psql(&format!(
            "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE; CREATE SCHEMA \"{schema}\"; SET search_path = \"{schema}\"; {sql}"
        ));
    }
}

/// The connector's process-level state, loading drivers from
/// `ADBC_DRIVER_DIRECTORY`.
pub fn library() -> AdbcLibrary {
    let directory = std::env::var_os("ADBC_DRIVER_DIRECTORY")
        .expect("ADBC_DRIVER_DIRECTORY locates the ADBC drivers; run inside `nix develop`");
    PostgresAccess::open_library(&LibrarySettings {
        adbc_driver_directory: Some(PathBuf::from(directory)),
    })
    .expect("the library initializes")
}

/// Options for connecting as `user`, before validation.
pub fn options(user: &str, password: &str) -> Vec<(String, SecretString)> {
    let port = server().port.to_string();
    [
        ("host", "127.0.0.1"),
        ("port", port.as_str()),
        ("database", "postgres"),
        ("user", user),
        ("password", password),
        ("sslmode", "disable"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), SecretString::new(value.to_owned())))
    .collect()
}

/// Opens a Data Source with validated options.
pub fn open(
    library: &AdbcLibrary,
    options: Vec<(String, SecretString)>,
) -> Result<PostgresDataSource, ConnectorError> {
    let options = ConnectionOptions::parse(&PostgresConnector::descriptor(), options)?;
    PostgresAccess::open_data_source(
        library,
        connector_postgresql::CONNECTOR,
        options,
        Some(Duration::from_secs(10)),
    )
}

/// Opens a session on a Data Source.
pub fn open_session(data_source: &PostgresDataSource) -> Result<PostgresSession, ConnectorError> {
    Session::open(PostgresAccess::open_session(data_source)?, ())
}

/// Opens a Data Source as the superuser.
pub fn data_source() -> PostgresDataSource {
    open(&library(), options("postgres", PASSWORD)).expect("the data source opens")
}

/// Opens a session as the superuser.
pub fn session() -> PostgresSession {
    open_session(&data_source()).expect("the session opens")
}

/// Pulls a synchronous stream to its end, keeping items and the final error.
pub fn collect<S, T>(stream: &mut S) -> (Vec<T>, Option<ConnectorError>)
where
    S: Stream<Item = Result<T, ConnectorError>> + Unpin,
{
    let mut context = Context::from_waker(Waker::noop());
    let mut items = Vec::new();
    loop {
        match std::pin::Pin::new(&mut *stream).poll_next(&mut context) {
            Poll::Ready(Some(Ok(item))) => items.push(item),
            Poll::Ready(Some(Err(error))) => return (items, Some(error)),
            Poll::Ready(None) => return (items, None),
            Poll::Pending => panic!("ADBC reads never return pending"),
        }
    }
}

/// Pulls a stream to its end and fails on an error.
pub fn collect_ok<S, T>(stream: &mut S) -> Vec<T>
where
    S: Stream<Item = Result<T, ConnectorError>> + Unpin,
{
    let (items, error) = collect(stream);
    if let Some(error) = error {
        panic!(
            "the stream failed: {error} ({:?})",
            error.data_source_message()
        );
    }
    items
}
