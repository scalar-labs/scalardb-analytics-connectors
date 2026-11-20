//! An HTTPS proxy on `127.0.0.1` that the connector reaches the workspace
//! through, so that the fixture can break its connections.
//!
//! The driver verifies the workspace's certificate against its host name, so
//! a proxy cannot stand in for the host. Instead the suite runs with
//! `HTTPS_PROXY` naming this proxy, which databricks-sql-go's HTTP transport
//! honours: each connection asks the proxy to open a tunnel with `CONNECT`,
//! and TLS runs end to end through it. Whether CloudFetch's downloads from
//! cloud storage use the same transport, and so the proxy, is not verified;
//! if they bypass it, breaking connections does not interrupt a download in
//! progress.

use std::sync::{Arc, Mutex, PoisonError};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional},
    net::{TcpListener, TcpStream},
    runtime::Runtime,
    task::AbortHandle,
};

/// The proxy, serving until its runtime stops.
pub struct Proxy {
    tunnels: Arc<Mutex<Vec<AbortHandle>>>,
}

impl Proxy {
    /// Starts the proxy on `runtime` at `127.0.0.1:port`.
    pub fn start(runtime: &Runtime, port: u16) -> Self {
        let listener = runtime
            .block_on(TcpListener::bind(("127.0.0.1", port)))
            .expect("the proxy binds its port");
        let tunnels = Arc::new(Mutex::new(Vec::new()));
        let accepting = Arc::clone(&tunnels);
        runtime.spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let tunnel = tokio::spawn(tunnel(client));
                accepting
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(tunnel.abort_handle());
            }
        });
        Self { tunnels }
    }

    /// Closes every open tunnel, as a network failure would. A tunnel opened
    /// afterwards works.
    pub fn break_connections(&self) {
        let tunnels =
            std::mem::take(&mut *self.tunnels.lock().unwrap_or_else(PoisonError::into_inner));
        for tunnel in tunnels {
            tunnel.abort();
        }
        // An aborted task closes its connections when the runtime drops it.
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Serves one `CONNECT host:port` request: opens the connection to the host
/// and copies bytes both ways until either side closes.
async fn tunnel(mut client: TcpStream) {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0_u8; 1];
        match client.read(&mut byte).await {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&head);
    let target = head
        .strip_prefix("CONNECT ")
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned);
    let Some(target) = target else {
        let _ = client
            .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nconnection: close\r\n\r\n")
            .await;
        return;
    };
    let Ok(mut upstream) = TcpStream::connect(target).await else {
        let _ = client
            .write_all(b"HTTP/1.1 502 Bad Gateway\r\nconnection: close\r\n\r\n")
            .await;
        return;
    };
    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .is_ok()
    {
        let _ = copy_bidirectional(&mut client, &mut upstream).await;
    }
}
