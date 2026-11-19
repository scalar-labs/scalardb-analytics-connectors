//! The fixture's tables in DynamoDB Local and the administrative client that
//! prepares and changes them.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use aws_sdk_dynamodb::{
    Client,
    config::{BehaviorVersion, Credentials, Region},
    primitives::Blob,
    types::{
        AttributeDefinition, AttributeValue, BillingMode, KeySchemaElement, KeyType,
        ScalarAttributeType,
    },
};
use aws_smithy_http_client::{
    Builder,
    tls::{Provider, rustls_provider::CryptoMode},
};

/// The partition key of every item of `texts` and `blobs`, so that a `Query`
/// returns them in sort key order.
pub const PARTITION: &str = "p";

/// The sort keys of `texts`, in the byte order of their UTF-8 encodings: the
/// shortest key, the longest DynamoDB allows (1024 bytes), case, trailing
/// space, and two-, three-, and four-byte characters.
pub fn text_keys() -> Vec<String> {
    vec![
        "A".to_owned(),
        "a".to_owned(),
        "trailing ".to_owned(),
        "z".repeat(1024),
        "\u{e9}".to_owned(),
        "\u{65e5}\u{672c}\u{8a9e}".to_owned(),
        "\u{1f600}".to_owned(),
    ]
}

/// The sort keys of `blobs`, in unsigned byte order: the shortest key, a
/// zero byte, bytes on both sides of the sign bit, and the longest DynamoDB
/// allows (1024 bytes).
pub fn blob_keys() -> Vec<Vec<u8>> {
    vec![
        vec![0x00],
        vec![0x00, 0x00],
        vec![0x01],
        vec![0x7f],
        vec![0x80],
        vec![0xff; 1024],
    ]
}

/// The one item of `documents`, with a value of every DynamoDB type.
fn document() -> HashMap<String, AttributeValue> {
    let s = |value: &str| AttributeValue::S(value.to_owned());
    let n = |value: &str| AttributeValue::N(value.to_owned());
    HashMap::from([
        ("pk".to_owned(), s("doc")),
        ("s".to_owned(), s("")),
        ("n".to_owned(), n("-12345678901234567890123456789012345678")),
        ("f".to_owned(), n("1.5")),
        ("b".to_owned(), AttributeValue::B(Blob::new([0x00, 0xff]))),
        ("t".to_owned(), AttributeValue::Bool(true)),
        ("z".to_owned(), AttributeValue::Null(true)),
        ("ss".to_owned(), AttributeValue::Ss(vec!["a".to_owned()])),
        ("ns".to_owned(), AttributeValue::Ns(vec!["1".to_owned()])),
        ("bs".to_owned(), AttributeValue::Bs(vec![Blob::new([0x01])])),
        (
            "l".to_owned(),
            AttributeValue::L(vec![
                s("x"),
                n("2"),
                AttributeValue::L(Vec::new()),
                AttributeValue::M(HashMap::new()),
            ]),
        ),
        (
            "m".to_owned(),
            AttributeValue::M(HashMap::from([
                ("k".to_owned(), s("v")),
                ("a".to_owned(), AttributeValue::Bool(false)),
            ])),
        ),
    ])
}

/// The `item` text of [`document`].
pub const DOCUMENT_JSON: &str = concat!(
    r#"{"b":{"B":"AP8="},"bs":{"BS":["AQ=="]},"f":{"N":"1.5"},"#,
    r#""l":{"L":[{"S":"x"},{"N":"2"},{"L":[]},{"M":{}}]},"#,
    r#""m":{"M":{"a":{"BOOL":false},"k":{"S":"v"}}},"#,
    r#""n":{"N":"-12345678901234567890123456789012345678"},"ns":{"NS":["1"]},"#,
    r#""pk":{"S":"doc"},"s":{"S":""},"ss":{"SS":["a"]},"t":{"BOOL":true},"z":{"NULL":true}}"#,
);

/// Tables keyed by one string attribute `pk`, each holding the item `a`, in
/// the order they are created.
///
/// No two names differ only in case, which DynamoDB Local, backed by SQLite,
/// whose table names ignore case, may not keep apart.
pub const STRING_KEYED: [&str; 8] = [
    "cased",
    "locked",
    "slow",
    "endless",
    "blocked",
    "drift_type",
    "drift_dropped",
    "drift_same",
];

