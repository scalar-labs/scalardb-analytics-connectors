//! Connector types that exercise ADBC access without opening Data Sources.

use connector_sdk::{
    Catalog, Connector, ConnectorError, ExecutableQueryResult, ExecutionOutput, Executor, Listed,
    LogicalPlan, NameFilter, NamespacePath, Planner, PlanningError, PortableQuery, RewriteRuleSet,
    Session, TableEntry, TableRecord, TableReference, test_connector::TestTable,
};

/// The catalog, planner, executor, and query of a test Connector whose tests
/// never plan, read, or request metadata.
pub(crate) struct Unused;

impl PortableQuery for Unused {
    fn to_bytes(&self) -> Vec<u8> {
        unreachable!("ADBC access tests plan nothing")
    }

    fn from_bytes(_bytes: &[u8]) -> Result<Self, ConnectorError> {
        unreachable!("ADBC access tests plan nothing")
    }
}

impl<C: Connector> Catalog<C> for Unused {
    type Table = TestTable;
    type Context = ();

    fn open_context(_session: &connector_sdk::AccessSession<C>) -> Result<(), ConnectorError> {
        unreachable!("ADBC access tests open no session")
    }

    fn namespace_list(
        _session: &Session<C>,
        _filter: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<NamespacePath>>, ConnectorError> {
        unreachable!("ADBC access tests request no metadata")
    }

    fn table_list(
        _session: &Session<C>,
        _namespaces: &NameFilter,
        _tables: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<TableEntry>>, ConnectorError> {
        unreachable!("ADBC access tests request no metadata")
    }

    fn table_metadata(
        _session: &Session<C>,
        _tables: &[TableReference],
    ) -> Result<ExecutionOutput<TableRecord<TestTable>>, ConnectorError> {
        unreachable!("ADBC access tests request no metadata")
    }
}

impl<C: Connector> Planner<C> for Unused {
    type Facts = ();

    fn planning_facts(
        _session: &Session<C>,
        _offered_plan: &LogicalPlan,
    ) -> Result<(TableReference, ()), PlanningError> {
        unreachable!("ADBC access tests plan nothing")
    }

    fn rewrite_rules() -> RewriteRuleSet<()> {
        RewriteRuleSet::new()
    }

    fn generate_query(
        _remote_plan: &LogicalPlan,
        _facts: &(),
    ) -> Result<ExecutableQueryResult<C::Query>, PlanningError> {
        unreachable!("ADBC access tests plan nothing")
    }
}

impl<C: Connector> Executor<C> for Unused {
    fn run(_session: &Session<C>, _query: &C::Query) -> Result<ExecutionOutput, ConnectorError> {
        unreachable!("ADBC access tests read nothing")
    }
}

/// Declares a unit Connector type using ADBC access.
///
/// Only its error handler matters to these tests; nothing else is called.
macro_rules! adbc_test_connector {
    ($name:ident, error_handler = $error_handler:ty) => {
        struct $name;

        impl connector_sdk::Connector for $name {
            type Access = connector_adbc::Adbc<Self>;
            type Catalog = connector::Unused;
            type Planner = connector::Unused;
            type Executor = connector::Unused;
            type ErrorHandler = $error_handler;
            type Query = connector::Unused;

            fn descriptor() -> connector_sdk::ConnectorDescriptor {
                unreachable!("ADBC access tests describe no connector")
            }

            fn convert_column(
                _column: &arrow_array::ArrayRef,
                _target: &analytics_types::AnalyticsType,
            ) -> std::result::Result<arrow_array::ArrayRef, connector_sdk::ConnectorError> {
                unreachable!("ADBC access tests convert nothing")
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
                unreachable!("ADBC access tests open no Data Source")
            }
        }
    };
}

pub(crate) use adbc_test_connector;
