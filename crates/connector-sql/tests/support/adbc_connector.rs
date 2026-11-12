//! A Connector type that runs SQL through the ADBC fixture without opening
//! Data Sources.

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
        unreachable!("SQL execution tests plan nothing")
    }

    fn from_bytes(_bytes: &[u8]) -> Result<Self, ConnectorError> {
        unreachable!("SQL execution tests plan nothing")
    }
}

impl<C: Connector> Catalog<C> for Unused {
    type Table = TestTable;
    type Context = ();

    fn open_context(_session: &connector_sdk::AccessSession<C>) -> Result<(), ConnectorError> {
        unreachable!("SQL execution tests open no session")
    }

    fn namespace_list(
        _session: &Session<C>,
        _filter: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<NamespacePath>>, ConnectorError> {
        unreachable!("SQL execution tests request no metadata")
    }

    fn table_list(
        _session: &Session<C>,
        _namespaces: &NameFilter,
        _tables: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<TableEntry>>, ConnectorError> {
        unreachable!("SQL execution tests request no metadata")
    }

    fn table_metadata(
        _session: &Session<C>,
        _tables: &[TableReference],
    ) -> Result<ExecutionOutput<TableRecord<TestTable>>, ConnectorError> {
        unreachable!("SQL execution tests request no metadata")
    }
}

impl<C: Connector> Planner<C> for Unused {
    type Facts = ();

    fn planning_facts(
        _session: &Session<C>,
        _offered_plan: &LogicalPlan,
    ) -> Result<(TableReference, ()), PlanningError> {
        unreachable!("SQL execution tests plan nothing")
    }

    fn rewrite_rules() -> RewriteRuleSet<()> {
        RewriteRuleSet::new()
    }

    fn generate_query(
        _remote_plan: &LogicalPlan,
        _facts: &(),
    ) -> Result<ExecutableQueryResult<C::Query>, PlanningError> {
        unreachable!("SQL execution tests plan nothing")
    }
}

impl<C: Connector> Executor<C> for Unused {
    fn run(_session: &Session<C>, _query: &C::Query) -> Result<ExecutionOutput, ConnectorError> {
        unreachable!("SQL execution tests read nothing")
    }
}

/// The Connector whose connections reach the ADBC fixture; driver errors are
/// classified by the generic rules alone.
pub(crate) struct FixtureAdbc;

impl Connector for FixtureAdbc {
    type Access = connector_adbc::Adbc<Self>;
    type Catalog = Unused;
    type Planner = Unused;
    type Executor = Unused;
    type ErrorHandler = connector_sdk::GenericErrorHandler<connector_adbc::AdbcDiagnostic>;
    type Query = Unused;

    fn descriptor() -> connector_sdk::ConnectorDescriptor {
        unreachable!("SQL execution tests describe no connector")
    }

    fn convert_column(
        _column: &arrow_array::ArrayRef,
        _target: &analytics_types::AnalyticsType,
    ) -> Result<arrow_array::ArrayRef, ConnectorError> {
        unreachable!("SQL execution tests convert nothing")
    }
}

impl connector_adbc::AdbcConnector for FixtureAdbc {
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
    ) -> Result<
        Vec<(
            adbc_core::options::OptionDatabase,
            adbc_core::options::OptionValue,
        )>,
        ConnectorError,
    > {
        unreachable!("SQL execution tests open no Data Source")
    }
}
