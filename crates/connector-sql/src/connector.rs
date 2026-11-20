//! What a SQL-speaking Connector declares, and the catalog built from it.

use std::marker::PhantomData;

use connector_sdk::{
    AccessSession, Catalog, Connector, ConnectorError, ExecutionOutput, Listed, LogicalExpression,
    LogicalPlan, NameFilter, NamespacePath, PlanningError, Session, TableEntry, TableMetadata,
    TableRecord, TableReference,
};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    SqlDialect, SqlExecutor, SqlPlanner, SqlRead,
    catalog::{CatalogReads, catalog_read, read_catalog},
};

/// A Connector whose Data Source speaks SQL.
///
/// Its catalog, planner, and executor are this crate's [`SqlCatalog`],
/// [`SqlPlanner`], and [`SqlExecutor`], and its query is a [`SqlRead`]. The
/// Connector declares how its access runs one statement ([`Self::query`]) and
/// its Data Source knowledge: how it reads its catalog, written as functions
/// that run statements on [`CatalogReads`] and use each result where it is
/// fetched, the facts planning needs, which offered expressions its Data
/// Source evaluates with the same meaning, and how it spells SQL
/// ([`SqlDialect`]).
pub trait SqlConnector:
    SqlDialect
    + Connector<
        Catalog = SqlCatalog<Self>,
        Planner = SqlPlanner<Self>,
        Executor = SqlExecutor<Self>,
        Query = SqlRead,
    >
{
    /// The connection statements run on.
    ///
    /// A catalog read runs its statements when its stream is first pulled,
    /// after the request that created it returned, so it holds its own
    /// handle.
    type Connection: Clone + Send + 'static;
    /// The Connector's source metadata for one table
    /// ([`Catalog::Table`]).
    type Table: Serialize + DeserializeOwned + JsonSchema + Send + 'static;
    /// Session-level facts read once when a session opens, such as the
    /// server version that decides which catalog columns exist.
    type Context: Clone + Send + Sync + 'static;
    /// Facts about the read table that one planning call needs.
    type PlanningFacts;

    /// The connection of a session.
    fn connection(session: &AccessSession<Self>) -> Self::Connection;

    /// Starts one statement on `connection`.
    ///
    /// Nothing is sent before the first pull. The returned cancel stops the
    /// statement at the source, and the Connector classifies the errors its
    /// access reports.
    fn query(connection: &Self::Connection, sql: String) -> ExecutionOutput;

    /// Reads a new session's context.
    fn session_context(
        catalog: &mut CatalogReads<'_, Self>,
    ) -> Result<Self::Context, ConnectorError>;

    /// Lists namespaces whose names match `filter`, within what the Data
    /// Source's `settings` list.
    fn namespace_list(
        catalog: &mut CatalogReads<'_, Self>,
        settings: &Self::Settings,
        filter: &NameFilter,
    ) -> Result<Vec<Listed<NamespacePath>>, ConnectorError>;

    /// Lists the matching tables of the matching namespaces, within what the
    /// Data Source's `settings` list.
    fn table_list(
        catalog: &mut CatalogReads<'_, Self>,
        settings: &Self::Settings,
        namespaces: &NameFilter,
        tables: &NameFilter,
    ) -> Result<Vec<Listed<TableEntry>>, ConnectorError>;

    /// Reads one record per requested table.
    fn table_metadata(
        catalog: &mut CatalogReads<'_, Self>,
        tables: &[TableReference],
        context: &Self::Context,
    ) -> Result<Vec<TableRecord<Self::Table>>, ConnectorError>;

    /// Derives a table's table metadata from its source metadata and the
    /// Data Source's settings ([`Catalog::derive_table_metadata`]).
    fn derive_table_metadata(table: &Self::Table, settings: &Self::Settings) -> TableMetadata;

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

    fn open_context(session: &AccessSession<C>) -> Result<C::Context, ConnectorError> {
        read_catalog(&C::connection(session), C::session_context)
    }

    fn namespace_list(
        session: &Session<C>,
        filter: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<NamespacePath>>, ConnectorError> {
        let (settings, filter) = (session.settings.clone(), filter.clone());
        Ok(catalog_read(
            &C::connection(session.access()),
            Box::new(move |catalog| C::namespace_list(catalog, &settings, &filter)),
        ))
    }

    fn table_list(
        session: &Session<C>,
        namespaces: &NameFilter,
        tables: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<TableEntry>>, ConnectorError> {
        let (settings, namespaces, tables) =
            (session.settings.clone(), namespaces.clone(), tables.clone());
        Ok(catalog_read(
            &C::connection(session.access()),
            Box::new(move |catalog| C::table_list(catalog, &settings, &namespaces, &tables)),
        ))
    }

    fn table_metadata(
        session: &Session<C>,
        tables: &[TableReference],
    ) -> Result<ExecutionOutput<TableRecord<C::Table>>, ConnectorError> {
        let (tables, context) = (tables.to_vec(), session.context().clone());
        Ok(catalog_read(
            &C::connection(session.access()),
            Box::new(move |catalog| C::table_metadata(catalog, &tables, &context)),
        ))
    }

    fn derive_table_metadata(table: &C::Table, settings: &C::Settings) -> TableMetadata {
        C::derive_table_metadata(table, settings)
    }
}
