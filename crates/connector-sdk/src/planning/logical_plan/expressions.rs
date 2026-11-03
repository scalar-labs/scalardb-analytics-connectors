//! Validation of the initial Substrait expression profile.

use analytics_types::{AnalyticsSchema, AnalyticsType, DecimalType};
use substrait::proto::{Expression, FunctionArgument, Type, expression, function_argument};

use super::{
    FunctionDeclaration, ScalarFunction, extensions::ExtensionRegistry, types::parse_type,
};
use crate::RequestError;

pub(super) const COMPARISON_URI: &str =
    "https://github.com/substrait-io/substrait/blob/v0.69.0/extensions/functions_comparison.yaml";
pub(super) const BOOLEAN_URI: &str =
    "https://github.com/substrait-io/substrait/blob/v0.69.0/extensions/functions_boolean.yaml";

/// Classifies only the pinned URI/signature pairs owned by the initial profile.
///
/// A declared function outside this set remains structurally valid as
/// `Unknown` so a Connector can return `Unsupported` during translation.
pub(super) fn classify_function(declaration: FunctionDeclaration<'_>) -> ScalarFunction {
    match (declaration.uri, declaration.name) {
        (COMPARISON_URI, "equal:any1_any1") => ScalarFunction::Equal,
        (COMPARISON_URI, "not_equal:any1_any1") => ScalarFunction::NotEqual,
        (COMPARISON_URI, "lt:any1_any1") => ScalarFunction::LessThan,
        (COMPARISON_URI, "lte:any1_any1") => ScalarFunction::LessThanOrEqual,
        (COMPARISON_URI, "gt:any1_any1") => ScalarFunction::GreaterThan,
        (COMPARISON_URI, "gte:any1_any1") => ScalarFunction::GreaterThanOrEqual,
        (BOOLEAN_URI, "and:bool_bool") => ScalarFunction::And,
        (BOOLEAN_URI, "or:bool_bool") => ScalarFunction::Or,
        (BOOLEAN_URI, "not:bool") => ScalarFunction::Not,
        (COMPARISON_URI, "is_null:any") => ScalarFunction::IsNull,
        (COMPARISON_URI, "is_not_null:any") => ScalarFunction::IsNotNull,
        _ => ScalarFunction::Unknown,
    }
}

/// Validates one filter predicate against the read schema and extension declarations.
pub(super) fn validate_filter_expression(
    expression: &Expression,
    schema: &AnalyticsSchema,
    extensions: &ExtensionRegistry,
) -> Result<(), RequestError> {
    let data_type = validate_predicate(expression, schema, extensions)?;
    if data_type != AnalyticsType::Boolean {
        return Err(RequestError::invalid(
            "a filter condition must produce a Boolean value",
        ));
    }
    Ok(())
}

