//! A SQL Server in Docker, administered with `sqlcmd` inside its container.
//!
//! Tests share one container per image, named after the image and left
//! running between runs so that later runs start quickly.
//! `SQLSERVER_TEST_IMAGE` selects the server image (default
//! `mcr.microsoft.com/mssql/server:2022-latest`), which runs as `linux/amd64`.
//! The connector reaches the server through the ODBC driver the driver
//! manager finds under the connector's driver name, which the Nix development
//! shell registers through `ODBCSYSINI`.

#![allow(dead_code)]

use std::{
    io::Write,
    process::{Child, Command, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

/// The password of the `sa` login, which administers the server.
pub const SA_PASSWORD: &str = "Connector-test-secret1";

/// The SQL Server the tests use.
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
    let image = std::env::var("SQLSERVER_TEST_IMAGE")
        .unwrap_or_else(|_| "mcr.microsoft.com/mssql/server:2022-latest".to_owned());
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
                "--platform",
                "linux/amd64",
                "--name",
                &container,
                "-e",
                "ACCEPT_EULA=Y",
                "-e",
                &format!("MSSQL_SA_PASSWORD={SA_PASSWORD}"),
                "-p",
                "127.0.0.1::1433",
                &image,
            ])
            .is_err()
            {
                docker(&["start", &container]).expect("the test container starts");
            }
        }
    }
    let port = docker(&["port", &container, "1433/tcp"]).expect("the container publishes a port");
    let port = port
        .lines()
        .next()
        .and_then(|line| line.rsplit(':').next())
        .and_then(|port| port.trim().parse().ok())
        .expect("the published port is a number");
    let server = Server { container, port };
    let deadline = Instant::now() + Duration::from_secs(120);
    while server.try_sqlcmd("SELECT 1").is_err() {
        assert!(Instant::now() < deadline, "SQL Server did not become ready");
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
    /// The `sqlcmd` invocation inside the container, as `sa`, stopping at the
    /// first error and printing bare values.
    fn sqlcmd_command(&self) -> Command {
        let mut command = Command::new("docker");
        command.args([
            "exec",
            "-i",
            &self.container,
            "/opt/mssql-tools18/bin/sqlcmd",
            "-C",
            "-S",
            "127.0.0.1",
            "-U",
            "sa",
            "-P",
            SA_PASSWORD,
            "-b",
            "-I",
            "-h",
            "-1",
            "-W",
        ]);
        command
    }

    /// Runs a script of `GO`-separated batches as `sa`, failing on the first
    /// error, and returns what it printed.
    pub fn sqlcmd(&self, sql: &str) -> String {
        self.try_sqlcmd(sql)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn try_sqlcmd(&self, sql: &str) -> Result<String, String> {
        let mut child = self
            .sqlcmd_command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(format!("SET NOCOUNT ON;\n{sql}\nGO\n").as_bytes())
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

    /// Starts a script as `sa` in a `sqlcmd` process that keeps running,
    /// with `workstation` as its host name at the server.
    pub fn spawn_sqlcmd(&self, workstation: &str, sql: &str) -> Child {
        let mut command = self.sqlcmd_command();
        command.args(["-H", workstation]);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sqlcmd starts");
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(format!("{sql}\nGO\n").as_bytes())
            .expect("sqlcmd reads the SQL");
        child
    }
}
