//! Comparing two observations of one table through the C boundary: the table
//! metadata and source metadata of retrieved records, passed as they were
//! received or as a caller stored them.

mod support;

use std::ptr;

use connector_api::abi::*;
use serde_json::{Value, json};
use support::*;

#[test]
fn retrieved_table_metadata_compares_with_a_later_retrieval_as_received() {
    initialize();
    let session = DataSource::open_probed().0.session().unwrap();

    // Complete, not offered, and incomplete tables with hidden columns.
    for table in ["orders", "order_view", "events"] {
        let stored = session.read_table(table);
        let fresh = session.read_table(table);
        assert!(
            compare_table_metadata(stored.table_metadata(), fresh.table_metadata())
                .unwrap()
                .is_none(),
            "{table} has no differences"
        );
    }
}

#[test]
fn table_metadata_a_caller_stored_compares_after_the_retrieval_is_freed() {
    initialize();
    let session = DataSource::open_probed().0.session().unwrap();
    let stored = StoredTableMetadata::copy(session.read_table("orders").table_metadata());

    let fresh = session.read_table("orders");

    assert!(
        compare_table_metadata(&stored.view, fresh.table_metadata())
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_renamed_column_with_the_same_source_id_is_matched_with_its_name_changed() {
    initialize();
    let session = DataSource::open_probed().0.session().unwrap();
    let (orders, renamed) = (
        session.read_table("orders"),
        session.read_table("orders_renamed"),
    );

    let differences = compare_table_metadata(orders.table_metadata(), renamed.table_metadata())
        .unwrap()
        .unwrap();

    assert_eq!(differences.table_attribute_count, 0);
    assert_eq!(
        column_changes(&differences),
        [change("matched", Some("amount"), Some("total"), &[1])]
    );
    assert_eq!(differences.constraint_count, 0);
}

#[test]
fn columns_of_a_recreated_table_are_matched_by_name_rather_than_by_identifier() {
    initialize();
    let session = DataSource::open_probed().0.session().unwrap();
    let (orders, recreated) = (
        session.read_table("orders"),
        session.read_table("orders_recreated"),
    );

    let differences = compare_table_metadata(orders.table_metadata(), recreated.table_metadata())
        .unwrap()
        .unwrap();

    // SAFETY: the library returns `table_attribute_count` attributes.
    let table = unsafe {
        items(
            differences.table_attributes,
            differences.table_attribute_count,
        )
    };
    assert_eq!(table, [ScalardbAnalyticsConnectorTableAttribute::SOURCE_ID]);
    assert_eq!(
        column_changes(&differences),
        [
            change("removed", Some("amount"), None, &[]),
            change("added", None, Some("price"), &[]),
        ]
    );
}

#[test]
fn items_one_side_could_not_read_are_not_compared() {
    initialize();
    let session = DataSource::open_probed().0.session().unwrap();
    let (orders, partial) = (
        session.read_table("orders"),
        session.read_table("orders_partial"),
    );

    assert!(
        compare_table_metadata(orders.table_metadata(), partial.table_metadata())
            .unwrap()
            .is_none()
    );
}

#[test]
fn table_metadata_that_is_not_valid_is_an_invalid_request() {
    initialize();
    let session = DataSource::open_probed().0.session().unwrap();
    let orders = session.read_table("orders");
    let valid = *orders.table_metadata();
    let invalid = |metadata: ScalardbAnalyticsConnectorTableMetadata| {
        assert_eq!(
            compare_table_metadata(&valid, &metadata)
                .err()
                .unwrap()
                .kind,
            ScalardbAnalyticsConnectorErrorKind::InvalidRequest
        );
    };

    let mut unknown_kind = valid;
    unknown_kind.kind = ScalardbAnalyticsConnectorTableKind(99);
    invalid(unknown_kind);

    let mut no_names = valid;
    no_names.table.name_count = 0;
    invalid(no_names);

    let not_utf8 = [0xff_u8];
    let name = ScalardbAnalyticsConnectorString {
        data: not_utf8.as_ptr().cast(),
        length: not_utf8.len(),
    };
    let names = [name];
    let mut not_text = valid;
    not_text.table = ScalardbAnalyticsConnectorNames {
        names: names.as_ptr(),
        name_count: names.len(),
    };
    invalid(not_text);

    let mut no_constraints = valid;
    no_constraints.constraints = ptr::null();
    invalid(no_constraints);

    // A truth value other than 0 and 1, as a caller writing raw bytes may
    // produce, is rejected rather than read.
    let mut not_a_truth_value = valid;
    not_a_truth_value.columns_may_be_hidden = ScalardbAnalyticsConnectorBool(2);
    invalid(not_a_truth_value);
}

#[test]
fn retrieved_source_metadata_is_compared_through_its_connector() {
    initialize();
    let session = DataSource::open_probed().0.session().unwrap();
    let (orders, renamed) = (
        session.read_table("orders"),
        session.read_table("orders_renamed"),
    );

    assert!(
        compare_source_metadata(orders.source_metadata(), orders.source_metadata())
            .unwrap()
            .is_none()
    );

    let differences = compare_source_metadata(orders.source_metadata(), renamed.source_metadata())
        .unwrap()
        .unwrap();
    assert_eq!(
        json_differences(&differences),
        [
            (
                "/fixture".to_owned(),
                Some(json!("orders")),
                Some(json!("orders_renamed"))
            ),
            (
                "/table/2".to_owned(),
                Some(json!("orders")),
                Some(json!("orders_renamed"))
            ),
        ]
    );
}

#[test]
fn a_change_table_metadata_does_not_hold_is_a_source_metadata_difference() {
    initialize();
    let orders = json!({"table": ["test", "public", "orders"], "fixture": "orders", "note": null});
    let mut noted = orders.clone();
    noted["note"] = json!("税込");
    let (orders, noted) = (orders.to_string(), noted.to_string());

    let differences = compare_source_metadata(
        &source_metadata("test", &orders),
        &source_metadata("test", &noted),
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        json_differences(&differences),
        [("/note".to_owned(), Some(Value::Null), Some(json!("税込")))]
    );
}

#[test]
fn an_inserted_array_element_is_the_only_source_metadata_difference() {
    initialize();
    let document = |table: &[&str]| json!({"table": table, "fixture": "orders", "note": null});
    let before = document(&["test", "public", "orders"]).to_string();
    let after = document(&["test", "sales", "public", "orders"]).to_string();

    let differences = compare_source_metadata(
        &source_metadata("test", &before),
        &source_metadata("test", &after),
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        json_differences(&differences),
        [("/table/1".to_owned(), None, Some(json!("sales")))]
    );
}

#[test]
fn source_metadata_its_connector_cannot_restore_is_an_invalid_request() {
    initialize();
    let orders = json!({"table": ["test", "public", "orders"], "fixture": "orders", "note": null})
        .to_string();
    let unrestorable = r#"{"oid": 16384}"#;

    for (before, after) in [
        (("test", orders.as_str()), ("test", unrestorable)),
        (("test", orders.as_str()), ("other", orders.as_str())),
        (("other", orders.as_str()), ("other", orders.as_str())),
    ] {
        let error = compare_source_metadata(
            &source_metadata(before.0, before.1),
            &source_metadata(after.0, after.1),
        )
        .err()
        .unwrap();
        assert_eq!(
            error.kind,
            ScalardbAnalyticsConnectorErrorKind::InvalidRequest
        );
    }
}

/// Source metadata as a caller builds it from its stored text.
fn source_metadata(connector: &str, document: &str) -> ScalardbAnalyticsConnectorSourceMetadata {
    ScalardbAnalyticsConnectorSourceMetadata {
        connector: string(connector),
        document: ScalardbAnalyticsConnectorJson {
            text: string(document),
        },
    }
}

/// A column change as its kind, the names of both sides, and the attributes.
type ColumnChange = (String, Option<String>, Option<String>, Vec<u32>);

fn change(
    kind: &str,
    before: Option<&str>,
    after: Option<&str>,
    attributes: &[u32],
) -> ColumnChange {
    (
        kind.to_owned(),
        before.map(str::to_owned),
        after.map(str::to_owned),
        attributes.to_vec(),
    )
}

fn column_changes(
    differences: &ScalardbAnalyticsConnectorTableMetadataDifferences,
) -> Vec<ColumnChange> {
    let name = |column: *const ScalardbAnalyticsConnectorColumn| {
        // SAFETY: a set side points to a live column.
        unsafe { column.as_ref() }.map(|column| read_string(column.name).unwrap())
    };
    // SAFETY: the library returns `column_count` changes.
    unsafe { items(differences.columns, differences.column_count) }
        .iter()
        .map(|change| {
            let kind = match change.change {
                ScalardbAnalyticsConnectorChangeKind::ADDED => "added",
                ScalardbAnalyticsConnectorChangeKind::REMOVED => "removed",
                ScalardbAnalyticsConnectorChangeKind::MATCHED => "matched",
                other => panic!("unknown change {other:?}"),
            };
            // SAFETY: the library returns `attribute_count` attributes.
            let attributes = unsafe { items(change.attributes, change.attribute_count) };
            (
                kind.to_owned(),
                name(change.before),
                name(change.after),
                attributes.iter().map(|attribute| attribute.0).collect(),
            )
        })
        .collect()
}

fn json_differences(
    differences: &ScalardbAnalyticsConnectorSourceMetadataDifferences,
) -> Vec<(String, Option<Value>, Option<Value>)> {
    let json = |json: ScalardbAnalyticsConnectorJson| {
        read_string(json.text).map(|text| serde_json::from_str(&text).unwrap())
    };
    // SAFETY: the library returns `difference_count` differences.
    unsafe { items(differences.differences, differences.difference_count) }
        .iter()
        .map(|difference| {
            (
                read_string(difference.path).unwrap(),
                json(difference.before),
                json(difference.after),
            )
        })
        .collect()
}

/// Table metadata copied into caller-owned memory, as a caller that stores
/// what it received rebuilds it before comparing.
struct StoredTableMetadata {
    view: ScalardbAnalyticsConnectorTableMetadata,
    _storage: Storage,
}

/// Owns copies of the texts and arrays a stored view points into.
#[derive(Default)]
struct Storage {
    texts: Vec<Box<[u8]>>,
    names: Vec<Box<[ScalardbAnalyticsConnectorString]>>,
    columns: Vec<Box<[ScalardbAnalyticsConnectorColumn]>>,
    constraints: Vec<Box<[ScalardbAnalyticsConnectorConstraint]>>,
    /// Boxed so that each type keeps its address as the vector grows.
    #[allow(clippy::vec_box)]
    types: Vec<Box<ScalardbAnalyticsConnectorAnalyticsType>>,
}

impl StoredTableMetadata {
    fn copy(metadata: &ScalardbAnalyticsConnectorTableMetadata) -> Self {
        let mut storage = Storage::default();
        let mut view = *metadata;
        view.table = storage.names(&metadata.table);
        view.other_kind = storage.text(metadata.other_kind);
        view.source_id = storage.reported(metadata.source_id);
        view.constraints_missing_reason = storage.text(metadata.constraints_missing_reason);
        // SAFETY: the library returns `column_count` columns.
        let columns: Box<[_]> = unsafe { items(metadata.columns, metadata.column_count) }
            .iter()
            .map(|column| storage.column(column))
            .collect();
        view.columns = columns.as_ptr();
        storage.columns.push(columns);
        // SAFETY: the library returns `constraint_count` constraints.
        let constraints: Box<[_]> =
            unsafe { items(metadata.constraints, metadata.constraint_count) }
                .iter()
                .map(|constraint| storage.constraint(constraint))
                .collect();
        view.constraints = if constraints.is_empty() {
            ptr::null()
        } else {
            constraints.as_ptr()
        };
        storage.constraints.push(constraints);
        Self {
            view,
            _storage: storage,
        }
    }
}

impl Storage {
    fn text(&mut self, text: ScalardbAnalyticsConnectorString) -> ScalardbAnalyticsConnectorString {
        let Some(text) = read_string(text) else {
            return text;
        };
        let bytes: Box<[u8]> = text.into_bytes().into_boxed_slice();
        let view = ScalardbAnalyticsConnectorString {
            data: bytes.as_ptr().cast(),
            length: bytes.len(),
        };
        self.texts.push(bytes);
        view
    }

    fn names(
        &mut self,
        names: &ScalardbAnalyticsConnectorNames,
    ) -> ScalardbAnalyticsConnectorNames {
        // SAFETY: the library returns `name_count` names.
        let copied: Box<[_]> = unsafe { items(names.names, names.name_count) }
            .iter()
            .map(|&name| self.text(name))
            .collect();
        let view = ScalardbAnalyticsConnectorNames {
            names: copied.as_ptr(),
            name_count: copied.len(),
        };
        self.names.push(copied);
        view
    }

    fn reported(
        &mut self,
        item: ScalardbAnalyticsConnectorReportedText,
    ) -> ScalardbAnalyticsConnectorReportedText {
        ScalardbAnalyticsConnectorReportedText {
            state: item.state,
            value: self.text(item.value),
            missing_reason: self.text(item.missing_reason),
        }
    }

    fn column(
        &mut self,
        column: &ScalardbAnalyticsConnectorColumn,
    ) -> ScalardbAnalyticsConnectorColumn {
        ScalardbAnalyticsConnectorColumn {
            name: self.text(column.name),
            source_id: self.reported(column.source_id),
            data_source_type: self.text(column.data_source_type),
            analytics_type: self.analytics_type(&column.analytics_type),
            unread_reason: self.text(column.unread_reason),
            ..*column
        }
    }

    /// Copies the type parameters the fixture uses: decimals and arrays.
    fn analytics_type(
        &mut self,
        analytics_type: &ScalardbAnalyticsConnectorAnalyticsType,
    ) -> ScalardbAnalyticsConnectorAnalyticsType {
        let mut copied = *analytics_type;
        // SAFETY: a set element type points to a live type.
        if let Some(element) = unsafe { analytics_type.element_type.as_ref() } {
            let element = Box::new(self.analytics_type(element));
            copied.element_type = &*element;
            self.types.push(element);
        }
        copied
    }

    fn constraint(
        &mut self,
        constraint: &ScalardbAnalyticsConnectorConstraint,
    ) -> ScalardbAnalyticsConnectorConstraint {
        ScalardbAnalyticsConnectorConstraint {
            name: self.text(constraint.name),
            columns: self.names(&constraint.columns),
            condition: self.text(constraint.condition),
            referenced_table: self.names(&constraint.referenced_table),
            referenced_columns: self.names(&constraint.referenced_columns),
            expression: self.text(constraint.expression),
            ..*constraint
        }
    }
}