/// Recursively validates predicate arity and types while preserving unknown
/// declared functions for the later capability decision.
fn validate_predicate(
    expression: &Expression,
    schema: &AnalyticsSchema,
    extensions: &ExtensionRegistry,
) -> Result<AnalyticsType, RequestError> {
    let Some(expression::RexType::ScalarFunction(function)) = expression.rex_type.as_ref() else {
        return Err(RequestError::undeclared(
            "the initial filter profile accepts only declared scalar functions as predicates",
        ));
    };
    #[allow(deprecated)]
    let has_deprecated_arguments = !function.args.is_empty();
    if has_deprecated_arguments || !function.options.is_empty() {
        return Err(RequestError::undeclared(
            "deprecated scalar arguments and function options are not accepted",
        ));
    }
    let declaration = extensions
        .functions
        .get(&function.function_reference)
        .ok_or_else(|| {
            RequestError::invalid(format!(
                "scalar function anchor {} is not declared",
                function.function_reference
            ))
        })?;
    require_boolean_output(function.output_type.as_ref(), extensions)?;

    let kind = classify_function(FunctionDeclaration {
        uri: &declaration.uri,
        name: &declaration.name,
    });
    match kind {
        ScalarFunction::Equal
        | ScalarFunction::NotEqual
        | ScalarFunction::LessThan
        | ScalarFunction::LessThanOrEqual
        | ScalarFunction::GreaterThan
        | ScalarFunction::GreaterThanOrEqual => {
            let [left, right] = require_value_arguments::<2>(&function.arguments)?;
            let left = validate_value(left, schema, extensions)?;
            let right = validate_value(right, schema, extensions)?;
            if left != right {
                return Err(RequestError::invalid(
                    "comparison operands must have the same Analytics type",
                ));
            }
        }
        ScalarFunction::And | ScalarFunction::Or => {
            let [left, right] = require_value_arguments::<2>(&function.arguments)?;
            validate_predicate(left, schema, extensions)?;
            validate_predicate(right, schema, extensions)?;
        }
        ScalarFunction::Not => {
            let [argument] = require_value_arguments::<1>(&function.arguments)?;
            validate_predicate(argument, schema, extensions)?;
        }
        ScalarFunction::IsNull | ScalarFunction::IsNotNull => {
            let [argument] = require_value_arguments::<1>(&function.arguments)?;
            validate_value(argument, schema, extensions)?;
        }
        ScalarFunction::Unknown => {
            for argument in value_arguments(&function.arguments)? {
                validate_expression_shape(argument, schema, extensions)?;
            }
        }
    }
    Ok(AnalyticsType::Boolean)
}

/// Applies the common structural profile to arguments of an unknown function.
///
/// Their semantics are not interpreted here, but malformed nested expressions
/// must not bypass the normal field, literal, and function checks.
fn validate_expression_shape(
    expression: &Expression,
    schema: &AnalyticsSchema,
    extensions: &ExtensionRegistry,
) -> Result<(), RequestError> {
    match expression.rex_type.as_ref() {
        Some(expression::RexType::Selection(_)) | Some(expression::RexType::Literal(_)) => {
            validate_value(expression, schema, extensions)?;
        }
        Some(expression::RexType::ScalarFunction(_)) => {
            validate_predicate(expression, schema, extensions)?;
        }
        _ => {
            return Err(RequestError::undeclared(
                "the expression uses a shape outside the initial filter profile",
            ));
        }
    }
    Ok(())
}

/// Resolves one accepted value operand to its Analytics type.
fn validate_value(
    expression: &Expression,
    schema: &AnalyticsSchema,
    extensions: &ExtensionRegistry,
) -> Result<AnalyticsType, RequestError> {
    match expression.rex_type.as_ref() {
        Some(expression::RexType::Selection(selection)) => {
            let position = direct_field_position(selection)?;
            schema
                .fields()
                .get(position)
                .map(|field| field.data_type().clone())
                .ok_or_else(|| {
                    RequestError::invalid(format!(
                        "filter field position {position} is outside the read schema"
                    ))
                })
        }
        Some(expression::RexType::Literal(literal)) => literal_type(literal, extensions),
        _ => Err(RequestError::undeclared(
            "comparison and null-test operands must be direct fields or typed literals",
        )),
    }
}

/// Extracts a base-schema position and rejects nested or non-root references.
fn direct_field_position(selection: &expression::FieldReference) -> Result<usize, RequestError> {
    if !matches!(
        selection.root_type,
        Some(expression::field_reference::RootType::RootReference(_))
    ) {
        return Err(RequestError::undeclared(
            "a filter field reference must use the input root",
        ));
    }
    let Some(expression::field_reference::ReferenceType::DirectReference(reference)) =
        selection.reference_type.as_ref()
    else {
        return Err(RequestError::undeclared(
            "a filter field reference must be direct",
        ));
    };
    let Some(expression::reference_segment::ReferenceType::StructField(field)) =
        reference.reference_type.as_ref()
    else {
        return Err(RequestError::undeclared(
            "a filter field reference must select a struct field",
        ));
    };
    if field.child.is_some() {
        return Err(RequestError::undeclared(
            "nested filter field references are not accepted",
        ));
    }
    usize::try_from(field.field)
        .map_err(|_| RequestError::invalid("a filter field position is negative"))
}

