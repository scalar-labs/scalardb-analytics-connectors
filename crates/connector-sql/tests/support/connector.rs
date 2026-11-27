//! SQL-speaking Connector types for exercising the shared SQL path.

use connector_adbc::{AdbcConnection, AdbcSession};
use connector_sdk::{Component, Connector, Session};

/// Declares a unit Connector type implementing `SqlConnector`.
///
/// Its session has no context, its planning facts are the plan's table
/// binding alone, its filters run remotely exactly when `$runs_remotely` is
/// true, and it returns driver output unchanged. Literals render as a marker,
/// because these tests check SQL shape rather than spelling. Its catalog
/// reads are never requested.
macro_rules! sql_test_connector {
    ($name:ident, runs_remotely = $runs_remotely:expr) => {
        pub(crate) struct $name;

        impl connector_sdk::Connector for $name {
            type Access = connector_adbc::Adbc<Self>;
            type Catalog = connector_sql::SqlCatalog<Self>;
            type Planner = connector_sql::SqlPlanner<Self>;
            type Executor = connector_sql::SqlExecutor<Self>;
            type ErrorHandler = connector_sdk::GenericErrorHandler<connector_adbc::AdbcDiagnostic>;
            type Query = connector_sql::SqlRead;
            type Settings = ();

            fn descriptor() -> connector_sdk::ConnectorDescriptor {
                unreachable!("SQL path tests describe no connector")
            }

            fn settings(
                _options: &connector_sdk::ConnectionOptions,
            ) -> std::result::Result<(), connector_sdk::ConnectorError> {
                Ok(())
            }

            fn convert_column(
                column: &arrow_array::ArrayRef,
                _target: &analytics_types::AnalyticsType,
            ) -> std::result::Result<arrow_array::ArrayRef, connector_sdk::ConnectorError> {
                Ok(std::sync::Arc::clone(column))
            }
        }

        impl connector_adbc::AdbcConnector for $name {
            const DRIVER: connector_adbc::DriverRequirement = connector_adbc::DriverRequirement {
                library: "adbc_driver_fixture",
                minimum: connector_adbc::DriverVersion {
                    major: 1,
                    minor: 0,
                    patch: 0,
                },
                below: connector_adbc::DriverVersion {
                    major: 2,
                    minor: 0,
                    patch: 0,
                },
            };

            fn database_options(
                _options: connector_sdk::ConnectionOptions,
                _connection_timeout: Option<std::time::Duration>,
            ) -> std::result::Result<
                Vec<(
                    adbc_core::options::OptionDatabase,
                    adbc_core::options::OptionValue,
                )>,
                connector_sdk::ConnectorError,
            > {
                unreachable!("SQL path tests open no Data Source")
            }
        }

        impl connector_sql::SqlDialect for $name {
            fn quote_identifier(identifier: &str) -> String {
                format!("\"{}\"", identifier.replace('"', "\"\""))
            }

            fn write_literal(_literal: &connector_sdk::LiteralValue, sql: &mut String) {
                sql.push_str("test_literal");
            }
        }

        impl connector_sql::SqlConnector for $name {
            type Connection = connector_adbc::AdbcConnection<Self>;
            type Table = connector_sdk::test_connector::TestTable;
            type Context = ();
            type PlanningFacts = ();

            fn connection(
                session: &connector_adbc::AdbcSession<Self>,
            ) -> connector_adbc::AdbcConnection<Self> {
                session.connection().clone()
            }

            fn query(
                connection: &connector_adbc::AdbcConnection<Self>,
                sql: String,
            ) -> connector_sdk::ExecutionOutput {
                let statement = connector_adbc::query(connection, sql);
                let cancel = std::sync::Arc::new(statement.cancel_handle());
                connector_sdk::ExecutionOutput::new(Box::pin(statement), cancel)
            }

            fn session_context(
                _catalog: &mut connector_sql::catalog::CatalogReads<'_, Self>,
            ) -> std::result::Result<(), connector_sdk::ConnectorError> {
                Ok(())
            }

            fn namespace_list(
                _catalog: &mut connector_sql::catalog::CatalogReads<'_, Self>,
                _settings: &Self::Settings,
                _filter: &connector_sdk::NameFilter,
            ) -> std::result::Result<
                Vec<connector_sdk::Listed<connector_sdk::NamespacePath>>,
                connector_sdk::ConnectorError,
            > {
                unreachable!("SQL path tests list no namespaces")
            }

            fn table_list(
                _catalog: &mut connector_sql::catalog::CatalogReads<'_, Self>,
                _settings: &Self::Settings,
                _namespaces: &connector_sdk::NameFilter,
                _tables: &connector_sdk::NameFilter,
            ) -> std::result::Result<
                Vec<connector_sdk::Listed<connector_sdk::TableEntry>>,
                connector_sdk::ConnectorError,
            > {
                unreachable!("SQL path tests list no tables")
            }

            fn table_metadata(
                _catalog: &mut connector_sql::catalog::CatalogReads<'_, Self>,
                _tables: &[connector_sdk::TableReference],
                _context: &(),
            ) -> std::result::Result<
                Vec<connector_sdk::TableRecord<connector_sdk::test_connector::TestTable>>,
                connector_sdk::ConnectorError,
            > {
                unreachable!("SQL path tests read no table metadata")
            }

            fn derive_table_metadata(
                _table: &connector_sdk::test_connector::TestTable,
                _settings: &(),
            ) -> connector_sdk::TableMetadata {
                unreachable!("SQL path tests read no table metadata")
            }

            fn planning_facts(
                _session: &connector_sdk::Session<Self>,
                offered_plan: &connector_sdk::LogicalPlan,
            ) -> std::result::Result<
                (connector_sdk::TableReference, ()),
                connector_sdk::PlanningError,
            > {
                let (binding, _) = offered_plan
                    .table_read()
                    .expect("offered plans read a table");
                let table = connector_sdk::TableReference::new(binding.components().to_vec())
                    .expect("bindings have non-empty components");
                Ok((table, ()))
            }

            fn runs_remotely(_expression: &connector_sdk::LogicalExpression, _facts: &()) -> bool {
                $runs_remotely
            }
        }
    };
}

pub(crate) use sql_test_connector;

/// Opens a session of Connector `C` on a fixture connection.
// Not every test crate that includes this module opens sessions.
#[allow(dead_code)]
pub(crate) fn session<C>(connection: AdbcConnection<C>) -> Session<C>
where
    C: Connector<Access = connector_adbc::Adbc<C>, Settings = ()> + connector_adbc::AdbcConnector,
{
    let driver = Component {
        name: "fixture".to_owned(),
        version: "1.0.0".to_owned(),
    };
    Session::open(AdbcSession::new(connection, driver), ()).expect("a fixture session opens")
}
