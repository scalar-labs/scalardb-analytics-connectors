//! SQL Server source metadata for one table and the table metadata derived from it.

use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Reported, TableKind, TableMetadata,
    TableReference, UniqueKind,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What the `sys` catalog views state about one table or view.
///
/// It is the source metadata document. Besides the facts table metadata
/// derives from, it carries what states a table's meaning, such as comments,
/// view definitions, and column expressions, for the Semantic Catalog to
/// interpret. Expressions and definitions are as SQL Server stores them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SqlServerTable {
    pub namespace: String,
    pub name: String,
    /// `sys.objects.object_id`.
    pub object_id: i32,
    pub kind: SqlServerTableKind,
    /// Whether the reading credential has `VIEW DEFINITION` on the table.
    ///
    /// Without it, SQL Server hides the table's constraints and every
    /// definition and expression, so they are not known. Every column of a
    /// table the credential can see is shown, whatever its column
    /// permissions.
    pub definition_visible: bool,
    /// The `MS_Description` extended property of the table.
    pub comment: Option<String>,
    /// A view's definition; `None` for a table or an encrypted view.
    pub definition: Option<String>,
    /// Columns in `column_id` order.
    pub columns: Vec<SqlServerColumn>,
    pub primary_key: Option<SqlServerKey>,
    /// Unique constraints and the unique indexes that back no constraint, in
    /// name order.
    pub unique_keys: Vec<SqlServerUniqueKey>,
    /// Foreign keys in name order.
    pub foreign_keys: Vec<SqlServerForeignKey>,
    /// CHECK constraints in name order.
    pub checks: Vec<SqlServerCheck>,
}

/// The kind of object, from `sys.objects.type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SqlServerTableKind {
    Table,
    View,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SqlServerColumn {
    pub name: String,
    /// `sys.columns.column_id`: the column's number within its table, which
    /// identifies it across renames.
    pub column_id: i32,
    /// `sys.columns.is_nullable`.
    pub nullable: bool,
    /// The type the column is declared with; `None` for an alias type the
    /// credential has no permission on, which SQL Server hides.
    #[serde(rename = "type")]
    pub ty: Option<SqlServerType>,
    /// `sys.columns.max_length`: the length in bytes, or `-1` for `(max)`.
    pub max_length: i16,
    /// `sys.columns.precision`: the precision of a numeric or date and time
    /// type, such as the total number of digits of a `decimal`; otherwise
    /// `0`.
    pub precision: u8,
    /// `sys.columns.scale`: the scale of a numeric or date and time type, such
    /// as the digits after the decimal point of a `decimal`; otherwise `0`.
    pub scale: u8,
    /// The `MS_Description` extended property of the column.
    pub comment: Option<String>,
    /// Where SQL Server takes the column's value from, other than the row
    /// itself; `None` for a column whose value comes only from the row.
    pub value_source: Option<SqlServerValueSource>,
}

/// A column's declared type, from `sys.types`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SqlServerType {
    pub name: String,
    /// The schema the type is defined in, such as `sys`.
    pub schema: String,
    /// `sys.types.is_user_defined`: an alias type or CLR type a user created.
    pub user_defined: bool,
    /// The system type the type is: itself for a system type, the one an
    /// alias type is created from, or `None` for a CLR type such as
    /// `hierarchyid`.
    pub system_type: Option<String>,
}

/// Where SQL Server takes a column's value from, other than the row itself;
/// the three are exclusive for one column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SqlServerValueSource {
    /// A default constraint, used when a row omits the column.
    Default { expression: Option<String> },
    /// An identity column, numbered by SQL Server.
    Identity,
    /// A computed column, always computed by `expression`; `persisted` when
    /// its values are stored.
    Computed {
        persisted: bool,
        expression: Option<String>,
    },
}

/// A primary key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SqlServerKey {
    pub name: String,
    /// The key's columns in order.
    pub columns: Vec<String>,
    /// False when its index is disabled, so SQL Server does not enforce it.
    pub enforced: bool,
}

/// A unique constraint, or a unique index that backs no constraint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SqlServerUniqueKey {
    pub name: String,
    pub kind: SqlServerUniqueKind,
    /// The key's columns in order, without included columns.
    pub columns: Vec<String>,
    /// A filtered index's condition; `None` when the index has none or the
    /// condition is not visible.
    pub condition: Option<String>,
    /// Whether the index has a filter, even when its condition is not visible.
    pub filtered: bool,
    /// False when its index is disabled.
    pub enforced: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SqlServerUniqueKind {
    Constraint,
    Index,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SqlServerForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    /// The referenced table; `None` when the credential cannot see it, so
    /// the catalog names neither it nor its columns.
    pub referenced: Option<SqlServerReferencedTable>,
    /// `sys.foreign_keys.is_disabled`, negated.
    pub enforced: bool,
    /// `sys.foreign_keys.is_not_trusted`, negated: false when existing rows
    /// were not checked.
    pub validated: bool,
}

