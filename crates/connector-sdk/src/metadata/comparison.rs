//! Comparison of two observations of one table.
//!
//! A comparison reports only facts: values observed on both sides that
//! differ. An item that either side could not read, or never offers, is not
//! compared, so nothing is inferred about it. What a difference means, such
//! as a drop, a rename, or a change of privileges, is for the receiver to
//! decide.

use std::collections::BTreeSet;

use analytics_types::AnalyticsType;
use serde_json::Value;

use super::{
    Column, ColumnType, Constraint, ConstraintKind, Reported, SourceTableMetadata, TableMetadata,
    TableReference, UniqueKind, UnsupportedReason,
};
use crate::{ConnectorError, ErrorKind};

/// The differences between two observations of one table's table metadata.
///
/// Unchanged items are not listed. A comparison with no difference returns
/// no value rather than empty differences.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableMetadataDifferences {
    /// The table's own attributes whose values differ.
    pub table: Vec<TableAttribute>,
    /// Removed and matched columns in the older ordinal order, then added
    /// columns in the newer ordinal order.
    pub columns: Vec<Change<Column, ColumnAttribute>>,
    /// Removed and matched constraints in the older order, then added
    /// constraints in the newer order. Constraints are compared only when
    /// both sides read them.
    pub constraints: Vec<Change<Constraint, ConstraintAttribute>>,
}

impl TableMetadataDifferences {
    fn is_empty(&self) -> bool {
        self.table.is_empty() && self.columns.is_empty() && self.constraints.is_empty()
    }
}

/// One difference between two JSON documents.
///
/// `path` is a JSON Pointer. A value only the older document holds, or one
/// both hold, is located in the older document; a value only the newer
/// document holds is located in the newer one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonDifference {
    pub path: String,
    /// The older value; `None` when only the newer document holds one.
    pub before: Option<Value>,
    /// The newer value; `None` when only the older document holds one.
    pub after: Option<Value>,
}

/// An attribute of the table itself. Its name is not one: a caller pairs the
/// two observations by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableAttribute {
    Kind,
    SourceId,
}

/// A compared attribute of a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnAttribute {
    Name,
    Ordinal,
    Nullable,
    DataSourceType,
    /// The Analytics type, or the reason the column is unsupported.
    AnalyticsType,
    SourceId,
}

/// A compared attribute of a constraint, beyond its kind and name, which
/// match it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstraintAttribute {
    Enforced,
    Columns,
    UniqueKind,
    Condition,
    ReferencedTable,
    ReferencedColumns,
    Expression,
}

/// How one column or constraint differs between two observations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change<T, A> {
    /// Only the newer observation has it.
    Added(T),
    /// Only the older observation has it.
    Removed(T),
    /// Both observations have it, with these attributes differing.
    Matched {
        before: T,
        after: T,
        attributes: Vec<A>,
    },
}

impl TableMetadata {
    /// Compares this observation of a table with `newer`, a later observation
    /// of the table the caller paired with it; `None` when no observed value
    /// differs.
    ///
    /// Columns are matched by source identifier when both tables have the
    /// same source identifier and both columns have one, and otherwise by
    /// name: a column identifier, such as a PostgreSQL `attnum`, identifies a
    /// column only within its table.
    pub fn compare(&self, newer: &Self) -> Option<TableMetadataDifferences> {
        let differences = TableMetadataDifferences {
            table: table_differences(self, newer),
            columns: column_changes(self, newer),
            constraints: constraint_changes(self.constraints(), newer.constraints()),
        };
        (!differences.is_empty()).then_some(differences)
    }
}

/// Compares two observations of one table's source metadata as the JSON
/// documents the connector defines; `None` when they do not differ.
///
/// No table metadata is derived here: each observation's table metadata is
/// the one its read returned, compared on its own by
/// [`TableMetadata::compare`].
pub(crate) fn compare_source_metadata<T: SourceTableMetadata>(
    before: &T,
    after: &T,
) -> Result<Option<Vec<JsonDifference>>, ConnectorError> {
    let differences = json_differences(&document(before)?, &document(after)?);
    Ok((!differences.is_empty()).then_some(differences))
}

/// The source metadata as the JSON document the connector defines.
fn document<T: SourceTableMetadata>(metadata: &T) -> Result<Value, ConnectorError> {
    serde_json::to_value(metadata).map_err(|_| {
        ConnectorError::new(
            ErrorKind::Internal,
            false,
            "the source metadata could not be serialized as JSON",
        )
    })
}

