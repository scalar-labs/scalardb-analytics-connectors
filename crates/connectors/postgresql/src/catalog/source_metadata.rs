//! PostgreSQL source metadata for one table and the table metadata derived from it.

use std::collections::HashSet;

use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Reported, TableKind, TableMetadata,
    TableReference, UniqueKind,
};
use serde::{Deserialize, Serialize};

use super::type_mapping::{BaseType, map_type};

/// What `pg_catalog` states about one table, as the catalog query returns it.
///
/// It is the source metadata document. `pg_catalog` hides no rows from a role
/// without privileges, so every column and constraint is present.
///
/// The document describes one table, so its top level states the table. It
/// is shaped by the concepts a reader works with rather than by the catalogs:
/// names are PostgreSQL's display names, and each fact appears once, with the
/// table, column, or type it belongs to. Besides the facts table metadata
/// derives from, it carries what states a table's meaning, such as comments
/// and view definitions, for the Semantic Catalog to interpret.
///
/// Restoring a document checks that every type it refers to is listed and
/// that domains reach a type that is not a domain, so a restored document
/// derives table metadata as one this connector read does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedTable")]
pub struct PostgresTable {
    pub namespace: String,
    pub name: String,
    /// `pg_class.oid` as decimal text.
    pub oid: String,
    pub kind: PostgresTableKind,
    /// The table's comment, from `obj_description`.
    pub comment: Option<String>,
    /// A view's or materialized view's query as `pg_get_viewdef` prints it.
    pub definition: Option<String>,
    /// Columns in `attnum` order, without dropped columns.
    pub columns: Vec<PostgresColumn>,
    /// The types the columns are of, and the types those lead to: the base
    /// types of domains and the element types of arrays, each once, in name
    /// order.
    pub types: Vec<PostgresType>,
    pub primary_key: Option<PostgresPrimaryKey>,
    /// Unique constraints and the unique indexes that back no constraint, in
    /// name order.
    pub unique_keys: Vec<PostgresUniqueKey>,
    /// Foreign keys in name order.
    pub foreign_keys: Vec<PostgresForeignKey>,
    /// The table's CHECK constraints in name order.
    pub checks: Vec<PostgresCheck>,
}

/// The kind of relation, from `pg_class.relkind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PostgresTableKind {
    #[serde(rename = "table")]
    Table,
    #[serde(rename = "partitioned table")]
    PartitionedTable,
    #[serde(rename = "view")]
    View,
    #[serde(rename = "materialized view")]
    MaterializedView,
    #[serde(rename = "foreign table")]
    ForeignTable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresColumn {
    pub name: String,
    pub attnum: i16,
    /// The column's own `NOT NULL`, if it declares one; a `NOT NULL` domain
    /// is stated on its type.
    pub not_null: Option<PostgresNotNull>,
    /// `format_type(atttypid, atttypmod)`, the Data Source type.
    pub data_source_type: String,
    /// The display name of the column's type, one of the table's `types`.
    #[serde(rename = "type")]
    pub ty: String,
    /// `pg_attribute.atttypmod`; `-1` when the column declares none.
    pub typmod: i32,
    /// The column's comment, from `col_description`.
    pub comment: Option<String>,
    /// Where PostgreSQL takes the column's value from, other than the row
    /// itself; `None` for a column whose value comes only from the row, with
    /// no default, identity, or generation expression.
    pub value_source: Option<PostgresValueSource>,
}

/// A column's declared `NOT NULL`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresNotNull {
    /// False for a `NOT NULL ... NOT VALID` constraint, which PostgreSQL 18
    /// allows and which existing rows may violate; always true before it.
    pub validated: bool,
}

/// Where PostgreSQL takes a column's value from, other than the row itself;
/// the three are exclusive for one column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PostgresValueSource {
    /// A `DEFAULT` expression as `pg_get_expr` prints it, used when a row
    /// omits the column.
    Default { expression: String },
    /// An identity column, numbered from its sequence.
    Identity { generation: PostgresIdentity },
    /// A generated column, always computed by `expression` and never stated
    /// by a row.
    Generated {
        storage: PostgresGeneratedStorage,
        expression: String,
    },
}

