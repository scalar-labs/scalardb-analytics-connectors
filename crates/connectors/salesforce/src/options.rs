//! The connection options and the client configuration built from them.

use std::time::Duration;

use connector_salesforce_client::{ClientConfig, Credentials, Secret, Url};
use connector_sdk::{
    ConnectionOptions, ConnectorError, ErrorKind, OptionDescriptor, OptionValue, OptionValueType,
};

use crate::API_VERSION;

const LOGIN_URL: &str = "login_url";
const AUTH_FLOW: &str = "auth_flow";
const CLIENT_ID: &str = "client_id";
const CLIENT_SECRET: &str = "client_secret";
const REFRESH_TOKEN: &str = "refresh_token";
const USERNAME: &str = "username";
const PRIVATE_KEY: &str = "private_key";
const JWT_AUDIENCE: &str = "jwt_audience";

const DEFAULT_LOGIN_URL: &str = "https://login.salesforce.com";

/// The options each OAuth flow uses besides `login_url`, `auth_flow`, and
/// `client_id`; any other option is refused for that flow.
const REFRESH_TOKEN_OPTIONS: [&str; 2] = [CLIENT_SECRET, REFRESH_TOKEN];
const JWT_BEARER_OPTIONS: [&str; 3] = [USERNAME, PRIVATE_KEY, JWT_AUDIENCE];

/// Every option the connector accepts.
pub(crate) fn descriptors() -> Vec<OptionDescriptor> {
    let option =
        |key: &str, secret, required, default: Option<&str>, description: &str| OptionDescriptor {
            key: key.to_owned(),
            secret,
            required,
            value_type: OptionValueType::String,
            default: default.map(str::to_owned),
            description: description.to_owned(),
        };
    vec![
        option(
            LOGIN_URL,
            false,
            false,
            Some(DEFAULT_LOGIN_URL),
            "Where access tokens are requested: https://login.salesforce.com, \
             https://test.salesforce.com for a sandbox, or the org's My Domain URL.",
        ),
        option(
            AUTH_FLOW,
            false,
            true,
            None,
            "The OAuth 2.0 flow: `refresh_token` or `jwt_bearer`.",
        ),
        option(
            CLIENT_ID,
            false,
            true,
            None,
            "The consumer key of the connected app.",
        ),
        option(
            CLIENT_SECRET,
            true,
            false,
            None,
            "The consumer secret of the connected app, when it requires the secret \
             for the refresh token flow (`refresh_token` flow).",
        ),
        option(
            REFRESH_TOKEN,
            true,
            false,
            None,
            "The refresh token issued to the connected app (`refresh_token` flow, required).",
        ),
        option(
            USERNAME,
            false,
            false,
            None,
            "The Salesforce username the connection acts as (`jwt_bearer` flow, required).",
        ),
        option(
            PRIVATE_KEY,
            true,
            false,
            None,
            "The PEM-encoded RSA private key whose certificate the connected app holds \
             (`jwt_bearer` flow, required).",
        ),
        option(
            JWT_AUDIENCE,
            false,
            false,
            None,
            "The `aud` claim of the JWT bearer assertion (`jwt_bearer` flow). When omitted, \
             the login URL.",
        ),
    ]
}

/// Builds the client configuration from validated options.
///
/// Errors are `invalid settings` naming the offending option, never a value.
pub(crate) fn client_config(
    options: &ConnectionOptions,
    connection_timeout: Option<Duration>,
) -> Result<ClientConfig, ConnectorError> {
    let login_url = plain(options, LOGIN_URL).unwrap_or(DEFAULT_LOGIN_URL);
    let login_url = Url::parse(login_url)
        .ok()
        .filter(|url| matches!(url.scheme(), "https" | "http"))
        .ok_or_else(|| invalid(format!("option `{LOGIN_URL}` must be an HTTP(S) URL")))?;
    let client_id = required_plain(options, CLIENT_ID)?.to_owned();
    let credentials = match required_plain(options, AUTH_FLOW)? {
        "refresh_token" => {
            refuse_others(options, &JWT_BEARER_OPTIONS, "refresh_token")?;
            Credentials::RefreshToken {
                client_id,
                client_secret: secret(options, CLIENT_SECRET),
                refresh_token: secret(options, REFRESH_TOKEN)
                    .ok_or_else(|| missing(REFRESH_TOKEN))?,
            }
        }
        "jwt_bearer" => {
            refuse_others(options, &REFRESH_TOKEN_OPTIONS, "jwt_bearer")?;
            Credentials::JwtBearer {
                client_id,
                username: required_plain(options, USERNAME)?.to_owned(),
                audience: plain(options, JWT_AUDIENCE)
                    .map(str::to_owned)
                    .unwrap_or_else(|| login_url.as_str().trim_end_matches('/').to_owned()),
                private_key: secret(options, PRIVATE_KEY).ok_or_else(|| missing(PRIVATE_KEY))?,
            }
        }
        _ => {
            return Err(invalid(format!(
                "option `{AUTH_FLOW}` must be `refresh_token` or `jwt_bearer`"
            )));
        }
    };
    Ok(ClientConfig {
        login_url,
        api_version: API_VERSION.to_owned(),
        credentials,
        connect_timeout: connection_timeout,
    })
}

fn plain<'a>(options: &'a ConnectionOptions, key: &str) -> Option<&'a str> {
    match options.get(key) {
        Some(OptionValue::Plain(value)) => Some(value),
        Some(OptionValue::Secret(_)) | None => None,
    }
}

fn required_plain<'a>(
    options: &'a ConnectionOptions,
    key: &str,
) -> Result<&'a str, ConnectorError> {
    plain(options, key).ok_or_else(|| missing(key))
}

/// Copies a secret option into the client's secret holder.
fn secret(options: &ConnectionOptions, key: &str) -> Option<Secret> {
    match options.get(key) {
        Some(OptionValue::Secret(value)) => Some(Secret::new(value.expose())),
        Some(OptionValue::Plain(_)) | None => None,
    }
}

/// Refuses an option that belongs to the other flow, so that a mistyped flow
/// is reported rather than its options silently ignored.
fn refuse_others(
    options: &ConnectionOptions,
    others: &[&str],
    flow: &str,
) -> Result<(), ConnectorError> {
    match others.iter().find(|key| options.get(key).is_some()) {
        Some(key) => Err(invalid(format!(
            "option `{key}` is not used by the `{flow}` flow"
        ))),
        None => Ok(()),
    }
}

fn missing(key: &str) -> ConnectorError {
    invalid(format!("option `{key}` is required"))
}

fn invalid(message: String) -> ConnectorError {
    ConnectorError::new(ErrorKind::InvalidSettings, false, message)
}