/// Validates a literal's representation and returns the exact Analytics type
/// used for operand compatibility checks and Arrow parameter construction.
fn literal_type(
    literal: &expression::Literal,
    extensions: &ExtensionRegistry,
) -> Result<AnalyticsType, RequestError> {
    if literal.type_variation_reference != 0 {
        return Err(RequestError::undeclared(
            "literal type variations are not accepted",
        ));
    }
    match literal.literal_type.as_ref() {
        Some(expression::literal::LiteralType::Boolean(_)) => Ok(AnalyticsType::Boolean),
        Some(expression::literal::LiteralType::I8(value)) => {
            i8::try_from(*value)
                .map_err(|_| RequestError::invalid("an i8 literal is out of range"))?;
            Ok(AnalyticsType::Byte)
        }
        Some(expression::literal::LiteralType::I16(value)) => {
            i16::try_from(*value)
                .map_err(|_| RequestError::invalid("an i16 literal is out of range"))?;
            Ok(AnalyticsType::SmallInt)
        }
        Some(expression::literal::LiteralType::I32(_)) => Ok(AnalyticsType::Int),
        Some(expression::literal::LiteralType::I64(_)) => Ok(AnalyticsType::BigInt),
        Some(expression::literal::LiteralType::Fp32(_)) => Ok(AnalyticsType::Float),
        Some(expression::literal::LiteralType::Fp64(_)) => Ok(AnalyticsType::Double),
        Some(expression::literal::LiteralType::String(_)) => Ok(AnalyticsType::Text),
        Some(expression::literal::LiteralType::Binary(_)) => Ok(AnalyticsType::Blob),
        Some(expression::literal::LiteralType::Date(_)) => Ok(AnalyticsType::Date),
        Some(expression::literal::LiteralType::PrecisionTime(value)) if value.precision == 6 => {
            if !(0..86_400_000_000).contains(&value.value) {
                return Err(RequestError::invalid("a time literal is outside one day"));
            }
            Ok(AnalyticsType::Time)
        }
        Some(expression::literal::LiteralType::PrecisionTimestamp(value))
            if value.precision == 6 =>
        {
            Ok(AnalyticsType::Timestamp)
        }
        Some(expression::literal::LiteralType::PrecisionTimestampTz(value))
            if value.precision == 6 =>
        {
            Ok(AnalyticsType::TimestampTz)
        }
        Some(expression::literal::LiteralType::IntervalDayToSecond(value)) => {
            require_interval_precision(value, 6)?;
            validate_duration_value(value)?;
            Ok(AnalyticsType::Duration)
        }
        Some(expression::literal::LiteralType::IntervalCompound(value)) => {
            let day = value.interval_day_to_second.as_ref().ok_or_else(|| {
                RequestError::invalid("an interval compound literal requires a day-to-second part")
            })?;
            require_interval_precision(day, 9)?;
            let year_month = value.interval_year_to_month.as_ref().ok_or_else(|| {
                RequestError::invalid("an interval compound literal requires a year-to-month part")
            })?;
            validate_compound_interval_value(year_month, day)?;
            Ok(AnalyticsType::Interval)
        }
        Some(expression::literal::LiteralType::Decimal(value)) => {
            let precision = u8::try_from(value.precision)
                .map_err(|_| RequestError::invalid("literal decimal precision is invalid"))?;
            let scale = i8::try_from(value.scale)
                .map_err(|_| RequestError::invalid("literal decimal scale is invalid"))?;
            let decimal = DecimalType::new(precision, scale)
                .map_err(|error| RequestError::invalid(error.to_string()))?;
            let bytes: [u8; 16] = value.value.as_slice().try_into().map_err(|_| {
                RequestError::invalid("a decimal literal must contain exactly 16 bytes")
            })?;
            let unscaled = i128::from_le_bytes(bytes);
            let limit = 10_u128.pow(u32::from(precision));
            if unscaled.unsigned_abs() >= limit {
                return Err(RequestError::invalid(
                    "a decimal literal exceeds its declared precision",
                ));
            }
            Ok(AnalyticsType::Decimal(decimal))
        }
        Some(expression::literal::LiteralType::Null(data_type)) => {
            let (data_type, nullable) = parse_scalar_type(data_type, extensions)?;
            if !nullable {
                return Err(RequestError::invalid(
                    "a typed null must declare a nullable type",
                ));
            }
            Ok(data_type)
        }
        Some(expression::literal::LiteralType::UserDefined(value)) => {
            let Some(expression::literal::user_defined::Val::Struct(struct_value)) =
                value.val.as_ref()
            else {
                return Err(RequestError::undeclared(
                    "a logical literal must use the declared struct representation",
                ));
            };
            let [carrier] = struct_value.fields.as_slice() else {
                return Err(RequestError::invalid(
                    "a logical literal must contain exactly one carrier value",
                ));
            };
            let data_type = Type {
                kind: Some(substrait::proto::r#type::Kind::UserDefined(
                    substrait::proto::r#type::UserDefined {
                        type_reference: value.type_reference,
                        type_variation_reference: literal.type_variation_reference,
                        nullability: substrait::proto::r#type::Nullability::Required as i32,
                        type_parameters: value.type_parameters.clone(),
                    },
                )),
            };
            let (logical, _) = parse_scalar_type(&data_type, extensions)?;
            let carrier_type = literal_type(carrier, extensions)?;
            if logical_carrier_type(&logical) != Some(&carrier_type) {
                return Err(RequestError::invalid(
                    "a logical literal carrier does not match its declared logical type",
                ));
            }
            Ok(logical)
        }
        Some(_) => Err(RequestError::undeclared(
            "the literal uses a type outside the initial SQL translation profile",
        )),
        None => Err(RequestError::invalid(
            "a literal is missing its value and type",
        )),
    }
}

