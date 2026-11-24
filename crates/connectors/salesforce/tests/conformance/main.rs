//! The connector conformance suite against a mock Salesforce org.
//!
//! The mock serves the OAuth token endpoint, describeGlobal, describe, and
//! queries over HTTP, so the connector runs unchanged against it. It models
//! Salesforce's behavior as `soql.rs` states; the opt-in tests against a real
//! org check that model.

mod metadata;
mod mock;
mod reads;
mod soql;

use std::collections::BTreeMap;

use connector_salesforce::{CONNECTOR, SalesforceConnector};
use connector_sdk::{
    LibrarySettings, SecretString, TableReference,
    conformance::{
        self, DriftCase, ErrorCase, Fixture, Interruption, MetadataCase, ReadCase,
        plan::{ReadPlan, text},
    },
};
use substrait::proto::Plan;

use mock::{Field, Mock, Object, QueryBehavior, Record, User};

const NAMESPACE: &str = "sobjects";

/// The refresh token of the user the suite reads every object as.
const READER: &str = "reader-refresh-token";
/// A user who may see only some objects and fields.
const OUTSIDER: &str = "outsider-refresh-token";
/// A user whose access tokens Salesforce rejects.
const REVOKED: &str = "revoked-refresh-token";

#[test]
fn the_salesforce_connector_meets_the_contract() {
    conformance::run(&SalesforceFixture {
        mock: Mock::start(objects(), users()),
    });
}

struct SalesforceFixture {
    mock: Mock,
}

impl SalesforceFixture {
    /// Connection options of the user with `refresh_token`.
    fn options(&self, refresh_token: &str) -> Vec<(String, SecretString)> {
        [
            ("login_url", self.mock.url.as_str()),
            ("auth_flow", "refresh_token"),
            ("client_id", "conformance-client"),
            ("client_secret", "conformance-client-secret"),
            ("refresh_token", refresh_token),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), SecretString::new(value.to_owned())))
        .collect()
    }
}

fn reference(name: &str) -> TableReference {
    TableReference::new(vec![NAMESPACE.to_owned(), name.to_owned()]).unwrap()
}

