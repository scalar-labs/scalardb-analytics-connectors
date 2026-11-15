//! The C representation of comparison results.
//!
//! A comparison with no difference has no result; a result lists only what
//! differs, with the compared columns and constraints in the structures of
//! [`super::metadata`].

use connector_sdk::{
    Change, ColumnAttribute, ConstraintAttribute, JsonDifference, TableAttribute,
    TableMetadataDifferences,
};

use super::{
    metadata::{
        ScalardbAnalyticsConnectorColumn, ScalardbAnalyticsConnectorConstraint,
        ScalardbAnalyticsConnectorJson,
    },
    owned::Arena,
};

/// How a column or constraint differs between two observations.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorChangeKind(pub u32);

impl ScalardbAnalyticsConnectorChangeKind {
    /// Only the newer observation has it; `after` is set.
    pub const ADDED: Self = Self(1);
    /// Only the older observation has it; `before` is set.
    pub const REMOVED: Self = Self(2);
    /// Both have it with the listed attributes differing; both are set.
    pub const MATCHED: Self = Self(3);
}

/// A compared attribute of the table itself.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorTableAttribute(pub u32);

impl ScalardbAnalyticsConnectorTableAttribute {
    pub const KIND: Self = Self(1);
    pub const SOURCE_ID: Self = Self(2);
}

/// A compared attribute of a column.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorColumnAttribute(pub u32);

impl ScalardbAnalyticsConnectorColumnAttribute {
    pub const NAME: Self = Self(1);
    pub const ORDINAL: Self = Self(2);
    pub const NULLABLE: Self = Self(3);
    pub const DATA_SOURCE_TYPE: Self = Self(4);
    /// The Analytics type, or the reason the column is unsupported.
    pub const ANALYTICS_TYPE: Self = Self(5);
    pub const SOURCE_ID: Self = Self(6);
}

/// A compared attribute of a constraint, beyond its kind and name, which
/// match it.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorConstraintAttribute(pub u32);

impl ScalardbAnalyticsConnectorConstraintAttribute {
    pub const ENFORCED: Self = Self(1);
    pub const COLUMNS: Self = Self(2);
    pub const UNIQUE_KIND: Self = Self(3);
    pub const CONDITION: Self = Self(4);
    pub const REFERENCED_TABLE: Self = Self(5);
    pub const REFERENCED_COLUMNS: Self = Self(6);
    pub const EXPRESSION: Self = Self(7);
}

/// How one column differs. `attributes` is set for `MATCHED`.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorColumnChange {
    pub change: ScalardbAnalyticsConnectorChangeKind,
    pub before: *const ScalardbAnalyticsConnectorColumn,
    pub after: *const ScalardbAnalyticsConnectorColumn,
    pub attributes: *const ScalardbAnalyticsConnectorColumnAttribute,
    pub attribute_count: usize,
}

/// How one constraint differs. `attributes` is set for `MATCHED`.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorConstraintChange {
    pub change: ScalardbAnalyticsConnectorChangeKind,
    pub before: *const ScalardbAnalyticsConnectorConstraint,
    pub after: *const ScalardbAnalyticsConnectorConstraint,
    pub attributes: *const ScalardbAnalyticsConnectorConstraintAttribute,
    pub attribute_count: usize,
}

/// The differences between two observations of one table's table metadata.
///
/// Removed and matched columns come in the older ordinal order, then added
/// columns in the newer one; constraints likewise in their order. Freed with
/// `table_metadata_differences_free` when
/// returned by `table_metadata_compare`.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorTableMetadataDifferences {
    pub table_attributes: *const ScalardbAnalyticsConnectorTableAttribute,
    pub table_attribute_count: usize,
    pub columns: *const ScalardbAnalyticsConnectorColumnChange,
    pub column_count: usize,
    pub constraints: *const ScalardbAnalyticsConnectorConstraintChange,
    pub constraint_count: usize,
}

/// One difference between two JSON documents.
///
/// `path` is a JSON Pointer. A value only the older document holds, or one
/// both hold, is located in the older document; a value only the newer
/// document holds is located in the newer one. An absent side has a null
/// `text.data`.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorJsonDifference {
    pub path: super::ScalardbAnalyticsConnectorString,
    pub before: ScalardbAnalyticsConnectorJson,
    pub after: ScalardbAnalyticsConnectorJson,
}

/// The differences between two observations of one table's source metadata
/// documents. Freed with `source_metadata_differences_free`.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorSourceMetadataDifferences {
    pub differences: *const ScalardbAnalyticsConnectorJsonDifference,
    pub difference_count: usize,
}

