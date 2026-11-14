//! Metadata retrieval: lists and table metadata against the fixture's
//! expectations, and the determinism of table metadata.

use super::{ExpectedRecord, Fixture, MetadataCase, MetadataRequest, Probe};
use crate::{
    TableRecord,
    runtime::{TableMetadataResult, TableRead},
};

pub(super) fn check<F: Fixture>(probe: &Probe<'_, F>) {
    for MetadataCase {
        name,
        options,
        request,
    } in probe.fixture.metadata_cases()
    {
        let session = probe
            .session_with(options)
            .unwrap_or_else(|error| panic!("{name}: the session does not open: {error}"));
        match request {
            MetadataRequest::Namespaces { filter, expected } => {
                let listed = succeeds(probe, name, session.namespace_list(&filter, None, None));
                assert_eq!(listed, expected, "{name}");
            }
            MetadataRequest::Tables {
                namespaces,
                tables,
                expected,
            } => {
                let listed = succeeds(
                    probe,
                    name,
                    session.table_list(&namespaces, &tables, None, None),
                );
                assert_eq!(listed, expected, "{name}");
            }
            MetadataRequest::TableMetadata { tables, expected } => {
                let first = records(succeeds(
                    probe,
                    name,
                    session.table_metadata(&tables, None, None),
                ));
                let second = records(succeeds(
                    probe,
                    name,
                    session.table_metadata(&tables, None, None),
                ));
                assert_eq!(second, first, "{name}: a second retrieval differs");
                let derived: Vec<_> = first.into_iter().map(|(record, _)| record).collect();
                assert_eq!(derived, expected, "{name}");
            }
        }
        session.close();
    }
}

/// Records the request's error and fails the case with it.
fn succeeds<F: Fixture, T>(
    probe: &Probe<'_, F>,
    case: &str,
    result: Result<T, crate::ConnectorError>,
) -> T {
    probe
        .observed
        .result(result, &[])
        .unwrap_or_else(|error| panic!("{case}: the request fails: {error}"))
}

/// Each record without retrieval info, with its source metadata document.
fn records(result: TableMetadataResult) -> Vec<(ExpectedRecord, Option<String>)> {
    result
        .records
        .into_iter()
        .map(|record| match record {
            TableRecord::Read {
                metadata:
                    TableRead {
                        table_metadata,
                        source_metadata,
                    },
                ..
            } => (
                ExpectedRecord::Read(table_metadata),
                Some(source_metadata.document),
            ),
            TableRecord::NotFound { .. } => (ExpectedRecord::NotFound, None),
            TableRecord::Unavailable { .. } => (ExpectedRecord::Unavailable, None),
        })
        .collect()
}
