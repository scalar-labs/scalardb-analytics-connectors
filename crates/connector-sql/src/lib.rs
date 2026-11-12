//! Processing shared by SQL-speaking Connectors.
//!
//! A Connector whose Data Source speaks SQL through an ADBC driver implements
//! [`SqlConnector`] and names this crate's components: [`SqlCatalog`] runs its
//! catalog reads (see [`catalog`]), [`SqlPlanner`] moves work into the remote SQL by the
//! Connector's semantic decision and renders one non-parameterized statement
//! from dialect-rendered identifiers and typed literals, and [`SqlExecutor`]
//! sends that statement. This crate does not define a SQL grammar.

#![forbid(unsafe_code)]

pub mod catalog;
mod connector;
mod executor;
pub mod literal;
mod planner;
mod read;
mod step;
mod translation;

use connector_sdk::LiteralValue;

pub use connector::{SqlCatalog, SqlConnector};
pub use executor::SqlExecutor;
pub use planner::{PlanningMetadata, SqlPlanner};
pub use read::SqlRead;
pub use translation::{TranslationError, translate};

/// Connector-owned representation rules needed while rendering SQL.
pub trait SqlDialect {
    /// Quotes one physical identifier according to the Data Source's rules.
    fn quote_identifier(identifier: &str) -> String;

    /// Writes one validated typed literal according to the Data Source's rules.
    fn write_literal(literal: &LiteralValue, sql: &mut String);
}

/// Connector-trusted syntax and dialect-rendered values composed as SQL text.
pub struct Fragment {
    tokens: Vec<Token>,
}

impl Fragment {
    /// Renders a physical identifier only through the SQL dialect.
    pub fn identifier<D: SqlDialect>(identifier: &str) -> Self {
        Self::text(D::quote_identifier(identifier))
    }

    /// Renders one typed scalar only through the SQL dialect.
    pub fn literal<D: SqlDialect>(value: &LiteralValue) -> Self {
        let mut sql = String::new();
        D::write_literal(value, &mut sql);
        Self::text(sql)
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

    /// Finalizes one non-parameterized SQL statement.
    pub fn render(&self) -> RenderedQuery {
        let mut sql = String::new();
        for token in &self.tokens {
            token.render(&mut sql);
        }
        RenderedQuery { sql }
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
}

impl Token {
    /// Renders one token without adding implicit whitespace.
    fn render(&self, sql: &mut String) {
        match self {
            Self::Static(text) => sql.push_str(text),
            Self::Text(text) => sql.push_str(text),
        }
    }
}

/// Immutable non-parameterized SQL rendered from one remote plan.
#[derive(Clone, PartialEq, Eq)]
pub struct RenderedQuery {
    pub sql: String,
}

impl std::fmt::Debug for RenderedQuery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RenderedQuery")
            .field("sql_length", &self.sql.len())
            .finish()
    }
}
