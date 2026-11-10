use std::time::{Duration, SystemTime};

use analytics_types::AnalyticsType;
use connector_sdk::{
    Column, ColumnAttribute, ColumnChange, ColumnType, Completeness, Constraint, ConstraintChange,
    ConstraintChanges, ConstraintKind, Difference, Reported, Side, TableAttribute, TableChanges,
    TableComparison, TableKind, TableMetadata, TableRecord, TableReference, TableState, UniqueKind,
    UnsupportedReason,
};

#[test]
fn identical_retrievals_at_different_times_have_no_changes() {
    let stored = Table::orders().retrieved();
    let mut newer = Table::orders();
    newer.read_at += Duration::from_secs(3600);

    assert_eq!(
        stored.compare(&newer.retrieved()),
        compared(Completeness::Complete, Completeness::Complete, no_changes())
    );
}

#[test]
fn a_column_with_the_same_source_id_and_a_new_name_is_renamed() {
    let stored = Table::orders().retrieved();
    let mut newer = Table::orders();
    newer.columns[1].name = "amount".to_owned();

    let changes = changes_of(stored.compare(&newer.retrieved()));

    assert_eq!(
        changes.columns,
        vec![ColumnChange::Changed {
            before: Table::orders().columns[1].clone(),
            after: newer.columns[1].clone(),
            attributes: vec![Difference::Changed(ColumnAttribute::Name)],
        }]
    );
}

#[test]
fn a_column_with_the_same_name_and_a_new_source_id_is_dropped_and_recreated() {
    let stored = Table::orders().retrieved();
    let mut newer = Table::orders();
    newer.columns[1].source_id = id("9");

    let changes = changes_of(stored.compare(&newer.retrieved()));

    assert_eq!(
        changes.columns,
        vec![
            ColumnChange::Removed(Table::orders().columns[1].clone()),
            ColumnChange::Added(newer.columns[1].clone()),
        ]
    );
}

#[test]
fn a_table_with_a_new_source_id_is_reported_as_recreated() {
    let stored = Table::orders().retrieved();
    let mut newer = Table::orders();
    newer.source_id = id("16500");

    let changes = changes_of(stored.compare(&newer.retrieved()));

    assert_eq!(
        changes.table,
        vec![Difference::Changed(TableAttribute::SourceId)]
    );
}

#[test]
fn columns_without_source_ids_are_matched_by_name() {
    let mut stored = Table::orders();
    let mut newer = Table::orders();
    for table in [&mut stored, &mut newer] {
        for column in &mut table.columns {
            column.source_id = Reported::NotOffered;
        }
    }
    newer.columns[1].nullable = true;
    newer.columns.push(column(
        3,
        "note",
        mapped("text", AnalyticsType::Text),
        Reported::NotOffered,
    ));

    let changes = changes_of(stored.retrieved().compare(&newer.retrieved()));

    assert_eq!(
        changes.columns,
        vec![
            ColumnChange::Changed {
                before: stored.columns[1].clone(),
                after: newer.columns[1].clone(),
                attributes: vec![Difference::Changed(ColumnAttribute::Nullable)],
            },
            ColumnChange::Added(newer.columns[2].clone()),
        ]
    );
}

#[test]
fn a_changed_mapping_of_the_same_source_type_changes_only_the_analytics_type() {
    let stored = Table::orders().retrieved();
    let mut newer = Table::orders();
    newer.columns[1].ty = ColumnType::Unsupported {
        data_source_type: "text".to_owned(),
        reason: UnsupportedReason::MappingNotImplemented,
    };

    let changes = changes_of(stored.compare(&newer.retrieved()));

    assert_eq!(
        changes.columns,
        vec![ColumnChange::Changed {
            before: Table::orders().columns[1].clone(),
            after: newer.columns[1].clone(),
            attributes: vec![Difference::Changed(ColumnAttribute::AnalyticsType)],
        }]
    );
}

