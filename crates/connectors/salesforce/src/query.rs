//! The planned query and sending it: one SOQL statement and the Arrow schema
//! its records are decoded into.

use std::sync::Arc;

use analytics_types::{AnalyticsSchema, arrow_schema::Schema};
use arrow_array::{RecordBatch, RecordBatchOptions};
use connector_salesforce_client::Error;
use connector_sdk::{
    Connector, ConnectorError, ErrorKind, ExecutionOutput, Executor, PortableQuery, Session,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::SalesforceConnector;

/// One SOQL query implementing a remote plan.
///
/// SOQL selects each field once and has no column aliases, so the query
/// selects `fields`, each once, and `columns` places them in the remote
/// output: output column `i` is `fields[columns[i]]`. It intentionally does
/// not implement `Debug`: the SOQL contains filter literals.
#[derive(Clone, PartialEq)]
pub struct SoqlRead {
    pub soql: String,
    /// The selected fields by their Salesforce names, with the Analytics
    /// types their values are decoded into.
    pub fields: AnalyticsSchema,
    pub columns: Vec<usize>,
}

/// The serialized form of a [`SoqlRead`].
#[derive(Serialize, Deserialize)]
struct SoqlReadBytes {
    soql: String,
    fields: Schema,
    columns: Vec<usize>,
}

impl PortableQuery for SoqlRead {
    fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&SoqlReadBytes {
            soql: self.soql.clone(),
            fields: self.fields.to_arrow(),
            columns: self.columns.clone(),
        })
        .expect("a SOQL read serializes to JSON")
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, ConnectorError> {
        let invalid = || {
            ConnectorError::new(
                ErrorKind::InvalidRequest,
                false,
                "the bytes are not a Salesforce query",
            )
        };
        let read: SoqlReadBytes = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        let fields = AnalyticsSchema::try_from_arrow(&read.fields).map_err(|_| invalid())?;
        if read
            .columns
            .iter()
            .any(|column| *column >= fields.fields().len())
        {
            return Err(invalid());
        }
        Ok(Self {
            soql: read.soql,
            fields,
            columns: read.columns,
        })
    }
}

/// Sends a planned SOQL query through the client.
pub struct SalesforceExecutor;

impl Executor<SalesforceConnector> for SalesforceExecutor {
    /// Returns one batch per page of records, with the remote output's
    /// columns in order.
    fn run(
        session: &Session<SalesforceConnector>,
        query: &SoqlRead,
    ) -> Result<ExecutionOutput, ConnectorError> {
        let fields = Arc::new(query.fields.to_arrow());
        let records = session
            .access()
            .client
            .query(query.soql.clone(), Arc::clone(&fields))
            .map_err(|error| match error {
                // The schema came from the connector's own type mapping.
                Error::Config(_) => ConnectorError::new(
                    ErrorKind::Internal,
                    false,
                    "the client cannot decode the planned schema",
                ),
                error => SalesforceConnector::classify(error),
            })?;
        let columns = query.columns.clone();
        let schema = Arc::new(Schema::new(
            columns
                .iter()
                .map(|column| fields.field(*column).clone())
                .collect::<Vec<_>>(),
        ));
        let batches = records.map(move |batch| {
            let batch = batch.map_err(SalesforceConnector::classify)?;
            RecordBatch::try_new_with_options(
                Arc::clone(&schema),
                columns
                    .iter()
                    .map(|column| Arc::clone(batch.column(*column)))
                    .collect(),
                &RecordBatchOptions::new().with_row_count(Some(batch.num_rows())),
            )
            .map_err(|_| {
                ConnectorError::new(
                    ErrorKind::Internal,
                    false,
                    "the decoded records do not form the remote output",
                )
            })
        });
        Ok(session.access().source_read(batches))
    }
}