/// Restricts interval precision to the Arrow representation selected by the
/// initial translation profile.
fn require_interval_precision(
    value: &expression::literal::IntervalDayToSecond,
    expected: i32,
) -> Result<(), RequestError> {
    use expression::literal::interval_day_to_second::PrecisionMode;
    let precision = match value.precision_mode {
        Some(PrecisionMode::Microseconds(_)) => 6,
        Some(PrecisionMode::Precision(precision)) => precision,
        None => {
            return Err(RequestError::invalid(
                "an interval literal has no precision",
            ));
        }
    };
    if precision != expected {
        return Err(RequestError::undeclared(format!(
            "an interval literal must use precision {expected}"
        )));
    }
    Ok(())
}

/// Proves that a duration literal can be represented as checked microseconds.
fn validate_duration_value(
    value: &expression::literal::IntervalDayToSecond,
) -> Result<(), RequestError> {
    let subseconds = interval_subseconds(value, 6)?;
    require_consistent_interval_sign(value.days, value.seconds, subseconds)?;
    i64::from(value.days)
        .checked_mul(86_400_000_000)
        .and_then(|days| {
            i64::from(value.seconds)
                .checked_mul(1_000_000)
                .and_then(|seconds| days.checked_add(seconds))
        })
        .and_then(|whole| whole.checked_add(subseconds))
        .ok_or_else(|| RequestError::invalid("a duration literal is out of range"))?;
    Ok(())
}

/// Proves that both halves of a compound interval fit Arrow's month/day/nano
/// representation before translation constructs the parameter.
fn validate_compound_interval_value(
    year_month: &expression::literal::IntervalYearToMonth,
    day_time: &expression::literal::IntervalDayToSecond,
) -> Result<(), RequestError> {
    year_month
        .years
        .checked_mul(12)
        .and_then(|years| years.checked_add(year_month.months))
        .ok_or_else(|| RequestError::invalid("interval months are out of range"))?;
    let subseconds = interval_subseconds(day_time, 9)?;
    require_consistent_interval_sign(day_time.days, day_time.seconds, subseconds)?;
    i64::from(day_time.seconds)
        .checked_mul(1_000_000_000)
        .and_then(|seconds| seconds.checked_add(subseconds))
        .ok_or_else(|| RequestError::invalid("interval nanoseconds are out of range"))?;
    Ok(())
}

