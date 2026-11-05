//! Connector-owned expressions in the accepted Substrait profile.

use std::fmt;

use analytics_types::{AnalyticsSchema, AnalyticsType};
use substrait::proto::{
    Expression as SubstraitExpression, FunctionArgument, expression, function_argument,
};

use super::{
    LiteralValue, literal,
    profile::{PlanExtensions, ProfileFunction},
    types::{encode_type, parse_type},
};
use crate::RequestError;

/// One connector-owned expression that can be moved independently during planning.
#[derive(Clone, PartialEq)]
pub struct LogicalExpression {
    pub data_type: AnalyticsType,
    pub nullable: bool,
    pub kind: LogicalExpressionKind,
}

/// A unary operator recognized by the connector profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOperator {
    Not,
    IsNull,
    IsNotNull,
}

/// A binary operator recognized by the connector profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOperator {
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
    And,
    Or,
}

/// The connector-owned meaning of an expression.
#[derive(Clone, PartialEq)]
pub enum LogicalExpressionKind {
    Field {
        position: usize,
    },
    Literal(LiteralValue),
    Unary {
        operator: UnaryOperator,
        operand: Box<LogicalExpression>,
    },
    Binary {
        operator: BinaryOperator,
        left: Box<LogicalExpression>,
        right: Box<LogicalExpression>,
    },
    UnsupportedScalarFunction {
        uri: String,
        name: String,
        arguments: Vec<LogicalExpression>,
    },
}

impl LogicalExpression {
    /// Returns the first function that prevents this expression from being pushed down.
    pub fn unsupported_function(&self) -> Option<(&str, &str)> {
        self.kind.unsupported_function()
    }

    pub(super) fn from_substrait(
        expression: &SubstraitExpression,
        schema: &AnalyticsSchema,
        extensions: &PlanExtensions,
    ) -> Result<Self, RequestError> {
        parse_expression(expression, schema, extensions)
    }

    pub(super) fn into_substrait(self, extensions: &PlanExtensions) -> SubstraitExpression {
        let data_type = encode_type(&self.data_type, self.nullable, extensions);
        match self.kind {
            LogicalExpressionKind::Field { position } => field_reference(position),
            LogicalExpressionKind::Literal(value) => SubstraitExpression {
                rex_type: Some(expression::RexType::Literal(literal::encode(
                    value,
                    self.nullable,
                    extensions,
                ))),
            },
            LogicalExpressionKind::Unary { operator, operand } => scalar_function(
                extensions.profile_function_anchor(ProfileFunction::Unary(operator)),
                vec![*operand],
                data_type,
                extensions,
            ),
            LogicalExpressionKind::Binary {
                operator,
                left,
                right,
            } => scalar_function(
                extensions.profile_function_anchor(ProfileFunction::Binary(operator)),
                vec![*left, *right],
                data_type,
                extensions,
            ),
            LogicalExpressionKind::UnsupportedScalarFunction {
                uri,
                name,
                arguments,
            } => scalar_function(
                extensions.declared_function_anchor(&uri, &name),
                arguments,
                data_type,
                extensions,
            ),
        }
    }
}

impl fmt::Debug for LogicalExpression {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LogicalExpression")
            .field("data_type", &self.data_type)
            .field("nullable", &self.nullable)
            .field("kind", &self.kind)
            .finish()
    }
}

impl fmt::Debug for LogicalExpressionKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Field { position } => formatter
                .debug_struct("Field")
                .field("position", position)
                .finish(),
            Self::Literal(_) => formatter.write_str("Literal(<redacted>)"),
            Self::Unary { operator, operand } => formatter
                .debug_struct("Unary")
                .field("operator", operator)
                .field("operand", operand)
                .finish(),
            Self::Binary {
                operator,
                left,
                right,
            } => formatter
                .debug_struct("Binary")
                .field("operator", operator)
                .field("left", left)
                .field("right", right)
                .finish(),
            Self::UnsupportedScalarFunction {
                uri,
                name,
                arguments,
            } => formatter
                .debug_struct("UnsupportedScalarFunction")
                .field("uri", uri)
                .field("name", name)
                .field("arguments", arguments)
                .finish(),
        }
    }
}

impl LogicalExpressionKind {
    fn unsupported_function(&self) -> Option<(&str, &str)> {
        match self {
            Self::Field { .. } | Self::Literal(_) => None,
            Self::Unary { operand, .. } => operand.unsupported_function(),
            Self::Binary { left, right, .. } => left
                .unsupported_function()
                .or_else(|| right.unsupported_function()),
            Self::UnsupportedScalarFunction { uri, name, .. } => Some((uri, name)),
        }
    }
}

/// Resolves one expression without imposing a relation-specific role on it.
fn parse_expression(
    expression: &SubstraitExpression,
    schema: &AnalyticsSchema,
    extensions: &PlanExtensions,
) -> Result<LogicalExpression, RequestError> {
    match expression.rex_type.as_ref() {
        Some(expression::RexType::Selection(selection)) => {
            let position = direct_field_position(selection)?;
            let field = schema.fields().get(position).ok_or_else(|| {
                RequestError::invalid(format!(
                    "field position {position} is outside the expression input schema"
                ))
            })?;
            Ok(LogicalExpression {
                data_type: field.data_type().clone(),
                nullable: field.is_nullable(),
                kind: LogicalExpressionKind::Field { position },
            })
        }
        Some(expression::RexType::Literal(value)) => {
            let nullable = value.nullable
                || matches!(
                    value.literal_type.as_ref(),
                    Some(expression::literal::LiteralType::Null(_))
                );
            let (data_type, value) = literal::parse(value, extensions)?;
            Ok(LogicalExpression {
                data_type,
                nullable,
                kind: LogicalExpressionKind::Literal(value),
            })
        }
        Some(expression::RexType::ScalarFunction(function)) => {
            parse_scalar_function(function, schema, extensions)
        }
        _ => Err(RequestError::undeclared(
            "the expression uses a shape outside the initial expression profile",
        )),
    }
}

