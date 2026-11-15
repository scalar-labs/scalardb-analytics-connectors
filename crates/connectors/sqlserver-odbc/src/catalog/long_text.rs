//! Reading `nvarchar(max)` catalog values in pieces.
//!
//! View definitions, default and computed column expressions, CHECK
//! expressions, filtered index conditions, and comments have no length limit.
//! `arrow-odbc` reads each column into a buffer of a fixed length, so it
//! cannot read such a value whole. The statement here returns each value as
//! its UTF-16LE bytes in pieces of at most 8000 bytes, one row per piece, and
//! the pieces are joined and decoded here. Splitting bytes rather than
//! characters keeps the value exact whatever the database's collation, which
//! decides whether `SUBSTRING` counts surrogate pairs as one character.

use std::collections::HashMap;

use arrow_array::BinaryArray;
use connector_sdk::{ConnectorError, ErrorKind};
use connector_sql::catalog::{column, int32, required, text, unexpected_catalog_result};

use super::Catalog;

/// The bytes of one piece; `varbinary(8000)` is the longest bounded binary.
const PIECE_BYTES: u32 = 8000;

/// What a long text value belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TextKind {
    /// A view's definition; `minor` is 0.
    Definition,
    /// A table's comment, `minor` 0, or a column's comment, `minor` its
    /// `column_id`.
    Comment,
    /// A column's default expression; `minor` is its `column_id`.
    Default,
    /// A computed column's expression; `minor` is its `column_id`.
    Computed,
    /// A CHECK constraint's expression; `minor` is the constraint's
    /// `object_id`.
    Check,
    /// A filtered index's condition; `minor` is its `index_id`.
    Filter,
}

impl TextKind {
    const ALL: [Self; 6] = [
        Self::Definition,
        Self::Comment,
        Self::Default,
        Self::Computed,
        Self::Check,
        Self::Filter,
    ];

    fn code(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Comment => "comment",
            Self::Default => "default",
            Self::Computed => "computed",
            Self::Check => "check",
            Self::Filter => "filter",
        }
    }

    /// A statement selecting `object_id`, `minor`, and the `nvarchar(max)`
    /// value of this kind for the tables `object_ids` selects.
    fn source(self, object_ids: &str) -> String {
        match self {
            Self::Definition => format!(
                "SELECT m.object_id, 0, m.definition FROM sys.sql_modules m \
                 WHERE m.object_id IN ({object_ids})"
            ),
            // A comment is the `MS_Description` extended property, by
            // convention; its value is a `sql_variant`.
            Self::Comment => format!(
                "SELECT p.major_id, p.minor_id, CAST(p.value AS nvarchar(max)) \
                 FROM sys.extended_properties p \
                 WHERE p.class = 1 AND p.name = N'MS_Description' \
                   AND p.major_id IN ({object_ids})"
            ),
            Self::Default => format!(
                "SELECT d.parent_object_id, d.parent_column_id, d.definition \
                 FROM sys.default_constraints d WHERE d.parent_object_id IN ({object_ids})"
            ),
            Self::Computed => format!(
                "SELECT c.object_id, c.column_id, c.definition FROM sys.computed_columns c \
                 WHERE c.object_id IN ({object_ids})"
            ),
            Self::Check => format!(
                "SELECT c.parent_object_id, c.object_id, c.definition \
                 FROM sys.check_constraints c WHERE c.parent_object_id IN ({object_ids})"
            ),
            Self::Filter => format!(
                "SELECT i.object_id, i.index_id, i.filter_definition FROM sys.indexes i \
                 WHERE i.has_filter = 1 AND i.object_id IN ({object_ids})"
            ),
        }
    }
}

/// The long text values of the tables `object_ids` selects, keyed by kind,
/// `object_id`, and minor identifier. A value the catalog leaves NULL, such
/// as an encrypted view's definition or one hidden without `VIEW
/// DEFINITION`, is absent.
pub(crate) struct LongTexts(HashMap<(TextKind, i32, i32), String>);

