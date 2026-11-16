//! A MySQL server in Docker and the options for connecting to it.
//!
//! Tests share one container per image, named after the image and left
//! running between runs so that later runs start quickly. Each test file
//! works in its own databases. `MYSQL_TEST_IMAGE` selects the server image
//! (default `mysql:8.4`).

use std::{
    io::Write,
    process::{Command, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use connector_sdk::SecretString;

const PASSWORD: &str = "connector-test-secret";

/// The MySQL server the tests use.
pub struct Server {
    container: String,
    port: u16,
}

/// Returns the running test server, starting its container if needed.
pub fn server() -> &'static Server {
    static SERVER: OnceLock<Server> = OnceLock::new();
    SERVER.get_or_init(start)
}

fn start() -> Server {
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
            if let Err(run) = docker(&[
                "run",
                "-d",
                "--name",
                &container,
                "-e",
                &format!("MYSQL_ROOT_PASSWORD={PASSWORD}"),
                "-p",
                "127.0.0.1::3306",
                &image,
            ]) && let Err(start) = docker(&["start", &container])
            {
                panic!("the test container starts: run failed with {run:?}, start with {start:?}");
            }
        }
    }
    let port = docker(&["port", &container, "3306/tcp"]).expect("the container publishes a port");
    let port = port
        .lines()
        .next()
        .and_then(|line| line.rsplit(':').next())
        .and_then(|port| port.trim().parse().ok())
        .expect("the published port is a number");
    let server = Server { container, port };
    // The entrypoint starts a temporary server without TCP before the real
    // one, so readiness is checked over TCP.
    let deadline = Instant::now() + Duration::from_secs(120);
    while server.try_mysql("SELECT 1").is_err() {
        assert!(Instant::now() < deadline, "MySQL did not become ready");
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
    /// Runs SQL as root over TCP, failing on the first error.
    pub fn mysql(&self, sql: &str) -> String {
        self.try_mysql(sql)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// Runs SQL as root over TCP with the `mysql` client, printing rows as
    /// tab-separated values without headers, and returns the client's error
    /// output on failure.
    pub fn try_mysql(&self, sql: &str) -> Result<String, String> {
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
                "--default-character-set=utf8mb4",
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

/// Options for connecting to the server as `user`, before validation.
pub fn options(user: &str, password: &str) -> Vec<(String, SecretString)> {
    let port = server().port.to_string();
    [
        ("host", "127.0.0.1"),
        ("port", port.as_str()),
        ("user", user),
        ("password", password),
        ("tls", "false"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), SecretString::new(value.to_owned())))
    .collect()
}
