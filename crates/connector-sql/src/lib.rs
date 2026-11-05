//! Connector-defined SQL rendering primitives.
//!
//! This crate composes dialect-rendered identifiers and typed literals with
//! Connector-trusted SQL syntax into one non-parameterized statement. It does
//! not define a SQL grammar.

#![forbid(unsafe_code)]

mod translation;

use adbc_driver_manager::ManagedConnection;
use connector_adbc::AdbcQueryStream;
use connector_sdk::{ExecutableQuery, LiteralValue};

pub use translation::{TranslationError, translate};

/// Connector-owned representation rules needed while rendering SQL.
pub trait SqlDialect {
    /// Quotes one physical identifier according to the Data Source's rules.
    fn quote_identifier(&self, identifier: &str) -> String;

    /// Writes one validated typed literal according to the Data Source's rules.
    fn write_literal(&self, literal: &LiteralValue, sql: &mut String);
}

/// Connector-trusted syntax and dialect-rendered values composed as SQL text.
pub struct Fragment {
    tokens: Vec<Token>,
}

impl Fragment {
    /// Renders a physical identifier only through the SQL dialect.
    pub fn identifier<D: SqlDialect>(dialect: &D, identifier: &str) -> Self {
        Self::text(dialect.quote_identifier(identifier))
    }

    /// Renders one typed scalar only through the SQL dialect.
    pub fn literal<D: SqlDialect>(dialect: &D, value: &LiteralValue) -> Self {
        let mut sql = String::new();
        dialect.write_literal(value, &mut sql);
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

/// Immutable non-parameterized SQL consumed by a query execution boundary.
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

/// Executes one SQL [`ExecutableQuery`] through the shared ADBC query path.
///
/// The SQL was rendered while creating the Connector-owned planning state, so
/// execution only recovers that text and does not translate it again.
pub fn execute(
    connection: ManagedConnection,
    executable_query: &ExecutableQuery<RenderedQuery>,
) -> AdbcQueryStream {
    connector_adbc::query(connection, executable_query.query.sql.clone())
}
