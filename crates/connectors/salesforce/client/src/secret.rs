//! Secret values the client is given.

use std::fmt;

use zeroize::Zeroize;

/// A secret value, such as a refresh token, a client secret, or a private
/// key.
///
/// It never prints its value in `Debug` and erases it when dropped.
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(..)")
    }
}
