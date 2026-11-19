//! The connector conformance suite against DynamoDB Local.
//!
//! The fixture prepares its tables in the DynamoDB Local server of
//! `tests/support`, and the suite reaches it through the proxy of
//! `proxy.rs`, which adds the IAM denials, waiting requests, and broken
//! connections that DynamoDB Local cannot produce. The fixture's own
//! administrative access goes to DynamoDB Local directly.

#[path = "../support/mod.rs"]
mod support;

mod metadata;
mod proxy;
mod reads;
mod tables;

use connector_dynamodb::{CONNECTOR, DynamoDbConnector, NAMESPACE};
use connector_sdk::{
    LibrarySettings, SecretString, TableReference,
    conformance::{
        self, DriftCase, ErrorCase, Fixture, Interruption, MetadataCase, ReadCase,
        plan::{ReadPlan, text},
    },
};
use substrait::proto::Plan;
use tokio::runtime::Runtime;

use proxy::{BLOCKED, ENDLESS, Policy, Proxy, SLOW};
use tables::Admin;

#[test]
fn the_dynamodb_connector_meets_the_contract() {
    conformance::run(&DynamoDbFixture::start());
}

struct DynamoDbFixture {
    /// Runs the proxy and the administrative client.
    runtime: Runtime,
    proxy: Proxy,
    admin: Admin,
}

impl DynamoDbFixture {
    fn start() -> Self {
        let local = support::local();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("the fixture's runtime starts");
        let proxy = Proxy::start(&runtime, local.address);
        let admin = Admin::new(&local.endpoint());
        Self {
            runtime,
            proxy,
            admin,
        }
    }

    /// Connection options of the access key of `policy`, through the proxy.
    fn options(&self, policy: Policy) -> Vec<(String, SecretString)> {
        options(&self.proxy.url, policy)
    }

    /// Runs an administrative request against DynamoDB Local.
    fn admin<T>(&self, request: impl AsyncFnOnce(&Admin) -> T) -> T {
        self.runtime.block_on(request(&self.admin))
    }
}

/// Connection options of the access key of `policy` for `endpoint`.
fn options(endpoint: &str, policy: Policy) -> Vec<(String, SecretString)> {
    let secret = format!("conformance-{}-secret", policy.key_id());
    [
        ("region", "us-east-1"),
        ("endpoint", endpoint),
        ("access_key_id", policy.key_id()),
        ("secret_access_key", secret.as_str()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), SecretString::new(value.to_owned())))
    .collect()
}

fn reference(name: &str) -> TableReference {
    TableReference::new(vec![NAMESPACE.to_owned(), name.to_owned()]).unwrap()
}

impl Fixture for DynamoDbFixture {
    type Connector = DynamoDbConnector;
    type MetadataBlock = proxy::Block;

    fn connector_name(&self) -> &str {
        CONNECTOR
    }

    fn library_settings(&self) -> LibrarySettings {
        LibrarySettings::default()
    }

    fn options(&self) -> Vec<(String, SecretString)> {
        self.options(Policy::Reader)
    }

    fn prepare(&self) {
        self.admin(async |admin| admin.prepare().await);
    }

    fn metadata_cases(&self) -> Vec<MetadataCase> {
        metadata::cases(self)
    }

    fn read_cases(&self) -> Vec<ReadCase> {
        reads::read_cases()
    }

    fn drift_cases(&self) -> Vec<DriftCase<'_>> {
        reads::drift_cases(self)
    }

    fn error_cases(&self) -> Vec<ErrorCase> {
        reads::error_cases(self)
    }

    fn waiting_read(&self) -> Plan {
        ReadPlan::new([NAMESPACE, SLOW], vec![("pk", text(false))]).build()
    }

    fn long_read(&self) -> Plan {
        ReadPlan::new([NAMESPACE, ENDLESS], vec![("pk", text(false))]).build()
    }

    /// Makes `DescribeTable` of `blocked` wait at the proxy.
    fn block_metadata(&self) -> Option<(TableReference, proxy::Block)> {
        Some((reference(BLOCKED), self.proxy.block()))
    }

    fn source_is_idle(&self) -> bool {
        self.proxy.running() == 0
    }

    fn break_connections(&self) {
        self.proxy.break_connections();
    }

    fn known_interruption_deviations(&self) -> Vec<(Interruption, &'static str)> {
        Vec::new()
    }
}
