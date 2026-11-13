//! The comparison of two observations of one table: only values observed on
//! both sides are compared, and only facts are reported.

use analytics_types::AnalyticsType;
use connector_sdk::{
    Change, Column, ColumnAttribute, ColumnType, Constraint, ConstraintAttribute, ConstraintKind,
    Reported, TableAttribute, TableKind, TableMetadata, TableMetadataDifferences, TableReference,
    UniqueKind, UnsupportedReason,
};

#[test]
fn two_identical_observations_have_no_differences() {
    assert_eq!(Table::orders().compare(&Table::orders()), None);
}

#[test]
fn a_column_with_the_same_source_id_and_a_new_name_is_matched_with_its_name_changed() {
    let mut newer = Table::orders();
    newer.columns[1].name = "amount".to_owned();

    assert_eq!(
        columns(Table::orders().compare(&newer)),
        vec![Change::Matched {
            before: Table::orders().columns[1].clone(),
            after: newer.columns[1].clone(),
            attributes: vec![ColumnAttribute::Name],
        }]
    );
}

#[test]
fn a_column_with_the_same_name_and_a_new_source_id_is_removed_and_added() {
    let mut newer = Table::orders();
    newer.columns[1].source_id = id("9");

    assert_eq!(
        columns(Table::orders().compare(&newer)),
        vec![
            Change::Removed(Table::orders().columns[1].clone()),
            Change::Added(newer.columns[1].clone()),
        ]
    );
}

#[test]
fn columns_of_a_table_with_a_new_source_id_are_matched_by_name() {
    // A re-created table reuses column identifiers that are unique only
    // within one table, such as PostgreSQL attnums.
    let mut newer = Table::orders();
    newer.source_id = id("16999");
    newer.columns[1].name = "price".to_owned();

    let differences = Table::orders().compare(&newer).unwrap();

    assert_eq!(differences.table, vec![TableAttribute::SourceId]);
    assert_eq!(
        differences.columns,
        vec![
            Change::Removed(Table::orders().columns[1].clone()),
            Change::Added(newer.columns[1].clone()),
        ]
    );
}

#[test]
fn columns_without_source_ids_are_matched_by_name() {
    let without_ids = || {
        let mut table = Table::orders();
        for column in &mut table.columns {
            column.source_id = Reported::NotOffered;
        }
        table
    };
    let mut renamed = without_ids();
    renamed.columns[1].name = "amount".to_owned();
    let mut nullable = without_ids();
    nullable.columns[1].nullable = true;

    assert_eq!(
        columns(without_ids().compare(&renamed)),
        vec![
            Change::Removed(without_ids().columns[1].clone()),
            Change::Added(renamed.columns[1].clone()),
        ]
    );
    assert_eq!(
        columns(without_ids().compare(&nullable)),
        vec![Change::Matched {
            before: without_ids().columns[1].clone(),
            after: nullable.columns[1].clone(),
            attributes: vec![ColumnAttribute::Nullable],
        }]
    );
}

#[test]
fn a_changed_mapping_of_the_same_source_type_changes_only_the_analytics_type() {
    let mut newer = Table::orders();
    newer.columns[1].ty = ColumnType::Unsupported {
        data_source_type: "text".to_owned(),
        reason: UnsupportedReason::MappingNotImplemented,
    };

    assert_eq!(
        attributes(Table::orders().compare(&newer)),
        vec![vec![ColumnAttribute::AnalyticsType]]
    );
}

#[test]
fn items_not_observed_on_both_sides_are_not_compared() {
    let mut newer = Table::orders();
    newer.source_id = missing();
    newer.columns[0].source_id = missing();
    newer.columns[1].ty = ColumnType::Unread {
        reason: "permission denied".to_owned(),
    };
    newer.columns_may_be_hidden = true;
    newer.constraints = missing();

    assert_eq!(Table::orders().compare(&newer), None);
    assert_eq!(newer.compare(&Table::orders()), None);
}

#[test]
fn a_column_observed_on_one_side_only_is_added_or_removed_even_where_columns_may_be_hidden() {
    let mut newer = Table::orders();
    newer.columns.pop();
    newer.columns_may_be_hidden = true;

    assert_eq!(
        columns(Table::orders().compare(&newer)),
        vec![Change::Removed(Table::orders().columns[1].clone())]
    );
}

#[test]
fn a_changed_kind_is_a_table_difference() {
    let mut newer = Table::orders();
    newer.kind = TableKind::View;

    assert_eq!(
        Table::orders().compare(&newer).unwrap().table,
        vec![TableAttribute::Kind]
    );
}

