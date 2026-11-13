//! The PostgreSQL Connector through the C ABI, when the `postgresql` feature
//! builds it into the library.
//!
//! The test runs against a PostgreSQL server in Docker, in the container the
//! PostgreSQL connector's own tests use, and loads the ADBC driver from
//! `ADBC_DRIVER_DIRECTORY`, which the Nix development shell sets.
//! `POSTGRESQL_TEST_IMAGE` selects the server image (default `postgres:18`).

#![cfg(feature = "postgresql")]

mod support;

use std::{
    io::Write,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use connector_api::abi::*;
use serde_json::Value;
use support::*;

const PASSWORD: &str = "connector-test-secret";
const SCHEMA: &str = "connector_api_registration";

/// A PostgreSQL server in Docker.
struct Server {
    container: String,
    port: u16,
}

impl Server {
    /// Returns the running server, starting its container if needed.
    fn start() -> Self {
        let image =
            std::env::var("POSTGRESQL_TEST_IMAGE").unwrap_or_else(|_| "postgres:18".to_owned());
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
        let port = docker(&["port", &container, "5432/tcp"])
            .expect("the container publishes a port")
            .lines()
            .next()
            .and_then(|line| line.rsplit(':').next())
            .and_then(|port| port.trim().parse().ok())
            .expect("the published port is a number");
        let server = Self { container, port };
        let deadline = Instant::now() + Duration::from_secs(60);
        while server.psql("SELECT 1").is_err() {
            assert!(Instant::now() < deadline, "PostgreSQL did not become ready");
            thread::sleep(Duration::from_millis(250));
        }
        server
    }

    /// Runs SQL as the superuser, failing on the first error.
    fn psql(&self, sql: &str) -> Result<String, String> {
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

/// Initializes the library with the ADBC drivers of the development shell.
fn initialize_with_drivers() {
    let directory = std::env::var("ADBC_DRIVER_DIRECTORY")
        .expect("ADBC_DRIVER_DIRECTORY locates the ADBC drivers; run inside `nix develop`");
    let directory = text(&directory);
    let settings = ScalardbAnalyticsConnectorLibrarySettings {
        adbc_driver_directory: directory.as_ptr(),
    };
    // SAFETY: the settings are valid for the call.
    check(unsafe { (api().library_initialize)(&settings) }).unwrap();
}

#[test]
fn a_postgresql_data_source_is_read_through_the_c_abi() {
    let information: Value =
        serde_json::from_str(&read_string((api().static_information)()).unwrap()).unwrap();
    let postgresql = information["connectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|connector| connector["name"] == "postgresql")
        .expect("the static information lists the PostgreSQL Connector");
    assert_eq!(postgresql["connectors"][0], "postgresql");
    assert_eq!(postgresql["data_source_access"]["form"], "adbc");
    assert_eq!(
        postgresql["data_source_access"]["driver"],
        "adbc_driver_postgresql"
    );
    let password = postgresql["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["key"] == "password")
        .unwrap();
    assert_eq!(password["secret"], true);

    let server = Server::start();
    server
        .psql(&format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; CREATE SCHEMA {SCHEMA}; \
             CREATE TABLE {SCHEMA}.registered (id bigint PRIMARY KEY);"
        ))
        .unwrap();

    initialize_with_drivers();
    let port = server.port.to_string();
    let data_source = DataSource::open_connector(
        "postgresql",
        &[
            ("host", "127.0.0.1"),
            ("port", &port),
            ("database", "postgres"),
            ("user", "postgres"),
            ("password", PASSWORD),
            ("sslmode", "disable"),
        ],
    )
    .unwrap();
    let session = data_source.session().unwrap();

    let namespaces = session.namespace_list(Some(&[SCHEMA]), 0, None).unwrap();
    // SAFETY: the library returns `entry_count` entries.
    let entries = unsafe { items(namespaces.entries, namespaces.entry_count) };
    let names: Vec<_> = entries
        .iter()
        .map(|entry| read_names(&entry.namespace))
        .collect();
    assert_eq!(names, [[SCHEMA]]);

    let tables = session.table_list(Some(&[SCHEMA]), None).unwrap();
    // SAFETY: the library returns `entry_count` entries.
    let entries = unsafe { items(tables.entries, tables.entry_count) };
    let names: Vec<_> = entries
        .iter()
        .map(|entry| read_names(&entry.table))
        .collect();
    assert_eq!(names, [[SCHEMA, "registered"]]);
    assert_eq!(entries[0].kind, ScalardbAnalyticsConnectorTableKind::TABLE);

    session.close();
    data_source.close();
}
