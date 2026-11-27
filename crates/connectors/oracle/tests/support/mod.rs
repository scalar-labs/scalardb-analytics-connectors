//! An Oracle Database in Docker, administered with SQL*Plus inside its
//! container.
//!
//! Tests share one container per image, named after the image and left
//! running between runs so that later runs start quickly.
//! `ORACLE_TEST_IMAGE` selects the server image (default
//! `gvenzl/oracle-free:23-slim-faststart`), whose pluggable database is
//! `FREEPDB1`. The connector reaches the server through the ODBC driver in
//! `ODBC_DRIVER_DIRECTORY`, which the Nix development shell sets.

#![allow(dead_code)]

use std::{
    io::Write,
    process::{Command, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

/// The password of `SYSTEM`, which administers the server.
pub const SYSTEM_PASSWORD: &str = "Connector-test-secret1";

/// The service name of the pluggable database the tests use.
pub const SERVICE_NAME: &str = "FREEPDB1";

/// The Oracle Database the tests use.
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
    let image = std::env::var("ORACLE_TEST_IMAGE")
        .unwrap_or_else(|_| "gvenzl/oracle-free:23-slim-faststart".to_owned());
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
                &format!("ORACLE_PASSWORD={SYSTEM_PASSWORD}"),
                "-p",
                "127.0.0.1::1521",
                &image,
            ])
            .is_err()
            {
                docker(&["start", &container]).expect("the test container starts");
            }
        }
    }
    let port = docker(&["port", &container, "1521/tcp"]).expect("the container publishes a port");
    let port = port
        .lines()
        .next()
        .and_then(|line| line.rsplit(':').next())
        .and_then(|port| port.trim().parse().ok())
        .expect("the published port is a number");
    let server = Server { container, port };
    let deadline = Instant::now() + Duration::from_secs(180);
    while server.try_sqlplus("SELECT 1 FROM dual;").is_err() {
        assert!(
            Instant::now() < deadline,
            "Oracle did not become ready: {}",
            docker(&["logs", "--tail", "20", &server.container]).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(500));
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
    /// Runs a SQL*Plus script as `SYSTEM` in the pluggable database, failing
    /// on the first error, and returns what it printed: query results only,
    /// without headings or feedback.
    pub fn sqlplus(&self, script: &str) -> String {
        self.try_sqlplus(script)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn try_sqlplus(&self, script: &str) -> Result<String, String> {
        let mut child = Command::new("docker")
            .args([
                "exec",
                "-i",
                "-e",
                "NLS_LANG=AMERICAN_AMERICA.AL32UTF8",
                &self.container,
                "sqlplus",
                "-S",
                "-L",
                &format!("system/{SYSTEM_PASSWORD}@//localhost/{SERVICE_NAME}"),
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
            .write_all(
                format!(
                    "WHENEVER SQLERROR EXIT FAILURE\nWHENEVER OSERROR EXIT FAILURE\n\
                     SET HEADING OFF FEEDBACK OFF PAGESIZE 0 LINESIZE 32767 TRIMSPOOL ON \
                     TRIMOUT ON TAB OFF\n{script}\nEXIT\n"
                )
                .as_bytes(),
            )
            .map_err(|error| error.to_string())?;
        let output = child
            .wait_with_output()
            .map_err(|error| error.to_string())?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() {
            Ok(stdout)
        } else {
            Err(format!(
                "{stdout}{}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }
}
