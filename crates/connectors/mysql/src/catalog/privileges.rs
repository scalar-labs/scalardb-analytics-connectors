//! The account's `SELECT` grants, as far as `information_schema` shows them.
//!
//! `information_schema` shows a table only to an account with some privilege
//! on it, and a column only to one with a privilege on the column or above it.
//! `SELECT` at the global, database, or table level covers every column of a
//! table, so with one the read sees every column; and with one at the global or
//! database level, a table it does not see does not exist.
//!
//! The privilege tables show only grants made to the account itself. Grants
//! held through roles are not shown, so a table readable only through a role
//! is treated as one whose columns may be hidden.

use connector_sdk::ConnectorError;
use connector_sql::catalog::{required, text};

use super::Catalog;

/// The account's `GRANTEE` as the privilege tables spell it, `'user'@'host'`.
/// The host follows the last `@` of `CURRENT_USER()`.
const GRANTEE: &str = "CONCAT('''', \
    LEFT(CURRENT_USER(), CHAR_LENGTH(CURRENT_USER()) \
         - CHAR_LENGTH(SUBSTRING_INDEX(CURRENT_USER(), '@', -1)) - 1), \
    '''@''', SUBSTRING_INDEX(CURRENT_USER(), '@', -1), '''')";

/// The account's `SELECT` grants.
pub(crate) struct SelectGrants {
    global: bool,
    /// Database name patterns of database-level grants, where `%` and `_`
    /// are wildcards and `\` escapes them.
    schemas: Vec<String>,
    /// Database and table names of table-level grants.
    tables: Vec<(String, String)>,
}

impl SelectGrants {
    /// Reads the account's `SELECT` grants at each level.
    pub(crate) fn read(catalog: Catalog<'_, '_>) -> Result<Self, ConnectorError> {
        let grantee = |table: &str| {
            format!(
                "FROM information_schema.{table} \
                 WHERE CAST(GRANTEE AS BINARY) = CAST({GRANTEE} AS BINARY) \
                   AND PRIVILEGE_TYPE = 'SELECT'"
            )
        };
        let sql = format!(
            "SELECT 'global', NULL, NULL {} \
             UNION ALL SELECT 'schema', TABLE_SCHEMA, NULL {} \
             UNION ALL SELECT 'table', TABLE_SCHEMA, TABLE_NAME {}",
            grantee("USER_PRIVILEGES"),
            grantee("SCHEMA_PRIVILEGES"),
            grantee("TABLE_PRIVILEGES")
        );
        let mut grants = Self {
            global: false,
            schemas: Vec::new(),
            tables: Vec::new(),
        };
        for batch in &catalog.query(sql)? {
            let (levels, schemas, tables) = (text(batch, 0)?, text(batch, 1)?, text(batch, 2)?);
            for row in 0..batch.num_rows() {
                match required(levels[row])? {
                    "global" => grants.global = true,
                    "schema" => grants.schemas.push(required(schemas[row])?.to_owned()),
                    _ => grants.tables.push((
                        required(schemas[row])?.to_owned(),
                        required(tables[row])?.to_owned(),
                    )),
                }
            }
        }
        Ok(grants)
    }

    /// Whether the account may read every table of the database, so that
    /// `information_schema` shows each of them.
    pub(crate) fn covers_schema(&self, schema: &str) -> bool {
        self.global
            || self
                .schemas
                .iter()
                .any(|pattern| pattern_matches(pattern, schema))
    }

    /// Whether the account may read every column of the table, so that
    /// `information_schema` shows each of them.
    pub(crate) fn covers_table(&self, schema: &str, table: &str) -> bool {
        self.covers_schema(schema)
            || self.tables.iter().any(|(granted_schema, granted_table)| {
                granted_schema == schema && granted_table == table
            })
    }
}

/// Whether a database-level grant's name pattern matches `name`: `%` matches
/// any sequence, `_` any one character, and `\` makes the next character
/// literal.
fn pattern_matches(pattern: &str, name: &str) -> bool {
    let (pattern, name): (Vec<char>, Vec<char>) =
        (pattern.chars().collect(), name.chars().collect());
    fn from(pattern: &[char], name: &[char]) -> bool {
        match pattern {
            [] => name.is_empty(),
            ['%', rest @ ..] => (0..=name.len()).any(|skip| from(rest, &name[skip..])),
            ['_', rest @ ..] => !name.is_empty() && from(rest, &name[1..]),
            ['\\', literal, rest @ ..] | [literal, rest @ ..] => {
                name.first() == Some(literal) && from(rest, &name[1..])
            }
        }
    }
    from(&pattern, &name)
}

#[cfg(test)]
mod tests {
    use super::pattern_matches;

    #[test]
    fn database_grant_patterns_follow_mysql_wildcards() {
        assert!(pattern_matches("sales", "sales"));
        assert!(!pattern_matches("sales", "Sales"));
        assert!(pattern_matches("sales_%", "sales_2024"));
        assert!(pattern_matches("sales_", "salesX"));
        assert!(!pattern_matches("sales\\_", "salesX"));
        assert!(pattern_matches("sales\\_", "sales_"));
        assert!(pattern_matches("%", ""));
        assert!(!pattern_matches("a_", "a"));
    }
}
