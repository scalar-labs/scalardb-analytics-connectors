//! Oracle source metadata for one table and the table metadata derived from it.

use connector_sdk::{
    Constraint, ConstraintKind, Reported, TableKind, TableMetadata, TableReference, UniqueKind,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What the `ALL_*` data dictionary views state about one table or view.
///
/// It is the source metadata document. Besides the facts table metadata
/// derives from, it carries what states a table's meaning, such as comments,
/// view definitions, and column expressions, for the Semantic Catalog to
/// interpret. Expressions and definitions are as Oracle stores them.
/// Constraints and unique indexes that involve a hidden column, which
/// `columns` does not list, are left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OracleTable {
    /// The owner, the schema the table belongs to.
    pub namespace: String,
    pub name: String,
    /// `ALL_OBJECTS.OBJECT_ID`, which identifies the table across renames.
    pub object_id: i64,
    pub kind: OracleTableKind,
    /// The table's comment, from `ALL_TAB_COMMENTS`.
    pub comment: Option<String>,
    /// A view's definition, from `ALL_VIEWS.TEXT`; `None` for a table.
    pub definition: Option<OracleLongText>,
    /// Columns in `COLUMN_ID` order.
    pub columns: Vec<OracleColumn>,
    pub primary_key: Option<OracleKey>,
    /// Unique constraints and the unique indexes on columns that back no
    /// constraint, in name order.
    pub unique_keys: Vec<OracleUniqueKey>,
    /// Foreign keys in name order.
    pub foreign_keys: Vec<OracleForeignKey>,
    /// CHECK constraints in name order, without the NOT NULL constraints that
    /// the columns' nullability states, nor a CHECK constraint written as
    /// `CHECK ("X" IS NOT NULL)` on a column that is not nullable, which is
    /// redundant. A NOT NULL constraint that is disabled or not validated
    /// leaves its column nullable and is among them.
    pub checks: Vec<OracleCheck>,
}

/// The kind of object: a table in `ALL_TABLES` or a view in `ALL_VIEWS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OracleTableKind {
    Table,
    View,
}

/// A column, from `ALL_TAB_COLS`, without hidden columns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OracleColumn {
    pub name: String,
    /// `COLUMN_ID`: the column's position. Oracle renumbers the columns after
    /// a dropped one, so it does not identify a column across changes.
    pub column_id: i64,
    /// `NULLABLE`: false when an enabled and validated NOT NULL constraint
    /// holds.
    pub nullable: bool,
    /// `DATA_TYPE`: the type's name, such as `NUMBER`, `VARCHAR2`, or
    /// `TIMESTAMP(6) WITH TIME ZONE`, which includes a timestamp's fractional
    /// second precision.
    pub data_type: String,
    /// `DATA_TYPE_OWNER`: the schema of an object type, such as `MDSYS` for
    /// `SDO_GEOMETRY`; `None` for a built-in type.
    pub data_type_owner: Option<String>,
    /// `DATA_LENGTH`: the length in bytes.
    pub data_length: i64,
    /// `DATA_PRECISION`: the decimal precision of a `NUMBER` or the binary
    /// precision of a `FLOAT`; `None` when it is not declared.
    pub data_precision: Option<i64>,
    /// `DATA_SCALE`: the scale of a `NUMBER`, or the fractional second
    /// precision of a timestamp or interval; `None` when it is not declared.
    pub data_scale: Option<i64>,
    /// `CHAR_LENGTH`: the declared length of a character type, in the unit
    /// `char_used` names.
    pub char_length: i64,
    /// `CHAR_USED`: whether the length of a `CHAR` or `VARCHAR2` counts
    /// characters (`C`) or bytes (`B`); `None` for other types.
    pub char_used: Option<String>,
    /// The column's comment, from `ALL_COL_COMMENTS`.
    pub comment: Option<String>,
    /// Where Oracle takes the column's value from, other than the row itself;
    /// `None` for a column whose value comes only from the row.
    pub value_source: Option<OracleValueSource>,
}