impl Arena {
    pub(super) fn table_metadata_differences(
        &mut self,
        differences: &TableMetadataDifferences,
    ) -> ScalardbAnalyticsConnectorTableMetadataDifferences {
        let table = differences
            .table
            .iter()
            .map(|attribute| match attribute {
                TableAttribute::Kind => ScalardbAnalyticsConnectorTableAttribute::KIND,
                TableAttribute::SourceId => ScalardbAnalyticsConnectorTableAttribute::SOURCE_ID,
            })
            .collect();
        let (table_attributes, table_attribute_count) = self.array(table);
        let columns = differences
            .columns
            .iter()
            .map(|change| {
                let (change, before, after, attributes) =
                    self.change(change, Arena::column, column_attribute);
                ScalardbAnalyticsConnectorColumnChange {
                    change,
                    before,
                    after,
                    attributes: attributes.0,
                    attribute_count: attributes.1,
                }
            })
            .collect();
        let (columns, column_count) = self.array(columns);
        let constraints = differences
            .constraints
            .iter()
            .map(|change| {
                let (change, before, after, attributes) =
                    self.change(change, Arena::constraint, constraint_attribute);
                ScalardbAnalyticsConnectorConstraintChange {
                    change,
                    before,
                    after,
                    attributes: attributes.0,
                    attribute_count: attributes.1,
                }
            })
            .collect();
        let (constraints, constraint_count) = self.array(constraints);
        ScalardbAnalyticsConnectorTableMetadataDifferences {
            table_attributes,
            table_attribute_count,
            columns,
            column_count,
            constraints,
            constraint_count,
        }
    }

    pub(super) fn source_metadata_differences(
        &mut self,
        differences: &[JsonDifference],
    ) -> ScalardbAnalyticsConnectorSourceMetadataDifferences {
        let differences = differences
            .iter()
            .map(|difference| self.json_difference(difference))
            .collect();
        let (differences, difference_count) = self.array(differences);
        ScalardbAnalyticsConnectorSourceMetadataDifferences {
            differences,
            difference_count,
        }
    }

    fn json_difference(
        &mut self,
        difference: &JsonDifference,
    ) -> ScalardbAnalyticsConnectorJsonDifference {
        let mut json = |value: Option<&serde_json::Value>| ScalardbAnalyticsConnectorJson {
            text: self.optional_text(value.map(ToString::to_string).as_deref()),
        };
        let before = json(difference.before.as_ref());
        let after = json(difference.after.as_ref());
        ScalardbAnalyticsConnectorJsonDifference {
            path: self.text(&difference.path),
            before,
            after,
        }
    }

    /// Writes one change: its kind, the views of both sides, and the
    /// attributes that differ.
    #[allow(clippy::type_complexity)]
    fn change<T, A, V: 'static, W: 'static>(
        &mut self,
        change: &Change<T, A>,
        view: fn(&mut Self, &T) -> V,
        attribute: fn(&A) -> W,
    ) -> (
        ScalardbAnalyticsConnectorChangeKind,
        *const V,
        *const V,
        (*const W, usize),
    ) {
        match change {
            Change::Added(after) => {
                let after = view(self, after);
                (
                    ScalardbAnalyticsConnectorChangeKind::ADDED,
                    std::ptr::null(),
                    self.one(after),
                    (std::ptr::null(), 0),
                )
            }
            Change::Removed(before) => {
                let before = view(self, before);
                (
                    ScalardbAnalyticsConnectorChangeKind::REMOVED,
                    self.one(before),
                    std::ptr::null(),
                    (std::ptr::null(), 0),
                )
            }
            Change::Matched {
                before,
                after,
                attributes,
            } => {
                let (before, after) = (view(self, before), view(self, after));
                let attributes = attributes.iter().map(attribute).collect();
                (
                    ScalardbAnalyticsConnectorChangeKind::MATCHED,
                    self.one(before),
                    self.one(after),
                    self.array(attributes),
                )
            }
        }
    }
}

fn column_attribute(attribute: &ColumnAttribute) -> ScalardbAnalyticsConnectorColumnAttribute {
    use ScalardbAnalyticsConnectorColumnAttribute as C;
    match attribute {
        ColumnAttribute::Name => C::NAME,
        ColumnAttribute::Ordinal => C::ORDINAL,
        ColumnAttribute::Nullable => C::NULLABLE,
        ColumnAttribute::DataSourceType => C::DATA_SOURCE_TYPE,
        ColumnAttribute::AnalyticsType => C::ANALYTICS_TYPE,
        ColumnAttribute::SourceId => C::SOURCE_ID,
    }
}

fn constraint_attribute(
    attribute: &ConstraintAttribute,
) -> ScalardbAnalyticsConnectorConstraintAttribute {
    use ScalardbAnalyticsConnectorConstraintAttribute as C;
    match attribute {
        ConstraintAttribute::Enforced => C::ENFORCED,
        ConstraintAttribute::Columns => C::COLUMNS,
        ConstraintAttribute::UniqueKind => C::UNIQUE_KIND,
        ConstraintAttribute::Condition => C::CONDITION,
        ConstraintAttribute::ReferencedTable => C::REFERENCED_TABLE,
        ConstraintAttribute::ReferencedColumns => C::REFERENCED_COLUMNS,
        ConstraintAttribute::Expression => C::EXPRESSION,
    }
}