fn parse_scalar_function(
    function: &expression::ScalarFunction,
    schema: &AnalyticsSchema,
    extensions: &PlanExtensions,
) -> Result<LogicalExpression, RequestError> {
    #[allow(deprecated)]
    let has_deprecated_arguments = !function.args.is_empty();
    if has_deprecated_arguments || !function.options.is_empty() {
        return Err(RequestError::undeclared(
            "deprecated scalar arguments and function options are not accepted",
        ));
    }
    let declaration = extensions
        .function_declaration(function.function_reference)
        .ok_or_else(|| {
            RequestError::invalid(format!(
                "scalar function anchor {} is not declared",
                function.function_reference
            ))
        })?;
    let output_type = function
        .output_type
        .as_ref()
        .ok_or_else(|| RequestError::invalid("a scalar function is missing its output type"))?;
    let (data_type, nullable) = parse_scalar_type(output_type, extensions)?;
    let mut arguments = value_arguments(&function.arguments)?
        .into_iter()
        .map(|argument| parse_expression(argument, schema, extensions))
        .collect::<Result<Vec<_>, _>>()?;

    let kind = match extensions.profile_function(function.function_reference) {
        Some(ProfileFunction::Unary(operator)) => {
            let [operand] = take_arguments::<1>(&mut arguments)?;
            LogicalExpressionKind::Unary {
                operator,
                operand: Box::new(operand),
            }
        }
        Some(ProfileFunction::Binary(operator)) => {
            let [left, right] = take_arguments::<2>(&mut arguments)?;
            LogicalExpressionKind::Binary {
                operator,
                left: Box::new(left),
                right: Box::new(right),
            }
        }
        None => LogicalExpressionKind::UnsupportedScalarFunction {
            uri: declaration.uri.to_owned(),
            name: declaration.name.to_owned(),
            arguments,
        },
    };
    Ok(LogicalExpression {
        data_type,
        nullable,
        kind,
    })
}

fn take_arguments<const N: usize>(
    arguments: &mut Vec<LogicalExpression>,
) -> Result<[LogicalExpression; N], RequestError> {
    if arguments.len() != N {
        return Err(RequestError::invalid(format!(
            "the scalar function requires {N} value arguments, but received {}",
            arguments.len()
        )));
    }
    Ok(std::mem::take(arguments)
        .try_into()
        .unwrap_or_else(|_| unreachable!("the argument count was checked before conversion")))
}

fn value_arguments(
    arguments: &[FunctionArgument],
) -> Result<Vec<&SubstraitExpression>, RequestError> {
    arguments
        .iter()
        .map(|argument| match argument.arg_type.as_ref() {
            Some(function_argument::ArgType::Value(value)) => Ok(value),
            _ => Err(RequestError::undeclared(
                "the initial expression profile accepts only value function arguments",
            )),
        })
        .collect()
}

fn direct_field_position(selection: &expression::FieldReference) -> Result<usize, RequestError> {
    if !matches!(
        selection.root_type,
        Some(expression::field_reference::RootType::RootReference(_))
    ) {
        return Err(RequestError::undeclared(
            "an expression field reference must use the input root",
        ));
    }
    let Some(expression::field_reference::ReferenceType::DirectReference(reference)) =
        selection.reference_type.as_ref()
    else {
        return Err(RequestError::undeclared(
            "an expression field reference must be direct",
        ));
    };
    let Some(expression::reference_segment::ReferenceType::StructField(field)) =
        reference.reference_type.as_ref()
    else {
        return Err(RequestError::undeclared(
            "an expression field reference must select a struct field",
        ));
    };
    if field.child.is_some() {
        return Err(RequestError::undeclared(
            "nested expression field references are not accepted",
        ));
    }
    usize::try_from(field.field)
        .map_err(|_| RequestError::invalid("an expression field position is negative"))
}

pub(super) fn parse_scalar_type(
    data_type: &substrait::proto::Type,
    extensions: &PlanExtensions,
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

fn field_reference(position: usize) -> SubstraitExpression {
    SubstraitExpression {
        rex_type: Some(expression::RexType::Selection(Box::new(
            expression::FieldReference {
                reference_type: Some(expression::field_reference::ReferenceType::DirectReference(
                    expression::ReferenceSegment {
                        reference_type: Some(
                            expression::reference_segment::ReferenceType::StructField(Box::new(
                                expression::reference_segment::StructField {
                                    field: i32::try_from(position)
                                        .expect("validated field positions fit in Substrait's i32"),
                                    child: None,
                                },
                            )),
                        ),
                    },
                )),
                root_type: Some(expression::field_reference::RootType::RootReference(
                    expression::field_reference::RootReference {},
                )),
            },
        ))),
    }
}

#[allow(deprecated)]
fn scalar_function(
    function_reference: u32,
    arguments: Vec<LogicalExpression>,
    output_type: substrait::proto::Type,
    extensions: &PlanExtensions,
) -> SubstraitExpression {
    SubstraitExpression {
        rex_type: Some(expression::RexType::ScalarFunction(
            expression::ScalarFunction {
                function_reference,
                arguments: arguments
                    .into_iter()
                    .map(|argument| FunctionArgument {
                        arg_type: Some(function_argument::ArgType::Value(
                            argument.into_substrait(extensions),
                        )),
                    })
                    .collect(),
                options: Vec::new(),
                output_type: Some(output_type),
                args: Vec::new(),
            },
        )),
    }
}