/// Where Oracle takes a column's value from, other than the row itself; the
/// three are exclusive for one column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OracleValueSource {
    /// A default, used when a row omits the column, and with `on_null` also
    /// when it gives NULL.
    Default {
        expression: OracleLongText,
        on_null: bool,
    },
    /// An identity column, numbered by Oracle.
    Identity,
    /// A virtual column, computed by `expression` and not stored.
    Virtual { expression: OracleLongText },
}

/// A primary key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OracleKey {
    pub name: String,
    /// The key's columns in order.
    pub columns: Vec<String>,
    /// `STATUS` is `ENABLED`.
    pub enforced: bool,
    /// `VALIDATED`: false when existing rows were not checked.
    pub validated: bool,
}

/// A unique constraint, or a unique index on columns that backs no constraint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OracleUniqueKey {
    pub name: String,
    pub kind: OracleUniqueKind,
    /// The key's columns in order.
    pub columns: Vec<String>,
    /// A constraint's `STATUS` is `ENABLED`; always true for an index.
    pub enforced: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OracleUniqueKind {
    Constraint,
    Index,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OracleForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    /// The referenced table; `None` when the credential cannot see the
    /// constraint it references, so the catalog names neither the table nor
    /// its columns.
    pub referenced: Option<OracleReferencedTable>,
    /// `STATUS` is `ENABLED`.
    pub enforced: bool,
    /// `VALIDATED`: false when existing rows were not checked.
    pub validated: bool,
}

/// The table and columns a foreign key references.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OracleReferencedTable {
    pub namespace: String,
    pub table: String,
    pub columns: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OracleCheck {
    pub name: String,
    /// `SEARCH_CONDITION`.
    pub expression: OracleLongText,
    /// `STATUS` is `ENABLED`.
    pub enforced: bool,
    /// `VALIDATED`: false when existing rows were not checked.
    pub validated: bool,
}

/// A definition or expression Oracle keeps as a `LONG` value: the value as
/// Oracle stores it, or why it was not read.
///
/// The connector reads a value in PL/SQL and converts it to UTF-8 there, so it
/// reads one that fits in a PL/SQL `LONG`, 32,760 bytes in the database
/// character set, and in 32,767 bytes once converted to UTF-8. In an
/// `AL32UTF8` database, the limit is 32,760 bytes of UTF-8. A longer value is
/// never read, so it is [`OracleLongText::TooLong`] even when it is not valid
/// UTF-8.
///
/// Oracle documents a replacement character in `CONVERT` only for a character
/// the destination character set lacks, and `AL32UTF8` represents every
/// character of the database character set, so a value is
/// [`OracleLongText::NotUtf8`] only when its stored bytes are not valid in the
/// database character set and pass through the conversion. Oracle does not
/// document what `CONVERT` does with such bytes, and this has not been tested
/// on a database whose character set is not `AL32UTF8`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OracleLongText {
    /// The value as Oracle stores it.
    Text(String),
    /// The value is longer than the connector reads.
    TooLong,
    /// The value's UTF-8 conversion is not valid UTF-8, so it cannot be
    /// returned exactly.
    NotUtf8,
}

impl OracleLongText {
    /// The value whose UTF-8 conversion is `bytes`.
    pub(crate) fn from_utf8(bytes: Vec<u8>) -> Self {
        String::from_utf8(bytes).map_or(Self::NotUtf8, Self::Text)
    }
}

/// Why the constraints of a table are missing when a foreign key's
/// referenced table is not visible.
const REFERENCE_HIDDEN: &str =
    "Oracle hides the table a foreign key references when the credential has no privilege on it";

/// Why the constraints of a table are missing when a CHECK expression is
/// longer than the connector reads.
const CHECK_TOO_LONG: &str = "a CHECK expression is longer than the connector reads: 32,760 bytes \
     in the database character set or 32,767 bytes in UTF-8";

/// Why the constraints of a table are missing when a CHECK expression is not
/// valid UTF-8.
const CHECK_NOT_UTF8: &str = "a CHECK expression is not valid UTF-8 and cannot be returned exactly";

