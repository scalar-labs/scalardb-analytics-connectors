//! Connector-defined SQL rendering primitives.
//!
//! This crate preserves SQL syntax and typed parameters as fragments until a
//! final traversal produces query text and an Arrow parameter batch. It does
//! not define a SQL grammar; SQL dialects supply identifier quoting,
//! placeholder notation, and Connector-trusted fragments.

#![forbid(unsafe_code)]

use std::sync::Arc;

use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{ArrowError, Field, Schema};
use thiserror::Error;

mod translation;

pub use translation::{TranslationError, translate};

/// Connector-owned representation rules needed while rendering SQL.
pub trait SqlDialect {
    /// Quotes one physical identifier according to the Data Source's rules.
    fn quote_identifier(&self, identifier: &str) -> String;

    /// Writes the placeholder for a one-based parameter position.
    fn write_placeholder(&self, position: usize, sql: &mut String);
}

/// Deferred SQL text and typed parameters rendered together at the boundary.
pub struct Fragment {
    tokens: Vec<Token>,
}

impl Fragment {
    /// Renders a physical identifier only through the SQL dialect.
    pub fn identifier<D: SqlDialect>(dialect: &D, identifier: &str) -> Self {
        Self::text(dialect.quote_identifier(identifier))
    }

    /// Preserves one typed scalar as a parameter node.
    pub fn parameter(value: ArrayRef) -> Result<Self, RenderError> {
        if value.len() != 1 {
            return Err(RenderError::InvalidParameterLength {
                actual: value.len(),
            });
        }
        Ok(Self {
            tokens: vec![Token::Parameter(value)],
        })
    }

    /// Composes fragments as one whitespace-separated token sequence.
    pub fn compose(fragments: impl IntoIterator<Item = Self>) -> Self {
        let mut result = Self { tokens: Vec::new() };
        for (index, fragment) in fragments.into_iter().enumerate() {
            if index != 0 {
                result.tokens.push(Token::Static(" "));
            }
            result.tokens.extend(fragment.tokens);
        }
        result
    }

    /// Wraps common fixed SQL text for composition with other fragments.
    pub(crate) fn fixed(text: &'static str) -> Self {
        Self {
            tokens: vec![Token::Static(text)],
        }
    }

    /// Concatenates fragments with fixed text and no surrounding whitespace.
    pub(crate) fn concatenate_with(
        fragments: impl IntoIterator<Item = Self>,
        separator: &'static str,
    ) -> Self {
        let mut result = Self { tokens: Vec::new() };
        for (index, fragment) in fragments.into_iter().enumerate() {
            if index != 0 {
                result.tokens.push(Token::Static(separator));
            }
            result.tokens.extend(fragment.tokens);
        }
        result
    }

    /// Adds parentheses without whitespace around this fragment.
    pub(crate) fn parenthesize(self) -> Self {
        let mut tokens = Vec::with_capacity(self.tokens.len() + 2);
        tokens.push(Token::Static("("));
        tokens.extend(self.tokens);
        tokens.push(Token::Static(")"));
        Self { tokens }
    }

    /// Wraps text already rendered by a dialect as one fragment.
    fn text(value: String) -> Self {
        Self {
            tokens: vec![Token::Text(value)],
        }
    }

    /// Finalizes SQL text and its ordered parameter mapping together.
    pub fn render<D: SqlDialect>(&self, dialect: &D) -> Result<RenderedQuery, RenderError> {
        let mut sql = String::new();
        let mut parameters = Vec::new();
        self.render_tokens(dialect, &mut sql, &mut parameters);

        let parameters = if parameters.is_empty() {
            None
        } else {
            let fields = parameters
                .iter()
                .enumerate()
                .map(|(index, parameter)| {
                    Field::new(
                        format!("p{}", index + 1),
                        parameter.data_type().clone(),
                        true,
                    )
                })
                .collect::<Vec<_>>();
            Some(RecordBatch::try_new(
                Arc::new(Schema::new(fields)),
                parameters,
            )?)
        };

        Ok(RenderedQuery { sql, parameters })
    }

    /// Writes the flat token sequence and assigns placeholder positions.
    fn render_tokens<D: SqlDialect>(
        &self,
        dialect: &D,
        sql: &mut String,
        parameters: &mut Vec<ArrayRef>,
    ) {
        for token in &self.tokens {
            token.render(dialect, sql, parameters);
        }
    }
}

impl std::fmt::Debug for Fragment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Fragment")
            .field("tokens", &self.tokens.len())
            .finish_non_exhaustive()
    }
}

/// One element in a flat deferred-rendering sequence.
enum Token {
    Static(&'static str),
    Text(String),
    Parameter(ArrayRef),
}

impl Token {
    /// Renders one token without adding implicit whitespace.
    fn render<D: SqlDialect>(&self, dialect: &D, sql: &mut String, parameters: &mut Vec<ArrayRef>) {
        match self {
            Self::Static(text) => sql.push_str(text),
            Self::Text(text) => sql.push_str(text),
            Self::Parameter(value) => {
                parameters.push(Arc::clone(value));
                dialect.write_placeholder(parameters.len(), sql);
            }
        }
    }
}

/// Immutable output consumed by a query execution boundary.
pub struct RenderedQuery {
    pub sql: String,
    pub parameters: Option<RecordBatch>,
}

impl std::fmt::Debug for RenderedQuery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RenderedQuery")
            .field("sql", &self.sql)
            .field(
                "parameter_count",
                &self.parameters.as_ref().map_or(0, RecordBatch::num_columns),
            )
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("a typed SQL parameter must contain exactly one value, but contained {actual}")]
    InvalidParameterLength { actual: usize },

    #[error("failed to build the Arrow parameter batch: {0}")]
    ParameterBatch(#[from] ArrowError),
}
