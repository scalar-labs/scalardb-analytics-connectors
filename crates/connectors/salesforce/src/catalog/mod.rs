//! The catalog: objects from describeGlobal and their fields from describe.
//!
//! Salesforce objects have no hierarchy, so every object is in the one
//! namespace [`NAMESPACE`]. describeGlobal and describe hide the objects and
//! fields the user may not see, and an object that is hidden cannot be told
//! from one that does not exist, so an object describe does not find is "not
//! found" rather than "does not exist".

mod source_metadata;
mod type_mapping;

pub use source_metadata::{SalesforceField, SalesforceObject};

pub(crate) use source_metadata::reference;

use std::time::SystemTime;

use connector_salesforce_client::{Error, SObjectSummary};
use connector_sdk::{
    Catalog, Connector, ConnectorError, ExecutionOutput, Listed, NameFilter, NamespacePath,
    Session, TableEntry, TableKind, TableMetadata, TableRecord, TableReference,
};

use crate::{SalesforceConnector, SalesforceSession};

/// The one namespace that holds every object.
pub const NAMESPACE: &str = "sobjects";

/// The Salesforce catalog.
pub struct SalesforceCatalog;

impl Catalog<SalesforceConnector> for SalesforceCatalog {
    type Table = SalesforceObject;
    type Context = ();

    fn open_context(_session: &SalesforceSession) -> Result<(), ConnectorError> {
        Ok(())
    }

    fn derive_table_metadata(table: &SalesforceObject, _settings: &()) -> TableMetadata {
        table.table_metadata()
    }

    /// Lists the one namespace; nothing is sent to Salesforce.
    fn namespace_list(
        session: &Session<SalesforceConnector>,
        filter: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<NamespacePath>>, ConnectorError> {
        let matches = matches(filter, NAMESPACE);
        Ok(session.access().source_items(async move {
            Ok(matches
                .then(|| {
                    Listed::Item(NamespacePath {
                        names: vec![NAMESPACE.to_owned()],
                    })
                })
                .into_iter()
                .collect())
        }))
    }

    /// Lists the queryable objects describeGlobal returns, by name.
    fn table_list(
        session: &Session<SalesforceConnector>,
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
            let global = client
                .describe_global()
                .await
                .map_err(SalesforceConnector::classify)?;
            let mut names: Vec<String> = global
                .sobjects
                .into_iter()
                .filter(listed_object)
                .map(|object| object.name)
                .filter(|name| matches(&tables, name))
                .collect();
            names.sort();
            Ok(names
                .iter()
                .map(|name| {
                    Listed::Item(TableEntry {
                        table: reference(name),
                        kind: TableKind::Table,
                    })
                })
                .collect())
        }))
    }

    /// Describes each requested object.
    fn table_metadata(
        session: &Session<SalesforceConnector>,
        tables: &[TableReference],
    ) -> Result<ExecutionOutput<TableRecord<SalesforceObject>>, ConnectorError> {
        let tables = tables.to_vec();
        let client = session.access().client.clone();
        Ok(session.access().source_items(async move {
            let mut records = Vec::with_capacity(tables.len());
            for table in tables {
                let [namespace, name] = table.names() else {
                    records.push(TableRecord::DoesNotExist { table });
                    continue;
                };
                if namespace != NAMESPACE {
                    records.push(TableRecord::DoesNotExist { table });
                    continue;
                }
                let record = match client.describe(name).await {
                    Ok(describe) if describe.name == *name && describe.queryable => {
                        TableRecord::Read {
                            metadata: SalesforceObject::from_describe(describe),
                            read_at: SystemTime::now(),
                        }
                    }
                    // An object that cannot be queried is no table, and
                    // object names are unique without regard to case, so an
                    // object that describes under another spelling has no
                    // object of the requested spelling beside it.
                    Ok(_) => TableRecord::DoesNotExist { table },
                    Err(error) => match refusal(&error) {
                        Some(Refusal::NotFound) => TableRecord::NotFound {
                            table,
                            reason: "no object with this name is visible to the Salesforce user"
                                .to_owned(),
                        },
                        Some(Refusal::PermissionDenied) => TableRecord::PermissionDenied {
                            table,
                            reason: "the Salesforce user may not read the object".to_owned(),
                        },
                        None => return Err(SalesforceConnector::classify(error)),
                    },
                };
                records.push(record);
            }
            Ok(records)
        }))
    }
}

/// Whether table lists include the object: one that can be queried and is
/// not deprecated.
fn listed_object(object: &SObjectSummary) -> bool {
    object.queryable && !object.deprecated_and_hidden
}

/// How describe refused an object.
enum Refusal {
    /// HTTP 404: the object is hidden from the user or does not exist.
    NotFound,
    /// `INSUFFICIENT_ACCESS`: the user may see the object but not read it.
    PermissionDenied,
}

fn refusal(error: &Error) -> Option<Refusal> {
    match error {
        Error::Api { status: 404, .. } => Some(Refusal::NotFound),
        Error::Api { errors, .. }
            if errors
                .first()
                .is_some_and(|error| error.error_code == "INSUFFICIENT_ACCESS") =>
        {
            Some(Refusal::PermissionDenied)
        }
        _ => None,
    }
}

fn matches(filter: &NameFilter, name: &str) -> bool {
    filter.as_ref().is_none_or(|names| names.contains(name))
}
