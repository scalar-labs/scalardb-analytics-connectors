//! The connector conformance suite against SQL Server.
//!
//! The fixture prepares the `scalardb_conformance` database with the
//! `conformance` and `conformance_other` schemas and two logins in the server
//! the tests use, and states what the contract expects of the SQL Server
//! through ODBC connector there.

#[path = "../support/mod.rs"]
mod support;

mod metadata;
mod reads;

use std::{
    env,
    path::PathBuf,
    process::Child,
    thread,
    time::{Duration, Instant},
};

use connector_sdk::{
    LibrarySettings, SecretString, TableReference,
    conformance::{
        self, DriftCase, ErrorCase, Fixture, Interruption, MetadataCase, ReadCase,
        plan::{ReadPlan, i32},
    },
};
use connector_sqlserver_odbc::{CONNECTOR, SqlServerConnector};
use substrait::proto::Plan;

use support::server;

const DATABASE: &str = "scalardb_conformance";
const SCHEMA: &str = "conformance";
const OTHER_SCHEMA: &str = "conformance_other";

/// The login the suite reads every fixture object as: it has `VIEW
/// DEFINITION` and `SELECT` on the database.
const READER: (&str, &str) = ("conformance_reader", "Conformance-reader-secret1");

/// A login that may see and read only some fixture tables.
const OUTSIDER: (&str, &str) = ("conformance_outsider", "Conformance-outsider-secret1");

/// A login with `VIEW DEFINITION` on the database that is denied it on one
/// schema.
const DENIED: (&str, &str) = ("conformance_denied", "Conformance-denied-secret1");

/// The workstation name of the session that blocks metadata requests.
const BLOCKER: &str = "conformance_blocker";

/// The fixture's logins, as an SQL list.
const LOGINS: &str = "N'conformance_reader', N'conformance_outsider', N'conformance_denied'";

#[test]
fn the_sqlserver_odbc_connector_meets_the_contract() {
    conformance::run(&SqlServerFixture);
}

struct SqlServerFixture;

/// Connection options for `login`.
fn options((user, password): (&str, &str)) -> Vec<(String, SecretString)> {
    let address = format!("127.0.0.1,{}", server().port);
    [
        ("server", address.as_str()),
        ("database", DATABASE),
        ("user", user),
        ("password", password),
        // The container's certificate is self-signed.
        ("trust_server_certificate", "yes"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), SecretString::new(value.to_owned())))
    .collect()
}

fn reference(namespace: &str, name: &str) -> TableReference {
    TableReference::new(vec![namespace.to_owned(), name.to_owned()]).unwrap()
}

/// Runs `sql` in the fixture's database as `sa` and returns its output.
fn sqlcmd(sql: &str) -> String {
    server().sqlcmd(&format!("USE {DATABASE};\n{sql}"))
}

/// The single value a query prints.
fn scalar(sql: &str) -> String {
    sqlcmd(sql)
        .lines()
        .filter(|line| !line.starts_with("Changed database context"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

impl Fixture for SqlServerFixture {
    type Connector = SqlServerConnector;
    type MetadataBlock = MetadataBlock;

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
        server().sqlcmd(include_str!("setup.sql"));
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

    /// Holds a schema modification lock on `blocked` in an open transaction,
    /// which reading its catalog rows waits for.
    fn block_metadata(&self) -> Option<(TableReference, MetadataBlock)> {
        let session = server().spawn_sqlcmd(
            BLOCKER,
            &format!(
                "USE {DATABASE};\nBEGIN TRANSACTION;\n\
                 ALTER TABLE conformance.blocked ADD pending int;\n\
                 WAITFOR DELAY '00:02:00';"
            ),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while scalar(&format!(
            "SELECT COUNT(*) FROM sys.dm_tran_locks l \
             JOIN sys.dm_exec_sessions s ON s.session_id = l.request_session_id \
             WHERE s.host_name = N'{BLOCKER}' AND l.request_mode = N'Sch-M' \
               AND l.request_status = N'GRANT'"
        )) == "0"
        {
            assert!(Instant::now() < deadline, "the metadata block holds");
            thread::sleep(Duration::from_millis(100));
        }
        Some((reference(SCHEMA, "blocked"), MetadataBlock(session)))
    }

    fn source_is_idle(&self) -> bool {
        scalar(&format!(
            "SELECT COUNT(*) FROM sys.dm_exec_requests r \
             JOIN sys.dm_exec_sessions s ON s.session_id = r.session_id \
             WHERE s.login_name IN ({LOGINS})"
        )) == "0"
    }

    fn known_interruption_deviations(&self) -> Vec<(Interruption, &'static str)> {
        Vec::new()
    }

    fn break_connections(&self) {
        kill_sessions(&format!("login_name IN ({LOGINS})"));
    }
}

/// Ends every session `condition` selects in `sys.dm_exec_sessions`.
fn kill_sessions(condition: &str) {
    sqlcmd(&format!(
        "DECLARE @kill nvarchar(max) = N''; \
         SELECT @kill += N'KILL ' + CAST(session_id AS nvarchar(10)) + N';' \
         FROM sys.dm_exec_sessions WHERE {condition} AND session_id <> @@SPID; \
         EXEC (@kill);"
    ));
}

/// The `sqlcmd` session holding the metadata block; ending it rolls the
/// schema change back and releases the lock.
struct MetadataBlock(Child);

impl Drop for MetadataBlock {
    fn drop(&mut self) {
        kill_sessions(&format!("host_name = N'{BLOCKER}'"));
        // The process exits once its session is gone; its status says nothing more.
        let _ = self.0.wait();
    }
}