/// A value observed on both sides, or `None` when either side has none.
fn observed<'a, T>(before: &'a Reported<T>, after: &'a Reported<T>) -> Option<(&'a T, &'a T)> {
    match (before, after) {
        (Reported::Value(before), Reported::Value(after)) => Some((before, after)),
        _ => None,
    }
}

fn table_differences(before: &TableMetadata, after: &TableMetadata) -> Vec<TableAttribute> {
    let mut differences = Vec::new();
    if before.kind() != after.kind() {
        differences.push(TableAttribute::Kind);
    }
    if observed(before.source_id(), after.source_id())
        .is_some_and(|(before, after)| before != after)
    {
        differences.push(TableAttribute::SourceId);
    }
    differences
}

fn column_changes(
    before: &TableMetadata,
    after: &TableMetadata,
) -> Vec<Change<Column, ColumnAttribute>> {
    let same_table = observed(before.source_id(), after.source_id())
        .is_some_and(|(before, after)| before == after);
    let matches = match_columns(before.columns(), after.columns(), same_table);
    let mut matched_after = vec![false; after.columns().len()];
    let mut changes = Vec::new();
    for (column, matched) in before.columns().iter().zip(&matches) {
        let Some(index) = *matched else {
            changes.push(Change::Removed(column.clone()));
            continue;
        };
        matched_after[index] = true;
        let newer = &after.columns()[index];
        let attributes = column_differences(column, newer);
        if !attributes.is_empty() {
            changes.push(Change::Matched {
                before: column.clone(),
                after: newer.clone(),
                attributes,
            });
        }
    }
    changes.extend(
        after
            .columns()
            .iter()
            .zip(&matched_after)
            .filter(|(_, matched)| !**matched)
            .map(|(column, _)| Change::Added(column.clone())),
    );
    changes
}

/// For each older column, the index of the newer column it matches.
///
/// With `by_source_id`, source identifiers are matched first across all
/// columns, so a name match never takes a column another column's identifier
/// claims, and two columns with different identifiers never match by name.
fn match_columns(before: &[Column], after: &[Column], by_source_id: bool) -> Vec<Option<usize>> {
    let mut taken = vec![false; after.len()];
    let mut matches = vec![None; before.len()];
    let mut claim = |predicate: &dyn Fn(&Column) -> bool| {
        let index = after
            .iter()
            .zip(&taken)
            .position(|(column, taken)| !taken && predicate(column))?;
        taken[index] = true;
        Some(index)
    };
    if by_source_id {
        for (column, matched) in before.iter().zip(matches.iter_mut()) {
            if let Reported::Value(id) = &column.source_id {
                *matched = claim(
                    &|newer| matches!(&newer.source_id, Reported::Value(other) if other == id),
                );
            }
        }
    }
    for (column, matched) in before.iter().zip(matches.iter_mut()) {
        if matched.is_none() {
            *matched = claim(&|newer| {
                newer.name == column.name
                    && !(by_source_id && observed(&column.source_id, &newer.source_id).is_some())
            });
        }
    }
    matches
}

fn column_differences(before: &Column, after: &Column) -> Vec<ColumnAttribute> {
    let mut differences = Vec::new();
    if before.name != after.name {
        differences.push(ColumnAttribute::Name);
    }
    if before.ordinal != after.ordinal {
        differences.push(ColumnAttribute::Ordinal);
    }
    if before.nullable != after.nullable {
        differences.push(ColumnAttribute::Nullable);
    }
    if let (Some((before_type, before_mapping)), Some((after_type, after_mapping))) =
        (type_parts(&before.ty), type_parts(&after.ty))
    {
        if before_type != after_type {
            differences.push(ColumnAttribute::DataSourceType);
        }
        if before_mapping != after_mapping {
            differences.push(ColumnAttribute::AnalyticsType);
        }
    }
    if observed(&before.source_id, &after.source_id).is_some_and(|(before, after)| before != after)
    {
        differences.push(ColumnAttribute::SourceId);
    }
    differences
}

/// The Data Source type and the mapping outcome, or `None` when the type was
/// not read.
fn type_parts(ty: &ColumnType) -> Option<(&str, Result<&AnalyticsType, UnsupportedReason>)> {
    match ty {
        ColumnType::Mapped {
            data_source_type,
            analytics_type,
        } => Some((data_source_type, Ok(analytics_type))),
        ColumnType::Unsupported {
            data_source_type,
            reason,
        } => Some((data_source_type, Err(*reason))),
        ColumnType::Unread { .. } => None,
    }
}

