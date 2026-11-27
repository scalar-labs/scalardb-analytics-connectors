//! The connector conformance suite against a Databricks workspace.
//!
//! No container runs Databricks SQL, so the suite needs a workspace with a
//! SQL warehouse, named by the environment ([`workspace`]): without it the
//! test is skipped, saying which variables are missing. The fixture creates
//! two schemas with a unique suffix in the pre-made catalog
//! `DATABRICKS_TEST_CATALOG`, loads its tables there, and drops both schemas
//! when the test ends, whether it passes or not.
//!
//! The driver is a Go library, whose runtime reads the process environment
//! once, when it is loaded. The test therefore runs the suite in a child
//! process whose `HTTPS_PROXY` names the fixture's proxy ([`proxy`]), through
//! which the connector reaches the workspace and the fixture breaks its
//! connections.

mod metadata;
mod proxy;
mod reads;
mod workspace;

use std::{
    net::{Ipv4Addr, TcpListener},
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use connector_databricks::{CONNECTOR, DatabricksConnector};
use connector_sdk::{
    LibrarySettings, SecretString, TableReference,
    conformance::{
        self, DriftCase, ErrorCase, Fixture, Interruption, MetadataCase, ReadCase,
        plan::{ReadPlan, i64},
    },
};
use substrait::proto::Plan;

use proxy::Proxy;
use workspace::{Credential, Workspace};

/// The name of this test, which the child process runs alone.
const TEST: &str = "the_databricks_connector_meets_the_contract";

/// Set in the child process to the port its proxy listens on.
const PROXY_PORT: &str = "DATABRICKS_CONFORMANCE_PROXY_PORT";

#[test]
fn the_databricks_connector_meets_the_contract() {
    let workspace = match Workspace::from_env() {
        Ok(workspace) => workspace,
        Err(missing) => {
            eprintln!(
                "skipped: the conformance suite needs a Databricks workspace; set {}",
                missing.join("; ")
            );
            return;
        }
    };
    let Some(port) = std::env::var(PROXY_PORT)
        .ok()
        .and_then(|port| port.parse().ok())
    else {
        run_in_child();
        return;
    };
    let proxy = Proxy::start(workspace.runtime(), port);
    let fixture = DatabricksFixture::new(workspace, proxy);
    conformance::run(&fixture);
}

/// Runs this test again in a child process whose driver reaches the
/// workspace through a proxy on a free local port.
fn run_in_child() {
    let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .expect("a local port is free")
        .port();
    let proxy = format!("http://127.0.0.1:{port}");
    let status = Command::new(std::env::current_exe().expect("the test binary is known"))
        .args(["--exact", TEST, "--nocapture"])
        .env(PROXY_PORT, port.to_string())
        .env("HTTPS_PROXY", &proxy)
        .env("https_proxy", &proxy)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .status()
        .expect("the test binary runs");
    assert!(status.success(), "the conformance suite failed");
}

/// The fixture: the workspace, the proxy the connector reaches it through,
/// and the names of the schemas it creates.
pub struct DatabricksFixture {
    workspace: Workspace,
    proxy: Proxy,
    /// The schema most objects are in, unique to this run.
    schema: String,
    /// A second schema, for reads across namespaces.
    other_schema: String,
}

impl DatabricksFixture {
    fn new(workspace: Workspace, proxy: Proxy) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let schema = format!("conformance_{:x}_{}", nanos, std::process::id());
        let other_schema = format!("{schema}_other");
        Self {
            workspace,
            proxy,
            schema,
            other_schema,
        }
    }

    /// A table in `schema` of the fixture's catalog.
    pub fn reference(&self, schema: &str, name: &str) -> TableReference {
        TableReference::new(vec![
            self.workspace.catalog.clone(),
            schema.to_owned(),
            name.to_owned(),
        ])
        .unwrap()
    }

    /// The names of a table in the fixture's schema, for a read plan.
    pub fn table(&self, name: &str) -> [String; 3] {
        [
            self.workspace.catalog.clone(),
            self.schema.clone(),
            name.to_owned(),
        ]
    }

    /// The SQL name of `schema` in the fixture's catalog.
    fn qualified(&self, schema: &str) -> String {
        format!("`{}`.`{schema}`", self.workspace.catalog.replace('`', "``"))
    }

    /// Runs `sql` with `${schema}` and `${other}` naming the fixture's
    /// schemas.
    pub fn sql(&self, sql: &str) {
        self.workspace.sql(
            &sql.replace("${schema}", &self.qualified(&self.schema))
                .replace("${other}", &self.qualified(&self.other_schema)),
        );
    }

    /// The connector's options with the fixture's credential.
    pub fn credential_options(&self) -> Vec<(String, SecretString)> {
        self.workspace.options_with(&self.workspace.credential)
    }

    /// The connector's options with the fixture's credential made wrong: a
    /// token or OAuth secret the workspace rejects.
    pub fn rejected_options(&self) -> Vec<(String, SecretString)> {
        let wrong = match &self.workspace.credential {
            Credential::AccessToken(_) => {
                Credential::AccessToken("dapi-conformance-rejected-secret".to_owned())
            }
            Credential::OAuth { client_id, .. } => Credential::OAuth {
                client_id: client_id.clone(),
                client_secret: "conformance-rejected-secret".to_owned(),
            },
        };
        self.workspace.options_with(&wrong)
    }

    fn drop_schemas(&self) {
        for schema in [&self.schema, &self.other_schema] {
            let _ = self.workspace.try_sql(&format!(
                "DROP SCHEMA IF EXISTS {} CASCADE",
                self.qualified(schema)
            ));
        }
    }
}

