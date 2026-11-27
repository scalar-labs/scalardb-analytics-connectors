//! The ADBC Driver Foundry Databricks driver loaded from the ADBC driver
//! directory, against local endpoints that are no workspace.
//!
//! No workspace is needed: the driver connects to `127.0.0.1`, where nothing
//! answers or the connection closes at once, so opening a session fails at
//! the driver's first request. The tests check that the failure is
//! classified and reveals no credential. They are skipped when
//! `ADBC_DRIVER_DIRECTORY` is not set, as outside `nix develop`.

use std::{
    net::{Ipv4Addr, TcpListener},
    path::PathBuf,
    sync::Arc,
    thread,
    time::Duration,
};

use connector_databricks::{CONNECTOR, DatabricksConnector};
use connector_sdk::{
    ConnectorError, ErrorKind, LibrarySettings, SecretString,
    runtime::{BuiltInConnector, Library},
};

const TOKEN: &str = "dapi-driver-test-secret";
const CLIENT_SECRET: &str = "oauth-driver-test-secret";

/// The library with the Databricks Connector and the ADBC driver directory,
/// or `None`, after saying why, when the directory is not set.
fn library() -> Option<Arc<Library>> {
    let Some(directory) = std::env::var_os("ADBC_DRIVER_DIRECTORY") else {
        eprintln!(
            "skipped: ADBC_DRIVER_DIRECTORY is not set; run inside `nix develop`, which \
             provides the Databricks driver"
        );
        return None;
    };
    let settings = LibrarySettings {
        adbc_driver_directory: Some(PathBuf::from(directory)),
        odbc_driver_directory: None,
    };
    Some(
        Library::initialize(
            &[BuiltInConnector::of::<DatabricksConnector>()],
            &settings,
            "driver-test",
        )
        .expect("the library initializes"),
    )
}

/// Options for a workspace at `127.0.0.1:port`, with a personal access token
/// or OAuth machine-to-machine credentials.
fn options(port: u16, oauth: bool) -> Vec<(String, SecretString)> {
    let port = port.to_string();
    let mut options = vec![
        ("host", "127.0.0.1"),
        ("port", port.as_str()),
        ("http_path", "/sql/1.0/warehouses/driver-test"),
    ];
    if oauth {
        options.extend([
            ("client_id", "driver-test-principal"),
            ("client_secret", CLIENT_SECRET),
        ]);
    } else {
        options.push(("access_token", TOKEN));
    }
    options
        .into_iter()
        .map(|(key, value)| (key.to_owned(), SecretString::new(value.to_owned())))
        .collect()
}

/// Opens a Data Source and a session, which must fail; the Data Source opens,
/// since the driver connects only when a session opens.
fn session_error(library: &Arc<Library>, port: u16, oauth: bool) -> ConnectorError {
    let data_source = library
        .open_data_source(
            CONNECTOR,
            options(port, oauth),
            Some(Duration::from_secs(10)),
        )
        .expect("the Data Source opens without connecting");
    data_source
        .open_session()
        .err()
        .expect("no workspace answers")
}

/// Fails when any text of `error` holds a credential.
fn assert_reveals_no_secret(error: &ConnectorError) {
    let texts = [
        error.safe_message().to_owned(),
        error.data_source_message().unwrap_or_default().to_owned(),
        format!("{:?}", error.diagnostics()),
    ];
    for text in texts {
        for secret in [TOKEN, CLIENT_SECRET] {
            assert!(!text.contains(secret), "an error reveals a secret: {text}");
        }
    }
}

/// A port on `127.0.0.1` that nothing listens on.
fn closed_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

/// A port on `127.0.0.1` whose listener closes every connection at once.
fn closing_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for connection in listener.incoming() {
            drop(connection);
        }
    });
    port
}

#[test]
fn a_closed_port_is_a_connection_error() {
    let Some(library) = library() else { return };
    for oauth in [false, true] {
        let error = session_error(&library, closed_port(), oauth);
        assert_eq!(error.kind(), ErrorKind::Connection, "{error:?}");
        assert!(error.retryable(), "{error:?}");
        assert_reveals_no_secret(&error);
    }
}

#[test]
fn a_connection_closed_at_once_is_a_connection_error() {
    let Some(library) = library() else { return };
    for oauth in [false, true] {
        let error = session_error(&library, closing_port(), oauth);
        assert_eq!(error.kind(), ErrorKind::Connection, "{error:?}");
        assert!(error.retryable(), "{error:?}");
        assert_reveals_no_secret(&error);
    }
}
