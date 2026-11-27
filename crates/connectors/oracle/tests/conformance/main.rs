//! The connector conformance suite against Oracle.
//!
//! The fixture prepares the `CONFORMANCE` and `CONFORMANCE_OTHER` schemas
//! and two users in the pluggable database the tests use, and states what the
//! contract expects of the Oracle connector there.

#[path = "../support/mod.rs"]
mod support;

mod metadata;
mod reads;

use std::{env, path::PathBuf};

use connector_oracle::{CONNECTOR, OracleConnector};
use connector_sdk::{
    LibrarySettings, SecretString, TableReference,
    conformance::{
        self, DriftCase, ErrorCase, Fixture, Interruption, MetadataCase, ReadCase,
        plan::{ReadPlan, decimal},
    },
};
use substrait::proto::Plan;

use support::{SERVICE_NAME, server};

const SCHEMA: &str = "CONFORMANCE";
const OTHER_SCHEMA: &str = "CONFORMANCE_OTHER";

/// The user the suite reads every fixture object as: it may read any table.
const READER: (&str, &str) = ("CONFORMANCE_READER", "Conformance-reader-secret1");

/// A user that may see only some fixture tables, and read fewer. Its
/// password has characters the connection string grammar gives meaning.
const OUTSIDER: (&str, &str) = ("CONFORMANCE_OUTSIDER", "Conformance={outsider} secret1");

/// The fixture's users, as an SQL list.
const USERS: &str = "'CONFORMANCE_READER', 'CONFORMANCE_OUTSIDER'";

#[test]
fn the_oracle_connector_meets_the_contract() {
    conformance::run(&OracleFixture);
}

struct OracleFixture;

/// Connection options for `user`.
fn options((user, password): (&str, &str)) -> Vec<(String, SecretString)> {
    let port = server().port.to_string();
    [
        ("host", "127.0.0.1"),
        ("port", port.as_str()),
        ("service_name", SERVICE_NAME),
        ("user", user),
        ("password", password),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), SecretString::new(value.to_owned())))
    .collect()
}

fn reference(namespace: &str, name: &str) -> TableReference {
    TableReference::new(vec![namespace.to_owned(), name.to_owned()]).unwrap()
}

/// Runs `sql` as `SYSTEM` and returns the single value it prints.
fn scalar(sql: &str) -> String {
    server().sqlplus(sql).trim().to_owned()
}

impl Fixture for OracleFixture {
    type Connector = OracleConnector;
    type MetadataBlock = ();

    fn connector_name(&self) -> &str {
        CONNECTOR
    }

    fn library_settings(&self) -> LibrarySettings {
        let directory = env::var_os("ODBC_DRIVER_DIRECTORY")
            .expect("ODBC_DRIVER_DIRECTORY locates the ODBC drivers; run inside `nix develop`");
        LibrarySettings {
            adbc_driver_directory: None,
            odbc_driver_directory: Some(PathBuf::from(directory)),
        }
    }

    fn options(&self) -> Vec<(String, SecretString)> {
        options(READER)
    }

    fn prepare(&self) {
        server().sqlplus(include_str!("setup.sql"));
    }

    fn metadata_cases(&self) -> Vec<MetadataCase> {
        metadata::cases()
    }

    fn read_cases(&self) -> Vec<ReadCase> {
        reads::read_cases()
    }

    fn drift_cases(&self) -> Vec<DriftCase<'_>> {
        reads::drift_cases()
    }

    fn error_cases(&self) -> Vec<ErrorCase> {
        reads::error_cases()
    }

    fn waiting_read(&self) -> Plan {
        ReadPlan::new([SCHEMA, "SLOW"], vec![("N", decimal(10, 0, true))]).build()
    }

    fn long_read(&self) -> Plan {
        ReadPlan::new([SCHEMA, "ENDLESS"], vec![("N", decimal(10, 0, true))]).build()
    }

    /// The data dictionary views are read consistently without locks, so no
    /// change at Oracle makes reading them wait.
    fn block_metadata(&self) -> Option<(TableReference, ())> {
        None
    }

    fn source_is_idle(&self) -> bool {
        scalar(&format!(
            "SELECT COUNT(*) FROM v$session WHERE username IN ({USERS}) AND status = 'ACTIVE';"
        )) == "0"
    }

    fn known_interruption_deviations(&self) -> Vec<(Interruption, &'static str)> {
        Vec::new()
    }

    /// Kills the fixture users' sessions. A session that ended meanwhile
    /// (ORA-00030) or that Oracle marks to end once it can (ORA-00031) is
    /// ended.
    fn break_connections(&self) {
        server().sqlplus(&format!(
            "BEGIN\n\
             FOR s IN (SELECT sid, serial# AS serial FROM v$session WHERE username IN ({USERS})) LOOP\n\
             BEGIN\n\
             EXECUTE IMMEDIATE 'ALTER SYSTEM KILL SESSION ''' || s.sid || ',' || s.serial \
             || ''' IMMEDIATE';\n\
             EXCEPTION WHEN OTHERS THEN IF SQLCODE NOT IN (-30, -31) THEN RAISE; END IF;\n\
             END;\n\
             END LOOP;\n\
             END;\n/"
        ));
    }
}
