//! MySQL source metadata for one table and the table metadata derived from it
//! and the Data Source's settings.

use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Reported, TableKind, TableMetadata,
    TableReference, UniqueKind,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::type_mapping::map_type;
use crate::MysqlSettings;

/// What `information_schema` shows about one table to the reading account.
///
/// It is the source metadata document. `information_schema` silently omits
/// columns the account holds no privilege on, so the document records whether
/// the read could rule that out.
///
/// The document describes one table, so its top level states the table. Each
/// fact appears once, with the table or column it belongs to. Besides the
/// facts table metadata derives from, it carries what states a table's
/// meaning, such as comments and view definitions, for the Semantic Catalog
/// to interpret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MysqlTable {
    /// `TABLES.TABLE_SCHEMA`, the database.
    pub schema: String,
    pub name: String,
    pub kind: MysqlTableKind,
    /// `TABLES.TABLE_COMMENT`; `None` when it is empty, as it is for views.
    pub comment: Option<String>,
    /// A view's query as `VIEWS.VIEW_DEFINITION` prints it; `None` for a base
    /// table, and for a view whose definition the account may not see.
    pub definition: Option<String>,
    /// Whether some columns may be missing: the account holds no `SELECT` on
    /// the table at the global, database, or table level, so it may see only
    /// the columns it holds column privileges on.
    pub columns_may_be_hidden: bool,
    /// Columns in `ORDINAL_POSITION` order.
    pub columns: Vec<MysqlColumn>,
    pub primary_key: Option<MysqlPrimaryKey>,
    /// Unique indexes other than the primary key, in name order.
    pub unique_keys: Vec<MysqlUniqueKey>,
    /// Foreign keys in name order.
    pub foreign_keys: Vec<MysqlForeignKey>,
    /// CHECK constraints in name order.
    pub checks: Vec<MysqlCheck>,
}

/// `TABLES.TABLE_TYPE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum MysqlTableKind {
    #[serde(rename = "BASE TABLE")]
    BaseTable,
    #[serde(rename = "VIEW")]
    View,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MysqlColumn {
    pub name: String,
    /// `ORDINAL_POSITION`, from 1.
    pub ordinal_position: u32,
    /// `IS_NULLABLE`.
    pub nullable: bool,
    /// `COLUMN_TYPE`, the Data Source type, such as `int unsigned`,
    /// `decimal(10,2)`, or `enum('a','b')`.
    pub data_source_type: String,
    /// `DATA_TYPE`, the type's name without parameters or attributes, such as
    /// `int` or `decimal`.
    pub data_type: String,
    /// `CHARACTER_SET_NAME`, for a character column.
    pub character_set: Option<String>,
    /// `COLLATION_NAME`, for a character column.
    pub collation: Option<String>,
    /// `COLUMN_COMMENT`; `None` when it is empty.
    pub comment: Option<String>,
    /// Where MySQL takes the column's value from, other than the row itself;
    /// `None` for a column with no default, `AUTO_INCREMENT`, or generation
    /// expression.
    pub value_source: Option<MysqlValueSource>,
}

/// Where MySQL takes a column's value from, other than the row itself, from
/// `COLUMN_DEFAULT`, `EXTRA`, and `GENERATION_EXPRESSION`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MysqlValueSource {
    /// A literal `DEFAULT` value, as `COLUMN_DEFAULT` prints it.
    Default { value: String },
    /// A `DEFAULT` expression, such as `CURRENT_TIMESTAMP` or `(rand())`.
    DefaultExpression { expression: String },
    /// An `AUTO_INCREMENT` column.
    AutoIncrement,
    /// A generated column, always computed by `expression`.
    Generated {
        generation: MysqlGeneration,
        expression: String,
    },
}

/// How a generated column is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MysqlGeneration {
    Stored,
    Virtual,
}

/// The primary key, which MySQL always names `PRIMARY`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MysqlPrimaryKey {
    pub columns: Vec<String>,
}

/// A unique index. MySQL implements every unique constraint as one and
/// always enforces it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MysqlUniqueKey {
    pub name: String,
    /// The key parts in order.
    pub keys: Vec<MysqlKey>,
}