impl LongTexts {
    /// Reads every long text value of the tables in one statement.
    pub(crate) fn read(catalog: Catalog<'_, '_>, object_ids: &str) -> Result<Self, ConnectorError> {
        let sources: Vec<String> = TextKind::ALL
            .iter()
            .map(|kind| {
                format!(
                    "SELECT CAST(N'{}' AS nvarchar(16)) AS kind, s.object_id, s.minor, \
                            CAST(s.value AS varbinary(max)) AS bytes \
                     FROM ({}) s(object_id, minor, value) WHERE s.value IS NOT NULL",
                    kind.code(),
                    kind.source(object_ids)
                )
            })
            .collect();
        // The recursion numbers the pieces of each value from its length
        // alone; each piece is cut from the value once, in the final join.
        let sql = format!(
            "WITH texts AS ({}), \
             pieces(kind, object_id, minor, piece, length) AS ( \
               SELECT kind, object_id, minor, 0, CAST(DATALENGTH(bytes) AS bigint) FROM texts \
               UNION ALL \
               SELECT kind, object_id, minor, piece + 1, length FROM pieces \
               WHERE CAST(piece + 1 AS bigint) * {PIECE_BYTES} < length) \
             SELECT p.kind, p.object_id, p.minor, p.piece, \
                    CAST(SUBSTRING(t.bytes, p.piece * {PIECE_BYTES} + 1, {PIECE_BYTES}) \
                         AS varbinary({PIECE_BYTES})) \
             FROM pieces p JOIN texts t \
               ON t.kind = p.kind AND t.object_id = p.object_id AND t.minor = p.minor \
             ORDER BY p.kind, p.object_id, p.minor, p.piece \
             OPTION (MAXRECURSION 0)",
            sources.join(" UNION ALL ")
        );
        let mut bytes: HashMap<(TextKind, i32, i32), Vec<u8>> = HashMap::new();
        for batch in &catalog.query(sql)? {
            let (kinds, object_ids, minors) = (text(batch, 0)?, int32(batch, 1)?, int32(batch, 2)?);
            let pieces = column::<BinaryArray>(batch, 4)?;
            for (row, code) in kinds.into_iter().enumerate() {
                let code = required(code)?;
                let kind = TextKind::ALL
                    .into_iter()
                    .find(|kind| kind.code() == code)
                    .ok_or_else(unexpected_catalog_result)?;
                bytes
                    .entry((kind, object_ids.value(row), minors.value(row)))
                    .or_default()
                    .extend_from_slice(pieces.value(row));
            }
        }
        let texts = bytes
            .into_iter()
            .map(|(key, bytes)| Ok((key, decode_utf16le(&bytes)?)))
            .collect::<Result<_, ConnectorError>>()?;
        Ok(Self(texts))
    }

    /// The value of `kind` for `object_id` and `minor`, if the catalog has one.
    pub(crate) fn get(&self, kind: TextKind, object_id: i32, minor: i32) -> Option<String> {
        self.0.get(&(kind, object_id, minor)).cloned()
    }
}

/// Decodes the UTF-16LE bytes of an `nvarchar` value.
///
/// `nvarchar` can hold unpaired surrogates, which no `String` holds; such a
/// value cannot be returned exactly, so it is a `data` error.
fn decode_utf16le(bytes: &[u8]) -> Result<String, ConnectorError> {
    let not_unicode = || {
        ConnectorError::new(
            ErrorKind::Data,
            false,
            "a catalog text is not valid Unicode and cannot be returned exactly",
        )
    };
    let (units, rest) = bytes.as_chunks::<2>();
    if !rest.is_empty() {
        return Err(not_unicode());
    }
    let units: Vec<u16> = units.iter().copied().map(u16::from_le_bytes).collect();
    String::from_utf16(&units).map_err(|_| not_unicode())
}
