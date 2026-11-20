//! Databricks source metadata for one table and the table metadata derived
//! from it.

use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Reported, TableKind, TableMetadata,
    TableReference,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::type_mapping::map_full_data_type;

/// What Unity Catalog shows about one table to the reading principal.
///
/// It is the source metadata document. Unity Catalog shows a table to a
/// principal with any privilege on it, and every column of a table it shows,
/// so the document holds every column. It leaves out what changes without a
/// change of the table's definition, such as owners and creation and
/// alteration times.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DatabricksTable {
    /// `TABLE_CATALOG`, the Unity Catalog catalog.
    pub catalog: String,
    /// `TABLE_SCHEMA`.
    pub schema: String,
    pub name: String,
    /// `TABLE_TYPE`, such as `MANAGED`, `EXTERNAL`, `VIEW`,
    /// `MATERIALIZED_VIEW`, `STREAMING_TABLE`, or `FOREIGN`.
    pub table_type: String,
    /// `DATA_SOURCE_FORMAT`, the storage format, such as `DELTA`, `ICEBERG`,
    /// or `PARQUET`; every format is read through the SQL warehouse.
    pub data_source_format: String,
    /// `COMMENT`.
    pub comment: Option<String>,
    /// Columns in `ORDINAL_POSITION` order.
    pub columns: Vec<DatabricksColumn>,
    /// The declared primary key. Unity Catalog does not enforce it.
    pub primary_key: Option<DatabricksPrimaryKey>,
    /// Declared foreign keys in name order. Unity Catalog does not enforce
    /// them.
    pub foreign_keys: Vec<DatabricksForeignKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DatabricksColumn {
    pub name: String,
    /// `ORDINAL_POSITION`, from 0.
    pub ordinal_position: u32,
    /// `IS_NULLABLE`.
    pub nullable: bool,
    /// `FULL_DATA_TYPE`, the Data Source type as the column definition
    /// states it, such as `decimal(10,2)`, `string collate UTF8_LCASE`, or
    /// `array<struct<a:int>>`.
    pub full_data_type: String,
    /// `COMMENT`.
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DatabricksPrimaryKey {
    pub name: String,
    /// The key's columns in key order.
    pub columns: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DatabricksForeignKey {
    pub name: String,
    /// The key's columns in key order.
    pub columns: Vec<String>,
    /// The referenced table and columns; `None` when the principal cannot see
    /// the referenced table's key.
    pub referenced: Option<DatabricksReferencedTable>,
}

/// The table and columns a foreign key references.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DatabricksReferencedTable {
    pub catalog: String,
    pub schema: String,
    pub table: String,
    /// The referenced columns, paired with the key's columns by position.
    pub columns: Vec<String>,
}

/// The table kind of a `TABLE_TYPE`.
///
/// A shallow clone is the kind of table it is. A kind Unity Catalog adds
/// later is another kind in its own term.
pub(crate) fn table_kind(table_type: &str) -> TableKind {
    match table_type {
        "MANAGED" | "MANAGED_SHALLOW_CLONE" => TableKind::Table,
        "EXTERNAL" | "EXTERNAL_SHALLOW_CLONE" => TableKind::External,
        "VIEW" => TableKind::View,
        other => TableKind::Other(other.to_owned()),
    }
}

impl DatabricksColumn {
    /// The column as table metadata states it. Unity Catalog offers no
    /// column identifiers through SQL.
    fn to_column(&self) -> Column {
        let data_source_type = self.full_data_type.clone();
        let ty = match map_full_data_type(&self.full_data_type) {
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
            ordinal: self.ordinal_position + 1,
            nullable: self.nullable,
            source_id: Reported::NotOffered,
            ty,
        }
    }
}

impl DatabricksTable {
    pub(crate) fn reference(&self) -> TableReference {
        TableReference::new(vec![
            self.catalog.clone(),
            self.schema.clone(),
            self.name.clone(),
        ])
        .expect("Unity Catalog names are never empty")
    }

    /// The table's declared constraints, none of them enforced. A foreign key
    /// whose referenced table the principal cannot see leaves the constraints
    /// missing.
    fn table_constraints(&self) -> Reported<Vec<Constraint>> {
        let primary_key = self.primary_key.iter().map(|key| {
            Some(Constraint {
                name: key.name.clone(),
                enforced: false,
                kind: ConstraintKind::PrimaryKey {
                    columns: key.columns.clone(),
                },
            })
        });
        let foreign_keys = self.foreign_keys.iter().map(|key| {
            let referenced = key.referenced.as_ref()?;
            Some(Constraint {
                name: key.name.clone(),
                enforced: false,
                kind: ConstraintKind::ForeignKey {
                    columns: key.columns.clone(),
                    referenced_table: TableReference::new(vec![
                        referenced.catalog.clone(),
                        referenced.schema.clone(),
                        referenced.table.clone(),
                    ])
                    .ok()?,
                    referenced_columns: referenced.columns.clone(),
                },
            })
        });
        match primary_key.chain(foreign_keys).collect::<Option<Vec<_>>>() {
            Some(constraints) => Reported::Value(constraints),
            None => Reported::Missing {
                reason: "a foreign key references a table the principal cannot see".to_owned(),
            },
        }
    }

    /// Derives the table's table metadata.
    pub(crate) fn to_table_metadata(&self) -> TableMetadata {
        TableMetadata::new(
            self.reference(),
            table_kind(&self.table_type),
            Reported::NotOffered,
            self.columns
                .iter()
                .map(DatabricksColumn::to_column)
                .collect(),
            false,
            self.table_constraints(),
        )
    }
}

#[cfg(test)]
mod tests {
    use analytics_types::{AnalyticsType, ArrayType};
    use connector_sdk::UnsupportedReason;

    use super::*;

    fn column(name: &str, ordinal_position: u32, full_data_type: &str) -> DatabricksColumn {
        DatabricksColumn {
            name: name.to_owned(),
            ordinal_position,
            nullable: ordinal_position != 0,
            full_data_type: full_data_type.to_owned(),
            comment: None,
        }
    }

    fn orders() -> DatabricksTable {
        DatabricksTable {
            catalog: "main".to_owned(),
            schema: "sales".to_owned(),
            name: "orders".to_owned(),
            table_type: "MANAGED".to_owned(),
            data_source_format: "DELTA".to_owned(),
            comment: Some("All orders".to_owned()),
            columns: vec![
                column("id", 0, "bigint"),
                column("tags", 1, "array<string>"),
                column("span", 2, "interval day to second"),
                column("customer_id", 3, "bigint"),
            ],
            primary_key: Some(DatabricksPrimaryKey {
                name: "orders_pk".to_owned(),
                columns: vec!["id".to_owned()],
            }),
            foreign_keys: vec![DatabricksForeignKey {
                name: "orders_customer_fk".to_owned(),
                columns: vec!["customer_id".to_owned()],
                referenced: Some(DatabricksReferencedTable {
                    catalog: "main".to_owned(),
                    schema: "crm".to_owned(),
                    table: "customers".to_owned(),
                    columns: vec!["id".to_owned()],
                }),
            }],
        }
    }

    #[test]
    fn columns_are_numbered_from_one_and_unmapped_types_are_kept() {
        let metadata = orders().to_table_metadata();
        assert_eq!(
            metadata.table().names(),
            ["main", "sales", "orders"].map(str::to_owned)
        );
        assert_eq!(metadata.kind(), &TableKind::Table);
        assert_eq!(metadata.source_id(), &Reported::NotOffered);
        assert!(!metadata.columns_may_be_hidden());
        let columns = metadata.columns();
        assert_eq!(
            columns
                .iter()
                .map(|column| (column.name.as_str(), column.ordinal, column.nullable))
                .collect::<Vec<_>>(),
            [
                ("id", 1, false),
                ("tags", 2, true),
                ("span", 3, true),
                ("customer_id", 4, true)
            ]
        );
        assert_eq!(
            columns[1].ty,
            ColumnType::Mapped {
                data_source_type: "array<string>".to_owned(),
                analytics_type: AnalyticsType::Array(ArrayType::new(AnalyticsType::Text, true)),
            }
        );
        assert_eq!(
            columns[2].ty,
            ColumnType::Unsupported {
                data_source_type: "interval day to second".to_owned(),
                reason: UnsupportedReason::NotMapped,
            }
        );
        assert!(metadata.is_complete());
    }

    #[test]
    fn keys_are_declared_and_not_enforced() {
        let metadata = orders().to_table_metadata();
        assert_eq!(
            metadata.constraints(),
            &Reported::Value(vec![
                Constraint {
                    name: "orders_pk".to_owned(),
                    enforced: false,
                    kind: ConstraintKind::PrimaryKey {
                        columns: vec!["id".to_owned()],
                    },
                },
                Constraint {
                    name: "orders_customer_fk".to_owned(),
                    enforced: false,
                    kind: ConstraintKind::ForeignKey {
                        columns: vec!["customer_id".to_owned()],
                        referenced_table: TableReference::new(
                            ["main", "crm", "customers"].map(str::to_owned).to_vec()
                        )
                        .unwrap(),
                        referenced_columns: vec!["id".to_owned()],
                    },
                },
            ])
        );
    }

    #[test]
    fn a_foreign_key_to_a_hidden_table_leaves_the_constraints_missing() {
        let mut table = orders();
        table.foreign_keys[0].referenced = None;
        let metadata = table.to_table_metadata();
        assert!(matches!(metadata.constraints(), Reported::Missing { .. }));
        assert!(!metadata.is_complete());
    }

    #[test]
    fn table_types_map_to_kinds() {
        for (table_type, kind) in [
            ("MANAGED", TableKind::Table),
            ("MANAGED_SHALLOW_CLONE", TableKind::Table),
            ("EXTERNAL", TableKind::External),
            ("EXTERNAL_SHALLOW_CLONE", TableKind::External),
            ("VIEW", TableKind::View),
            (
                "MATERIALIZED_VIEW",
                TableKind::Other("MATERIALIZED_VIEW".to_owned()),
            ),
            (
                "STREAMING_TABLE",
                TableKind::Other("STREAMING_TABLE".to_owned()),
            ),
            ("FOREIGN", TableKind::Other("FOREIGN".to_owned())),
        ] {
            assert_eq!(table_kind(table_type), kind, "{table_type}");
        }
    }
}
