//! The SOQL comparison semantics the mock org models
//! (`tests/conformance/soql.rs`), checked by sending SOQL through the client.
//!
//! The connector keeps comparisons local where it understands Salesforce to
//! compare differently from the offered filter, so reads through it cannot
//! show those semantics; these queries do.

use std::{pin::pin, sync::Arc};

use arrow_array::{Array, Int64Array, StringArray};
use arrow_schema::{DataType, Field, Schema};
use connector_salesforce::API_VERSION;
use connector_salesforce_client::{Client, ClientConfig, Credentials, Secret, Url};
use futures_util::StreamExt;

use super::{OBJECT, env_name, env_option};

pub fn check() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime starts");
    runtime.block_on(async {
        let client = client();
        let min_id = select(&client, "Id", "Key__c = 'min'").await;
        let [min_id] = min_id.as_slice() else {
            panic!("one record has Key__c min: {min_id:?}");
        };
        let cases = [
            (
                "text equality ignores case",
                "Name = 'TRAILING SPACE'".to_owned(),
                vec!["max"],
            ),
            (
                "text equality keeps a literal's trailing spaces",
                "Name = 'trailing space '".to_owned(),
                vec![],
            ),
            (
                "email equality ignores case",
                "Email__c = 'SOMEONE@EXAMPLE.COM'".to_owned(),
                vec!["max", "min"],
            ),
            (
                "a date-time literal after the last instant Salesforce holds",
                "DateTime__c > 4000-12-31T12:00:00Z".to_owned(),
                vec![],
            ),
            (
                "not-equal is true for null",
                "Percent5_2__c != 0".to_owned(),
                vec!["max", "min", "null"],
            ),
            (
                "an ordering comparison is false for null",
                "Percent5_2__c < 1000".to_owned(),
                vec!["max", "min"],
            ),
            (
                "an ID equals its 15-character form",
                format!("Id = '{}'", &min_id[..15]),
                vec!["min"],
            ),
            (
                "Boolean equality",
                "Checkbox__c = false".to_owned(),
                vec!["max", "null"],
            ),
        ];
        for (name, condition, expected) in cases {
            let mut actual = select(&client, "Key__c", &condition).await;
            actual.sort();
            assert_eq!(actual, expected, "{name}: WHERE {condition}");
        }
    });
}

/// `field` of the records of `ConnectorTest__c` that match `condition`.
async fn select(client: &Client, field: &str, condition: &str) -> Vec<String> {
    let schema = Arc::new(Schema::new(vec![Field::new(field, DataType::Utf8, true)]));
    let soql = format!("SELECT {field} FROM {OBJECT} WHERE {condition}");
    let mut batches = pin!(
        client
            .query(soql, schema)
            .unwrap_or_else(|error| panic!("WHERE {condition}: {error}"))
    );
    let mut values = Vec::new();
    while let Some(batch) = batches.next().await {
        let batch = batch.unwrap_or_else(|error| panic!("WHERE {condition}: {error}"));
        let column = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("a text column");
        values.extend(column.iter().map(|value| value.unwrap_or("").to_owned()));
    }
    values
}

/// How many records `object` holds, as Salesforce counts them in one
/// aggregate query, which returns a single row however many records there
/// are.
pub fn count(object: &str) -> i64 {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime starts");
    runtime.block_on(async {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "total",
            DataType::Int64,
            false,
        )]));
        let soql = format!("SELECT COUNT(Id) total FROM {object}");
        let mut batches = pin!(
            client()
                .query(soql, schema)
                .unwrap_or_else(|error| panic!("counting {object}: {error}"))
        );
        let batch = batches
            .next()
            .await
            .expect("an aggregate query returns a row")
            .unwrap_or_else(|error| panic!("counting {object}: {error}"));
        batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("the count is a long")
            .value(0)
    })
}

/// A client with the connection options from the environment, configured as
/// the connector configures its own.
fn client() -> Client {
    let var = env_option;
    let required = |key: &str| var(key).unwrap_or_else(|| panic!("{} is not set", env_name(key)));
    let login_url = var("login_url").unwrap_or_else(|| "https://login.salesforce.com".to_owned());
    let client_id = required("client_id");
    let credentials = match required("auth_flow").as_str() {
        "refresh_token" => Credentials::RefreshToken {
            client_id,
            client_secret: var("client_secret").map(Secret::new),
            refresh_token: Secret::new(required("refresh_token")),
        },
        "jwt_bearer" => Credentials::JwtBearer {
            client_id,
            username: required("username"),
            audience: var("jwt_audience")
                .unwrap_or_else(|| login_url.trim_end_matches('/').to_owned()),
            private_key: Secret::new(required("private_key")),
        },
        other => panic!("unknown auth flow {other}"),
    };
    Client::new(ClientConfig {
        login_url: Url::parse(&login_url).expect("the login URL is a URL"),
        api_version: API_VERSION.to_owned(),
        credentials,
        connect_timeout: None,
    })
    .expect("the client is configured")
}
