//! The Databricks workspace the suite runs against, from environment
//! variables, and the fixture's administrative access to it through the REST
//! API.
//!
//! The fixture prepares and changes its objects with the SQL Statement
//! Execution API on the same SQL warehouse the connector reads, and observes
//! the queries running there through the query history API. Both use the
//! connector's credentials, so the principal needs `USE CATALOG` and
//! `CREATE SCHEMA` on the catalog, "Can Use" on the warehouse, and, for the
//! query history, to see its own queries.

use std::{
    sync::{Mutex, PoisonError},
    thread,
    time::{Duration, Instant},
};

use connector_sdk::SecretString;
use serde_json::{Value, json};
use tokio::runtime::Runtime;

/// The variables that name the workspace; a credential is either
/// `DATABRICKS_TEST_TOKEN` or both `DATABRICKS_TEST_CLIENT_ID` and
/// `DATABRICKS_TEST_CLIENT_SECRET`.
const TARGET_VARIABLES: [&str; 3] = [
    "DATABRICKS_TEST_HOST",
    "DATABRICKS_TEST_HTTP_PATH",
    "DATABRICKS_TEST_CATALOG",
];

/// How the fixture authenticates, the same way as the connector.
pub enum Credential {
    AccessToken(String),
    OAuth {
        client_id: String,
        client_secret: String,
    },
}

/// The workspace, its SQL warehouse, and the pre-made catalog the fixture
/// creates its schemas in.
pub struct Workspace {
    pub host: String,
    pub http_path: String,
    pub catalog: String,
    pub credential: Credential,
    runtime: Runtime,
    client: reqwest::Client,
    /// An OAuth access token and when it was fetched.
    token: Mutex<Option<(String, Instant)>>,
}

