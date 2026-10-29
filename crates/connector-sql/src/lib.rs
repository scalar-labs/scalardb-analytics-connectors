//! Connector-defined SQL rendering primitives.
//!
//! This crate preserves SQL syntax and typed parameters as fragments until a
//! final traversal produces query text and an Arrow parameter batch. It does
//! not define a SQL grammar; SQL dialects supply identifier quoting,
//! placeholder notation, and Connector-trusted static syntax.

#![forbid(unsafe_code)]

use std::sync::Arc;

use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{ArrowError, Field, Schema};
use thiserror::Error;

/// Connector-owned representation rules needed while rendering SQL.
pub trait SqlDialect {
    /// A Connector-owned closed token or recipe for trusted SQL syntax.
    type OpaqueFragment;

    /// Quotes one physical identifier according to the Data Source's rules.
    fn quote_identifier(&self, identifier: &str) -> String;

    /// Writes the placeholder for a one-based parameter position.
    fn write_placeholder(&self, position: usize, sql: &mut String);

    /// Writes one Connector-owned opaque token or recipe.
    fn write_opaque_fragment(&self, fragment: &Self::OpaqueFragment, sql: &mut String);
}

/// A SQL fragment belonging to exactly one rendering context.
#[derive(Clone)]
pub struct Fragment {
    context: Arc<ContextIdentity>,
    parts: Arc<[Part]>,
}

impl std::fmt::Debug for Fragment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Fragment")
            .field("parts", &self.parts.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct ContextIdentity;

#[derive(Clone)]
enum Part {
    Text(String),
    Parameter(ArrayRef),
    Nested(Arc<[Part]>),
}

/// Builds and finalizes fragments under one SQL dialect.
pub struct RenderContext<D> {
    dialect: D,
    identity: Arc<ContextIdentity>,
}

impl<D: SqlDialect> RenderContext<D> {
    pub fn new(dialect: D) -> Self {
        Self {
            dialect,
            identity: Arc::new(ContextIdentity),
        }
    }

    /// Renders a physical identifier only through the SQL dialect.
    pub fn identifier(&self, identifier: &str) -> Fragment {
        self.text(self.dialect.quote_identifier(identifier))
    }

    /// Adds Connector-owned trusted syntax without accepting raw SQL text.
    pub fn opaque(&self, fragment: &D::OpaqueFragment) -> Fragment {
        let mut sql = String::new();
        self.dialect.write_opaque_fragment(fragment, &mut sql);
        self.text(sql)
    }

    /// Preserves one typed scalar as a parameter node.
    pub fn parameter(&self, value: ArrayRef) -> Result<Fragment, RenderError> {
        if value.len() != 1 {
            return Err(RenderError::InvalidParameterLength {
                actual: value.len(),
            });
        }
        Ok(Fragment {
            context: Arc::clone(&self.identity),
            parts: Arc::from([Part::Parameter(value)]),
        })
    }

    /// Composes fragments without flattening their parameters.
    pub fn compose<'a>(
        &self,
        fragments: impl IntoIterator<Item = &'a Fragment>,
    ) -> Result<Fragment, RenderError> {
        let mut parts = Vec::new();
        for fragment in fragments {
            self.require_local(fragment)?;
            parts.push(Part::Nested(Arc::clone(&fragment.parts)));
        }
        Ok(Fragment {
            context: Arc::clone(&self.identity),
            parts: parts.into(),
        })
    }

    /// Finalizes SQL text and its ordered parameter mapping together.
    pub fn render(&self, fragment: &Fragment) -> Result<RenderedQuery, RenderError> {
        self.require_local(fragment)?;

        let mut sql = String::new();
        let mut parameters = Vec::new();
        self.render_parts(&fragment.parts, &mut sql, &mut parameters);

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

    fn text(&self, value: String) -> Fragment {
        Fragment {
            context: Arc::clone(&self.identity),
            parts: Arc::from([Part::Text(value)]),
        }
    }

    fn require_local(&self, fragment: &Fragment) -> Result<(), RenderError> {
        if Arc::ptr_eq(&self.identity, &fragment.context) {
            Ok(())
        } else {
            Err(RenderError::ForeignFragment)
        }
    }

    fn render_parts(&self, parts: &[Part], sql: &mut String, parameters: &mut Vec<ArrayRef>) {
        for part in parts {
            match part {
                Part::Text(text) => sql.push_str(text),
                Part::Parameter(value) => {
                    parameters.push(Arc::clone(value));
                    self.dialect.write_placeholder(parameters.len(), sql);
                }
                Part::Nested(parts) => self.render_parts(parts, sql, parameters),
            }
        }
    }
}

/// Immutable output consumed by a query execution boundary.
pub struct RenderedQuery {
    sql: String,
    parameters: Option<RecordBatch>,
}

impl RenderedQuery {
    pub fn sql(&self) -> &str {
        &self.sql
    }

    pub fn parameters(&self) -> Option<&RecordBatch> {
        self.parameters.as_ref()
    }
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

    #[error("a SQL fragment belongs to a different rendering context")]
    ForeignFragment,

    #[error("failed to build the Arrow parameter batch: {0}")]
    ParameterBatch(#[from] ArrowError),
}
