//! The client against a mock Salesforce, through its public API.

use std::{pin::pin, sync::Arc, time::Duration};

use arrow_array::{Array, Int32Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use connector_salesforce_client::{Client, ClientConfig, Credentials, Error, Secret, Url};
use futures_util::StreamExt;
use ring::signature::{KeyPair, RSA_PKCS1_2048_8192_SHA256, RsaKeyPair, UnparsedPublicKey};
use rustls_pki_types::{PrivateKeyDer, pem::PemObject};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{body_string_contains, header, method, path, query_param},
};

const API: &str = "/services/data/v62.0";
const TEST_KEY: &str = include_str!("fixtures/jwt-test-key.pem");
const REFRESH_TOKEN: &str = "refresh-token-value";
const SOQL: &str = "SELECT Id, Name FROM Account WHERE Name = 'secret literal'";

fn refresh_token_client(server: &MockServer) -> Client {
    Client::new(ClientConfig {
        login_url: Url::parse(&server.uri()).unwrap(),
        api_version: "62.0".to_owned(),
        credentials: Credentials::RefreshToken {
            client_id: "client-id".to_owned(),
            client_secret: Some(Secret::new("client-secret-value")),
            refresh_token: Secret::new(REFRESH_TOKEN),
        },
        connect_timeout: None,
    })
    .unwrap()
}

/// Issues `access_token` for the instance at `instance_url`.
fn token_response(access_token: &str, instance_url: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "access_token": access_token,
        "instance_url": instance_url,
        "token_type": "Bearer",
    }))
}

async fn mount_token(server: &MockServer, access_token: &str) {
    Mock::given(method("POST"))
        .and(path("/services/oauth2/token"))
        .respond_with(token_response(access_token, &server.uri()))
        .mount(server)
        .await;
}

fn account_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("Id", DataType::Utf8, false),
        Field::new("NumberOfEmployees", DataType::Int32, true),
    ]))
}

fn account(id: &str, employees: Option<i32>) -> serde_json::Value {
    json!({
        "attributes": {"type": "Account", "url": format!("{API}/sobjects/Account/{id}")},
        "Id": id,
        "NumberOfEmployees": employees,
    })
}

async fn collect(client: &Client, soql: &str) -> Result<Vec<RecordBatch>, Error> {
    let stream = client.query(soql, account_schema())?;
    let mut stream = pin!(stream);
    let mut batches = Vec::new();
    while let Some(batch) = stream.next().await {
        batches.push(batch?);
    }
    Ok(batches)
}

#[tokio::test]
async fn refresh_token_flow_authorizes_describe_requests() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/oauth2/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains(format!(
            "refresh_token={REFRESH_TOKEN}"
        )))
        .and(body_string_contains("client_secret=client-secret-value"))
        .respond_with(token_response("access-1", &server.uri()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/sobjects")))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "encoding": "UTF-8",
            "sobjects": [{"name": "Account", "label": "Account", "queryable": true, "custom": false}],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/sobjects/Account/describe")))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "Account",
            "fields": [
                {"name": "AnnualRevenue", "type": "currency", "precision": 18, "scale": 0,
                 "nillable": true, "soapType": "xsd:double"},
                {"name": "Future__c", "type": "a type the client has never seen"},
            ],
        })))
        .mount(&server)
        .await;
    let client = refresh_token_client(&server);

    let global = client.describe_global().await.unwrap();
    assert_eq!(global.sobjects[0].name, "Account");
    assert!(global.sobjects[0].queryable);

    let account = client.describe("Account").await.unwrap();
    assert_eq!(account.fields[0].field_type, "currency");
    assert_eq!(account.fields[0].precision, 18);
    assert_eq!(
        account.fields[1].field_type,
        "a type the client has never seen"
    );
}

#[tokio::test]
async fn jwt_bearer_flow_sends_a_signed_assertion() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/oauth2/token"))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer",
        ))
        .respond_with(token_response("access-jwt", &server.uri()))
        .mount(&server)
        .await;
    let client = Client::new(ClientConfig {
        login_url: Url::parse(&server.uri()).unwrap(),
        api_version: "62.0".to_owned(),
        credentials: Credentials::JwtBearer {
            client_id: "client-id".to_owned(),
            username: "user@example.com".to_owned(),
            audience: "https://login.salesforce.com".to_owned(),
            private_key: Secret::new(TEST_KEY),
        },
        connect_timeout: None,
    })
    .unwrap();

    client.authenticate().await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let assertion = form_value(&requests[0], "assertion");
    let parts: Vec<&str> = assertion.split('.').collect();
    assert_eq!(parts.len(), 3);
    let header: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    assert_eq!(header, json!({"alg": "RS256"}));
    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["iss"], "client-id");
    assert_eq!(claims["sub"], "user@example.com");
    assert_eq!(claims["aud"], "https://login.salesforce.com");
    assert!(claims["exp"].as_u64().is_some());
    let PrivateKeyDer::Pkcs8(der) = PrivateKeyDer::from_pem_slice(TEST_KEY.as_bytes()).unwrap()
    else {
        panic!("the test key is PKCS #8");
    };
    let key = RsaKeyPair::from_pkcs8(der.secret_pkcs8_der()).unwrap();
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key.public_key().as_ref())
        .verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
        )
        .expect("the assertion is signed with the configured key");
}