/// A record from field names and values as JSON text.
fn record(values: &[(&'static str, &str)]) -> Record {
    values
        .iter()
        .map(|(field, value)| (*field, (*value).to_owned()))
        .collect()
}

fn id(n: u32) -> String {
    format!("\"a0100000000000{n}AAA\"")
}

/// The fields of `Values__c`: one per mapping rule of the type mapping, in
/// describe order.
fn value_fields() -> Vec<Field> {
    vec![
        Field::id(),
        Field::new("Name", "string"),
        Field::new("LongTextArea__c", "textarea").unfilterable(),
        Field::new("Picklist__c", "picklist"),
        Field::new("MultiPicklist__c", "multipicklist"),
        Field::new("Combobox__c", "combobox"),
        Field::new("Email__c", "email"),
        Field::new("Phone__c", "phone"),
        Field::new("Url__c", "url"),
        Field::new("Encryptedstring__c", "encryptedstring").unfilterable(),
        Field::new("Reference__c", "reference").reference(&["Account"]),
        Field::new("Checkbox__c", "boolean").required(),
        Field::new("Int__c", "int"),
        Field::new("Long__c", "long"),
        Field::numeric("Number18_2__c", "double", 18, 2),
        Field::numeric("Percent5_2__c", "percent", 5, 2),
        Field::numeric("Currency18_2__c", "currency", 18, 2),
        Field::new("Date__c", "date"),
        Field::new("DateTime__c", "datetime"),
        Field::new("Time__c", "time"),
    ]
}

/// The minimum, the maximum, and NULL of each field of `Values__c`, as a
/// real org stores them: without trailing spaces in text, with email
/// addresses in lower case, with numbers that a double holds exactly, and
/// with date-times from 1700-01-01T00:00:00Z to 4000-12-31T00:00:00Z.
fn value_records() -> Vec<Record> {
    let (one, two, three) = (id(1), id(2), id(3));
    vec![
        record(&[
            ("Id", &one),
            ("Name", "\"\u{dc}n\u{ef}code \u{2713}\""),
            ("LongTextArea__c", "\"line one\\nline two\""),
            ("Picklist__c", "\"Prospecting\""),
            ("MultiPicklist__c", "\"Red;Blue\""),
            ("Combobox__c", "\"Other\""),
            ("Email__c", "\"someone@example.com\""),
            ("Phone__c", "\"+81 3-0000-0000\""),
            ("Url__c", "\"https://example.com\""),
            ("Encryptedstring__c", "\"****1234\""),
            ("Reference__c", "\"0010000000000AAAAA\""),
            ("Checkbox__c", "true"),
            ("Int__c", "-2147483648"),
            ("Long__c", "-9223372036854775808"),
            ("Number18_2__c", "-99999999999999.98"),
            ("Percent5_2__c", "-999.99"),
            ("Currency18_2__c", "0.01"),
            ("Date__c", "\"1700-01-01\""),
            ("DateTime__c", "\"1700-01-01T00:00:00.000+0000\""),
            ("Time__c", "\"00:00:00.000Z\""),
        ]),
        record(&[
            ("Id", &two),
            ("Name", "\"Mixed Case\""),
            ("LongTextArea__c", "\"\""),
            ("Picklist__c", "\"Closed Won\""),
            ("MultiPicklist__c", "\"Green\""),
            ("Combobox__c", "\"Partner\""),
            ("Email__c", "\"someone@example.org\""),
            ("Phone__c", "\"0\""),
            ("Url__c", "\"http://example.org/a?b=c\""),
            ("Encryptedstring__c", "\"****9999\""),
            ("Reference__c", "\"0010000000000BAAAA\""),
            ("Checkbox__c", "false"),
            ("Int__c", "2147483647"),
            ("Long__c", "9223372036854775807"),
            ("Number18_2__c", "99999999999999.98"),
            ("Percent5_2__c", "999.99"),
            ("Currency18_2__c", "1234.5"),
            ("Date__c", "\"4000-12-31\""),
            ("DateTime__c", "\"4000-12-31T00:00:00.000+0000\""),
            ("Time__c", "\"23:59:59.999Z\""),
        ]),
        record(&[("Id", &three), ("Checkbox__c", "false")]),
    ]
}

/// The objects of the org as each run starts.
fn objects() -> BTreeMap<&'static str, Object> {
    let id_only = || vec![Field::id()];
    let mut objects = BTreeMap::new();
    objects.insert("Values__c", Object::new(value_fields(), value_records()));
    objects.insert(
        "Special__c",
        Object::new(
            vec![
                Field::id(),
                Field::numeric("Amount__c", "double", 18, 2),
                Field::new("Count__c", "int"),
            ],
            vec![
                record(&[("Id", &id(1)), ("Amount__c", "1.005"), ("Count__c", "1")]),
                record(&[("Id", &id(2)), ("Amount__c", "2.50"), ("Count__c", "2")]),
            ],
        ),
    );
    objects.insert(
        "Unmapped__c",
        Object::new(
            vec![
                Field::id(),
                Field::new("File__c", "base64"),
                Field::new("BillingAddress", "address"),
                Field::new("Spot__c", "location"),
                Field::new("Value__c", "anyType"),
                Field::new("Setting__c", "complexvalue"),
                Field::numeric("Unscaled__c", "double", 0, 0),
                Field::numeric("Huge__c", "double", 40, 2),
            ],
            Vec::new(),
        ),
    );
    objects.insert(
        "Account",
        Object::new(
            vec![Field::id(), Field::new("Name", "string").required()],
            Vec::new(),
        ),
    );
    objects.insert(
        "Contact",
        Object::new(
            vec![
                Field::id(),
                Field::new("LastName", "string").required(),
                Field::new("Email", "email").unique(),
                Field::new("AccountId", "reference").reference(&["Account"]),
                Field::new("WhoId", "reference").reference(&["Contact", "Lead"]),
            ],
            vec![record(&[
                ("Id", &id(1)),
                ("LastName", "\"Doe\""),
                ("Email", "\"doe@example.com\""),
            ])],
        ),
    );
    objects.insert(
        "Locked__c",
        Object::new(vec![Field::id(), Field::new("Name", "string")], Vec::new()),
    );
    objects.insert(
        "Restricted__c",
        Object::new(vec![Field::id(), Field::new("Name", "string")], Vec::new()),
    );
    for (name, field) in [
        ("DriftType__c", Field::numeric("Amount__c", "double", 18, 2)),
        ("DriftField__c", Field::new("Gone__c", "string")),
        ("DriftObject__c", Field::new("Name", "string")),
        ("DriftNullable__c", Field::new("Count__c", "int").required()),
        ("DriftRequired__c", Field::new("Count__c", "int")),
    ] {
        objects.insert(
            name,
            Object::new(
                vec![Field::id(), field],
                vec![record(&[
                    ("Id", &id(1)),
                    ("Amount__c", "1.00"),
                    ("Count__c", "1"),
                ])],
            ),
        );
    }
    let mut special = |name, query| {
        let mut object = Object::new(
            vec![Field::id(), Field::new("Name", "string")],
            vec![record(&[("Id", &id(1))])],
        );
        object.query = query;
        objects.insert(name, object);
    };
    special("Slow__c", QueryBehavior::Wait);
    special("Endless__c", QueryBehavior::Endless);
    special(
        "Limited__c",
        QueryBehavior::Fail {
            status: 403,
            code: "REQUEST_LIMIT_EXCEEDED",
        },
    );
    objects.insert("Blocked__c", Object::new(id_only(), Vec::new()));
    objects
}

fn users() -> Vec<User> {
    vec![
        User {
            refresh_token: READER,
            objects: None,
            hidden_fields: Vec::new(),
            denied: Vec::new(),
            revoked: false,
        },
        User {
            refresh_token: OUTSIDER,
            objects: Some(vec!["Account", "Contact", "Locked__c", "Values__c"]),
            hidden_fields: vec![("Contact", "Email")],
            denied: vec!["Locked__c"],
            revoked: false,
        },
        User {
            refresh_token: REVOKED,
            objects: None,
            hidden_fields: Vec::new(),
            denied: Vec::new(),
            revoked: true,
        },
    ]
}

impl Fixture for SalesforceFixture {
    type Connector = SalesforceConnector;
    type MetadataBlock = mock::Block;

    fn connector_name(&self) -> &str {
        CONNECTOR
    }

    fn library_settings(&self) -> LibrarySettings {
        LibrarySettings::default()
    }

    fn options(&self) -> Vec<(String, SecretString)> {
        self.options(READER)
    }

    fn prepare(&self) {
        self.mock.change(|org| {
            org.objects = objects();
            org.blocked.clear();
        });
    }

    fn metadata_cases(&self) -> Vec<MetadataCase> {
        metadata::cases(self)
    }

    fn read_cases(&self) -> Vec<ReadCase> {
        reads::read_cases()
    }

    fn drift_cases(&self) -> Vec<DriftCase<'_>> {
        reads::drift_cases(&self.mock)
    }

    fn error_cases(&self) -> Vec<ErrorCase> {
        reads::error_cases(self)
    }

    fn waiting_read(&self) -> Plan {
        ReadPlan::new([NAMESPACE, "Slow__c"], vec![("Id", text(false))]).build()
    }

    fn long_read(&self) -> Plan {
        ReadPlan::new([NAMESPACE, "Endless__c"], vec![("Id", text(false))]).build()
    }

    /// Makes describe requests for `Blocked__c` wait.
    fn block_metadata(&self) -> Option<(TableReference, mock::Block)> {
        Some((reference("Blocked__c"), self.mock.block("Blocked__c")))
    }

    fn source_is_idle(&self) -> bool {
        self.mock.running() == 0
    }

    fn break_connections(&self) {
        self.mock.break_connections();
    }

    fn known_interruption_deviations(&self) -> Vec<(Interruption, &'static str)> {
        Vec::new()
    }
}
