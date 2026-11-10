//! Comparison of stored table metadata with a newer retrieval.
//!
//! Table metadata has one shape for every connector, so its comparison
//! depends on no connector, and every receiver gets the same report for one
//! source change. Information that could not be observed on either side is
//! reported as unknown, never as a removal or change.

use std::mem;

use analytics_types::AnalyticsType;

use super::{
    Column, ColumnType, Constraint, Reported, TableMetadata, TableRecord, UnsupportedReason,
};

/// What changed between stored table metadata and a newer retrieval of the
/// table the caller paired with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableComparison {
    /// At least one side does not exist or could not be read, so no items are
    /// compared. Only `NotFound` on the newer side indicates a drop.
    NotCompared {
        before: TableState,
        after: TableState,
    },
    /// Both sides were read.
    Compared {
        before: Completeness,
        after: Completeness,
        changes: TableChanges,
    },
}

/// The state of one side of a comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableState {
    Read(Completeness),
    NotFound,
    Unavailable,
}

/// Whether table metadata that was read has missing items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completeness {
    Complete,
    Incomplete,
}

/// The item differences between two reads of one table; unchanged items are
/// not listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableChanges {
    /// Differences in the table's own name, kind, and source identifier.
    /// Different source identifiers on both sides mean a drop and re-creation.
    pub table: Vec<Difference<TableAttribute>>,
    /// Column differences: removed and matched columns in the older ordinal
    /// order, then added columns in the newer ordinal order.
    pub columns: Vec<ColumnChange>,
    pub constraints: ConstraintChanges,
}

/// An attribute that changed, or whose change cannot be known because it was
/// not observed on both sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Difference<A> {
    Changed(A),
    Unknown(A),
}

/// A compared attribute of the table itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableAttribute {
    Name,
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

/// The difference for one column.
///
/// Columns are matched by source identifier when both sides have one, and
/// otherwise by name; the same name with different source identifiers is a
/// removal and an addition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnChange {
    Added(Column),
    Removed(Column),
    /// A matched column with at least one changed or unknown attribute; a
    /// changed name is a rename.
    Changed {
        before: Column,
        after: Column,
        attributes: Vec<Difference<ColumnAttribute>>,
    },
    /// A column present on one side only whose addition or removal cannot be
    /// known: the other side may have hidden columns, or an unmatched column
    /// there may be this column renamed.
    Unknown {
        column: Column,
        present_in: Side,
    },
}

/// Which side of a comparison holds an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Before,
    After,
}

/// The constraint differences of one table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConstraintChanges {
    /// Constraints matched by kind and name; empty when nothing changed.
    Listed(Vec<ConstraintChange>),
    /// Constraints are missing on either side, or offered on only one side.
    Unknown,
}

/// The difference for one constraint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConstraintChange {
    Added(Constraint),
    Removed(Constraint),
    Changed {
        before: Constraint,
        after: Constraint,
    },
}

impl TableRecord<TableMetadata> {
    /// Compares this stored record with `newer`, a later retrieval of the
    /// table the caller paired with it. Retrieval times are not compared.
    pub fn compare(&self, newer: &Self) -> TableComparison {
        let (
            TableRecord::Read {
                metadata: before, ..
            },
            TableRecord::Read {
                metadata: after, ..
            },
        ) = (self, newer)
        else {
            return TableComparison::NotCompared {
                before: self.state(),
                after: newer.state(),
            };
        };
        TableComparison::Compared {
            before: before.completeness(),
            after: after.completeness(),
            changes: before.compare(after),
        }
    }

    fn state(&self) -> TableState {
        match self {
            TableRecord::Read { metadata, .. } => TableState::Read(metadata.completeness()),
            TableRecord::NotFound { .. } => TableState::NotFound,
            TableRecord::Unavailable { .. } => TableState::Unavailable,
        }
    }
}