#[tokio::test]
async fn an_unusable_private_key_is_rejected_when_the_client_is_created() {
    let error = Client::new(ClientConfig {
        login_url: Url::parse("https://login.salesforce.com").unwrap(),
        api_version: "62.0".to_owned(),
        credentials: Credentials::JwtBearer {
            client_id: "client-id".to_owned(),
            username: "user@example.com".to_owned(),
            audience: "https://login.salesforce.com".to_owned(),
            private_key: Secret::new("not a key"),
        },
        connect_timeout: None,
    })
    .unwrap_err();
    assert!(matches!(error, Error::Config(_)));
}

#[tokio::test]
async fn a_query_follows_its_pages_and_sends_nothing_before_the_first_poll() {
    let server = MockServer::start().await;
    mount_token(&server, "access-1").await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/query")))
        .and(query_param("q", SOQL))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalSize": 3,
            "done": false,
            "nextRecordsUrl": format!("{API}/query/01gxx0000000001-2000"),
            "records": [account("001A", Some(10)), account("001B", None)],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/query/01gxx0000000001-2000")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalSize": 3,
            "done": true,
            "records": [account("001C", Some(30))],
        })))
        .mount(&server)
        .await;
    let client = refresh_token_client(&server);

    let stream = client.query(SOQL, account_schema()).unwrap();
    assert!(server.received_requests().await.unwrap().is_empty());
    let batches: Vec<RecordBatch> = stream.map(Result::unwrap).collect().await;

    assert_eq!(batches.len(), 2);
    let ids = |batch: &RecordBatch| {
        let ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        (0..ids.len())
            .map(|row| ids.value(row).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&batches[0]), ["001A", "001B"]);
    assert_eq!(ids(&batches[1]), ["001C"]);
    let employees = batches[0]
        .column(1)
        .as_any()
        .downcast_ref::<Int32Array>()
        .unwrap();
    assert_eq!(employees.value(0), 10);
    assert!(employees.is_null(1));
}

#[tokio::test]
async fn a_next_page_on_another_origin_is_not_requested() {
    let server = MockServer::start().await;
    mount_token(&server, "access-1").await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/query")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "done": false,
            "nextRecordsUrl": "https://elsewhere.example.com/services/data/v62.0/query/x-2000",
            "records": [account("001A", Some(1))],
        })))
        .mount(&server)
        .await;
    let client = refresh_token_client(&server);

    let stream = client.query(SOQL, account_schema()).unwrap();
    let mut stream = pin!(stream);
    assert!(stream.next().await.unwrap().is_ok());
    assert!(matches!(
        stream.next().await.unwrap(),
        Err(Error::InvalidResponse(_))
    ));
}

#[tokio::test]
async fn an_expired_token_is_renewed_once_and_the_request_sent_again() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/oauth2/token"))
        .respond_with(token_response("access-1", &server.uri()))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/services/oauth2/token"))
        .respond_with(token_response("access-2", &server.uri()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!([
            {"message": "Session expired or invalid", "errorCode": "INVALID_SESSION_ID"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/query")))
        .and(header("authorization", "Bearer access-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "done": true,
            "records": [account("001A", Some(1))],
        })))
        .mount(&server)
        .await;
    let client = refresh_token_client(&server);

    let batches = collect(&client, SOQL).await.unwrap();

    assert_eq!(batches[0].num_rows(), 1);
}

#[tokio::test]
async fn a_token_rejected_again_after_renewal_is_reported() {
    let server = MockServer::start().await;
    mount_token(&server, "access-1").await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!([
            {"message": "Session expired or invalid", "errorCode": "INVALID_SESSION_ID"}
        ])))
        .mount(&server)
        .await;
    let client = refresh_token_client(&server);

    let error = collect(&client, SOQL).await.unwrap_err();

    assert!(
        matches!(&error, Error::Api { status: 401, errors } if errors[0].error_code == "INVALID_SESSION_ID")
    );
    let token_requests = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .count();
    assert_eq!(token_requests, 2);
}