/// `pg_attribute.attidentity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PostgresIdentity {
    /// `GENERATED ALWAYS`: a row states a value only with
    /// `OVERRIDING SYSTEM VALUE`.
    #[serde(rename = "always")]
    Always,
    /// `GENERATED BY DEFAULT`: a row may state a value.
    #[serde(rename = "by default")]
    ByDefault,
}

/// `pg_attribute.attgenerated`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PostgresGeneratedStorage {
    #[serde(rename = "stored")]
    Stored,
    /// Computed when read; PostgreSQL 18 and later.
    #[serde(rename = "virtual")]
    Virtual,
}

/// One `pg_type` row the table's columns lead to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresType {
    /// `format_type(oid, NULL)`, which qualifies a type off the `search_path`
    /// with its schema; columns and other types refer to the type by it.
    pub name: String,
    pub schema: String,
    /// `pg_type.typname`, such as `int4` for `integer` or `_int4` for
    /// `integer[]`.
    pub typname: String,
    /// The comment of a type created by a user or an extension, from
    /// `obj_description`; built-in types' descriptions are not read.
    pub comment: Option<String>,
    #[serde(flatten)]
    pub form: PostgresTypeForm,
}

/// What a type is, from `pg_type.typtype` and, for arrays, `typcategory`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PostgresTypeForm {
    /// A base type, built in or from an extension.
    Base,
    /// An array of `element`, another of the table's types.
    Array {
        element: String,
    },
    /// A domain over `base`, another of the table's types.
    Domain {
        base: String,
        /// `pg_type.typtypmod`; `-1` when the domain declares none.
        typmod: i32,
        /// `pg_type.typnotnull`. A domain's `NOT NULL` cannot be `NOT VALID`.
        not_null: bool,
        /// The domain's CHECK constraints in name order.
        checks: Vec<PostgresDomainCheck>,
    },
    /// An enum, with its labels in sort order.
    Enum {
        labels: Vec<String>,
    },
    Composite,
    Range,
    Multirange,
    Pseudo,
}

/// A CHECK constraint of a domain. Domain constraints are always enforced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresDomainCheck {
    pub name: String,
    /// The expression over `VALUE` as `pg_get_expr` prints it.
    pub expression: String,
    /// `pg_constraint.convalidated`: false for a `NOT VALID` constraint that
    /// existing values may violate.
    pub validated: bool,
}

/// A primary key, which PostgreSQL always enforces and validates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresPrimaryKey {
    pub name: String,
    pub columns: Vec<String>,
}

/// A unique constraint or a unique index that backs no constraint, which
/// PostgreSQL always enforces and validates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresUniqueKey {
    pub name: String,
    pub kind: PostgresUniqueKind,
    /// The key in order.
    pub keys: Vec<PostgresKey>,
    /// A partial index's predicate as `pg_get_expr` prints it.
    pub condition: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostgresUniqueKind {
    Constraint,
    Index,
}

/// One key of a unique key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostgresKey {
    Column(String),
    /// An index expression as `pg_get_indexdef` prints it.
    Expression(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced: PostgresReferencedTable,
    /// `pg_constraint.conenforced`; true before PostgreSQL 18, which enforces
    /// every constraint.
    pub enforced: bool,
    /// `pg_constraint.convalidated`: false for a `NOT VALID` constraint that
    /// existing rows may violate.
    pub validated: bool,
}

/// The table and columns a foreign key references.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresReferencedTable {
    pub namespace: String,
    pub table: String,
    pub columns: Vec<String>,
}

