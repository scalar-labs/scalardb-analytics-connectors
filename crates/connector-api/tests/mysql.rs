//! The MySQL Connector through the C ABI, when the `mysql` feature builds it
//! into the library.
//!
//! The test runs against a MySQL server in Docker, in the container the MySQL
//! connector's own tests use, and loads the ADBC driver from
//! `ADBC_DRIVER_DIRECTORY`, which the Nix development shell sets.
//! `MYSQL_TEST_IMAGE` selects the server image (default `mysql:8.4`).

#![cfg(feature = "mysql")]

mod support;

use std::{
    io::Write,
    process::{Command, Stdio},
    sync::{Once, OnceLock},
    thread,
    time::{Duration, Instant},
};

use connector_api::abi::*;
use serde_json::Value;
use support::*;

const PASSWORD: &str = "connector-test-secret";
const SCHEMA: &str = "connector_api_registration";

/// A MySQL server in Docker.
struct Server {
    container: String,
    port: u16,
}

impl Server {
    /// Returns the server every test in this process shares. Tests that
    /// started it concurrently would race on a new container: one creates it
    /// while it pulls the image, and another finds the name in use before the
    /// container exists.
    fn shared() -> &'static Self {
        static SERVER: OnceLock<Server> = OnceLock::new();
        SERVER.get_or_init(Self::start)
    }

    /// Returns the running server, starting its container if needed.
    fn start() -> Self {
        let image = std::env::var("MYSQL_TEST_IMAGE").unwrap_or_else(|_| "mysql:8.4".to_owned());
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
                    &format!("MYSQL_ROOT_PASSWORD={PASSWORD}"),
                    "-p",
                    "127.0.0.1::3306",
                    &image,
                ])
                .is_err()
                {
                    docker(&["start", &container]).expect("the test container starts");
                }
            }
        }
        let port = docker(&["port", &container, "3306/tcp"])
            .expect("the container publishes a port")
            .lines()
            .next()
            .and_then(|line| line.rsplit(':').next())
            .and_then(|port| port.trim().parse().ok())
            .expect("the published port is a number");
        let server = Self { container, port };
        let deadline = Instant::now() + Duration::from_secs(120);
        while server.mysql("SELECT 1").is_err() {
            assert!(Instant::now() < deadline, "MySQL did not become ready");
            thread::sleep(Duration::from_millis(500));
        }
        server
    }

    /// Runs SQL as root over TCP, failing on the first error.
    fn mysql(&self, sql: &str) -> Result<String, String> {
        let mut child = Command::new("docker")
            .args([
                "exec",
                "-i",
                "-e",
                &format!("MYSQL_PWD={PASSWORD}"),
                &self.container,
                "mysql",
                "--protocol=TCP",
                "-h127.0.0.1",
                "-uroot",
                "--batch",
                "--skip-column-names",
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

/// Initializes the library once for every test in this process, with the
/// ADBC drivers of the development shell.
fn initialize_with_drivers() {
    static INITIALIZED: Once = Once::new();
    INITIALIZED.call_once(|| {
        let directory = std::env::var("ADBC_DRIVER_DIRECTORY")
            .expect("ADBC_DRIVER_DIRECTORY locates the ADBC drivers; run inside `nix develop`");
        let directory = text(&directory);
        let settings = ScalardbAnalyticsConnectorLibrarySettings {
            adbc_driver_directory: directory.as_ptr(),
        };
        // SAFETY: the settings are valid for the call.
        check(unsafe { (api().library_initialize)(&settings) }).unwrap();
    });
}

/// Opens a MySQL Data Source as root, with `extra` options.
fn open_mysql(server: &Server, extra: &[(&str, &str)]) -> DataSource {
    let port = server.port.to_string();
    let mut options = vec![
        ("host", "127.0.0.1"),
        ("port", port.as_str()),
        ("user", "root"),
        ("password", PASSWORD),
        ("tls", "false"),
    ];
    options.extend_from_slice(extra);
    DataSource::open_connector("mysql", &options).unwrap()
}

#[test]
fn a_mysql_data_source_is_read_through_the_c_abi() {
    let information: Value =
        serde_json::from_str(&read_string((api().static_information)()).unwrap()).unwrap();
    let mysql = information["connectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|connector| connector["name"] == "mysql")
        .expect("the static information lists the MySQL Connector");
    assert_eq!(mysql["connectors"][0], "mysql");
    assert_eq!(mysql["data_source_access"]["form"], "adbc");
    assert_eq!(mysql["data_source_access"]["driver"], "adbc_driver_mysql");
    let password = mysql["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["key"] == "password")
        .unwrap();
    assert_eq!(password["secret"], true);

    let server = Server::shared();
    server
        .mysql(&format!(
            "DROP DATABASE IF EXISTS {SCHEMA}; CREATE DATABASE {SCHEMA}; \
             CREATE TABLE {SCHEMA}.registered (id bigint PRIMARY KEY);"
        ))
        .unwrap();

    initialize_with_drivers();
    let data_source = open_mysql(server, &[]);
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

/// Reading one table under each `tinyint1_as_boolean` choice observes the
/// same source, so only the table metadata, which the option maps, differs.
#[test]
fn the_tinyint1_option_changes_the_table_metadata_and_not_the_source_metadata() {
    const OPTION_SCHEMA: &str = "connector_api_tinyint1";
    let server = Server::shared();
    server
        .mysql(&format!(
            "DROP DATABASE IF EXISTS {OPTION_SCHEMA}; CREATE DATABASE {OPTION_SCHEMA}; \
             CREATE TABLE {OPTION_SCHEMA}.flags (id bigint PRIMARY KEY, flag BOOLEAN);"
        ))
        .unwrap();
    initialize_with_drivers();
    let read = |extra: &[(&str, &str)]| {
        let data_source = open_mysql(server, extra);
        let session = data_source.session().unwrap();
        let result = session
            .table_metadata_of(&[&[OPTION_SCHEMA, "flags"]], 0, None)
            .unwrap();
        session.close();
        data_source.close();
        result
    };
    let byte = read(&[]);
    let boolean = read(&[("tinyint1_as_boolean", "true")]);

    assert!(
        compare_source_metadata(byte.source_metadata(), boolean.source_metadata())
            .unwrap()
            .is_none()
    );
    let differences = compare_table_metadata(byte.table_metadata(), boolean.table_metadata())
        .unwrap()
        .expect("the flag column maps to another Analytics type");
    // SAFETY: the library returns `column_count` column changes.
    let columns = unsafe { items(differences.columns, differences.column_count) };
    assert_eq!(columns.len(), 1);
    // SAFETY: a matched change points to both versions and its attributes.
    let (column, attributes) = unsafe {
        (
            &*columns[0].before,
            items(columns[0].attributes, columns[0].attribute_count),
        )
    };
    assert_eq!(
        columns[0].change,
        ScalardbAnalyticsConnectorChangeKind::MATCHED
    );
    assert_eq!(read_string(column.name).as_deref(), Some("flag"));
    assert_eq!(
        attributes,
        [ScalardbAnalyticsConnectorColumnAttribute::ANALYTICS_TYPE]
    );
}
