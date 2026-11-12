//! What a SQL-speaking Connector declares, and the catalog built from it.

use std::marker::PhantomData;

use connector_adbc::{AdbcConnector, AdbcSession};
use connector_sdk::{
    Catalog, Connector, ConnectorError, ExecutionOutput, Listed, LogicalExpression, LogicalPlan,
    NameFilter, NamespacePath, PlanningError, Session, SourceTableMetadata, TableEntry,
    TableRecord, TableReference,
};

use crate::{
    SqlDialect, SqlExecutor, SqlPlanner, SqlRead,
    catalog::{CatalogReads, catalog_read, read_catalog},
};

/// A Connector whose Data Source speaks SQL through an ADBC driver.
///
/// Its catalog, planner, and executor are this crate's [`SqlCatalog`],
/// [`SqlPlanner`], and [`SqlExecutor`], and its query is a [`SqlRead`]. The
/// Connector declares only its Data Source knowledge: how it reads its catalog,
/// written as functions that run statements on [`CatalogReads`] and use each
/// result where it is fetched, the facts planning needs, which offered
/// expressions its Data Source evaluates with the same meaning, and how it
/// spells SQL ([`SqlDialect`]).
pub trait SqlConnector:
    AdbcConnector
    + SqlDialect
    + Connector<
        Catalog = SqlCatalog<Self>,
        Planner = SqlPlanner<Self>,
        Executor = SqlExecutor<Self>,
        Query = SqlRead,
    >
{
    /// The Connector's source metadata for one table.
    type Table: SourceTableMetadata;
    /// Session-level facts read once when a session opens, such as the
    /// server version that decides which catalog columns exist.
    type Context: Clone + Send + Sync + 'static;
    /// Facts about the read table that one planning call needs.
    type PlanningFacts;

    /// Reads a new session's context.
    fn session_context(
        catalog: &mut CatalogReads<'_, Self>,
    ) -> Result<Self::Context, ConnectorError>;

    /// Lists namespaces whose names match `filter`.
    fn namespace_list(
        catalog: &mut CatalogReads<'_, Self>,
        filter: &NameFilter,
    ) -> Result<Vec<Listed<NamespacePath>>, ConnectorError>;

    /// Lists the matching tables of the matching namespaces.
    fn table_list(
        catalog: &mut CatalogReads<'_, Self>,
        namespaces: &NameFilter,
        tables: &NameFilter,
    ) -> Result<Vec<Listed<TableEntry>>, ConnectorError>;

    /// Reads one record per requested table.
    fn table_metadata(
        catalog: &mut CatalogReads<'_, Self>,
        tables: &[TableReference],
        context: &Self::Context,
    ) -> Result<Vec<TableRecord<Self::Table>>, ConnectorError>;

    /// Resolves the offered plan's table binding and reads the current facts
    /// its planning needs.
    ///
    /// Returns the table as metadata requests name it; its names are also the
    /// physical relation the generated SQL reads.
    fn planning_facts(
        session: &Session<Self>,
        offered_plan: &LogicalPlan,
    ) -> Result<(TableReference, Self::PlanningFacts), PlanningError>;

    /// Whether the Data Source evaluates a filter condition with its offered
    /// meaning, given the current facts.
    fn runs_remotely(expression: &LogicalExpression, facts: &Self::PlanningFacts) -> bool;
}

/// The catalog of a SQL-speaking Connector: its statements run on the
/// session's connection, one stoppable read per request.
pub struct SqlCatalog<C>(PhantomData<fn() -> C>);

impl<C: SqlConnector> Catalog<C> for SqlCatalog<C> {
    type Table = C::Table;
    type Context = C::Context;

    fn open_context(session: &AdbcSession<C>) -> Result<C::Context, ConnectorError> {
        read_catalog(session.connection(), C::session_context)
    }

    fn namespace_list(
        session: &Session<C>,
        filter: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<NamespacePath>>, ConnectorError> {
        let filter = filter.clone();
        Ok(catalog_read(
            session.access().connection(),
            Box::new(move |catalog| C::namespace_list(catalog, &filter)),
        ))
    }

    fn table_list(
        session: &Session<C>,
        namespaces: &NameFilter,
        tables: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<TableEntry>>, ConnectorError> {
        let (namespaces, tables) = (namespaces.clone(), tables.clone());
        Ok(catalog_read(
            session.access().connection(),
            Box::new(move |catalog| C::table_list(catalog, &namespaces, &tables)),
        ))
    }

    fn table_metadata(
        session: &Session<C>,
        tables: &[TableReference],
    ) -> Result<ExecutionOutput<TableRecord<C::Table>>, ConnectorError> {
        let (tables, context) = (tables.to_vec(), session.context().clone());
        Ok(catalog_read(
            session.access().connection(),
            Box::new(move |catalog| C::table_metadata(catalog, &tables, &context)),
        ))
    }
}