#[tokio::test]
async fn a_server_error_is_retried() {
    let server = MockServer::start().await;
    mount_token(&server, "access-1").await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/query")))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/query")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "done": true,
            "records": [account("001A", Some(1))],
        })))
        .mount(&server)
        .await;
    let client = refresh_token_client(&server);

    let batches = collect(&client, SOQL).await.unwrap();

    assert_eq!(batches[0].num_rows(), 1);
}

#[tokio::test]
async fn an_internal_server_error_is_not_retried() {
    let server = MockServer::start().await;
    mount_token(&server, "access-1").await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/query")))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!([
            {"message": "An unexpected error occurred", "errorCode": "UNKNOWN_EXCEPTION"}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let client = refresh_token_client(&server);

    let error = collect(&client, SOQL).await.unwrap_err();

    assert!(matches!(error, Error::Api { status: 500, .. }));
}

#[tokio::test]
async fn an_api_error_is_structured_and_keeps_query_text_out_of_its_formatting() {
    let server = MockServer::start().await;
    mount_token(&server, "access-1").await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/query")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!([{
            "message": "\nName = 'secret literal'\n^\nERROR at Row:1:Column:40\nunexpected token",
            "errorCode": "MALFORMED_QUERY",
        }])))
        .mount(&server)
        .await;
    let client = refresh_token_client(&server);

    let error = collect(&client, SOQL).await.unwrap_err();

    let Error::Api { status, errors } = &error else {
        panic!("expected an API error");
    };
    assert_eq!(*status, 400);
    assert_eq!(errors[0].error_code, "MALFORMED_QUERY");
    assert!(errors[0].message.as_str().contains("secret literal"));
    for text in [format!("{error:?}"), error.to_string()] {
        assert!(!text.contains("secret literal"), "{text}");
        assert!(!text.contains("SELECT"), "{text}");
    }
}

#[tokio::test]
async fn an_authentication_failure_keeps_secrets_out_of_its_formatting() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/oauth2/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "expired access/refresh token",
        })))
        .mount(&server)
        .await;
    let client = refresh_token_client(&server);

    let error = client.authenticate().await.unwrap_err();

    assert!(
        matches!(&error, Error::Authentication { status: 400, error, .. } if error == "invalid_grant")
    );
    for text in [
        format!("{error:?}"),
        error.to_string(),
        format!("{client:?}"),
    ] {
        assert!(!text.contains(REFRESH_TOKEN), "{text}");
        assert!(!text.contains("client-secret-value"), "{text}");
    }
}

#[tokio::test]
async fn a_transport_failure_keeps_query_text_out_of_its_formatting() {
    let server = MockServer::start().await;
    // An instance that refuses connections.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let instance = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    Mock::given(method("POST"))
        .and(path("/services/oauth2/token"))
        .respond_with(token_response("access-1", &instance))
        .mount(&server)
        .await;
    let client = refresh_token_client(&server);

    let error = collect(&client, SOQL).await.unwrap_err();

    assert!(matches!(error, Error::Transport { .. }));
    for text in [format!("{error:?}"), error.to_string()] {
        assert!(!text.contains("secret"), "{text}");
        assert!(!text.contains("SELECT"), "{text}");
    }
}

#[tokio::test]
async fn dropping_a_query_closes_its_connection() {
    let server = MockServer::start().await;
    // An instance that accepts the query and never answers, then reports when
    // the client closes the connection.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let instance = format!("http://{}", listener.local_addr().unwrap());
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        while socket.read(&mut buffer).await.unwrap_or(0) > 0 {}
        let _ = socket.shutdown().await;
        let _ = closed_tx.send(());
    });
    Mock::given(method("POST"))
        .and(path("/services/oauth2/token"))
        .respond_with(token_response("access-1", &instance))
        .mount(&server)
        .await;
    let client = refresh_token_client(&server);

    let stream = client.query(SOQL, account_schema()).unwrap();
    let mut stream = Box::pin(stream);
    let first = tokio::time::timeout(Duration::from_millis(300), stream.next()).await;
    assert!(first.is_err(), "the instance never answers");
    drop(stream);

    tokio::time::timeout(Duration::from_secs(5), closed_rx)
        .await
        .expect("dropping the stream closes the connection")
        .unwrap();
}

fn form_value(request: &Request, name: &str) -> String {
    url::form_urlencoded::parse(&request.body)
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
}