/// A CHECK constraint of the table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresCheck {
    pub name: String,
    /// The expression as `pg_get_expr` prints it.
    pub expression: String,
    /// `pg_constraint.conenforced`; true before PostgreSQL 18, which enforces
    /// every constraint.
    pub enforced: bool,
    /// `pg_constraint.convalidated`: false for a `NOT VALID` constraint that
    /// existing rows may violate.
    pub validated: bool,
}

/// A source metadata document as written, before restoring checks its type
/// references.
///
/// It mirrors [`PostgresTable`] only so that serde can restore the fields and
/// then check them: no existing type expresses a document whose references
/// are not yet known to resolve.
#[derive(Deserialize)]
struct UncheckedTable {
    namespace: String,
    name: String,
    oid: String,
    kind: PostgresTableKind,
    comment: Option<String>,
    definition: Option<String>,
    columns: Vec<PostgresColumn>,
    types: Vec<PostgresType>,
    primary_key: Option<PostgresPrimaryKey>,
    unique_keys: Vec<PostgresUniqueKey>,
    foreign_keys: Vec<PostgresForeignKey>,
    checks: Vec<PostgresCheck>,
}

impl TryFrom<UncheckedTable> for PostgresTable {
    type Error = String;

    fn try_from(unchecked: UncheckedTable) -> Result<Self, Self::Error> {
        let table = PostgresTable {
            namespace: unchecked.namespace,
            name: unchecked.name,
            oid: unchecked.oid,
            kind: unchecked.kind,
            comment: unchecked.comment,
            definition: unchecked.definition,
            columns: unchecked.columns,
            types: unchecked.types,
            primary_key: unchecked.primary_key,
            unique_keys: unchecked.unique_keys,
            foreign_keys: unchecked.foreign_keys,
            checks: unchecked.checks,
        };
        table.check_types()?;
        Ok(table)
    }
}

/// A column's type resolved through its domains.
pub(crate) struct ResolvedType {
    /// The type the mapping rule is looked up by.
    pub(crate) base: BaseType,
    /// Whether some domain on the way is `NOT NULL`.
    pub(crate) domain_not_null: bool,
}

impl PostgresTable {
    pub(crate) fn reference(&self) -> TableReference {
        TableReference::new(vec![self.namespace.clone(), self.name.clone()])
            .expect("PostgreSQL namespace and table names are never empty")
    }

    fn find_type(&self, name: &str) -> Option<&PostgresType> {
        self.types.iter().find(|ty| ty.name == name)
    }

    /// Checks that type names are unique, that every type a column, a domain,
    /// or an array refers to is listed, and that every type resolves.
    pub(crate) fn check_types(&self) -> Result<(), String> {
        let mut names = HashSet::new();
        for ty in &self.types {
            if !names.insert(ty.name.as_str()) {
                return Err(format!("the type {} is listed twice", ty.name));
            }
        }
        let referenced =
            self.columns
                .iter()
                .map(|column| &column.ty)
                .chain(self.types.iter().filter_map(|ty| match &ty.form {
                    PostgresTypeForm::Domain { base, .. } => Some(base),
                    PostgresTypeForm::Array { element } => Some(element),
                    _ => None,
                }));
        for name in referenced {
            if !names.contains(name.as_str()) {
                return Err(format!("the type {name} is not listed"));
            }
        }
        for ty in &self.types {
            if self.resolve(&ty.name, -1).is_none() {
                return Err(format!(
                    "the domain {} never reaches its base type",
                    ty.name
                ));
            }
        }
        Ok(())
    }

