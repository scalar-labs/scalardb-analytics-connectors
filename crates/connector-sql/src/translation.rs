//! Direct translation of a validated remote logical plan into SQL fragments.

use analytics_types::AnalyticsField;
use connector_sdk::{LogicalPlan, LogicalRelation, ScalarFunction};
use substrait::proto::{Expression, FunctionArgument, expression, function_argument};
use thiserror::Error;

use crate::{Fragment, RenderError, RenderedQuery, SqlDialect};

mod literal;

use literal::literal_array;

const SELECT: &str = "SELECT";
const FROM: &str = "FROM";
const WHERE: &str = "WHERE";
const AS: &str = "AS";
const DOT: &str = ".";
const AND: &str = "AND";
const OR: &str = "OR";
const NOT: &str = "NOT";
const IS: &str = "IS";
const NULL: &str = "NULL";
const EQUAL: &str = "=";
const NOT_EQUAL: &str = "<>";
const LESS_THAN: &str = "<";
const LESS_THAN_OR_EQUAL: &str = "<=";
const GREATER_THAN: &str = ">";
const GREATER_THAN_OR_EQUAL: &str = ">=";

/// Failure while translating a validated remote plan.
#[derive(Debug, Error)]
pub enum TranslationError {
    #[error("the SQL translation is unsupported: {reason}")]
    Unsupported { reason: String },

