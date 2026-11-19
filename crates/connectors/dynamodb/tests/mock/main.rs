//! The connector against a mock DynamoDB endpoint.
//!
//! The mock answers the JSON requests the SDK sends with canned responses,
//! so these tests drive the real connector through the caller-facing runtime
//! and check what DynamoDB's answers become: catalog records, batches, and
//! error kinds. DynamoDB Local cannot answer with what these tests need most,
//! such as IAM denials, rejected credentials, and throttling.

mod connection;
mod metadata;
mod queries;
mod reads;

use std::{sync::Arc, time::Duration};

use connector_dynamodb::{CONNECTOR, DynamoDbConnector};
use connector_sdk::{
    ConnectorError, LibrarySettings, SecretString,
    runtime::{BuiltInConnector, DataSourceState, Library, SessionState},
};
use serde_json::{Value, json};
use tokio::runtime::Runtime;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, header, method},
};

/// The secret access key of every test; no error may reveal it.
const SECRET_ACCESS_KEY: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

/// A mock DynamoDB endpoint on its own runtime, apart from the connector's.
struct MockDynamoDb {
    runtime: Runtime,
    server: MockServer,
}

impl MockDynamoDb {
    /// Starts an endpoint whose `ListTables` lists no table, which every
    /// Data Source calls when it opens.
    fn start() -> Self {
        let runtime = Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        let mock = Self { runtime, server };
        mock.on("ListTables", None, ok(json!({ "TableNames": [] })));
        mock
    }

    fn url(&self) -> String {
        self.server.uri()
    }

    /// Answers the `operation` requests whose JSON body contains `body` with
    /// `response`. A mock with a body takes precedence over one without.
    fn on(&self, operation: &str, body: Option<Value>, response: ResponseTemplate) {
        let mock = Mock::given(method("POST")).and(header(
            "x-amz-target",
            format!("DynamoDB_20120810.{operation}").as_str(),
        ));
        let mock = match body {
            Some(body) => mock
                .and(body_partial_json(body))
                .respond_with(response)
                .with_priority(1),
            None => mock.respond_with(response).with_priority(2),
        };
        self.runtime.block_on(mock.mount(&self.server));
    }

    /// Forgets every mock and received request, then lists no table again.
    fn reset(&self) {
        self.runtime.block_on(self.server.reset());
        self.on("ListTables", None, ok(json!({ "TableNames": [] })));
    }

    /// The operation and JSON body of every request received, in order.
    fn requests(&self) -> Vec<(String, Value)> {
        self.runtime
            .block_on(self.server.received_requests())
            .unwrap_or_default()
            .into_iter()
            .map(|request| {
                let target = request
                    .headers
                    .get("x-amz-target")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .trim_start_matches("DynamoDB_20120810.")
                    .to_owned();
                (target, serde_json::from_slice(&request.body).unwrap())
            })
            .collect()
    }

    /// The JSON bodies of the `operation` requests received, in order.
    fn bodies(&self, operation: &str) -> Vec<Value> {
        self.requests()
            .into_iter()
            .filter(|(target, _)| target == operation)
            .map(|(_, body)| body)
            .collect()
    }
}

/// A successful response with a JSON body.
fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.to_string(), "application/x-amz-json-1.0")
}

/// A DynamoDB error response.
fn error(status: u16, code: &str, message: &str) -> ResponseTemplate {
    let body = json!({
        "__type": format!("com.amazonaws.dynamodb.v20120810#{code}"),
        "message": message,
    });
    ResponseTemplate::new(status).set_body_raw(body.to_string(), "application/x-amz-json-1.0")
}

/// The connection options with static credentials for `endpoint`.
fn options(endpoint: &str) -> Vec<(String, SecretString)> {
    [
        ("region", "us-east-1"),
        ("endpoint", endpoint),
        ("access_key_id", "AKIDEXAMPLE"),
        ("secret_access_key", SECRET_ACCESS_KEY),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), SecretString::new(value.to_owned())))
    .collect()
}

fn library() -> Arc<Library> {
    Library::initialize(
        &[BuiltInConnector::of::<DynamoDbConnector>()],
        &LibrarySettings::default(),
        "test",
    )
    .unwrap()
}

fn open(
    options: Vec<(String, SecretString)>,
    connection_timeout: Option<Duration>,
) -> Result<Arc<DataSourceState>, ConnectorError> {
    library().open_data_source(CONNECTOR, options, connection_timeout)
}

/// A session on a Data Source of `mock`.
fn session(mock: &MockDynamoDb) -> Arc<SessionState> {
    open(options(&mock.url()), Some(Duration::from_secs(10)))
        .unwrap()
        .open_session()
        .unwrap()
}

/// Asserts that no part of `error` reveals the secret access key.
fn assert_no_secret(error: &ConnectorError) {
    for text in [
        format!("{error}"),
        format!("{error:?}"),
        error.data_source_message().unwrap_or_default().to_owned(),
    ] {
        assert!(!text.contains(SECRET_ACCESS_KEY), "{text}");
    }
}
