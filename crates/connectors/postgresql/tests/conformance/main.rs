//! The connector conformance suite against a PostgreSQL server.
//!
//! The fixture prepares the `conformance` and `conformance_other` schemas and
//! two roles in the server the other tests use, and states what the contract
//! expects of the PostgreSQL connector there.

#[path = "../support/mod.rs"]
mod support;

mod metadata;
mod reads;

use std::{
    path::PathBuf,
    process::Child,
    thread,
    time::{Duration, Instant},
};

use connector_postgresql::{CONNECTOR, PostgresConnector};
use connector_sdk::{
    LibrarySettings, SecretString, TableReference,
    conformance::{
        self, DriftCase, ErrorCase, Fixture, Interruption, MetadataCase, ReadCase,
        plan::{ReadPlan, i32},
    },
};
use substrait::proto::Plan;

use support::server;

const SCHEMA: &str = "conformance";
const OTHER_SCHEMA: &str = "conformance_other";

/// The role the suite reads every fixture object as.
const READER: (&str, &str) = ("conformance_reader", "conformance-reader-secret");

/// A role that may read only some fixture tables.
const OUTSIDER: (&str, &str) = ("conformance_outsider", "conformance-outsider-secret");

/// The application name of the session that blocks metadata requests.
const BLOCKER: &str = "conformance_blocker";

#[test]
fn the_postgresql_connector_meets_the_contract() {
    conformance::run(&PostgresFixture);
}

struct PostgresFixture;

/// Connection options for `role`.
fn options((user, password): (&str, &str)) -> Vec<(String, SecretString)> {
    let port = server().port.to_string();
    [
        ("host", "127.0.0.1"),
        ("port", port.as_str()),
        ("database", "postgres"),
        ("user", user),
        ("password", password),
        ("sslmode", "disable"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), SecretString::new(value.to_owned())))
    .collect()
}

fn reference(namespace: &str, name: &str) -> TableReference {
    TableReference::new(vec![namespace.to_owned(), name.to_owned()]).unwrap()
}

/// The fixture's roles, as an SQL list.
const ROLES: &str = "'conformance_reader', 'conformance_outsider'";

impl Fixture for PostgresFixture {
    type Connector = PostgresConnector;
    type MetadataBlock = MetadataBlock;

    fn connector_name(&self) -> &str {
        CONNECTOR
    }

    fn library_settings(&self) -> LibrarySettings {
        let directory = std::env::var_os("ADBC_DRIVER_DIRECTORY")
            .expect("ADBC_DRIVER_DIRECTORY locates the ADBC drivers; run inside `nix develop`");
        LibrarySettings {
            adbc_driver_directory: Some(PathBuf::from(directory)),
        }
    }

    fn options(&self) -> Vec<(String, SecretString)> {
        options(READER)
    }

    fn prepare(&self) {
        server().psql(include_str!("setup.sql"));
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
        ReadPlan::new([SCHEMA, "slow"], vec![("n", i32(true))]).build()
    }

    fn long_read(&self) -> Plan {
        ReadPlan::new([SCHEMA, "endless"], vec![("n", i32(true))]).build()
    }

    /// Holds an exclusive lock on `pg_enum`, which reading the enum column of
    /// `Mixed Case` waits for.
    fn block_metadata(&self) -> (TableReference, MetadataBlock) {
        let session = server().spawn_psql(
            BLOCKER,
            "BEGIN; LOCK TABLE pg_catalog.pg_enum IN ACCESS EXCLUSIVE MODE; SELECT pg_sleep(120);",
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while server()
            .psql(&format!(
                "SELECT count(*) FROM pg_catalog.pg_locks l \
                 JOIN pg_catalog.pg_stat_activity a ON a.pid = l.pid \
                 WHERE a.application_name = '{BLOCKER}' AND l.granted \
                   AND l.relation = 'pg_catalog.pg_enum'::regclass"
            ))
            .trim()
            == "0"
        {
            assert!(Instant::now() < deadline, "the metadata block holds");
            thread::sleep(Duration::from_millis(100));
        }
        (reference(SCHEMA, "Mixed Case"), MetadataBlock(session))
    }

    fn source_is_idle(&self) -> bool {
        server()
            .psql(&format!(
                "SELECT count(*) FROM pg_catalog.pg_stat_activity \
                 WHERE usename IN ({ROLES}) AND state = 'active'"
            ))
            .trim()
            == "0"
    }

    fn known_interruption_deviations(&self) -> Vec<(Interruption, &'static str)> {
        vec![(
            Interruption::BrokenConnection,
            "the Rust ADBC driver manager does not import a stream error's SQLSTATE, so a terminated connection is `data-source`",
        )]
    }

    fn break_connections(&self) {
        server().psql(&format!(
            "SELECT pg_terminate_backend(pid) FROM pg_catalog.pg_stat_activity \
             WHERE usename IN ({ROLES})"
        ));
    }
}

/// The `psql` session holding the metadata block; ending it releases the lock.
struct MetadataBlock(Child);

impl Drop for MetadataBlock {
    fn drop(&mut self) {
        server().psql(&format!(
            "SELECT pg_terminate_backend(pid) FROM pg_catalog.pg_stat_activity \
             WHERE application_name = '{BLOCKER}'"
        ));
        // The process exits once its backend is gone; its status says nothing more.
        let _ = self.0.wait();
    }
}