    /// Resolves a type through domains to the type the mapping rule is
    /// looked up by, with the type modifier `typmod` or, when that is `-1`,
    /// the innermost domain declares.
    ///
    /// `None` when a type on the way is not listed or the domains form a
    /// cycle; each step passes one listed type, so a longer chain has one.
    fn resolve<'a>(&'a self, mut name: &'a str, mut typmod: i32) -> Option<ResolvedType> {
        let mut domain_not_null = false;
        for _ in 0..=self.types.len() {
            let ty = self.find_type(name)?;
            let base = match &ty.form {
                PostgresTypeForm::Domain {
                    base,
                    typmod: declared,
                    not_null,
                    ..
                } => {
                    if typmod == -1 {
                        typmod = *declared;
                    }
                    domain_not_null |= not_null;
                    name = base;
                    continue;
                }
                PostgresTypeForm::Base => BaseType::Base {
                    schema: ty.schema.clone(),
                    typname: ty.typname.clone(),
                    typmod,
                },
                PostgresTypeForm::Enum { labels } => BaseType::Enum {
                    labels: labels.clone(),
                },
                PostgresTypeForm::Array { .. }
                | PostgresTypeForm::Composite
                | PostgresTypeForm::Range
                | PostgresTypeForm::Multirange
                | PostgresTypeForm::Pseudo => BaseType::Unmapped,
            };
            return Some(ResolvedType {
                base,
                domain_not_null,
            });
        }
        None
    }

    /// Resolves a column's type through its domains.
    pub(crate) fn resolve_type(&self, column: &PostgresColumn) -> ResolvedType {
        self.resolve(&column.ty, column.typmod)
            .expect("source metadata is checked to resolve when it is read or restored")
    }

    /// A column as table metadata states it.
    ///
    /// It is not nullable when it declares a validated `NOT NULL` or some
    /// domain its type resolves through is `NOT NULL`. CHECK constraints are
    /// not interpreted.
    fn table_column(&self, column: &PostgresColumn, ordinal: u32) -> Column {
        let resolved = self.resolve_type(column);
        let declared = column
            .not_null
            .as_ref()
            .is_some_and(|not_null| not_null.validated);
        let data_source_type = column.data_source_type.clone();
        let ty = match map_type(&resolved.base) {
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
            name: column.name.clone(),
            ordinal,
            nullable: !(declared || resolved.domain_not_null),
            source_id: Reported::Value(column.attnum.to_string()),
            ty,
        }
    }

    /// The table's constraints as table metadata states them. A unique index
    /// over expressions is not a column constraint and stays in the source
    /// metadata only.
    fn table_constraints(&self) -> Vec<Constraint> {
        let primary_key = self.primary_key.iter().map(|key| Constraint {
            name: key.name.clone(),
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
                    PostgresKey::Column(column) => Some(column.clone()),
                    PostgresKey::Expression(_) => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let kind = match key.kind {
                PostgresUniqueKind::Constraint => UniqueKind::Constraint,
                PostgresUniqueKind::Index => UniqueKind::Index,
            };
            Some(Constraint {
                name: key.name.clone(),
                enforced: true,
                kind: ConstraintKind::Unique {
                    columns,
                    kind,
                    condition: key.condition.clone(),
                },
            })
        });
        let foreign_keys = self.foreign_keys.iter().filter_map(|key| {
            let referenced_table = TableReference::new(vec![
                key.referenced.namespace.clone(),
                key.referenced.table.clone(),
            ])
            .ok()?;
            Some(Constraint {
                name: key.name.clone(),
                enforced: key.enforced,
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
}

impl PostgresTable {
    /// Derives the table's table metadata. No setting of a PostgreSQL Data
    /// Source changes it.
    pub fn to_table_metadata(&self) -> TableMetadata {
        let kind = match self.kind {
            PostgresTableKind::Table | PostgresTableKind::PartitionedTable => TableKind::Table,
            PostgresTableKind::View => TableKind::View,
            PostgresTableKind::ForeignTable => TableKind::External,
            PostgresTableKind::MaterializedView => TableKind::Other("materialized view".to_owned()),
        };
        let columns = self
            .columns
            .iter()
            .zip(1..)
            .map(|(column, ordinal)| self.table_column(column, ordinal))
            .collect();
        TableMetadata::new(
            self.reference(),
            kind,
            Reported::Value(self.oid.clone()),
            columns,
            false,
            Reported::Value(self.table_constraints()),
        )
    }
}