/// Drops the fixture's schemas, also when the suite fails.
impl Drop for DatabricksFixture {
    fn drop(&mut self) {
        self.drop_schemas();
    }
}

impl Fixture for DatabricksFixture {
    type Connector = DatabricksConnector;
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
        self.credential_options()
    }

    /// Replaces the schemas and loads `setup.sql`, one statement at a time,
    /// since the Statement Execution API runs one per request.
    fn prepare(&self) {
        self.drop_schemas();
        for statement in include_str!("setup.sql").split(";\n") {
            let statement: String = statement
                .lines()
                .filter(|line| !line.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n");
            if !statement.trim().is_empty() {
                self.sql(&statement);
            }
        }
    }

    fn metadata_cases(&self) -> Vec<MetadataCase> {
        metadata::cases(self)
    }

    fn read_cases(&self) -> Vec<ReadCase> {
        reads::read_cases(self)
    }

    fn drift_cases(&self) -> Vec<DriftCase<'_>> {
        reads::drift_cases(self)
    }

    fn error_cases(&self) -> Vec<ErrorCase> {
        reads::error_cases(self)
    }

    /// A view that counts a hundred billion generated rows in one task,
    /// which takes minutes, and returns nothing before it ends.
    fn waiting_read(&self) -> Plan {
        ReadPlan::new(
            self.table("slow").each_ref().map(String::as_str),
            vec![("n", i64(false))],
        )
        .build()
    }

    /// A view of a hundred billion generated rows, whose first rows arrive
    /// while the warehouse produces the rest.
    fn long_read(&self) -> Plan {
        ReadPlan::new(
            self.table("endless").each_ref().map(String::as_str),
            vec![("n", i64(false))],
        )
        .build()
    }

    /// Unity Catalog's `information_schema` takes no locks that a reader
    /// could wait for, so nothing makes the connector's metadata reads wait.
    fn block_metadata(&self) -> Option<(TableReference, ())> {
        None
    }

    /// No query on the warehouse that names the fixture's schema is queued
    /// or running, by the query history.
    fn source_is_idle(&self) -> bool {
        self.workspace.running_queries(&self.schema) == 0
    }

    fn known_interruption_deviations(&self) -> Vec<(Interruption, &'static str)> {
        Vec::new()
    }

    fn break_connections(&self) {
        self.proxy.break_connections();
    }
}