/// The administrative client, which reaches DynamoDB Local directly.
pub struct Admin {
    client: Client,
}

impl Admin {
    pub fn new(endpoint: &str) -> Self {
        let config = aws_sdk_dynamodb::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .endpoint_url(endpoint)
            .credentials_provider(Credentials::new(
                "admin",
                "conformance-admin-secret",
                None,
                None,
                "conformance",
            ))
            .http_client(
                Builder::new()
                    .tls_provider(Provider::Rustls(CryptoMode::Ring))
                    .build_https(),
            )
            .build();
        Self {
            client: Client::from_conf(config),
        }
    }

    /// Deletes every table, then creates the fixture's tables and items.
    pub async fn prepare(&self) {
        let names = self
            .client
            .list_tables()
            .send()
            .await
            .expect("DynamoDB Local lists its tables")
            .table_names
            .unwrap_or_default();
        for name in names {
            self.delete(&name).await;
        }
        let string = ScalarAttributeType::S;
        self.create(
            "texts",
            ("pk", string.clone()),
            Some(("sk", string.clone())),
        )
        .await;
        for key in text_keys() {
            self.put(
                "texts",
                HashMap::from([pk(PARTITION), ("sk".to_owned(), AttributeValue::S(key))]),
            )
            .await;
        }
        self.create(
            "blobs",
            ("pk", string.clone()),
            Some(("sk", ScalarAttributeType::B)),
        )
        .await;
        for key in blob_keys() {
            self.put(
                "blobs",
                HashMap::from([
                    pk(PARTITION),
                    ("sk".to_owned(), AttributeValue::B(Blob::new(key))),
                ]),
            )
            .await;
        }
        self.create("documents", ("pk", string.clone()), None).await;
        self.put("documents", document()).await;
        self.create("numbers", ("id", ScalarAttributeType::N), None)
            .await;
        self.put(
            "numbers",
            HashMap::from([("id".to_owned(), AttributeValue::N("1".to_owned()))]),
        )
        .await;
        for name in STRING_KEYED {
            self.create(name, ("pk", string.clone()), None).await;
            self.put(name, HashMap::from([pk("a")])).await;
        }
    }

    /// The `TableId` DynamoDB Local reports for `name`.
    pub async fn table_id(&self, name: &str) -> Option<String> {
        self.client
            .describe_table()
            .table_name(name)
            .send()
            .await
            .expect("DynamoDB Local describes the table")
            .table
            .and_then(|table| table.table_id)
    }

    /// Deletes `name`, if it exists, and waits until it is gone.
    pub async fn delete(&self, name: &str) {
        let _ = self.client.delete_table().table_name(name).send().await;
        let deadline = Instant::now() + Duration::from_secs(30);
        while self
            .client
            .describe_table()
            .table_name(name)
            .send()
            .await
            .is_ok()
        {
            assert!(
                Instant::now() < deadline,
                "the table {name} was not deleted"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Creates `name` keyed by `partition` and `sort`, billed per request.
    pub async fn create(
        &self,
        name: &str,
        partition: (&str, ScalarAttributeType),
        sort: Option<(&str, ScalarAttributeType)>,
    ) {
        let mut request = self
            .client
            .create_table()
            .table_name(name)
            .billing_mode(BillingMode::PayPerRequest);
        for ((attribute, attribute_type), key_type) in
            std::iter::once((partition, KeyType::Hash)).chain(sort.map(|key| (key, KeyType::Range)))
        {
            request = request
                .attribute_definitions(
                    AttributeDefinition::builder()
                        .attribute_name(attribute)
                        .attribute_type(attribute_type)
                        .build()
                        .unwrap(),
                )
                .key_schema(
                    KeySchemaElement::builder()
                        .attribute_name(attribute)
                        .key_type(key_type)
                        .build()
                        .unwrap(),
                );
        }
        request
            .send()
            .await
            .unwrap_or_else(|error| panic!("DynamoDB Local creates {name}: {error:?}"));
    }

    pub async fn put(&self, name: &str, item: HashMap<String, AttributeValue>) {
        self.client
            .put_item()
            .table_name(name)
            .set_item(Some(item))
            .send()
            .await
            .unwrap_or_else(|error| panic!("DynamoDB Local stores an item in {name}: {error:?}"));
    }
}

fn pk(value: &str) -> (String, AttributeValue) {
    ("pk".to_owned(), AttributeValue::S(value.to_owned()))
}
