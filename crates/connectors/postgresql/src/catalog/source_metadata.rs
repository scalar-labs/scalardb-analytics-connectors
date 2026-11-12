//! PostgreSQL source metadata for one table and the table metadata derived from it.

use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Reported, SourceTableMetadata, TableKind,
    TableMetadata, TableReference, UniqueKind,
};
use serde::{Deserialize, Serialize};

use super::type_mapping::{BaseType, map_type};

/// What `pg_catalog` states about one table, as the catalog query returns it.
///
/// It is the source metadata document. `pg_catalog` hides no rows from a role
/// without privileges, so every column and constraint is present.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresTable {
    pub namespace: String,
    pub name: String,
    /// `pg_class.oid` as decimal text.
    pub oid: String,
    /// `pg_class.relkind`.
    pub relkind: String,
    /// Columns in `attnum` order, without dropped columns.
    pub columns: Vec<PostgresColumn>,
    /// Primary key, unique, foreign key, and CHECK constraints in name order.
    pub constraints: Vec<PostgresConstraint>,
    /// Unique indexes that do not back a constraint, in name order.
    pub unique_indexes: Vec<PostgresUniqueIndex>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresColumn {
    pub name: String,
    pub attnum: i16,
    pub not_null: bool,
    /// `format_type(atttypid, atttypmod)`, the Data Source type.
    pub data_source_type: String,
    pub base_type: BaseType,
    /// Whether the column's type is a domain over `base_type`.
    pub domain: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresConstraint {
    pub name: String,
    /// `pg_constraint.contype`: `p`, `u`, `f`, or `c`.
    pub contype: String,
    /// `pg_constraint.conenforced`; true before PostgreSQL 18, which enforces
    /// every constraint.
    pub enforced: bool,
    /// `pg_constraint.convalidated`: false for a `NOT VALID` constraint that
    /// existing rows may violate.
    pub validated: bool,
    pub columns: Vec<String>,
    pub referenced_namespace: Option<String>,
    pub referenced_table: Option<String>,
    pub referenced_columns: Option<Vec<String>>,
    /// The CHECK expression as `pg_get_expr` prints it.
    pub expression: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresUniqueIndex {
    pub name: String,
    /// Key columns in order; empty when a key is an expression.
    pub columns: Vec<String>,
    /// Whether some key is an expression rather than a column.
    pub has_expressions: bool,
    /// A partial index's predicate as `pg_get_expr` prints it.
    pub condition: Option<String>,
}

impl PostgresTable {
    pub(crate) fn reference(&self) -> TableReference {
        TableReference::new(vec![self.namespace.clone(), self.name.clone()])
            .expect("PostgreSQL namespace and table names are never empty")
    }
}

impl PostgresColumn {
    /// The column's Data Source type and the Analytics type the mapping gives it.
    pub(crate) fn column_type(&self) -> ColumnType {
        match map_type(&self.base_type) {
            Ok(analytics_type) => ColumnType::Mapped {
                data_source_type: self.data_source_type.clone(),
                analytics_type,
            },
            Err(reason) => ColumnType::Unsupported {
                data_source_type: self.data_source_type.clone(),
                reason,
            },
        }
    }
}

impl SourceTableMetadata for PostgresTable {
    fn to_table_metadata(&self) -> TableMetadata {
        let kind = match self.relkind.as_str() {
            "r" | "p" => TableKind::Table,
            "v" => TableKind::View,
            "f" => TableKind::External,
            "m" => TableKind::Other("materialized view".to_owned()),
            other => TableKind::Other(other.to_owned()),
        };
        let columns = self
            .columns
            .iter()
            .zip(1..)
            .map(|(column, ordinal)| Column {
                name: column.name.clone(),
                ordinal,
                nullable: !column.not_null,
                source_id: Reported::Value(column.attnum.to_string()),
                ty: column.column_type(),
            })
            .collect();
        let constraints = self
            .constraints
            .iter()
            .filter_map(PostgresConstraint::to_constraint)
            .chain(
                self.unique_indexes
                    .iter()
                    .filter_map(PostgresUniqueIndex::to_constraint),
            )
            .collect();
        TableMetadata::new(
            self.reference(),
            kind,
            Reported::Value(self.oid.clone()),
            columns,
            false,
            Reported::Value(constraints),
        )
    }
}

impl PostgresConstraint {
    fn to_constraint(&self) -> Option<Constraint> {
        let columns = self.columns.clone();
        let kind = match self.contype.as_str() {
            "p" => ConstraintKind::PrimaryKey { columns },
            "u" => ConstraintKind::Unique {
                columns,
                kind: UniqueKind::Constraint,
                condition: None,
            },
            "f" => ConstraintKind::ForeignKey {
                columns,
                referenced_table: TableReference::new(vec![
                    self.referenced_namespace.clone()?,
                    self.referenced_table.clone()?,
                ])
                .ok()?,
                referenced_columns: self.referenced_columns.clone()?,
            },
            "c" => ConstraintKind::Check {
                expression: self.expression.clone()?,
            },
            _ => return None,
        };
        Some(Constraint {
            name: self.name.clone(),
            enforced: self.enforced,
            kind,
        })
    }
}

impl PostgresUniqueIndex {
    /// A unique index over columns; uniqueness over expressions is not a
    /// column constraint and stays in the source metadata only.
    fn to_constraint(&self) -> Option<Constraint> {
        if self.has_expressions {
            return None;
        }
        Some(Constraint {
            name: self.name.clone(),
            enforced: true,
            kind: ConstraintKind::Unique {
                columns: self.columns.clone(),
                kind: UniqueKind::Index,
                condition: self.condition.clone(),
            },
        })
    }
}
