//! An HTTP proxy in front of DynamoDB Local, which the fixture's credentials
//! reach DynamoDB through.
//!
//! DynamoDB Local accepts any credentials and answers every request at once,
//! so the proxy adds what the suite asks of the source and Local lacks:
//!
//! - IAM: requests are signed with an access key ID that names a [`Policy`],
//!   and the proxy answers what that policy denies with DynamoDB's error, as
//!   DynamoDB does before it looks at the table;
//! - waits: a read of [`SLOW`] waits until the client closes its connection,
//!   and `DescribeTable` of [`BLOCKED`] waits while [`Proxy::block`] holds;
//! - an endless read: every `Scan` page of [`ENDLESS`] has one item and a
//!   next page;
//! - observation: the requests being answered, and breaking every connection.
//!
//! Every other request is forwarded to Local unchanged. Each connection
//! carries one request and is closed after its response.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    runtime::Runtime,
    task::AbortHandle,
};

/// The table whose reads wait until the client closes the connection.
pub const SLOW: &str = "slow";
/// The table whose `Scan` never ends.
pub const ENDLESS: &str = "endless";
/// The table whose `DescribeTable` waits while a block holds.
pub const BLOCKED: &str = "blocked";

/// How long the network stays down after the connections break: longer than
/// the SDK's retries of the request that broke take.
const OUTAGE: Duration = Duration::from_secs(3);

/// What an access key ID may do; the key ID is the policy's name.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Everything.
    Reader,
    /// Not `DescribeTable` of `locked` or of a table that does not exist, and
    /// not reading `texts`.
    Outsider,
    /// Everything except `ListTables`.
    Unlister,
    /// Nothing: DynamoDB does not recognize the key.
    Rejected,
}

impl Policy {
    pub const ALL: [Self; 4] = [Self::Reader, Self::Outsider, Self::Unlister, Self::Rejected];

    /// The access key ID, which DynamoDB Local accepts only as letters and
    /// digits.
    pub fn key_id(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Outsider => "outsider",
            Self::Unlister => "unlister",
            Self::Rejected => "rejected",
        }
    }

    /// The error DynamoDB answers `operation` on `table` with, if the policy
    /// denies it.
    fn denial(self, operation: &str, table: Option<&str>) -> Option<(&'static str, String)> {
        let denied = |action: &str| {
            Some((
                "AccessDeniedException",
                format!(
                    "User: {} is not authorized to perform: dynamodb:{action}",
                    self.key_id()
                ),
            ))
        };
        match (self, operation, table) {
            (Self::Rejected, _, _) => Some((
                "UnrecognizedClientException",
                "The security token included in the request is invalid.".to_owned(),
            )),
            (Self::Unlister, "ListTables", _) => denied(operation),
            (Self::Outsider, "DescribeTable", Some("locked" | "absent"))
            | (Self::Outsider, "Scan" | "Query", Some("texts")) => denied(operation),
            _ => None,
        }
    }
}

/// The proxy, serving until its runtime stops.
pub struct Proxy {
    pub url: String,
    state: Arc<Mutex<State>>,
}

struct State {
    /// The requests being answered.
    running: usize,
    connections: Vec<AbortHandle>,
    blocked: bool,
    /// Until when every new connection is closed at once.
    outage_until: Option<Instant>,
}

/// Holds `DescribeTable` of [`BLOCKED`] while alive.
pub struct Block(Arc<Mutex<State>>);

impl Drop for Block {
    fn drop(&mut self) {
        lock(&self.0).blocked = false;
    }
}

