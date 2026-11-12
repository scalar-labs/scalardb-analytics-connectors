//! How a Connector treats the errors its access reports.

use std::marker::PhantomData;

use crate::ErrorKind;

/// A contract error kind with its retry flag and connector-composed message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ErrorClass {
    pub kind: ErrorKind,
    pub retryable: bool,
    /// A message that never contains Data Source output, SQL text, or values.
    pub safe_message: &'static str,
}

impl ErrorClass {
    pub const fn new(kind: ErrorKind, retryable: bool, safe_message: &'static str) -> Self {
        Self {
            kind,
            retryable,
            safe_message,
        }
    }
}

/// A Connector's error table and redaction of its access's diagnostics.
///
/// The handler is used as a type; it holds no state.
pub trait ErrorHandler {
    /// What the Connector's access reports about a Data Source error.
    type Diagnostic;

    /// Looks a diagnostic up in the Connector's error table.
    ///
    /// A match takes precedence over the access's generic classes.
    fn classify(_diagnostic: &Self::Diagnostic) -> Option<ErrorClass> {
        None
    }

    /// Removes what must not leave the connector from a diagnostic, such as
    /// statement text a driver copied into its message.
    ///
    /// It runs on every Data Source error before classification.
    fn redact(diagnostic: Self::Diagnostic) -> Self::Diagnostic {
        diagnostic
    }
}

/// The handler of a Connector with no error table or redaction of its own:
/// errors are classified by the access's generic classes alone.
pub struct GenericErrorHandler<D>(PhantomData<fn() -> D>);

impl<D> ErrorHandler for GenericErrorHandler<D> {
    type Diagnostic = D;
}