impl OracleTable {
    pub(crate) fn reference(&self) -> TableReference {
        TableReference::new(vec![self.namespace.clone(), self.name.clone()])
            .expect("Oracle owner and object names are never empty")
    }

    /// The table's constraints as table metadata states them.
    ///
    /// They are missing when a foreign key's referenced table is not visible
    /// or a CHECK expression is not read, which the reason states.
    fn table_constraints(&self) -> Reported<Vec<Constraint>> {
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
            constraints.push(Constraint {
                name: key.name.clone(),
                enforced: key.enforced,
                kind: ConstraintKind::Unique {
                    columns: key.columns.clone(),
                    kind: match key.kind {
                        OracleUniqueKind::Constraint => UniqueKind::Constraint,
                        OracleUniqueKind::Index => UniqueKind::Index,
                    },
                    condition: None,
                },
            });
        }
        for key in &self.foreign_keys {
            let Some(referenced) = &key.referenced else {
                return Reported::Missing {
                    reason: REFERENCE_HIDDEN.to_owned(),
                };
            };
            let referenced_table =
                TableReference::new(vec![referenced.namespace.clone(), referenced.table.clone()])
                    .expect("Oracle owner and object names are never empty");
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
            let expression = match &check.expression {
                OracleLongText::Text(expression) => expression,
                OracleLongText::TooLong => {
                    return Reported::Missing {
                        reason: CHECK_TOO_LONG.to_owned(),
                    };
                }
                OracleLongText::NotUtf8 => {
                    return Reported::Missing {
                        reason: CHECK_NOT_UTF8.to_owned(),
                    };
                }
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

    /// Derives the table's table metadata. No setting of an Oracle Data
    /// Source changes it.
    ///
    /// Oracle shows every column of a table the credential can see, so no
    /// column is hidden.
    pub fn to_table_metadata(&self) -> TableMetadata {
        let kind = match self.kind {
            OracleTableKind::Table => TableKind::Table,
            OracleTableKind::View => TableKind::View,
        };
        TableMetadata::new(
            self.reference(),
            kind,
            Reported::Value(self.object_id.to_string()),
            self.columns
                .iter()
                .map(OracleColumn::table_column)
                .collect(),
            false,
            self.table_constraints(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_text_is_its_utf8_value_or_not_utf8() {
        assert_eq!(
            OracleLongText::from_utf8("amount >= 0 😀".as_bytes().to_vec()),
            OracleLongText::Text("amount >= 0 😀".to_owned())
        );
        assert_eq!(
            OracleLongText::from_utf8(vec![b'a', 0xFF]),
            OracleLongText::NotUtf8
        );
    }

    /// A table with one check of `expression`.
    fn checked(expression: OracleLongText) -> OracleTable {
        OracleTable {
            namespace: "SALES".to_owned(),
            name: "ORDERS".to_owned(),
            object_id: 1,
            kind: OracleTableKind::Table,
            comment: None,
            definition: None,
            columns: Vec::new(),
            primary_key: None,
            unique_keys: Vec::new(),
            foreign_keys: Vec::new(),
            checks: vec![OracleCheck {
                name: "ORDERS_CHECK".to_owned(),
                expression,
                enforced: true,
                validated: true,
            }],
        }
    }

    #[test]
    fn a_check_expression_that_is_not_read_leaves_constraints_missing_with_its_reason() {
        let constraints = |expression| {
            checked(expression)
                .to_table_metadata()
                .constraints()
                .clone()
        };
        assert_eq!(
            constraints(OracleLongText::Text("AMOUNT >= 0".to_owned())),
            Reported::Value(vec![Constraint {
                name: "ORDERS_CHECK".to_owned(),
                enforced: true,
                kind: ConstraintKind::Check {
                    expression: "AMOUNT >= 0".to_owned()
                },
            }])
        );
        assert_eq!(
            constraints(OracleLongText::TooLong),
            Reported::Missing {
                reason: CHECK_TOO_LONG.to_owned()
            }
        );
        assert_eq!(
            constraints(OracleLongText::NotUtf8),
            Reported::Missing {
                reason: CHECK_NOT_UTF8.to_owned()
            }
        );
    }
}