#[test]
fn constraints_are_matched_by_kind_and_name_and_report_their_changed_attributes() {
    let mut newer = Table::orders();
    let Reported::Value(constraints) = &mut newer.constraints else {
        unreachable!("the fixture reads constraints");
    };
    constraints[1].enforced = false;
    constraints[1].kind = ConstraintKind::Unique {
        columns: vec!["total".to_owned()],
        kind: UniqueKind::Index,
        condition: Some("total > 0".to_owned()),
    };
    constraints.push(check("orders_total_check"));

    assert_eq!(
        Table::orders().compare(&newer).unwrap().constraints,
        vec![
            Change::Matched {
                before: Table::orders().constraint(1),
                after: newer.constraint(1),
                attributes: vec![
                    ConstraintAttribute::Enforced,
                    ConstraintAttribute::Condition
                ],
            },
            Change::Added(check("orders_total_check")),
        ]
    );
}

#[test]
fn a_constraint_of_another_kind_with_the_same_name_is_a_different_constraint() {
    let mut newer = Table::orders();
    let Reported::Value(constraints) = &mut newer.constraints else {
        unreachable!("the fixture reads constraints");
    };
    constraints[1] = check("orders_total_key");

    assert_eq!(
        Table::orders().compare(&newer).unwrap().constraints,
        vec![
            Change::Removed(Table::orders().constraint(1)),
            Change::Added(check("orders_total_key")),
        ]
    );
}

/// The items of one observed table, kept editable so each test states its
/// change.
#[derive(Clone)]
struct Table {
    kind: TableKind,
    source_id: Reported<String>,
    columns: Vec<Column>,
    columns_may_be_hidden: bool,
    constraints: Reported<Vec<Constraint>>,
}

impl Table {
    fn orders() -> Self {
        Self {
            kind: TableKind::Table,
            source_id: id("16384"),
            columns: vec![
                column(1, "id", mapped("int8", AnalyticsType::BigInt), id("1")),
                column(2, "total", mapped("text", AnalyticsType::Text), id("2")),
            ],
            columns_may_be_hidden: false,
            constraints: Reported::Value(vec![
                Constraint {
                    name: "orders_pkey".to_owned(),
                    enforced: true,
                    kind: ConstraintKind::PrimaryKey {
                        columns: vec!["id".to_owned()],
                    },
                },
                Constraint {
                    name: "orders_total_key".to_owned(),
                    enforced: true,
                    kind: ConstraintKind::Unique {
                        columns: vec!["total".to_owned()],
                        kind: UniqueKind::Index,
                        condition: None,
                    },
                },
            ]),
        }
    }

    fn metadata(&self) -> TableMetadata {
        TableMetadata::new(
            TableReference::new(vec!["public".to_owned(), "orders".to_owned()]).unwrap(),
            self.kind.clone(),
            self.source_id.clone(),
            self.columns.clone(),
            self.columns_may_be_hidden,
            self.constraints.clone(),
        )
    }

    fn compare(&self, newer: &Self) -> Option<TableMetadataDifferences> {
        self.metadata().compare(&newer.metadata())
    }

    fn constraint(&self, index: usize) -> Constraint {
        let Reported::Value(constraints) = &self.constraints else {
            unreachable!("the fixture reads constraints");
        };
        constraints[index].clone()
    }
}

fn columns(differences: Option<TableMetadataDifferences>) -> Vec<Change<Column, ColumnAttribute>> {
    differences.expect("the observations differ").columns
}

fn attributes(differences: Option<TableMetadataDifferences>) -> Vec<Vec<ColumnAttribute>> {
    columns(differences)
        .into_iter()
        .map(|change| match change {
            Change::Matched { attributes, .. } => attributes,
            other => panic!("the column stays matched: {other:?}"),
        })
        .collect()
}

fn column(ordinal: u32, name: &str, ty: ColumnType, source_id: Reported<String>) -> Column {
    Column {
        name: name.to_owned(),
        ordinal,
        nullable: false,
        source_id,
        ty,
    }
}

fn mapped(data_source_type: &str, analytics_type: AnalyticsType) -> ColumnType {
    ColumnType::Mapped {
        data_source_type: data_source_type.to_owned(),
        analytics_type,
    }
}

fn check(name: &str) -> Constraint {
    Constraint {
        name: name.to_owned(),
        enforced: true,
        kind: ConstraintKind::Check {
            expression: "total <> ''".to_owned(),
        },
    }
}

fn id(value: &str) -> Reported<String> {
    Reported::Value(value.to_owned())
}

fn missing<T>() -> Reported<T> {
    Reported::Missing {
        reason: "permission denied".to_owned(),
    }
}
