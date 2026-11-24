//! The Salesforce connector against a real org, checking the model of
//! Salesforce that the conformance suite's mock org follows.
//!
//! The tests run only when `SALESFORCE_TEST_AUTH_FLOW` is set, and skip
//! otherwise. Each connection option is read from `SALESFORCE_TEST_` followed
//! by its key in upper case, such as `SALESFORCE_TEST_REFRESH_TOKEN`. The org
//! must be prepared by `run.sh prepare`, which deploys `sfdx/`, loads its
//! records, and grants the user the `ConnectorTest` permission set.
//!
//! The tests only read. They call the connector through the SDK runtime, as
//! the conformance suite does, and send SOQL through the client to check the
//! comparison semantics the connector relies on when it keeps a comparison
//! local.

mod bulk;
mod errors;
mod metadata;
mod reads;
mod soql;
mod standard;

use std::{sync::Arc, time::Duration};

use connector_salesforce::{CONNECTOR, SalesforceConnector};
use connector_sdk::{
    LibrarySettings, SecretString, TableReference,
    runtime::{BuiltInConnector, Library, SessionState},
};

const NAMESPACE: &str = "sobjects";
/// The object `sfdx/` deploys.
const OBJECT: &str = "ConnectorTest__c";
const ENV_PREFIX: &str = "SALESFORCE_TEST_";
/// The connector options the environment can set.
const OPTIONS: [&str; 8] = [
    "login_url",
    "auth_flow",
    "client_id",
    "client_secret",
    "refresh_token",
    "username",
    "private_key",
    "jwt_audience",
];
/// Bounds opening a Data Source, which obtains an access token over the
/// network.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

#[test]
fn the_salesforce_connector_reads_a_real_org() {
    if std::env::var_os(env_name("auth_flow")).is_none() {
        eprintln!("skipped: set {ENV_PREFIX}AUTH_FLOW and the other options to test a real org");
        return;
    }
    let org = Org::open();
    metadata::check(&org);
    reads::check(&org);
    bulk::check(&org);
    standard::check(&org);
    errors::check(&org);
    soql::check();
}

/// The connection options the environment sets.
fn options() -> Vec<(String, SecretString)> {
    OPTIONS
        .iter()
        .filter_map(|key| Some(((*key).to_owned(), SecretString::new(env_option(key)?))))
        .collect()
}

/// An option from the environment; an empty value, such as the client secret
/// of a client that has none, leaves the option unset.
fn env_option(key: &str) -> Option<String> {
    std::env::var(env_name(key))
        .ok()
        .filter(|value| !value.is_empty())
}

fn env_name(key: &str) -> String {
    format!("{ENV_PREFIX}{}", key.to_uppercase())
}

/// The library with only the Salesforce connector.
struct Org {
    library: Arc<Library>,
}

impl Org {
    fn open() -> Self {
        let library = Library::initialize(
            &[BuiltInConnector::of::<SalesforceConnector>()],
            &LibrarySettings::default(),
            "org-test",
        )
        .expect("the library initializes");
        Self { library }
    }

    /// Opens a Data Source and a session as the org's user.
    fn session(&self) -> Arc<SessionState> {
        self.library
            .open_data_source(CONNECTOR, options(), Some(CONNECTION_TIMEOUT))
            .and_then(|data_source| data_source.open_session())
            .unwrap_or_else(|error| panic!("a session opens with the org's user: {error}"))
    }
}

fn reference(name: &str) -> TableReference {
    TableReference::new(vec![NAMESPACE.to_owned(), name.to_owned()]).unwrap()
}
