//! Direct translation of a validated remote logical plan into SQL fragments.

use std::marker::PhantomData;

use analytics_types::AnalyticsField;
use connector_sdk::{
    BinaryOperator, LogicalExpression, LogicalExpressionKind, LogicalPlan, LogicalRelation,
    UnaryOperator,
};
use thiserror::Error;

use crate::{Fragment, RenderedQuery, SqlDialect};

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
}

/// Translates one validated remote `LogicalPlan` in dialect `D` without
/// building another AST.
pub fn translate<D>(
    plan: &LogicalPlan,
    relation: &[String],
    columns: &[String],
) -> Result<RenderedQuery, TranslationError>
where
    D: SqlDialect,
{
    let translator = Translator::<D> {
        plan,
        relation,
        columns,
        _dialect: PhantomData,
    };
    let fragment = translator.render_plan()?;
    Ok(fragment.render())
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
    _dialect: PhantomData<fn() -> D>,
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
            let predicate = self.render_expression(condition)?;
            parts.extend([Fragment::fixed(WHERE), predicate]);
        }

        Ok(Fragment::compose(parts))
    }

    /// Renders one resolved expression while retaining unsupported function
    /// declarations as a normal translation outcome.
    fn render_expression(
        &self,
        expression: &LogicalExpression,
    ) -> Result<Fragment, TranslationError> {
        let fragment = match &expression.kind {
            LogicalExpressionKind::Binary {
                operator,
                left,
                right,
            } => match operator {
                BinaryOperator::Equal => self.render_comparison(left, right, EQUAL)?,
                BinaryOperator::NotEqual => self.render_comparison(left, right, NOT_EQUAL)?,
                BinaryOperator::LessThan => self.render_comparison(left, right, LESS_THAN)?,
                BinaryOperator::LessThanOrEqual => {
                    self.render_comparison(left, right, LESS_THAN_OR_EQUAL)?
                }
                BinaryOperator::GreaterThan => self.render_comparison(left, right, GREATER_THAN)?,
                BinaryOperator::GreaterThanOrEqual => {
                    self.render_comparison(left, right, GREATER_THAN_OR_EQUAL)?
                }
                BinaryOperator::And => self.render_binary_boolean(left, right, AND)?,
                BinaryOperator::Or => self.render_binary_boolean(left, right, OR)?,
            },
            LogicalExpressionKind::Unary { operator, operand } => match operator {
                UnaryOperator::Not => {
                    let operand = self.render_expression(operand)?;
                    Fragment::compose([Fragment::fixed(NOT), operand]).parenthesize()
                }
                UnaryOperator::IsNull => self.render_null_test(operand, false)?,
                UnaryOperator::IsNotNull => self.render_null_test(operand, true)?,
            },
            LogicalExpressionKind::UnsupportedScalarFunction { uri, name, .. } => {
                return Err(TranslationError::Unsupported {
                    reason: format!("scalar function {name} from {uri} has no SQL mapping"),
                });
            }
            LogicalExpressionKind::Field { position } => self.render_column(*position),
            LogicalExpressionKind::Literal(value) => Fragment::literal::<D>(value),
        };
        Ok(fragment)
    }

    /// Renders one comparison already approved for remote execution.
    fn render_comparison(
        &self,
        left: &LogicalExpression,
        right: &LogicalExpression,
        operator: &'static str,
    ) -> Result<Fragment, TranslationError> {
        let left = self.render_expression(left)?;
        let right = self.render_expression(right)?;
        Ok(Fragment::compose([left, Fragment::fixed(operator), right]).parenthesize())
    }

    /// Renders a binary Boolean node recursively and preserves grouping.
    fn render_binary_boolean(
        &self,
        left: &LogicalExpression,
        right: &LogicalExpression,
        operator: &'static str,
    ) -> Result<Fragment, TranslationError> {
        let left = self.render_expression(left)?;
        let right = self.render_expression(right)?;
        Ok(Fragment::compose([left, Fragment::fixed(operator), right]).parenthesize())
    }

    /// Renders a null test over a validated field or typed literal.
    fn render_null_test(
        &self,
        operand: &LogicalExpression,
        negated: bool,
    ) -> Result<Fragment, TranslationError> {
        let value = self.render_expression(operand)?;
        let mut tokens = vec![value, Fragment::fixed(IS)];
        if negated {
            tokens.push(Fragment::fixed(NOT));
        }
        tokens.push(Fragment::fixed(NULL));
        Ok(Fragment::compose(tokens).parenthesize())
    }

    /// Maps projected base positions to physical columns and logical aliases.
    fn render_select_list(&self, aliases: &[AnalyticsField], projection: &[usize]) -> Fragment {
        Fragment::concatenate_with(
            projection.iter().zip(aliases).map(|(position, alias)| {
                let column = self.render_column(*position);
                let alias = Fragment::identifier::<D>(alias.name());
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
            .map(|component| Fragment::identifier::<D>(component))
            .collect::<Vec<_>>();
        Fragment::concatenate_with(parts, DOT)
    }

    /// Resolves and quotes one physical column by base-schema position.
    fn render_column(&self, position: usize) -> Fragment {
        let column = self
            .columns
            .get(position)
            .expect("loaded metadata covers every validated base-schema field");
        Fragment::identifier::<D>(column)
    }
}