/// The table and columns a foreign key references.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SqlServerReferencedTable {
    pub namespace: String,
    pub table: String,
    pub columns: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SqlServerCheck {
    pub name: String,
    /// The expression as SQL Server stores it; `None` when it is not visible.
    pub expression: Option<String>,
    /// `sys.check_constraints.is_disabled`, negated.
    pub enforced: bool,
    /// `sys.check_constraints.is_not_trusted`, negated.
    pub validated: bool,
}

/// Why a column's Data Source type is not read.
const TYPE_HIDDEN: &str = "SQL Server hides an alias type the credential has no permission on";

/// Why constraints of a table without `VIEW DEFINITION` are missing.
const CONSTRAINTS_HIDDEN: &str =
    "SQL Server hides constraints without VIEW DEFINITION on the table";

impl SqlServerTable {
    pub(crate) fn reference(&self) -> TableReference {
        TableReference::new(vec![self.namespace.clone(), self.name.clone()])
            .expect("SQL Server schema and object names are never empty")
    }

    /// A column as table metadata states it; `ordinal` is its 1-based
    /// position, because `column_id` keeps the gaps of dropped columns.
    fn table_column(column: &SqlServerColumn, ordinal: u32) -> Column {
        let ty = match &column.ty {
            None => ColumnType::Unread {
                reason: TYPE_HIDDEN.to_owned(),
            },
            Some(ty) => {
                let data_source_type = column.data_source_type(ty);
                match column.map_type(ty) {
                    Ok(analytics_type) => ColumnType::Mapped {
                        data_source_type,
                        analytics_type,
                    },
                    Err(reason) => ColumnType::Unsupported {
                        data_source_type,
                        reason,
                    },
                }
            }
        };
        Column {
            name: column.name.clone(),
            ordinal,
            nullable: column.nullable,
            source_id: Reported::Value(column.column_id.to_string()),
            ty,
        }
    }

    /// The table's constraints as table metadata states them.
    ///
    /// They are missing without `VIEW DEFINITION`, and so are they when a
    /// filtered unique index's condition, a CHECK expression, or a foreign
    /// key's referenced table is not visible: uniqueness without its
    /// condition would mislead.
    fn table_constraints(&self) -> Reported<Vec<Constraint>> {
        let hidden = || Reported::Missing {
            reason: CONSTRAINTS_HIDDEN.to_owned(),
        };
        if !self.definition_visible {
            return hidden();
        }
        let mut constraints: Vec<Constraint> = self
            .primary_key
            .iter()
            .map(|key| Constraint {
                name: key.name.clone(),
                enforced: key.enforced,
                kind: ConstraintKind::PrimaryKey {
                    columns: key.columns.clone(),
                },
            })
            .collect();
        for key in &self.unique_keys {
            if key.filtered && key.condition.is_none() {
                return hidden();
            }
            constraints.push(Constraint {
                name: key.name.clone(),
                enforced: key.enforced,
                kind: ConstraintKind::Unique {
                    columns: key.columns.clone(),
                    kind: match key.kind {
                        SqlServerUniqueKind::Constraint => UniqueKind::Constraint,
                        SqlServerUniqueKind::Index => UniqueKind::Index,
                    },
                    condition: key.condition.clone(),
                },
            });
        }
        for key in &self.foreign_keys {
            let Some(referenced) = &key.referenced else {
                return hidden();
            };
            let referenced_table =
                TableReference::new(vec![referenced.namespace.clone(), referenced.table.clone()])
                    .expect("SQL Server schema and object names are never empty");
            constraints.push(Constraint {
                name: key.name.clone(),
                enforced: key.enforced,
                kind: ConstraintKind::ForeignKey {
                    columns: key.columns.clone(),
                    referenced_table,
                    referenced_columns: referenced.columns.clone(),
                },
            });
        }
        for check in &self.checks {
            let Some(expression) = &check.expression else {
                return hidden();
            };
            constraints.push(Constraint {
                name: check.name.clone(),
                enforced: check.enforced,
                kind: ConstraintKind::Check {
                    expression: expression.clone(),
                },
            });
        }
        Reported::Value(constraints)
    }
}

impl SqlServerTable {
    /// Derives the table's table metadata. No setting of a SQL Server Data
    /// Source changes it.
    pub fn to_table_metadata(&self) -> TableMetadata {
        let kind = match self.kind {
            SqlServerTableKind::Table => TableKind::Table,
            SqlServerTableKind::View => TableKind::View,
        };
        TableMetadata::new(
            self.reference(),
            kind,
            Reported::Value(self.object_id.to_string()),
            self.columns
                .iter()
                .zip(1..)
                .map(|(column, ordinal)| Self::table_column(column, ordinal))
                .collect(),
            false,
            self.table_constraints(),
        )
    }
}