/// One key part of a unique index, from `STATISTICS`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MysqlKey {
    /// A column, or its leading `prefix_length` characters or bytes.
    Column {
        column: String,
        prefix_length: Option<u32>,
    },
    /// A functional key part, as `STATISTICS.EXPRESSION` prints it.
    Expression { expression: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MysqlForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced: MysqlReferencedTable,
}

/// The table and columns a foreign key references.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MysqlReferencedTable {
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
}

/// A CHECK constraint of the table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MysqlCheck {
    pub name: String,
    /// `CHECK_CONSTRAINTS.CHECK_CLAUSE`.
    pub expression: String,
    /// `TABLE_CONSTRAINTS.ENFORCED`: false for a `NOT ENFORCED` constraint.
    pub enforced: bool,
}

impl MysqlColumn {
    /// The column as table metadata states it, mapped with the Data Source's
    /// `settings`. MySQL offers no column identifiers other than names.
    fn to_column(&self, settings: &MysqlSettings) -> Column {
        let data_source_type = self.data_source_type.clone();
        let ty = match map_type(self, settings) {
            Ok(analytics_type) => ColumnType::Mapped {
                data_source_type,
                analytics_type,
            },
            Err(reason) => ColumnType::Unsupported {
                data_source_type,
                reason,
            },
        };
        Column {
            name: self.name.clone(),
            ordinal: self.ordinal_position,
            nullable: self.nullable,
            source_id: Reported::NotOffered,
            ty,
        }
    }
}

impl MysqlTable {
    pub(crate) fn reference(&self) -> TableReference {
        TableReference::new(vec![self.schema.clone(), self.name.clone()])
            .expect("MySQL database and table names are never empty")
    }

    /// The table's constraints as table metadata states them. A unique index
    /// with a functional key part is not a column constraint and stays in
    /// the source metadata only; a column prefix that is unique makes its
    /// column unique too.
    fn table_constraints(&self) -> Vec<Constraint> {
        let primary_key = self.primary_key.iter().map(|key| Constraint {
            name: "PRIMARY".to_owned(),
            enforced: true,
            kind: ConstraintKind::PrimaryKey {
                columns: key.columns.clone(),
            },
        });
        let unique_keys = self.unique_keys.iter().filter_map(|key| {
            let columns = key
                .keys
                .iter()
                .map(|key| match key {
                    MysqlKey::Column { column, .. } => Some(column.clone()),
                    MysqlKey::Expression { .. } => None,
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Constraint {
                name: key.name.clone(),
                enforced: true,
                kind: ConstraintKind::Unique {
                    columns,
                    kind: UniqueKind::Constraint,
                    condition: None,
                },
            })
        });
        let foreign_keys = self.foreign_keys.iter().filter_map(|key| {
            let referenced_table = TableReference::new(vec![
                key.referenced.schema.clone(),
                key.referenced.table.clone(),
            ])
            .ok()?;
            Some(Constraint {
                name: key.name.clone(),
                enforced: true,
                kind: ConstraintKind::ForeignKey {
                    columns: key.columns.clone(),
                    referenced_table,
                    referenced_columns: key.referenced.columns.clone(),
                },
            })
        });
        let checks = self.checks.iter().map(|check| Constraint {
            name: check.name.clone(),
            enforced: check.enforced,
            kind: ConstraintKind::Check {
                expression: check.expression.clone(),
            },
        });
        primary_key
            .chain(unique_keys)
            .chain(foreign_keys)
            .chain(checks)
            .collect()
    }

    /// Derives the table's table metadata, mapping its column types with the
    /// Data Source's `settings`.
    pub(crate) fn to_table_metadata(&self, settings: &MysqlSettings) -> TableMetadata {
        let kind = match self.kind {
            MysqlTableKind::BaseTable => TableKind::Table,
            MysqlTableKind::View => TableKind::View,
        };
        TableMetadata::new(
            self.reference(),
            kind,
            Reported::NotOffered,
            self.columns
                .iter()
                .map(|column| column.to_column(settings))
                .collect(),
            self.columns_may_be_hidden,
            Reported::Value(self.table_constraints()),
        )
    }
}
