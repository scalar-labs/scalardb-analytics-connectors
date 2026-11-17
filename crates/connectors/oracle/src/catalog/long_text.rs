//! Reading the `LONG` catalog values: view definitions, CHECK conditions, and
//! column defaults and virtual column expressions.
//!
//! `arrow-odbc` reads each column into a buffer of a fixed length, so it
//! cannot read a `LONG` value, and the `VARCHAR2` copies Oracle keeps beside
//! them, such as `TEXT_VC`, are cut at 4000 bytes. The statement here reads
//! each value in an inline PL/SQL function, which holds a `LONG` of up to
//! 32,760 bytes, and returns its UTF-8 bytes in pieces of at most 2000 bytes,
//! one row per piece; the pieces are joined and decoded here. Splitting bytes
//! rather than characters keeps the value exact whatever characters it holds.
//! A longer value is not read.

use std::collections::HashMap;

use arrow_array::{Array, BinaryArray};
use connector_sdk::{ConnectorError, ErrorKind};
use connector_sql::catalog::{column, required, text, unexpected_catalog_result};

use super::Catalog;

/// The bytes of one piece; `RAW(2000)` is the longest `RAW` in SQL.
const PIECE_BYTES: u32 = 2000;

/// The most pieces of one value: 32,767 bytes, the longest a PL/SQL value
/// holds, in pieces of 2000 bytes.
const MAX_PIECES: u32 = 17;

/// What a long text value belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TextKind {
    /// A view's definition; its item is the view's name.
    Definition,
    /// A CHECK constraint's condition; its item is the constraint's name.
    Check,
    /// A column's default or virtual column expression; its item is the
    /// column's name.
    Default,
}

impl TextKind {
    const ALL: [Self; 3] = [Self::Definition, Self::Check, Self::Default];

    fn code(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Check => "check",
            Self::Default => "default",
        }
    }

    /// A statement selecting the kind, owner, table name, and item of every
    /// value of this kind for the tables `requested` selects as `owner` and
    /// `name` pairs.
    fn source(self, requested: &str) -> String {
        let code = self.code();
        match self {
            Self::Definition => format!(
                "SELECT '{code}' AS kind, v.owner, v.view_name AS name, v.view_name AS item \
                 FROM all_views v WHERE (v.owner, v.view_name) IN ({requested})"
            ),
            Self::Check => format!(
                "SELECT '{code}', c.owner, c.table_name, c.constraint_name FROM all_constraints c \
                 WHERE c.constraint_type = 'C' AND (c.owner, c.table_name) IN ({requested})"
            ),
            Self::Default => format!(
                "SELECT '{code}', c.owner, c.table_name, c.column_name FROM all_tab_cols c \
                 WHERE c.hidden_column = 'NO' AND c.default_length IS NOT NULL \
                   AND (c.owner, c.table_name) IN ({requested})"
            ),
        }
    }
}

/// The function that reads one `LONG` value as UTF-8 bytes, or NULL when it
/// is longer than a PL/SQL `LONG` holds.
const LONG_TEXT_FUNCTION: &str = "FUNCTION long_text(kind VARCHAR2, owner_name VARCHAR2, \
         table_name VARCHAR2, item VARCHAR2) RETURN BLOB IS \
       value LONG; \
     BEGIN \
       IF kind = 'definition' THEN \
         SELECT v.text INTO value FROM all_views v \
         WHERE v.owner = owner_name AND v.view_name = item; \
       ELSIF kind = 'check' THEN \
         SELECT c.search_condition INTO value FROM all_constraints c \
         WHERE c.owner = owner_name AND c.constraint_name = item; \
       ELSE \
         SELECT c.data_default INTO value FROM all_tab_cols c \
         WHERE c.owner = owner_name AND c.table_name = long_text.table_name \
           AND c.column_name = item; \
       END IF; \
       RETURN TO_BLOB(UTL_RAW.CAST_TO_RAW(CONVERT(value, 'AL32UTF8'))); \
     EXCEPTION WHEN VALUE_ERROR THEN RETURN NULL; \
     END;";

/// The long text values of the requested tables, keyed by kind, owner, table
/// name, and item. A value longer than 32,760 bytes is absent.
pub(crate) struct LongTexts(HashMap<(TextKind, String, String, String), String>);

impl LongTexts {
    /// Reads every long text value of the tables `requested` selects, in one
    /// statement.
    pub(crate) fn read(catalog: Catalog<'_, '_>, requested: &str) -> Result<Self, ConnectorError> {
        let sources: Vec<String> = TextKind::ALL
            .iter()
            .map(|kind| kind.source(requested))
            .collect();
        let sql = format!(
            "WITH {LONG_TEXT_FUNCTION} \
             texts AS ( \
               SELECT /*+ MATERIALIZE */ s.kind, s.owner, s.name, s.item, \
                      long_text(s.kind, s.owner, s.name, s.item) AS bytes \
               FROM ({}) s), \
             pieces AS (SELECT LEVEL - 1 AS piece FROM dual CONNECT BY LEVEL <= {MAX_PIECES}) \
             SELECT t.kind, t.owner, t.name, t.item, \
                    DBMS_LOB.SUBSTR(t.bytes, {PIECE_BYTES}, p.piece * {PIECE_BYTES} + 1) \
             FROM texts t JOIN pieces p ON p.piece * {PIECE_BYTES} < DBMS_LOB.GETLENGTH(t.bytes) \
             ORDER BY t.kind, t.owner, t.name, t.item, p.piece",
            sources.join(" UNION ALL ")
        );
        let mut bytes: HashMap<(TextKind, String, String, String), Vec<u8>> = HashMap::new();
        for batch in &catalog.query(sql)? {
            let (kinds, owners, names, items) = (
                text(batch, 0)?,
                text(batch, 1)?,
                text(batch, 2)?,
                text(batch, 3)?,
            );
            let pieces = column::<BinaryArray>(batch, 4)?;
            for row in 0..batch.num_rows() {
                let code = required(kinds[row])?;
                let kind = TextKind::ALL
                    .into_iter()
                    .find(|kind| kind.code() == code)
                    .ok_or_else(unexpected_catalog_result)?;
                if pieces.is_null(row) {
                    return Err(unexpected_catalog_result());
                }
                bytes
                    .entry((
                        kind,
                        required(owners[row])?.to_owned(),
                        required(names[row])?.to_owned(),
                        required(items[row])?.to_owned(),
                    ))
                    .or_default()
                    .extend_from_slice(pieces.value(row));
            }
        }
        let texts = bytes
            .into_iter()
            .map(|(key, bytes)| {
                let text = String::from_utf8(bytes).map_err(|_| {
                    ConnectorError::new(
                        ErrorKind::Data,
                        false,
                        "a catalog text is not valid Unicode and cannot be returned exactly",
                    )
                })?;
                Ok((key, text))
            })
            .collect::<Result<_, ConnectorError>>()?;
        Ok(Self(texts))
    }

    /// The value of `kind` for `item` of table `name` of `owner`, if it was
    /// read.
    pub(crate) fn get(
        &self,
        kind: TextKind,
        owner: &str,
        name: &str,
        item: &str,
    ) -> Option<String> {
        self.0
            .get(&(kind, owner.to_owned(), name.to_owned(), item.to_owned()))
            .cloned()
    }
}
