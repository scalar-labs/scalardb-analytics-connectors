use std::sync::Arc;

use arrow_array::{Array, Int32Array, StringArray};
use arrow_schema::DataType;
use connector_sql::{RenderContext, RenderError, SqlDialect};

struct PostgreSqlDialect;

enum Syntax {
    Equals,
    Comma,
}

impl SqlDialect for PostgreSqlDialect {
    type OpaqueFragment = Syntax;

    fn quote_identifier(&self, identifier: &str) -> String {
        format!("\"{}\"", identifier.replace('"', "\"\""))
    }

    fn write_placeholder(&self, position: usize, sql: &mut String) {
        sql.push('$');
        sql.push_str(&position.to_string());
    }

    fn write_opaque_fragment(&self, fragment: &Self::OpaqueFragment, sql: &mut String) {
        sql.push_str(match fragment {
            Syntax::Equals => " = ",
            Syntax::Comma => ", ",
        });
    }
}

#[test]
fn connector_syntax_and_runtime_values_use_distinct_boundaries() {
    let context = RenderContext::new(PostgreSqlDialect);
    let identifier = context.identifier("odd\"column");
    let separator = context.opaque(&Syntax::Equals);
    let value = context
        .parameter(Arc::new(StringArray::from(vec![Some("secret-value")])))
        .unwrap();
    let fragment = context.compose([&identifier, &separator, &value]).unwrap();

    let query = context.render(&fragment).unwrap();

    assert_eq!(query.sql(), "\"odd\"\"column\" = $1");
    assert!(!query.sql().contains("secret-value"));
    let parameters = query.parameters().unwrap();
    let values = parameters
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(values.value(0), "secret-value");
}

#[test]
fn final_traversal_orders_repeated_and_reordered_parameters() {
    let context = RenderContext::new(PostgreSqlDialect);
    let first = context
        .parameter(Arc::new(Int32Array::from(vec![Some(10)])))
        .unwrap();
    let second = context
        .parameter(Arc::new(Int32Array::from(vec![Some(20)])))
        .unwrap();
    let comma = context.opaque(&Syntax::Comma);
    let fragment = context
        .compose([&second, &comma, &first, &comma, &second])
        .unwrap();

    let query = context.render(&fragment).unwrap();

    assert_eq!(query.sql(), "$1, $2, $3");
    let parameters = query.parameters().unwrap();
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
    let context = RenderContext::new(PostgreSqlDialect);
    let null = context
        .parameter(Arc::new(StringArray::from(vec![None::<&str>])))
        .unwrap();

    let query = context.render(&null).unwrap();

    let parameters = query.parameters().unwrap();
    assert_eq!(parameters.schema().field(0).data_type(), &DataType::Utf8);
    assert!(parameters.column(0).is_null(0));
}

#[test]
fn foreign_fragments_are_rejected() {
    let first_context = RenderContext::new(PostgreSqlDialect);
    let second_context = RenderContext::new(PostgreSqlDialect);
    let foreign = first_context.identifier("foreign");

    assert!(matches!(
        second_context.compose([&foreign]),
        Err(RenderError::ForeignFragment)
    ));
    assert!(matches!(
        second_context.render(&foreign),
        Err(RenderError::ForeignFragment)
    ));
}

#[test]
fn parameter_must_be_one_typed_scalar() {
    let context = RenderContext::new(PostgreSqlDialect);

    assert!(matches!(
        context.parameter(Arc::new(Int32Array::from(Vec::<i32>::new()))),
        Err(RenderError::InvalidParameterLength { actual: 0 })
    ));
    assert!(matches!(
        context.parameter(Arc::new(Int32Array::from(vec![1, 2]))),
        Err(RenderError::InvalidParameterLength { actual: 2 })
    ));
}

#[test]
fn query_without_parameters_has_no_binding_batch() {
    let context = RenderContext::new(PostgreSqlDialect);
    let identifier = context.identifier("only_identifier");

    let query = context.render(&identifier).unwrap();

    assert_eq!(query.sql(), "\"only_identifier\"");
    assert!(query.parameters().is_none());
}
