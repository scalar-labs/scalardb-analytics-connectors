use std::sync::Arc;

use arrow_array::{Array, Int32Array, StringArray};
use arrow_schema::DataType;
use connector_sql::{Fragment, RenderError, SqlDialect};

struct PostgreSqlDialect;

impl SqlDialect for PostgreSqlDialect {
    fn quote_identifier(&self, identifier: &str) -> String {
        format!("\"{}\"", identifier.replace('"', "\"\""))
    }

    fn write_placeholder(&self, position: usize, sql: &mut String) {
        sql.push('$');
        sql.push_str(&position.to_string());
    }
}

#[test]
fn connector_syntax_and_runtime_values_use_distinct_boundaries() {
    let dialect = PostgreSqlDialect;
    let identifier = Fragment::identifier(&dialect, "odd\"column");
    let value =
        Fragment::parameter(Arc::new(StringArray::from(vec![Some("secret-value")]))).unwrap();
    let fragment = Fragment::compose([identifier, value]);

    let query = fragment.render(&dialect).unwrap();

    assert_eq!(query.sql, "\"odd\"\"column\" $1");
    assert!(!query.sql.contains("secret-value"));
    let parameters = query.parameters.unwrap();
    let values = parameters
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(values.value(0), "secret-value");
}

#[test]
fn final_traversal_orders_repeated_and_reordered_parameters() {
    let dialect = PostgreSqlDialect;
    let first = Arc::new(Int32Array::from(vec![Some(10)]));
    let second = Arc::new(Int32Array::from(vec![Some(20)]));
    let fragment = Fragment::compose([
        Fragment::parameter(second.clone()).unwrap(),
        Fragment::parameter(first).unwrap(),
        Fragment::parameter(second).unwrap(),
    ]);

    let query = fragment.render(&dialect).unwrap();

    assert_eq!(query.sql, "$1 $2 $3");
    let parameters = query.parameters.unwrap();
    assert_eq!(parameters.num_rows(), 1);
    assert_eq!(parameters.num_columns(), 3);
    let values = parameters
        .columns()
        .iter()
        .map(|column| {
            column
                .as_any()
                .downcast_ref::<Int32Array>()
                .unwrap()
                .value(0)
        })
        .collect::<Vec<_>>();
    assert_eq!(values, [20, 10, 20]);
}

#[test]
fn typed_null_retains_its_arrow_type() {
    let dialect = PostgreSqlDialect;
    let null = Fragment::parameter(Arc::new(StringArray::from(vec![None::<&str>]))).unwrap();

    let query = null.render(&dialect).unwrap();

    let parameters = query.parameters.unwrap();
    assert_eq!(parameters.schema().field(0).data_type(), &DataType::Utf8);
    assert!(parameters.column(0).is_null(0));
}

#[test]
fn parameter_must_be_one_typed_scalar() {
    assert!(matches!(
        Fragment::parameter(Arc::new(Int32Array::from(Vec::<i32>::new()))),
        Err(RenderError::InvalidParameterLength { actual: 0 })
    ));
    assert!(matches!(
        Fragment::parameter(Arc::new(Int32Array::from(vec![1, 2]))),
        Err(RenderError::InvalidParameterLength { actual: 2 })
    ));
}

#[test]
fn query_without_parameters_has_no_binding_batch() {
    let dialect = PostgreSqlDialect;
    let identifier = Fragment::identifier(&dialect, "only_identifier");

    let query = identifier.render(&dialect).unwrap();

    assert_eq!(query.sql, "\"only_identifier\"");
    assert!(query.parameters.is_none());
}