    #[error(transparent)]
    Render(#[from] RenderError),
}

/// Translates one validated remote `LogicalPlan` without building another AST.
pub fn translate<D>(
    plan: &LogicalPlan,
    relation: &[String],
    columns: &[String],
    dialect: D,
) -> Result<RenderedQuery, TranslationError>
where
    D: SqlDialect,
{
    let translator = Translator {
        plan,
        relation,
        columns,
        dialect: &dialect,
    };
    let fragment = translator.render_plan()?;
    Ok(fragment.render(&dialect)?)
}

/// State shared by every step of one direct `LogicalPlan` rendering traversal.
///
/// Keeping these dependencies together makes recursive expression rendering
/// explicit without threading them through every call.
struct Translator<'a, D>
where
    D: SqlDialect,
{
    plan: &'a LogicalPlan,
    relation: &'a [String],
    columns: &'a [String],
    dialect: &'a D,
}

impl<D> Translator<'_, D>
where
    D: SqlDialect,
{
    /// Renders the complete query shape so its `SELECT`/`FROM`/`WHERE` order is
    /// visible in one place.
    fn render_plan(&self) -> Result<Fragment, TranslationError> {
        let relation = match self.plan.relation() {
            LogicalRelation::Project { input, .. } => input.as_ref(),
            relation => relation,
        };
        let (read, filter) = match relation {
            LogicalRelation::Filter { input, condition } => (input.as_ref(), Some(condition)),
            relation => (relation, None),
        };
        let LogicalRelation::Read { .. } = read else {
            unreachable!("validated remote plans have the Project? -> Filter? -> Read shape")
        };

        let projection = self.plan.output_field_positions();
        let select_list = self.render_select_list(self.plan.output_schema().fields(), &projection);
        let from_relation = self.render_relation();
        let mut parts = vec![
            Fragment::fixed(SELECT),
            select_list,
            Fragment::fixed(FROM),
            from_relation,
        ];

        if let Some(condition) = filter {
            let predicate = self.render_predicate(condition)?;
            parts.extend([Fragment::fixed(WHERE), predicate]);
        }

        Ok(Fragment::compose(parts))
    }

    /// Renders one validated predicate while retaining unsupported function
    /// declarations as a normal translation outcome.
    fn render_predicate(&self, expression: &Expression) -> Result<Fragment, TranslationError> {
        let Some(expression::RexType::ScalarFunction(function)) = expression.rex_type.as_ref()
        else {
            unreachable!("validated filter predicates are scalar functions")
        };
        let kind = self
            .plan
            .scalar_function(function.function_reference)
            .expect("validated scalar function anchors are declared");
        let fragment = match kind {
            ScalarFunction::Equal => self.render_comparison(&function.arguments, EQUAL)?,
            ScalarFunction::NotEqual => self.render_comparison(&function.arguments, NOT_EQUAL)?,
            ScalarFunction::LessThan => self.render_comparison(&function.arguments, LESS_THAN)?,
            ScalarFunction::LessThanOrEqual => {
                self.render_comparison(&function.arguments, LESS_THAN_OR_EQUAL)?
            }
            ScalarFunction::GreaterThan => {
                self.render_comparison(&function.arguments, GREATER_THAN)?
            }
            ScalarFunction::GreaterThanOrEqual => {
                self.render_comparison(&function.arguments, GREATER_THAN_OR_EQUAL)?
            }
            ScalarFunction::And => self.render_binary_boolean(&function.arguments, AND)?,
            ScalarFunction::Or => self.render_binary_boolean(&function.arguments, OR)?,
            ScalarFunction::Not => {
                let argument = self.render_predicate(value_argument(&function.arguments[0]))?;
                Fragment::compose([Fragment::fixed(NOT), argument]).parenthesize()
            }
            ScalarFunction::IsNull => self.render_null_test(&function.arguments, false)?,
            ScalarFunction::IsNotNull => self.render_null_test(&function.arguments, true)?,
            ScalarFunction::Unknown => {
                let declaration = self
                    .plan
                    .function_declaration(function.function_reference)
                    .expect("validated scalar function anchors are declared");
                return Err(TranslationError::Unsupported {
                    reason: format!(
                        "scalar function {} from {} has no SQL mapping",
                        declaration.name, declaration.uri
                    ),
                });
            }
        };
        Ok(fragment)
    }

    /// Renders one comparison already approved for remote execution.
    fn render_comparison(
        &self,
        arguments: &[FunctionArgument],
        operator: &'static str,
    ) -> Result<Fragment, TranslationError> {
        let left = self.render_value(value_argument(&arguments[0]))?;
        let right = self.render_value(value_argument(&arguments[1]))?;
        Ok(Fragment::compose([left, Fragment::fixed(operator), right]).parenthesize())
    }

    /// Renders a binary Boolean node recursively and preserves grouping.
    fn render_binary_boolean(
        &self,
        arguments: &[FunctionArgument],
        operator: &'static str,
    ) -> Result<Fragment, TranslationError> {
        let left = self.render_predicate(value_argument(&arguments[0]))?;
        let right = self.render_predicate(value_argument(&arguments[1]))?;
        Ok(Fragment::compose([left, Fragment::fixed(operator), right]).parenthesize())
    }

    /// Renders a null test over a validated field or typed literal.
    fn render_null_test(
        &self,
        arguments: &[FunctionArgument],
        negated: bool,
    ) -> Result<Fragment, TranslationError> {
        let value = self.render_value(value_argument(&arguments[0]))?;
        let mut tokens = vec![value, Fragment::fixed(IS)];
        if negated {
            tokens.push(Fragment::fixed(NOT));
        }
        tokens.push(Fragment::fixed(NULL));
        Ok(Fragment::compose(tokens).parenthesize())
    }

    /// Renders one validated field or literal operand.
    fn render_value(&self, expression: &Expression) -> Result<Fragment, TranslationError> {
        match expression.rex_type.as_ref() {
            Some(expression::RexType::Selection(selection)) => {
                let position = direct_selection_position(selection);
                Ok(self.render_column(position))
            }
            Some(expression::RexType::Literal(literal)) => {
                Ok(Fragment::parameter(literal_array(self.plan, literal))?)
            }
            _ => unreachable!("validated values are direct fields or typed literals"),
        }
    }

    /// Maps projected base positions to physical columns and logical aliases.
    fn render_select_list(&self, aliases: &[AnalyticsField], projection: &[usize]) -> Fragment {
        Fragment::concatenate_with(
            projection.iter().zip(aliases).map(|(position, alias)| {
                let column = self.render_column(*position);
                let alias = Fragment::identifier(self.dialect, alias.name());
                Fragment::compose([column, Fragment::fixed(AS), alias])
            }),
            ", ",
        )
    }

    /// Quotes and joins the resolved physical relation components.
    fn render_relation(&self) -> Fragment {
        let parts = self
            .relation
            .iter()
            .map(|component| Fragment::identifier(self.dialect, component))
            .collect::<Vec<_>>();
        Fragment::concatenate_with(parts, DOT)
    }

    /// Resolves and quotes one physical column by base-schema position.
    fn render_column(&self, position: usize) -> Fragment {
        let column = self
            .columns
            .get(position)
            .expect("loaded metadata covers every validated base-schema field");
        Fragment::identifier(self.dialect, column)
    }
}

/// Reads one value from an SDK-validated scalar-function argument.
fn value_argument(argument: &FunctionArgument) -> &Expression {
    let Some(function_argument::ArgType::Value(value)) = argument.arg_type.as_ref() else {
        unreachable!("validated scalar functions have only value arguments")
    };
    value
}

/// Reads the position carried by an SDK-validated direct field reference.
fn direct_selection_position(selection: &expression::FieldReference) -> usize {
    let Some(expression::field_reference::ReferenceType::DirectReference(reference)) =
        selection.reference_type.as_ref()
    else {
        unreachable!("validated field references are direct")
    };
    let Some(expression::reference_segment::ReferenceType::StructField(field)) =
        reference.reference_type.as_ref()
    else {
        unreachable!("validated field references select struct fields")
    };
    usize::try_from(field.field).expect("validated field positions are non-negative")
}
