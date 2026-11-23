//! Credentials Salesforce rejects.

use connector_sdk::{ErrorKind, SecretString};

use super::{CONNECTION_TIMEOUT, Org, env_option, options};
use connector_salesforce::CONNECTOR;

/// A credential value Salesforce has never issued.
const REJECTED: &str = "connector-org-test-rejected-credential";

pub fn check(org: &Org) {
    // The refresh token flow is refused for an unknown token, and the JWT
    // bearer flow for an unknown user.
    let rejected = match env_option("auth_flow").as_deref() {
        Some("jwt_bearer") => "username",
        _ => "refresh_token",
    };
    let options: Vec<(String, SecretString)> = options()
        .into_iter()
        .map(|(key, value)| {
            if key == rejected {
                (key, SecretString::new(REJECTED.to_owned()))
            } else {
                (key, value)
            }
        })
        .collect();
    let error = org
        .library
        .open_data_source(CONNECTOR, options, Some(CONNECTION_TIMEOUT))
        .and_then(|data_source| data_source.open_session())
        .err()
        .unwrap_or_else(|| panic!("a session opens with a rejected {rejected}"));
    assert_eq!(
        (error.kind(), error.retryable()),
        (ErrorKind::Authentication, false),
        "a rejected {rejected}: {error}"
    );
    let reported = format!("{error} {error:?}");
    assert!(
        !reported.contains(REJECTED),
        "the error reveals the rejected {rejected}: {reported}"
    );
}
