//! DynamoDB Local in Docker.
//!
//! Tests share one container per image, named after the image and left
//! running between runs so that later runs start quickly. It keeps its tables
//! in memory, in one database for every access key and Region
//! (`-sharedDb`), with a capped Java heap. `DYNAMODB_TEST_IMAGE` selects the
//! image (default `amazon/dynamodb-local:3.3.2`).

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    process::Command,
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

/// The DynamoDB Local server the tests use.
pub struct Local {
    pub address: SocketAddr,
}

/// Returns the running server, starting its container if needed.
pub fn local() -> &'static Local {
    static LOCAL: OnceLock<Local> = OnceLock::new();
    LOCAL.get_or_init(start)
}

fn start() -> Local {
    let image = std::env::var("DYNAMODB_TEST_IMAGE")
        .unwrap_or_else(|_| "amazon/dynamodb-local:3.3.2".to_owned());
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
            // The image's entrypoint is `java`, so the arguments replace only
            // its default command.
            if let Err(run) = docker(&[
                "run",
                "-d",
                "--name",
                &container,
                "-p",
                "127.0.0.1::8000",
                &image,
                "-Xmx256m",
                "-jar",
                "DynamoDBLocal.jar",
                "-inMemory",
                "-sharedDb",
            ]) && let Err(start) = docker(&["start", &container])
            {
                panic!("the test container starts: run failed with {run:?}, start with {start:?}");
            }
        }
    }
    let port = docker(&["port", &container, "8000/tcp"]).expect("the container publishes a port");
    let port: u16 = port
        .lines()
        .next()
        .and_then(|line| line.rsplit(':').next())
        .and_then(|port| port.trim().parse().ok())
        .expect("the published port is a number");
    let local = Local {
        address: SocketAddr::from(([127, 0, 0, 1], port)),
    };
    // Docker accepts connections on the published port before the server
    // listens, so readiness is an HTTP response.
    let deadline = Instant::now() + Duration::from_secs(120);
    while !local.answers() {
        assert!(
            Instant::now() < deadline,
            "DynamoDB Local did not become ready"
        );
        thread::sleep(Duration::from_millis(500));
    }
    local
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

impl Local {
    /// The endpoint URL of the server.
    pub fn endpoint(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Whether the server answers an HTTP request, with any status.
    fn answers(&self) -> bool {
        let Ok(mut stream) = TcpStream::connect_timeout(&self.address, Duration::from_secs(2))
        else {
            return false;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        if stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .is_err()
        {
            return false;
        }
        let mut head = [0_u8; 5];
        stream.read_exact(&mut head).is_ok() && &head == b"HTTP/"
    }
}
