//! The catalog: tables from `ListTables` and their keys from `DescribeTable`.
//!
//! DynamoDB has no level between an account's Region and its tables, so every
//! table is in the one namespace [`NAMESPACE`]. DynamoDB declares the types of
//! key attributes only, so a table's columns are its key attributes and the
//! [`ITEM`] column holding each whole item.
//!
//! IAM checks a request before DynamoDB looks the table up, so a table the
//! credentials may not describe cannot be told from one that does not exist,
//! and is "not found"; `ResourceNotFoundException` is returned only to
//! credentials that may describe the table, so it proves the table does not
//! exist.

mod source_metadata;
mod type_mapping;

pub use source_metadata::{
    AttributeDefinition, DynamoDbTable, KeyElement, KeyType, Projection, SecondaryIndex,
    StreamSpecification,
};

pub(crate) use source_metadata::reference;

use std::time::SystemTime;

use aws_sdk_dynamodb::Client;
use connector_sdk::{
    Catalog, Connector, ConnectorError, ExecutionOutput, Listed, NameFilter, NamespacePath,
    Session, TableEntry, TableKind, TableMetadata, TableRecord, TableReference,
};

use crate::{
    DynamoDbConnector, DynamoDbSession, Error,
    error_handler::{ACCESS_DENIED, RESOURCE_NOT_FOUND},
};

/// The one namespace that holds every table of the account's Region.
pub const NAMESPACE: &str = "default";

/// The name and the Data Source type of the column that holds each whole
/// item, after the key columns.
pub const ITEM: &str = "item";

/// The DynamoDB catalog.
pub struct DynamoDbCatalog;

impl Catalog<DynamoDbConnector> for DynamoDbCatalog {
    type Table = DynamoDbTable;
    type Context = ();

    fn open_context(_session: &DynamoDbSession) -> Result<(), ConnectorError> {
        Ok(())
    }

    fn derive_table_metadata(table: &DynamoDbTable, _settings: &()) -> TableMetadata {
        table.table_metadata()
    }

    /// Lists the one namespace; nothing is sent to DynamoDB.
    fn namespace_list(
        session: &Session<DynamoDbConnector>,
        filter: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<NamespacePath>>, ConnectorError> {
        let matches = matches(filter, NAMESPACE);
        Ok(session.access().source_items(async move {
            Ok(matches
                .then(namespace)
                .map(Listed::Item)
                .into_iter()
                .collect())
        }))
    }

    /// Lists every table `ListTables` returns, by name. Credentials that may
    /// not list tables make the namespace unreadable.
    fn table_list(
        session: &Session<DynamoDbConnector>,
        namespaces: &NameFilter,
        tables: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<TableEntry>>, ConnectorError> {
        let listed = matches(namespaces, NAMESPACE);
        let tables = tables.clone();
        let client = session.access().client.clone();
        Ok(session.access().source_items(async move {
            if !listed {
                return Ok(Vec::new());
            }
            let names = match list_tables(&client).await {
                Ok(names) => names,
                Err(error) if error.code() == Some(ACCESS_DENIED) => {
                    return Ok(vec![Listed::Unreadable {
                        scope: namespace(),
                        reason: "the credentials may not list DynamoDB tables".to_owned(),
                    }]);
                }
                Err(error) => return Err(DynamoDbConnector::classify(error)),
            };
            Ok(names
                .iter()
                .filter(|name| matches(&tables, name))
                .map(|name| {
                    Listed::Item(TableEntry {
                        table: reference(name),
                        kind: TableKind::Table,
                    })
                })
                .collect())
        }))
    }

    /// Describes each requested table.
    fn table_metadata(
        session: &Session<DynamoDbConnector>,
        tables: &[TableReference],
    ) -> Result<ExecutionOutput<TableRecord<DynamoDbTable>>, ConnectorError> {
        let tables = tables.to_vec();
        let client = session.access().client.clone();
        Ok(session.access().source_items(async move {
            let mut records = Vec::with_capacity(tables.len());
            for table in tables {
                records.push(describe(&client, table).await?);
            }
            Ok(records)
        }))
    }
}

/// Reads one table's record with `DescribeTable`.
async fn describe(
    client: &Client,
    table: TableReference,
) -> Result<TableRecord<DynamoDbTable>, ConnectorError> {
    let name = match table.names() {
        [namespace, name] if namespace == NAMESPACE && valid_table_name(name) => name.clone(),
        _ => return Ok(TableRecord::DoesNotExist { table }),
    };
    let error = match client.describe_table().table_name(&name).send().await {
        Ok(output) => {
            return Ok(match output.table {
                Some(description) => TableRecord::Read {
                    metadata: DynamoDbTable::from_description(description),
                    read_at: SystemTime::now(),
                },
                None => TableRecord::DoesNotExist { table },
            });
        }
        Err(error) => Error::from(error),
    };
    match error.code() {
        Some(RESOURCE_NOT_FOUND) => Ok(TableRecord::DoesNotExist { table }),
        Some(ACCESS_DENIED) => Ok(TableRecord::NotFound {
            table,
            reason: "the credentials may not describe a table of this name".to_owned(),
        }),
        _ => Err(DynamoDbConnector::classify(error)),
    }
}

/// Every table name, one `ListTables` page after another.
async fn list_tables(client: &Client) -> Result<Vec<String>, Error> {
    let mut names = Vec::new();
    let mut start = None;
    loop {
        let page = client
            .list_tables()
            .set_exclusive_start_table_name(start)
            .send()
            .await?;
        names.extend(page.table_names.unwrap_or_default());
        start = page.last_evaluated_table_name;
        if start.is_none() {
            return Ok(names);
        }
    }
}

/// Whether `name` can name a DynamoDB table: 3 to 255 letters, digits, `_`,
/// `-`, and `.`. `DescribeTable` refuses other names, and also accepts a
/// table's ARN, which is not a table name.
fn valid_table_name(name: &str) -> bool {
    (3..=255).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn namespace() -> NamespacePath {
    NamespacePath {
        names: vec![NAMESPACE.to_owned()],
    }
}

fn matches(filter: &NameFilter, name: &str) -> bool {
    filter.as_ref().is_none_or(|names| names.contains(name))
}