impl Workspace {
    /// The workspace the environment names, or the names of the variables
    /// that are missing.
    pub fn from_env() -> Result<Self, Vec<&'static str>> {
        let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let mut missing: Vec<&'static str> = TARGET_VARIABLES
            .into_iter()
            .filter(|name| variable(name).is_none())
            .collect();
        let credential = match (
            variable("DATABRICKS_TEST_TOKEN"),
            variable("DATABRICKS_TEST_CLIENT_ID"),
            variable("DATABRICKS_TEST_CLIENT_SECRET"),
        ) {
            (Some(token), _, _) => Some(Credential::AccessToken(token)),
            (None, Some(client_id), Some(client_secret)) => Some(Credential::OAuth {
                client_id,
                client_secret,
            }),
            _ => {
                missing.push(
                    "DATABRICKS_TEST_TOKEN, or DATABRICKS_TEST_CLIENT_ID and \
                     DATABRICKS_TEST_CLIENT_SECRET",
                );
                None
            }
        };
        let (Some(credential), true) = (credential, missing.is_empty()) else {
            return Err(missing);
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_io()
            .enable_time()
            .build()
            .expect("the fixture's runtime starts");
        // The connector reaches the workspace through the fixture's proxy;
        // the fixture's own requests go directly.
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("the HTTP client builds");
        Ok(Self {
            host: variable(TARGET_VARIABLES[0]).unwrap(),
            http_path: variable(TARGET_VARIABLES[1]).unwrap(),
            catalog: variable(TARGET_VARIABLES[2]).unwrap(),
            credential,
            runtime,
            client,
            token: Mutex::new(None),
        })
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// The connector's options for the workspace with `credential`, listing
    /// only the fixture's catalog.
    pub fn options_with(&self, credential: &Credential) -> Vec<(String, SecretString)> {
        let mut options = vec![
            ("host", self.host.clone()),
            ("http_path", self.http_path.clone()),
            ("catalog", self.catalog.clone()),
        ];
        match credential {
            Credential::AccessToken(token) => options.push(("access_token", token.clone())),
            Credential::OAuth {
                client_id,
                client_secret,
            } => options.extend([
                ("client_id", client_id.clone()),
                ("client_secret", client_secret.clone()),
            ]),
        }
        options
            .into_iter()
            .map(|(key, value)| (key.to_owned(), SecretString::new(value)))
            .collect()
    }

    /// The SQL warehouse's ID, the last segment of its HTTP path.
    fn warehouse_id(&self) -> &str {
        self.http_path.rsplit('/').next().unwrap_or_default()
    }

    /// A bearer token for the REST API: the personal access token, or an
    /// OAuth token for the service principal, fetched again after 30 minutes.
    fn bearer(&self) -> String {
        let (client_id, client_secret) = match &self.credential {
            Credential::AccessToken(token) => return token.clone(),
            Credential::OAuth {
                client_id,
                client_secret,
            } => (client_id, client_secret),
        };
        let mut token = self.token.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((token, fetched)) = token.as_ref()
            && fetched.elapsed() < Duration::from_secs(1800)
        {
            return token.clone();
        }
        let response = self.send(
            self.client
                .post(format!("https://{}/oidc/v1/token", self.host))
                .basic_auth(client_id, Some(client_secret))
                .header("content-type", "application/x-www-form-urlencoded")
                .body("grant_type=client_credentials&scope=all-apis"),
        );
        let fetched = response["access_token"]
            .as_str()
            .expect("the token endpoint returns an access token")
            .to_owned();
        *token = Some((fetched.clone(), Instant::now()));
        fetched
    }

    /// Sends a request and returns its JSON body, panicking on a failure
    /// with the response, which holds no credential.
    fn send(&self, request: reqwest::RequestBuilder) -> Value {
        self.runtime.block_on(async {
            let response = request.send().await.expect("the workspace answers");
            let status = response.status();
            let body = response.bytes().await.expect("the response body arrives");
            assert!(
                status.is_success(),
                "the workspace answered {status}: {}",
                String::from_utf8_lossy(&body)
            );
            serde_json::from_slice(&body).expect("the response is JSON")
        })
    }

    fn api(&self, request: reqwest::RequestBuilder) -> Value {
        self.send(request.bearer_auth(self.bearer()))
    }

    /// Runs one SQL statement on the warehouse to its end, panicking when it
    /// fails, and returns its rows as text.
    pub fn sql(&self, statement: &str) -> Vec<Vec<Option<String>>> {
        self.try_sql(statement)
            .unwrap_or_else(|error| panic!("{error}\n{statement}"))
    }

    /// Runs one SQL statement on the warehouse to its end, returning its
    /// rows as text or the workspace's error.
    pub fn try_sql(&self, statement: &str) -> Result<Vec<Vec<Option<String>>>, String> {
        let mut response = self.api(
            self.client
                .post(format!("https://{}/api/2.0/sql/statements/", self.host))
                .header("content-type", "application/json")
                .body(
                    json!({
                        "warehouse_id": self.warehouse_id(),
                        "statement": statement,
                        "wait_timeout": "50s",
                        "on_wait_timeout": "CONTINUE",
                        "disposition": "INLINE",
                        "format": "JSON_ARRAY",
                    })
                    .to_string(),
                ),
        );
        // A stopped warehouse starts first, which can take minutes.
        let deadline = Instant::now() + Duration::from_secs(900);
        loop {
            match response["status"]["state"].as_str() {
                Some("SUCCEEDED") => break,
                Some("PENDING" | "RUNNING") if Instant::now() < deadline => {
                    thread::sleep(Duration::from_secs(2));
                    let id = response["statement_id"].as_str().unwrap_or_default();
                    response = self.api(
                        self.client
                            .get(format!("https://{}/api/2.0/sql/statements/{id}", self.host)),
                    );
                }
                _ => return Err(response["status"].to_string()),
            }
        }
        let rows = response["result"]["data_array"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(rows
            .into_iter()
            .map(|row| {
                row.as_array()
                    .into_iter()
                    .flatten()
                    .map(|value| value.as_str().map(str::to_owned))
                    .collect()
            })
            .collect())
    }

    /// How many queries on the warehouse are queued or running whose text
    /// contains `marker`, as the query history shows them.
    pub fn running_queries(&self, marker: &str) -> usize {
        let response = self.api(self.client.get(format!(
            "https://{}/api/2.0/sql/history/queries?filter_by.warehouse_ids={}\
             &filter_by.statuses=QUEUED&filter_by.statuses=RUNNING&max_results=1000",
            self.host,
            self.warehouse_id()
        )));
        response["res"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|query| {
                query["query_text"]
                    .as_str()
                    .is_some_and(|text| text.contains(marker))
            })
            .count()
    }
}