#[test]
fn information_missing_on_either_side_is_unknown_rather_than_changed() {
    let stored = Table::orders().retrieved();
    let mut newer = Table::orders();
    newer.source_id = missing();
    newer.columns[0].source_id = missing();
    newer.columns[1].ty = ColumnType::Unread {
        reason: "permission denied".to_owned(),
    };
    newer.constraints = missing();

    let comparison = stored.compare(&newer.retrieved());

    let TableComparison::Compared { after, changes, .. } = comparison else {
        panic!("both sides were read");
    };
    assert_eq!(after, Completeness::Incomplete);
    assert_eq!(
        changes.table,
        vec![Difference::Unknown(TableAttribute::SourceId)]
    );
    let attributes: Vec<Vec<Difference<ColumnAttribute>>> = changes
        .columns
        .into_iter()
        .map(|change| match change {
            ColumnChange::Changed { attributes, .. } => attributes,
            other => panic!("matched columns stay matched: {other:?}"),
        })
        .collect();
    assert_eq!(
        attributes,
        vec![
            vec![Difference::Unknown(ColumnAttribute::SourceId)],
            vec![
                Difference::Unknown(ColumnAttribute::DataSourceType),
                Difference::Unknown(ColumnAttribute::AnalyticsType),
            ],
        ]
    );
    assert_eq!(changes.constraints, ConstraintChanges::Unknown);
}

#[test]
fn items_offered_on_only_one_side_are_unknown_and_on_neither_side_equal() {
    let mut stored = Table::orders();
    stored.constraints = Reported::NotOffered;
    let mut newer = Table::orders();
    newer.constraints = Reported::NotOffered;

    assert_eq!(
        changes_of(Table::orders().retrieved().compare(&newer.retrieved())).constraints,
        ConstraintChanges::Unknown
    );
    assert_eq!(
        changes_of(stored.retrieved().compare(&newer.retrieved())).constraints,
        ConstraintChanges::Listed(Vec::new())
    );
}

#[test]
fn a_column_absent_where_columns_may_be_hidden_is_unknown() {
    let stored = Table::orders();
    let mut newer = Table::orders();
    newer.columns.pop();
    newer.columns_may_be_hidden = true;

    let changes = changes_of(stored.retrieved().compare(&newer.retrieved()));
    assert_eq!(
        changes.columns,
        vec![ColumnChange::Unknown {
            column: stored.columns[1].clone(),
            present_in: Side::Before,
        }]
    );

    let changes = changes_of(newer.retrieved().compare(&stored.retrieved()));
    assert_eq!(
        changes.columns,
        vec![ColumnChange::Unknown {
            column: stored.columns[1].clone(),
            present_in: Side::After,
        }]
    );
}

#[test]
fn a_renamed_column_whose_source_id_is_missing_is_not_reported_as_dropped() {
    let stored = Table::orders();
    let mut newer = Table::orders();
    newer.columns[1].name = "amount".to_owned();
    newer.columns[1].source_id = missing();

    let changes = changes_of(stored.retrieved().compare(&newer.retrieved()));

    assert_eq!(
        changes.columns,
        vec![
            ColumnChange::Unknown {
                column: stored.columns[1].clone(),
                present_in: Side::Before,
            },
            ColumnChange::Unknown {
                column: newer.columns[1].clone(),
                present_in: Side::After,
            },
        ]
    );
}

#[test]
fn ordinals_shifted_where_columns_may_be_hidden_are_unknown() {
    let stored = Table::orders();
    let mut newer = Table::orders();
    newer.columns.remove(0);
    newer.columns[0].ordinal = 1;
    newer.columns_may_be_hidden = true;

    let changes = changes_of(stored.retrieved().compare(&newer.retrieved()));

    assert_eq!(
        changes.columns,
        vec![
            ColumnChange::Unknown {
                column: stored.columns[0].clone(),
                present_in: Side::Before,
            },
            ColumnChange::Changed {
                before: stored.columns[1].clone(),
                after: newer.columns[0].clone(),
                attributes: vec![Difference::Unknown(ColumnAttribute::Ordinal)],
            },
        ]
    );
}