impl TableMetadata {
    /// Compares this read of a table with `newer`, a later read of the table
    /// the caller paired with it.
    ///
    /// Table metadata holds no retrieval time, so table metadata derived from
    /// restored source metadata is compared directly.
    pub fn compare(&self, newer: &Self) -> TableChanges {
        TableChanges {
            table: table_differences(self, newer),
            columns: column_changes(self, newer),
            constraints: constraint_changes(self.constraints(), newer.constraints()),
        }
    }

    fn completeness(&self) -> Completeness {
        if self.is_complete() {
            Completeness::Complete
        } else {
            Completeness::Incomplete
        }
    }
}

/// How a reported item differs: values are compared, items not offered on
/// either side are equal, and anything else was not observed on both sides.
fn reported_difference<T: PartialEq, A>(
    before: &Reported<T>,
    after: &Reported<T>,
    attribute: A,
) -> Option<Difference<A>> {
    match (before, after) {
        (Reported::Value(before), Reported::Value(after)) => {
            (before != after).then_some(Difference::Changed(attribute))
        }
        (Reported::NotOffered, Reported::NotOffered) => None,
        _ => Some(Difference::Unknown(attribute)),
    }
}

fn table_differences(
    before: &TableMetadata,
    after: &TableMetadata,
) -> Vec<Difference<TableAttribute>> {
    let mut differences = Vec::new();
    if before.table() != after.table() {
        differences.push(Difference::Changed(TableAttribute::Name));
    }
    if before.kind() != after.kind() {
        differences.push(Difference::Changed(TableAttribute::Kind));
    }
    differences.extend(reported_difference(
        before.source_id(),
        after.source_id(),
        TableAttribute::SourceId,
    ));
    differences
}

/// Column differences, never inferring an addition or removal that a renamed
/// or hidden column could explain.
///
/// An unmatched column is unknown rather than added or removed when the other
/// side may hide columns, or when an unmatched column on the other side cannot
/// be told apart by source identifier, because the two may be one renamed
/// column. When either side may hide columns, ordinals may shift without a
/// change at the source, so an ordinal difference is unknown.
fn column_changes(before: &TableMetadata, after: &TableMetadata) -> Vec<ColumnChange> {
    let matches = match_columns(before.columns(), after.columns());
    let mut matched_after = vec![false; after.columns().len()];
    for index in matches.iter().flatten() {
        matched_after[*index] = true;
    }
    let unmatched_before: Vec<&Column> = before
        .columns()
        .iter()
        .zip(&matches)
        .filter(|(_, matched)| matched.is_none())
        .map(|(column, _)| column)
        .collect();
    let unmatched_after: Vec<&Column> = after
        .columns()
        .iter()
        .zip(&matched_after)
        .filter(|(_, matched)| !**matched)
        .map(|(column, _)| column)
        .collect();
    let may_hide = before.columns_may_be_hidden() || after.columns_may_be_hidden();

    let mut changes = Vec::new();
    for (column, matched) in before.columns().iter().zip(&matches) {
        let change = match *matched {
            Some(index) => {
                let newer = &after.columns()[index];
                let mut attributes = column_differences(column, newer);
                if may_hide {
                    for difference in &mut attributes {
                        if *difference == Difference::Changed(ColumnAttribute::Ordinal) {
                            *difference = Difference::Unknown(ColumnAttribute::Ordinal);
                        }
                    }
                }
                if attributes.is_empty() {
                    continue;
                }
                ColumnChange::Changed {
                    before: column.clone(),
                    after: newer.clone(),
                    attributes,
                }
            }
            None if after.columns_may_be_hidden()
                || unmatched_after
                    .iter()
                    .any(|newer| !distinct_by_source_id(column, newer)) =>
            {
                ColumnChange::Unknown {
                    column: column.clone(),
                    present_in: Side::Before,
                }
            }
            None => ColumnChange::Removed(column.clone()),
        };
        changes.push(change);
    }
    for column in &unmatched_after {
        changes.push(
            if before.columns_may_be_hidden()
                || unmatched_before
                    .iter()
                    .any(|older| !distinct_by_source_id(older, column))
            {
                ColumnChange::Unknown {
                    column: (*column).clone(),
                    present_in: Side::After,
                }
            } else {
                ColumnChange::Added((*column).clone())
            },
        );
    }
    changes
}

