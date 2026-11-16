//! The connector conformance suite against a MySQL server.
//!
//! The fixture prepares the `conformance`, `conformance_other`, and
//! `Conformance` databases and two accounts in the server the other tests
//! use, and states what the contract expects of the MySQL connector there.

#[path = "../support/mod.rs"]
mod support;

mod metadata;
mod reads;

use std::path::PathBuf;

use connector_mysql::{CONNECTOR, MysqlConnector};
use connector_sdk::{
    LibrarySettings, SecretString, TableReference,
    conformance::{
        self, DriftCase, ErrorCase, Fixture, Interruption, MetadataCase, ReadCase,
        plan::{ReadPlan, i64},
    },
};
use substrait::proto::Plan;

use support::server;

const SCHEMA: &str = "conformance";
const OTHER_SCHEMA: &str = "conformance_other";
/// A database whose name differs from [`SCHEMA`] only in case.
const UPPER_SCHEMA: &str = "Conformance";

/// The account the suite reads every fixture object as.
const READER: (&str, &str) = ("conformance_reader", "conformance-reader-secret");

/// An account that may read only some fixture tables and columns.
const OUTSIDER: (&str, &str) = ("conformance_outsider", "conformance-outsider-secret");

/// The fixture's accounts, as an SQL list.
const ACCOUNTS: &str = "'conformance_reader', 'conformance_outsider'";

#[test]
fn the_mysql_connector_meets_the_contract() {
    conformance::run(&MysqlFixture);
}

struct MysqlFixture;

/// Connection options for `account`, mapping `tinyint(1)` to `Boolean` so
/// that the reads cover its conversion.
fn options(account: (&str, &str)) -> Vec<(String, SecretString)> {
    let mut options = byte_options(account);
    options.push((
        "tinyint1_as_boolean".to_owned(),
        SecretString::new("true".to_owned()),
    ));
    options
}

/// Connection options for `account` with the default mapping, under which
/// `tinyint(1)` is `Byte`.
fn byte_options(account: (&str, &str)) -> Vec<(String, SecretString)> {
    support::options(account.0, account.1)
}

fn reference(schema: &str, name: &str) -> TableReference {
    TableReference::new(vec![schema.to_owned(), name.to_owned()]).unwrap()
}

impl Fixture for MysqlFixture {
    type Connector = MysqlConnector;
    type MetadataBlock = ();

    fn connector_name(&self) -> &str {
        CONNECTOR
    }

    fn library_settings(&self) -> LibrarySettings {
        let directory = std::env::var_os("ADBC_DRIVER_DIRECTORY")
            .expect("ADBC_DRIVER_DIRECTORY locates the ADBC drivers; run inside `nix develop`");
        LibrarySettings {
            adbc_driver_directory: Some(PathBuf::from(directory)),
            odbc_driver_directory: None,
        }
    }

    fn options(&self) -> Vec<(String, SecretString)> {
        options(READER)
    }

    fn prepare(&self) {
        server().mysql(include_str!("setup.sql"));
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
        ReadPlan::new([SCHEMA, "slow"], vec![("n", i64(false))]).build()
    }

    fn long_read(&self) -> Plan {
        ReadPlan::new([SCHEMA, "endless"], vec![("n", i64(false))]).build()
    }

    /// MySQL reads `information_schema` from its data dictionary without
    /// locks: neither table locks nor pending DDL on the table or its database
    /// make the connector's metadata reads wait.
    fn block_metadata(&self) -> Option<(TableReference, ())> {
        None
    }

    fn source_is_idle(&self) -> bool {
        server()
            .mysql(&format!(
                "SELECT COUNT(*) FROM information_schema.PROCESSLIST \
                 WHERE USER IN ({ACCOUNTS}) AND COMMAND = 'Query'"
            ))
            .trim()
            == "0"
    }

    fn known_interruption_deviations(&self) -> Vec<(Interruption, &'static str)> {
        Vec::new()
    }

    fn break_connections(&self) {
        let ids = server().mysql(&format!(
            "SELECT ID FROM information_schema.PROCESSLIST WHERE USER IN ({ACCOUNTS})"
        ));
        let kills: String = ids
            .split_whitespace()
            .map(|id| format!("KILL {id};"))
            .collect();
        if !kills.is_empty() {
            // A connection may end before its KILL arrives.
            let _ = server().try_mysql(&kills);
        }
    }
}