/// Normalizes the two Substrait precision encodings without changing units.
fn interval_subseconds(
    value: &expression::literal::IntervalDayToSecond,
    expected_precision: i32,
) -> Result<i64, RequestError> {
    use expression::literal::interval_day_to_second::PrecisionMode;
    match value.precision_mode {
        Some(PrecisionMode::Microseconds(micros)) if expected_precision == 6 => {
            Ok(i64::from(micros))
        }
        Some(PrecisionMode::Precision(precision)) if precision == expected_precision => {
            Ok(value.subseconds)
        }
        _ => Err(RequestError::invalid(
            "interval literal precision is invalid",
        )),
    }
}

/// Rejects mixed-sign components whose normalization would be ambiguous.
fn require_consistent_interval_sign(
    first: i32,
    second: i32,
    third: i64,
) -> Result<(), RequestError> {
    let has_positive = first > 0 || second > 0 || third > 0;
    let has_negative = first < 0 || second < 0 || third < 0;
    if has_positive && has_negative {
        return Err(RequestError::invalid(
            "an interval literal cannot mix positive and negative components",
        ));
    }
    Ok(())
}

/// Returns the scalar carrier required by each supported logical type.
fn logical_carrier_type(data_type: &AnalyticsType) -> Option<&'static AnalyticsType> {
    static TEXT: AnalyticsType = AnalyticsType::Text;
    static SMALL_INT: AnalyticsType = AnalyticsType::SmallInt;
    match data_type {
        AnalyticsType::Uuid
        | AnalyticsType::Json
        | AnalyticsType::Enum(_)
        | AnalyticsType::TimeWithTimeZone => Some(&TEXT),
        AnalyticsType::Year => Some(&SMALL_INT),
        _ => None,
    }
}

/// Reuses schema type parsing for a scalar type that must consume no field names.
pub(super) fn parse_scalar_type(
    data_type: &Type,
    extensions: &ExtensionRegistry,
) -> Result<(AnalyticsType, bool), RequestError> {
    let names = Vec::<String>::new();
    let mut names = names.iter();
    let result = parse_type(data_type, &mut names, extensions)?;
    if names.next().is_some() {
        return Err(RequestError::invalid(
            "a scalar expression type unexpectedly consumed field names",
        ));
    }
    Ok(result)
}

/// Verifies the declared function result independently of its signature name.
fn require_boolean_output(
    output_type: Option<&Type>,
    extensions: &ExtensionRegistry,
) -> Result<(), RequestError> {
    let output_type = output_type
        .ok_or_else(|| RequestError::invalid("a scalar function is missing its output type"))?;
    let (data_type, _) = parse_scalar_type(output_type, extensions)?;
    if data_type != AnalyticsType::Boolean {
        return Err(RequestError::invalid(
            "a filter scalar function must declare a Boolean output type",
        ));
    }
    Ok(())
}

/// Extracts exactly `N` value arguments after rejecting other argument kinds.
fn require_value_arguments<const N: usize>(
    arguments: &[FunctionArgument],
) -> Result<[&Expression; N], RequestError> {
    let values = value_arguments(arguments)?;
    values.try_into().map_err(|values: Vec<&Expression>| {
        RequestError::invalid(format!(
            "the scalar function requires {N} value arguments, but received {}",
            values.len()
        ))
    })
}

/// Converts Substrait's extensible argument union to the initial value-only profile.
fn value_arguments(arguments: &[FunctionArgument]) -> Result<Vec<&Expression>, RequestError> {
    arguments
        .iter()
        .map(|argument| match argument.arg_type.as_ref() {
            Some(function_argument::ArgType::Value(value)) => Ok(value),
            _ => Err(RequestError::undeclared(
                "the initial filter profile accepts only value function arguments",
            )),
        })
        .collect()
}