fn constraint_changes(
    before: &Reported<Vec<Constraint>>,
    after: &Reported<Vec<Constraint>>,
) -> Vec<Change<Constraint, ConstraintAttribute>> {
    let Some((before, after)) = observed(before, after) else {
        return Vec::new();
    };
    let mut taken = vec![false; after.len()];
    let mut changes = Vec::new();
    for constraint in before {
        let matched = after.iter().zip(&taken).position(|(newer, taken)| {
            !taken
                && newer.name == constraint.name
                && std::mem::discriminant(&newer.kind) == std::mem::discriminant(&constraint.kind)
        });
        let Some(index) = matched else {
            changes.push(Change::Removed(constraint.clone()));
            continue;
        };
        taken[index] = true;
        let newer = &after[index];
        let attributes = constraint_differences(constraint, newer);
        if !attributes.is_empty() {
            changes.push(Change::Matched {
                before: constraint.clone(),
                after: newer.clone(),
                attributes,
            });
        }
    }
    changes.extend(
        after
            .iter()
            .zip(&taken)
            .filter(|(_, taken)| !**taken)
            .map(|(constraint, _)| Change::Added(constraint.clone())),
    );
    changes
}

/// The attributes of a constraint's kind, each absent where the kind has none.
#[derive(PartialEq)]
struct ConstraintParts<'a> {
    columns: Option<&'a [String]>,
    unique_kind: Option<UniqueKind>,
    condition: Option<&'a str>,
    referenced_table: Option<&'a TableReference>,
    referenced_columns: Option<&'a [String]>,
    expression: Option<&'a str>,
}

impl<'a> ConstraintParts<'a> {
    fn of(kind: &'a ConstraintKind) -> Self {
        let none = Self {
            columns: None,
            unique_kind: None,
            condition: None,
            referenced_table: None,
            referenced_columns: None,
            expression: None,
        };
        match kind {
            ConstraintKind::PrimaryKey { columns } => Self {
                columns: Some(columns),
                ..none
            },
            ConstraintKind::Unique {
                columns,
                kind,
                condition,
            } => Self {
                columns: Some(columns),
                unique_kind: Some(*kind),
                condition: condition.as_deref(),
                ..none
            },
            ConstraintKind::ForeignKey {
                columns,
                referenced_table,
                referenced_columns,
            } => Self {
                columns: Some(columns),
                referenced_table: Some(referenced_table),
                referenced_columns: Some(referenced_columns),
                ..none
            },
            ConstraintKind::Check { expression } => Self {
                expression: Some(expression),
                ..none
            },
        }
    }
}

fn constraint_differences(before: &Constraint, after: &Constraint) -> Vec<ConstraintAttribute> {
    let (older, newer) = (
        ConstraintParts::of(&before.kind),
        ConstraintParts::of(&after.kind),
    );
    [
        (
            before.enforced != after.enforced,
            ConstraintAttribute::Enforced,
        ),
        (older.columns != newer.columns, ConstraintAttribute::Columns),
        (
            older.unique_kind != newer.unique_kind,
            ConstraintAttribute::UniqueKind,
        ),
        (
            older.condition != newer.condition,
            ConstraintAttribute::Condition,
        ),
        (
            older.referenced_table != newer.referenced_table,
            ConstraintAttribute::ReferencedTable,
        ),
        (
            older.referenced_columns != newer.referenced_columns,
            ConstraintAttribute::ReferencedColumns,
        ),
        (
            older.expression != newer.expression,
            ConstraintAttribute::Expression,
        ),
    ]
    .into_iter()
    .filter_map(|(differs, attribute)| differs.then_some(attribute))
    .collect()
}

/// The differences between two JSON documents.
///
/// Objects are compared member by member. Array elements are matched by
/// equality along their longest common subsequence, so an inserted or removed
/// element does not make the elements after it differ; between two matched
/// elements, unmatched elements are paired in order, and the rest are added
/// or removed.
fn json_differences(before: &Value, after: &Value) -> Vec<JsonDifference> {
    let mut differences = Vec::new();
    value_differences("", before, after, &mut differences);
    differences
}

