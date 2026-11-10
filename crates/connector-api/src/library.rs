//! The one library a process initializes and shares among its callers.

use std::sync::{Arc, Mutex, PoisonError};

use connector_sdk::{ConnectorError, ErrorKind, LibrarySettings, runtime::Library};

use crate::registry::built_in_connectors;

/// The library currently initialized in this process, if any.
static CURRENT: Mutex<Option<Arc<Library>>> = Mutex::new(None);

/// Initializes the library once for the process.
///
/// A second initialization returns `invalid state` instead of being counted,
/// so that only the application decides when shutdown happens.
pub(crate) fn initialize(settings: LibrarySettings) -> Result<(), ConnectorError> {
    let mut current = CURRENT.lock().unwrap_or_else(PoisonError::into_inner);
    if current.is_some() {
        return Err(invalid_state("the library is already initialized"));
    }
    *current = Some(Library::initialize(built_in_connectors(), &settings)?);
    Ok(())
}

/// Shuts the library down; Data Sources still open become unusable.
pub(crate) fn shutdown() -> Result<(), ConnectorError> {
    CURRENT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .ok_or_else(|| invalid_state("the library is not initialized"))?
        .shut_down();
    Ok(())
}

/// Returns the initialized library.
pub(crate) fn current() -> Result<Arc<Library>, ConnectorError> {
    CURRENT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| invalid_state("the library is not initialized"))
}

fn invalid_state(message: &str) -> ConnectorError {
    ConnectorError::new(ErrorKind::InvalidState, false, message)
}