impl Proxy {
    /// Starts the proxy on `runtime`, forwarding to `upstream`.
    pub fn start(runtime: &Runtime, upstream: SocketAddr) -> Self {
        let listener = runtime
            .block_on(TcpListener::bind("127.0.0.1:0"))
            .expect("the proxy binds a local port");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State {
            running: 0,
            connections: Vec::new(),
            blocked: false,
            outage_until: None,
        }));
        let accepting = Arc::clone(&state);
        runtime.spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let mut state = lock(&accepting);
                if state
                    .outage_until
                    .is_some_and(|until| Instant::now() < until)
                {
                    drop(stream);
                    continue;
                }
                let connection = Arc::clone(&accepting);
                let task = tokio::spawn(serve(connection, stream, upstream));
                state.connections.push(task.abort_handle());
            }
        });
        Self { url, state }
    }

    /// How many requests are being answered.
    pub fn running(&self) -> usize {
        lock(&self.state).running
    }

    /// Makes `DescribeTable` of [`BLOCKED`] wait until the block is dropped.
    pub fn block(&self) -> Block {
        lock(&self.state).blocked = true;
        Block(Arc::clone(&self.state))
    }

    /// Closes every open connection, as a network failure would, and keeps
    /// the network down for [`OUTAGE`], so that the SDK's retries of a broken
    /// request fail too.
    pub fn break_connections(&self) {
        let connections = {
            let mut state = lock(&self.state);
            state.outage_until = Some(Instant::now() + OUTAGE);
            std::mem::take(&mut state.connections)
        };
        for connection in connections {
            connection.abort();
        }
        // An aborted task closes its connection when the runtime drops it.
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Counts one request as running while alive.
struct Running(Arc<Mutex<State>>);

impl Running {
    fn new(state: &Arc<Mutex<State>>) -> Self {
        lock(state).running += 1;
        Self(Arc::clone(state))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        lock(&self.0).running -= 1;
    }
}

/// One request: its head as received, its body, and what the proxy reads
/// from them.
struct Request {
    head: String,
    body: Vec<u8>,
    /// The DynamoDB operation, such as `Scan`.
    operation: String,
    table: Option<String>,
    policy: Option<Policy>,
}

async fn serve(state: Arc<Mutex<State>>, mut client: TcpStream, upstream: SocketAddr) {
    let Some(request) = read_request(&mut client).await else {
        return;
    };
    let _running = Running::new(&state);
    let table = request.table.as_deref();
    let Some(policy) = request.policy else {
        let _ = respond(
            &mut client,
            400,
            &error_body("MissingAuthenticationTokenException", "Missing credentials"),
        )
        .await;
        return;
    };
    if let Some((code, message)) = policy.denial(&request.operation, table) {
        let _ = respond(&mut client, 400, &error_body(code, &message)).await;
        return;
    }
    match (request.operation.as_str(), table) {
        ("Scan" | "Query", Some(SLOW)) => {
            // The client sends nothing while it waits, so a read ends only
            // when it closes the connection.
            let mut probe = [0_u8; 1];
            let _ = client.read(&mut probe).await;
            return;
        }
        ("Scan", Some(ENDLESS)) => {
            let item = json!({ "pk": { "S": "x" } });
            let page = json!({
                "Items": [item],
                "Count": 1,
                "ScannedCount": 1,
                "LastEvaluatedKey": item,
            });
            let _ = respond(&mut client, 200, &page).await;
            return;
        }
        ("DescribeTable", Some(BLOCKED)) => {
            while lock(&state).blocked {
                let mut probe = [0_u8; 1];
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(20)) => {}
                    _ = client.read(&mut probe) => return,
                }
            }
        }
        _ => {}
    }
    let _ = forward(&request, &mut client, upstream).await;
}

/// Sends `request` to Local on a new connection and copies its response
/// back until Local closes the connection.
async fn forward(
    request: &Request,
    client: &mut TcpStream,
    upstream: SocketAddr,
) -> std::io::Result<()> {
    let mut local = TcpStream::connect(upstream).await?;
    let mut head: String = request
        .head
        .split("\r\n")
        .filter(|line| !line.to_ascii_lowercase().starts_with("connection:"))
        .map(|line| format!("{line}\r\n"))
        .collect();
    head.push_str("connection: close\r\n\r\n");
    local.write_all(head.as_bytes()).await?;
    local.write_all(&request.body).await?;
    tokio::io::copy(&mut local, client).await?;
    client.shutdown().await
}

async fn respond(client: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let body = body.to_string();
    let head = format!(
        "HTTP/1.1 {status} Proxy\r\ncontent-type: application/x-amz-json-1.0\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    client.write_all(head.as_bytes()).await?;
    client.write_all(body.as_bytes()).await?;
    client.shutdown().await
}

fn error_body(code: &str, message: &str) -> Value {
    json!({
        "__type": format!("com.amazonaws.dynamodb.v20120810#{code}"),
        "message": message,
    })
}

/// Reads one request: its head up to the empty line, without it, and the
/// body its `content-length` gives. `None` when the client closes first.
async fn read_request(client: &mut TcpStream) -> Option<Request> {
    let mut buffer = Vec::new();
    let end = loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        let mut chunk = [0_u8; 4096];
        let read = client.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8(buffer[..end].to_vec()).ok()?;
    let mut body = buffer[end + 4..].to_vec();
    let header = |name: &str| {
        head.split("\r\n").skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    let length: usize = header("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    while body.len() < length {
        let mut chunk = [0_u8; 4096];
        let read = client.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    let operation = header("x-amz-target")
        .and_then(|target| target.rsplit('.').next().map(str::to_owned))
        .unwrap_or_default();
    let table = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|body| body["TableName"].as_str().map(str::to_owned));
    let policy = header("authorization").and_then(|authorization| {
        let key_id = authorization
            .split("Credential=")
            .nth(1)?
            .split('/')
            .next()?;
        Policy::ALL
            .into_iter()
            .find(|policy| policy.key_id() == key_id)
    });
    Some(Request {
        head,
        body,
        operation,
        table,
        policy,
    })
}
