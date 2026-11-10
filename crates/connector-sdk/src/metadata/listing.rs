//! Items of the namespace list and table list requests.

use std::collections::BTreeSet;

use super::{TableKind, TableReference};

/// A set of names matched exactly, without case folding; `None` matches everything.
pub type NameFilter = Option<BTreeSet<String>>;

/// A namespace as a sequence of names with no fixed depth.
///
/// For sources with catalogs, the first name is the catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespacePath {
    pub names: Vec<String>,
}

/// One table in a table list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableEntry {
    pub table: TableReference,
    pub kind: TableKind,
}

/// One item of a list, or a range whose list could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listed<T> {
    Item(T),
    /// No targets are returned for this range; this does not mean it is empty.
    Unreadable {
        scope: NamespacePath,
        reason: String,
    },
}