/// Whether both columns have source identifiers, which then decide whether
/// they are the same column.
fn distinct_by_source_id(before: &Column, after: &Column) -> bool {
    matches!(
        (&before.source_id, &after.source_id),
        (Reported::Value(_), Reported::Value(_))
    )
}

/// For each older column, the index of the newer column it matches.
///
/// Source identifiers are matched first across all columns, so a name match
/// never takes a column that another column's identifier claims. A name match
/// is refused when both sides have different source identifiers.
fn match_columns(before: &[Column], after: &[Column]) -> Vec<Option<usize>> {
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
    for (column, matched) in before.iter().zip(matches.iter_mut()) {
        if let Reported::Value(id) = &column.source_id {
            *matched =
                claim(&|newer| matches!(&newer.source_id, Reported::Value(other) if other == id));
        }
    }
    for (column, matched) in before.iter().zip(matches.iter_mut()) {
        if matched.is_none() {
            *matched =
                claim(&|newer| newer.name == column.name && !distinct_by_source_id(column, newer));
        }
    }
    matches
}

fn column_differences(before: &Column, after: &Column) -> Vec<Difference<ColumnAttribute>> {
    let mut differences = Vec::new();
    if before.name != after.name {
        differences.push(Difference::Changed(ColumnAttribute::Name));
    }
    if before.ordinal != after.ordinal {
        differences.push(Difference::Changed(ColumnAttribute::Ordinal));
    }
    if before.nullable != after.nullable {
        differences.push(Difference::Changed(ColumnAttribute::Nullable));
    }
    differences.extend(type_differences(&before.ty, &after.ty));
    differences.extend(reported_difference(
        &before.source_id,
        &after.source_id,
        ColumnAttribute::SourceId,
    ));
    differences
}

/// The Data Source type and its mapping are compared separately, so a changed
/// mapping of an unchanged source type is visible as such. An unread type
/// leaves both unknown.
fn type_differences(before: &ColumnType, after: &ColumnType) -> Vec<Difference<ColumnAttribute>> {
    let (Some((before_type, before_mapping)), Some((after_type, after_mapping))) =
        (type_parts(before), type_parts(after))
    else {
        return vec![
            Difference::Unknown(ColumnAttribute::DataSourceType),
            Difference::Unknown(ColumnAttribute::AnalyticsType),
        ];
    };
    let mut differences = Vec::new();
    if before_type != after_type {
        differences.push(Difference::Changed(ColumnAttribute::DataSourceType));
    }
    if before_mapping != after_mapping {
        differences.push(Difference::Changed(ColumnAttribute::AnalyticsType));
    }
    differences
}

/// The Data Source type and the mapping outcome, or `None` when unread.
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
) -> ConstraintChanges {
    let (before, after) = match (before, after) {
        (Reported::Value(before), Reported::Value(after)) => (before, after),
        (Reported::NotOffered, Reported::NotOffered) => {
            return ConstraintChanges::Listed(Vec::new());
        }
        _ => return ConstraintChanges::Unknown,
    };
    let mut taken = vec![false; after.len()];
    let mut changes = Vec::new();
    for constraint in before {
        let matched = after.iter().zip(&taken).position(|(newer, taken)| {
            !taken
                && newer.name == constraint.name
                && mem::discriminant(&newer.kind) == mem::discriminant(&constraint.kind)
        });
        match matched {
            Some(index) => {
                taken[index] = true;
                let newer = &after[index];
                if newer != constraint {
                    changes.push(ConstraintChange::Changed {
                        before: constraint.clone(),
                        after: newer.clone(),
                    });
                }
            }
            None => changes.push(ConstraintChange::Removed(constraint.clone())),
        }
    }
    changes.extend(
        after
            .iter()
            .zip(&taken)
            .filter(|(_, taken)| !**taken)
            .map(|(constraint, _)| ConstraintChange::Added(constraint.clone())),
    );
    ConstraintChanges::Listed(changes)
}