fn value_differences(path: &str, before: &Value, after: &Value, out: &mut Vec<JsonDifference>) {
    if before == after {
        return;
    }
    match (before, after) {
        (Value::Object(older), Value::Object(newer)) => {
            let keys: BTreeSet<&String> = older.keys().chain(newer.keys()).collect();
            for key in keys {
                let path = format!("{path}/{}", pointer_token(key));
                match (older.get(key), newer.get(key)) {
                    (Some(older), Some(newer)) => value_differences(&path, older, newer, out),
                    (older, newer) => out.push(JsonDifference {
                        path,
                        before: older.cloned(),
                        after: newer.cloned(),
                    }),
                }
            }
        }
        (Value::Array(older), Value::Array(newer)) => array_differences(path, older, newer, out),
        _ => out.push(JsonDifference {
            path: path.to_owned(),
            before: Some(before.clone()),
            after: Some(after.clone()),
        }),
    }
}

fn array_differences(path: &str, before: &[Value], after: &[Value], out: &mut Vec<JsonDifference>) {
    // common[i][j]: the length of the longest common subsequence of
    // before[i..] and after[j..].
    let mut common = vec![vec![0usize; after.len() + 1]; before.len() + 1];
    for i in (0..before.len()).rev() {
        for j in (0..after.len()).rev() {
            common[i][j] = if before[i] == after[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < before.len() || j < after.len() {
        if i < before.len() && j < after.len() && before[i] == after[j] {
            unmatched_elements(path, before, after, &mut removed, &mut added, out);
            i += 1;
            j += 1;
        } else if j == after.len() || (i < before.len() && common[i + 1][j] >= common[i][j + 1]) {
            removed.push(i);
            i += 1;
        } else {
            added.push(j);
            j += 1;
        }
    }
    unmatched_elements(path, before, after, &mut removed, &mut added, out);
}

/// Reports the unmatched elements between two matched ones, pairing them in
/// order, and clears them.
fn unmatched_elements(
    path: &str,
    before: &[Value],
    after: &[Value],
    removed: &mut Vec<usize>,
    added: &mut Vec<usize>,
    out: &mut Vec<JsonDifference>,
) {
    for k in 0..removed.len().max(added.len()) {
        let (older, newer) = (removed.get(k), added.get(k));
        let index = older.or(newer).copied().unwrap_or_default();
        out.push(JsonDifference {
            path: format!("{path}/{index}"),
            before: older.map(|&i| before[i].clone()),
            after: newer.map(|&j| after[j].clone()),
        });
    }
    removed.clear();
    added.clear();
}

/// Escapes a member name as a JSON Pointer reference token.
fn pointer_token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn difference(path: &str, before: Option<Value>, after: Option<Value>) -> JsonDifference {
        JsonDifference {
            path: path.to_owned(),
            before,
            after,
        }
    }

    #[test]
    fn an_inserted_array_element_does_not_make_later_elements_differ() {
        let before = json!({"columns": [{"name": "id"}, {"name": "amount"}]});
        let after = json!({"columns": [{"name": "tenant"}, {"name": "id"}, {"name": "amount"}]});

        assert_eq!(
            json_differences(&before, &after),
            vec![difference(
                "/columns/0",
                None,
                Some(json!({"name": "tenant"}))
            )]
        );
    }

    #[test]
    fn a_changed_element_between_matched_ones_is_reported_with_both_versions() {
        let before = json!([1, {"validated": false}, 3]);
        let after = json!([1, {"validated": true}, 3]);

        assert_eq!(
            json_differences(&before, &after),
            vec![difference(
                "/1",
                Some(json!({"validated": false})),
                Some(json!({"validated": true}))
            )]
        );
    }

    #[test]
    fn object_members_are_compared_by_name_and_escaped_in_paths() {
        let before = json!({"a/b": 1, "same": true, "gone": "x"});
        let after = json!({"a/b": 2, "same": true, "new~": null});

        assert_eq!(
            json_differences(&before, &after),
            vec![
                difference("/a~1b", Some(json!(1)), Some(json!(2))),
                difference("/gone", Some(json!("x")), None),
                difference("/new~0", None, Some(json!(null))),
            ]
        );
    }

    #[test]
    fn removed_elements_beyond_the_added_ones_keep_their_older_positions() {
        let before = json!(["a", "b", "c", "d"]);
        let after = json!(["a", "x", "d"]);

        assert_eq!(
            json_differences(&before, &after),
            vec![
                difference("/1", Some(json!("b")), Some(json!("x"))),
                difference("/2", Some(json!("c")), None),
            ]
        );
    }
}
