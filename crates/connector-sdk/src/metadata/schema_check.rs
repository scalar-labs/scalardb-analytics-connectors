//! The schema check a read runs before returning its first row.

use analytics_types::{AnalyticsField, AnalyticsSchema};

use super::{Column, ColumnType, TableMetadata};
use crate::{ConnectorError, DiagnosticEntry, DiagnosticValue, ErrorKind};

/// Compares the registered schema of the columns a read references with the
/// table's current metadata.
///
/// A column matches when the connector now maps it to the registered Analytics
/// type and it is not nullable where the registration says NOT NULL; a column
/// that became NOT NULL still matches. A table that no longer exists, a
/// missing column, or any mismatch is `stale registration`. Its diagnostics
/// hold, per mismatched column, `column`, `expected_type`, and
/// `expected_nullable`, then `data_source_type` and `nullable` when the column
/// still exists.
pub fn check_schema(
    expected: &AnalyticsSchema,
    current: Option<&TableMetadata>,
) -> Result<(), ConnectorError> {
    let Some(current) = current else {
        return Err(ConnectorError::new(
            ErrorKind::StaleRegistration,
            false,
            "the registered table no longer exists at the data source",
        ));
    };
    let mut details = Vec::new();
    for field in expected.fields() {
        let column = current
            .columns()
            .iter()
            .find(|column| column.name == field.name());
        let matches = column.is_some_and(|column| {
            matches!(
                &column.ty,
                ColumnType::Mapped { analytics_type, .. } if analytics_type == field.data_type()
            ) && (field.is_nullable() || !column.nullable)
        });
        if !matches {
            details.extend(mismatch(field, column));
        }
    }
    if details.is_empty() {
        return Ok(());
    }
    Err(ConnectorError::new(
        ErrorKind::StaleRegistration,
        false,
        "the table's current schema differs from its registration",
    )
    .with_diagnostics(details))
}

fn mismatch(expected: &AnalyticsField, current: Option<&Column>) -> Vec<DiagnosticEntry> {
    let mut entries = vec![
        DiagnosticEntry::new("column", DiagnosticValue::Utf8(expected.name().to_owned())),
        DiagnosticEntry::new(
            "expected_type",
            DiagnosticValue::Utf8(format!("{:?}", expected.data_type())),
        ),
        DiagnosticEntry::new(
            "expected_nullable",
            DiagnosticValue::Bool(expected.is_nullable()),
        ),
    ];
    if let Some(column) = current {
        let data_source_type = match &column.ty {
            ColumnType::Mapped {
                data_source_type, ..
            }
            | ColumnType::Unsupported {
                data_source_type, ..
            } => Some(data_source_type.clone()),
            ColumnType::Unread { .. } => None,
        };
        if let Some(data_source_type) = data_source_type {
            entries.push(DiagnosticEntry::new(
                "data_source_type",
                DiagnosticValue::Utf8(data_source_type),
            ));
        }
        entries.push(DiagnosticEntry::new(
            "nullable",
            DiagnosticValue::Bool(column.nullable),
        ));
    }
    entries
}

#[cfg(test)]
mod tests {
    use analytics_types::AnalyticsType;

    use super::*;
    use crate::{DiagnosticValue, Reported, TableKind, TableReference, UnsupportedReason};

    fn table(columns: Vec<(&str, ColumnType, bool)>) -> TableMetadata {
        TableMetadata::new(
            TableReference::new(vec!["orders".to_owned()]).unwrap(),
            TableKind::Table,
            Reported::NotOffered,
            columns
                .into_iter()
                .zip(1..)
                .map(|((name, ty, nullable), ordinal)| Column {
                    name: name.to_owned(),
                    ordinal,
                    nullable,
                    source_id: Reported::NotOffered,
                    ty,
                })
                .collect(),
            false,
            Reported::NotOffered,
        )
    }

    fn mapped(data_source_type: &str, analytics_type: AnalyticsType) -> ColumnType {
        ColumnType::Mapped {
            data_source_type: data_source_type.to_owned(),
            analytics_type,
        }
    }

    fn expected(fields: Vec<(&str, AnalyticsType, bool)>) -> AnalyticsSchema {
        AnalyticsSchema::new(
            fields
                .into_iter()
                .map(|(name, ty, nullable)| AnalyticsField::new(name, ty, nullable))
                .collect(),
        )
    }

    #[test]
    fn matching_types_and_tightened_nullability_pass() {
        let current = table(vec![
            ("id", mapped("bigint", AnalyticsType::BigInt), false),
            ("name", mapped("text", AnalyticsType::Text), false),
            ("unread", mapped("text", AnalyticsType::Text), true),
        ]);
        let registered = expected(vec![
            ("id", AnalyticsType::BigInt, false),
            ("name", AnalyticsType::Text, true),
        ]);
        assert_eq!(check_schema(&registered, Some(&current)), Ok(()));
    }

    #[test]
    fn changed_types_loosened_nullability_and_missing_columns_are_stale() {
        let current = table(vec![
            ("id", mapped("integer", AnalyticsType::Int), false),
            ("name", mapped("text", AnalyticsType::Text), true),
            (
                "amount",
                ColumnType::Unsupported {
                    data_source_type: "numeric".to_owned(),
                    reason: UnsupportedReason::ParameterOutOfRange,
                },
                true,
            ),
        ]);
        let registered = expected(vec![
            ("id", AnalyticsType::BigInt, false),
            ("name", AnalyticsType::Text, false),
            ("amount", AnalyticsType::Double, true),
            ("dropped", AnalyticsType::Text, true),
        ]);
        let error = check_schema(&registered, Some(&current)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::StaleRegistration);
        let columns: Vec<_> = error
            .diagnostics()
            .iter()
            .filter(|entry| entry.key() == "column")
            .map(|entry| entry.value().clone())
            .collect();
        assert_eq!(
            columns,
            ["id", "name", "amount", "dropped"].map(|name| DiagnosticValue::Utf8(name.to_owned()))
        );
        assert!(
            error
                .diagnostics()
                .iter()
                .any(|entry| entry.key() == "data_source_type"
                    && entry.value() == &DiagnosticValue::Utf8("integer".to_owned()))
        );

        let error = check_schema(&registered, None).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::StaleRegistration);
    }
}