#[test]
fn constraints_are_matched_by_kind_and_name() {
    let stored = Table::orders();
    let mut newer = Table::orders();
    newer.constraints = Reported::Value(vec![
        Constraint {
            name: "orders_pkey".to_owned(),
            enforced: false,
            kind: ConstraintKind::PrimaryKey {
                columns: vec!["id".to_owned()],
            },
        },
        Constraint {
            name: "orders_total_check".to_owned(),
            enforced: true,
            kind: ConstraintKind::Check {
                expression: "total >= 0".to_owned(),
            },
        },
    ]);

    let changes = changes_of(stored.retrieved().compare(&newer.retrieved()));

    let Reported::Value(before) = &stored.constraints else {
        unreachable!()
    };
    let Reported::Value(after) = &newer.constraints else {
        unreachable!()
    };
    assert_eq!(
        changes.constraints,
        ConstraintChanges::Listed(vec![
            ConstraintChange::Changed {
                before: before[0].clone(),
                after: after[0].clone(),
            },
            ConstraintChange::Removed(before[1].clone()),
            ConstraintChange::Added(after[1].clone()),
        ])
    );
}

#[test]
fn a_constraint_of_another_kind_with_the_same_name_is_a_different_constraint() {
    let stored = Table::orders();
    let mut newer = Table::orders();
    let unique_as_check = Constraint {
        name: "orders_total_key".to_owned(),
        enforced: true,
        kind: ConstraintKind::Check {
            expression: "total <> ''".to_owned(),
        },
    };
    let Reported::Value(constraints) = &mut newer.constraints else {
        unreachable!()
    };
    constraints[1] = unique_as_check.clone();

    let changes = changes_of(stored.retrieved().compare(&newer.retrieved()));

    let Reported::Value(before) = &stored.constraints else {
        unreachable!()
    };
    assert_eq!(
        changes.constraints,
        ConstraintChanges::Listed(vec![
            ConstraintChange::Removed(before[1].clone()),
            ConstraintChange::Added(unique_as_check),
        ])
    );
}

#[test]
fn tables_that_do_not_exist_or_could_not_be_read_are_not_compared() {
    let stored = Table::orders().retrieved();
    let not_found = TableRecord::NotFound {
        table: orders_reference(),
    };
    let unavailable = TableRecord::Unavailable {
        table: orders_reference(),
        reason: "permission denied".to_owned(),
    };

    assert_eq!(
        stored.compare(&not_found),
        TableComparison::NotCompared {
            before: TableState::Read(Completeness::Complete),
            after: TableState::NotFound,
        }
    );
    assert_eq!(
        unavailable.compare(&stored),
        TableComparison::NotCompared {
            before: TableState::Unavailable,
            after: TableState::Read(Completeness::Complete),
        }
    );
}

/// The fields of one read table and its retrieval time, kept editable so each
/// test states its change.
struct Table {
    source_id: Reported<String>,
    columns: Vec<Column>,
    columns_may_be_hidden: bool,
    constraints: Reported<Vec<Constraint>>,
    read_at: SystemTime,
}

impl Table {
    fn orders() -> Self {
        Self {
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
            read_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        }
    }

    fn retrieved(&self) -> TableRecord<TableMetadata> {
        TableRecord::Read {
            metadata: TableMetadata::new(
                orders_reference(),
                TableKind::Table,
                self.source_id.clone(),
                self.columns.clone(),
                self.columns_may_be_hidden,
                self.constraints.clone(),
            ),
            read_at: self.read_at,
        }
    }
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

fn id(value: &str) -> Reported<String> {
    Reported::Value(value.to_owned())
}

fn orders_reference() -> TableReference {
    TableReference::new(vec!["public".to_owned(), "orders".to_owned()])
        .expect("the reference has non-empty names")
}

fn missing<T>() -> Reported<T> {
    Reported::Missing {
        reason: "permission denied".to_owned(),
    }
}

fn no_changes() -> TableChanges {
    TableChanges {
        table: Vec::new(),
        columns: Vec::new(),
        constraints: ConstraintChanges::Listed(Vec::new()),
    }
}

fn compared(before: Completeness, after: Completeness, changes: TableChanges) -> TableComparison {
    TableComparison::Compared {
        before,
        after,
        changes,
    }
}

fn changes_of(comparison: TableComparison) -> TableChanges {
    match comparison {
        TableComparison::Compared { changes, .. } => changes,
        TableComparison::NotCompared { .. } => panic!("both sides were read"),
    }
}
