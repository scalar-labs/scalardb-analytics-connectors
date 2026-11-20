//! The connection options, their validation, and the ADBC driver options
//! built from them.
//!
//! [`DatabricksOptions::driver_options`] is the one place that knows the
//! driver's option names; another ADBC driver changes only that function.

use adbc_core::options::{OptionDatabase, OptionValue as AdbcOptionValue};
use connector_sdk::{
    ConnectionOptions, ConnectorError, ErrorKind, OptionDescriptor, OptionValue, OptionValueType,
    SecretString,
};

const HOST: &str = "host";
const HTTP_PATH: &str = "http_path";
const PORT: &str = "port";
const ACCESS_TOKEN: &str = "access_token";
const CLIENT_ID: &str = "client_id";
const CLIENT_SECRET: &str = "client_secret";
const CATALOG: &str = "catalog";

const DEFAULT_PORT: u16 = 443;

/// Every option the connector accepts.
pub(crate) fn descriptors() -> Vec<OptionDescriptor> {
    let option =
        |key: &str, secret, required, value_type, default: Option<&str>, description: &str| {
            OptionDescriptor {
                key: key.to_owned(),
                secret,
                required,
                value_type,
                default: default.map(str::to_owned),
                description: description.to_owned(),
            }
        };
    use OptionValueType::{Integer, String};
    vec![
        option(
            HOST,
            false,
            true,
            String,
            None,
            "Host name of the Databricks workspace, such as \
             adb-1234567890123456.7.azuredatabricks.net, without a scheme or path.",
        ),
        option(
            HTTP_PATH,
            false,
            true,
            String,
            None,
            "HTTP path of the SQL warehouse, such as /sql/1.0/warehouses/<id>.",
        ),
        option(
            PORT,
            false,
            false,
            Integer,
            Some("443"),
            "HTTPS port of the workspace.",
        ),
        option(
            ACCESS_TOKEN,
            true,
            false,
            String,
            None,
            "Personal access token. Give either this or client_id and client_secret.",
        ),
        option(
            CLIENT_ID,
            false,
            false,
            String,
            None,
            "OAuth client ID of a service principal, for OAuth machine-to-machine \
             authentication; requires client_secret.",
        ),
        option(
            CLIENT_SECRET,
            true,
            false,
            String,
            None,
            "OAuth secret of the service principal named by client_id.",
        ),
        option(
            CATALOG,
            false,
            false,
            String,
            None,
            "Unity Catalog catalog that the namespace and table lists are limited to; \
             without it, every catalog the principal can see is listed.",
        ),
    ]
}

/// Validated connection options of one Data Source.
///
/// `Debug` prints no credential.
#[derive(Debug)]
pub(crate) struct DatabricksOptions {
    host: String,
    http_path: String,
    port: u16,
    authentication: Authentication,
    /// The `catalog` option: the one catalog listed, when given.
    pub(crate) catalog: Option<String>,
}

/// How the connector authenticates to the workspace.
#[derive(Debug)]
enum Authentication {
    /// A personal access token.
    AccessToken(SecretString),
    /// OAuth machine-to-machine: a service principal's client ID and secret.
    OAuthM2m {
        client_id: String,
        client_secret: SecretString,
    },
}

impl DatabricksOptions {
    /// Validates the options beyond the descriptor's checks: the forms of the
    /// host, HTTP path, port, and catalog, and exactly one authentication.
    ///
    /// Errors are `invalid settings` naming the option, never its value.
    pub(crate) fn parse(options: &ConnectionOptions) -> Result<Self, ConnectorError> {
        let host = required_plain(options, HOST)?;
        if host.is_empty()
            || host.contains(['/', ':', '?', '#', '@'])
            || host.contains(char::is_whitespace)
        {
            return Err(invalid(format!(
                "option `{HOST}` must be a host name without a scheme, port, or path"
            )));
        }
        let http_path = required_plain(options, HTTP_PATH)?;
        if !http_path.starts_with('/') {
            return Err(invalid(format!("option `{HTTP_PATH}` must start with `/`")));
        }
        let port = match plain(options, PORT) {
            None => DEFAULT_PORT,
            Some(port) => port
                .parse::<u16>()
                .ok()
                .filter(|port| *port != 0)
                .ok_or_else(|| invalid(format!("option `{PORT}` must be from 1 to 65535")))?,
        };
        let catalog = plain(options, CATALOG).map(str::to_owned);
        if catalog.as_deref() == Some("") {
            return Err(invalid(format!("option `{CATALOG}` must not be empty")));
        }
        Ok(Self {
            host: host.to_owned(),
            http_path: http_path.to_owned(),
            port,
            authentication: authentication(options)?,
            catalog,
        })
    }

    /// The ADBC Driver Foundry Databricks driver's database options.
    ///
    /// The driver has no option that limits connecting, so a connection
    /// timeout cannot be passed to it. The `catalog` option is not passed:
    /// it narrows listing only, and generated SQL qualifies every table.
    pub(crate) fn driver_options(&self) -> Vec<(OptionDatabase, AdbcOptionValue)> {
        let option = |key: &str, value: &str| {
            (
                OptionDatabase::Other(key.to_owned()),
                AdbcOptionValue::String(value.to_owned()),
            )
        };
        let mut options = vec![
            option("databricks.server_hostname", &self.host),
            option("databricks.http_path", &self.http_path),
            option("databricks.port", &self.port.to_string()),
        ];
        match &self.authentication {
            Authentication::AccessToken(token) => {
                options.push(option("databricks.auth_type", "pat"));
                options.push(option("databricks.access_token", token.expose()));
            }
            Authentication::OAuthM2m {
                client_id,
                client_secret,
            } => {
                options.push(option("databricks.auth_type", "oauth-m2m"));
                options.push(option("databricks.oauth.client_id", client_id));
                options.push(option(
                    "databricks.oauth.client_secret",
                    client_secret.expose(),
                ));
            }
        }
        options
    }
}

/// Reads the one authentication the options give: an access token, or a
/// client ID with its secret.
fn authentication(options: &ConnectionOptions) -> Result<Authentication, ConnectorError> {
    let token = secret(options, ACCESS_TOKEN);
    let client_id = plain(options, CLIENT_ID);
    let client_secret = secret(options, CLIENT_SECRET);
    match (token, client_id, client_secret) {
        (Some(token), None, None) => Ok(Authentication::AccessToken(token)),
        (None, Some(client_id), Some(client_secret)) => Ok(Authentication::OAuthM2m {
            client_id: client_id.to_owned(),
            client_secret,
        }),
        (Some(_), _, _) => Err(invalid(format!(
            "option `{ACCESS_TOKEN}` cannot be combined with `{CLIENT_ID}` or `{CLIENT_SECRET}`"
        ))),
        (None, Some(_), None) => Err(invalid(format!(
            "option `{CLIENT_SECRET}` is required with `{CLIENT_ID}`"
        ))),
        (None, None, Some(_)) => Err(invalid(format!(
            "option `{CLIENT_ID}` is required with `{CLIENT_SECRET}`"
        ))),
        (None, None, None) => Err(invalid(format!(
            "option `{ACCESS_TOKEN}`, or `{CLIENT_ID}` and `{CLIENT_SECRET}`, is required"
        ))),
    }
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
    plain(options, key).ok_or_else(|| invalid(format!("option `{key}` is required")))
}

/// Copies a secret option into a value that is erased when dropped.
fn secret(options: &ConnectionOptions, key: &str) -> Option<SecretString> {
    match options.get(key) {
        Some(OptionValue::Secret(value)) => Some(SecretString::new(value.expose().to_owned())),
        Some(OptionValue::Plain(_)) | None => None,
    }
}

fn invalid(message: String) -> ConnectorError {
    ConnectorError::new(ErrorKind::InvalidSettings, false, message)
}

#[cfg(test)]
mod tests {
    use connector_sdk::{Connector, ConnectorDescriptor};

    use super::*;
    use crate::DatabricksConnector;

    const TOKEN: &str = "dapi-secret-token";
    const CLIENT_SECRET_VALUE: &str = "oauth-secret-value";

    fn descriptor() -> ConnectorDescriptor {
        DatabricksConnector::descriptor()
    }

    /// Validates `pairs` as the library does: the descriptor, then the
    /// connector.
    fn parse(pairs: &[(&str, &str)]) -> Result<DatabricksOptions, ConnectorError> {
        let options = ConnectionOptions::parse(
            &descriptor(),
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), SecretString::new((*value).to_owned()))),
        )?;
        DatabricksOptions::parse(&options)
    }

    fn with_base<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        let mut pairs = vec![
            ("host", "adb-1.2.azuredatabricks.net"),
            ("http_path", "/sql/1.0/warehouses/abc"),
        ];
        pairs.extend_from_slice(extra);
        pairs
    }

    fn driver_options(options: &DatabricksOptions) -> Vec<(String, String)> {
        options
            .driver_options()
            .into_iter()
            .map(|(key, value)| {
                let OptionDatabase::Other(key) = key else {
                    panic!("only driver-specific options are passed");
                };
                let AdbcOptionValue::String(value) = value else {
                    panic!("every option is text");
                };
                (key, value)
            })
            .collect()
    }

    fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn an_access_token_becomes_pat_authentication() {
        let options = parse(&with_base(&[("access_token", TOKEN)])).unwrap();
        assert_eq!(
            driver_options(&options),
            pairs(&[
                ("databricks.server_hostname", "adb-1.2.azuredatabricks.net"),
                ("databricks.http_path", "/sql/1.0/warehouses/abc"),
                ("databricks.port", "443"),
                ("databricks.auth_type", "pat"),
                ("databricks.access_token", TOKEN),
            ])
        );
        assert_eq!(options.catalog, None);
    }

    #[test]
    fn a_client_id_and_secret_become_oauth_m2m_authentication() {
        let options = parse(&with_base(&[
            ("port", "8443"),
            ("client_id", "principal"),
            ("client_secret", CLIENT_SECRET_VALUE),
            ("catalog", "main"),
        ]))
        .unwrap();
        assert_eq!(
            driver_options(&options),
            pairs(&[
                ("databricks.server_hostname", "adb-1.2.azuredatabricks.net"),
                ("databricks.http_path", "/sql/1.0/warehouses/abc"),
                ("databricks.port", "8443"),
                ("databricks.auth_type", "oauth-m2m"),
                ("databricks.oauth.client_id", "principal"),
                ("databricks.oauth.client_secret", CLIENT_SECRET_VALUE),
            ])
        );
        assert_eq!(options.catalog.as_deref(), Some("main"));
    }

    #[test]
    fn authentication_must_be_exactly_one_method() {
        for (extra, named) in [
            (&[][..], "access_token"),
            (&[("client_id", "principal")][..], "client_secret"),
            (&[("client_secret", CLIENT_SECRET_VALUE)][..], "client_id"),
            (
                &[("access_token", TOKEN), ("client_id", "principal")][..],
                "access_token",
            ),
            (
                &[
                    ("access_token", TOKEN),
                    ("client_id", "principal"),
                    ("client_secret", CLIENT_SECRET_VALUE),
                ][..],
                "access_token",
            ),
        ] {
            let error = parse(&with_base(extra)).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidSettings);
            assert!(!error.retryable());
            assert!(
                error.safe_message().contains(&format!("`{named}`")),
                "{}",
                error.safe_message()
            );
        }
    }

    #[test]
    fn malformed_options_are_invalid_settings_naming_the_option() {
        for (pairs, named) in [
            (
                vec![
                    ("host", "https://adb.example.net"),
                    ("http_path", "/sql/1.0/warehouses/abc"),
                ],
                "host",
            ),
            (
                vec![("host", "adb.example.net:443"), ("http_path", "/p")],
                "host",
            ),
            (vec![("host", ""), ("http_path", "/p")], "host"),
            (
                vec![("host", "adb.example.net"), ("http_path", "sql/1.0")],
                "http_path",
            ),
            (with_base(&[("port", "0")]), "port"),
            (with_base(&[("port", "65536")]), "port"),
            (with_base(&[("port", "-1")]), "port"),
            (with_base(&[("catalog", "")]), "catalog"),
            (vec![("http_path", "/p")], "host"),
            (vec![("host", "adb.example.net")], "http_path"),
        ] {
            let mut pairs = pairs;
            pairs.push(("access_token", TOKEN));
            let error = parse(&pairs).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidSettings);
            assert!(
                error.safe_message().contains(&format!("`{named}`")),
                "{}",
                error.safe_message()
            );
        }
    }

    #[test]
    fn credentials_never_appear_in_errors_or_debug_output() {
        let options = parse(&with_base(&[
            ("client_id", "principal"),
            ("client_secret", CLIENT_SECRET_VALUE),
        ]))
        .unwrap();
        let debug = format!("{options:?}");
        assert!(!debug.contains(CLIENT_SECRET_VALUE), "{debug}");
        let options = parse(&with_base(&[("access_token", TOKEN)])).unwrap();
        let debug = format!("{options:?}");
        assert!(!debug.contains(TOKEN), "{debug}");

        let error = parse(&with_base(&[
            ("access_token", TOKEN),
            ("client_secret", CLIENT_SECRET_VALUE),
        ]))
        .unwrap_err();
        for value in [TOKEN, CLIENT_SECRET_VALUE] {
            assert!(!error.safe_message().contains(value));
            assert!(!format!("{error:?}").contains(value));
        }
    }

    #[test]
    fn secret_options_are_declared_secret() {
        let descriptor = descriptor();
        let secret = |key: &str| {
            descriptor
                .options
                .iter()
                .find(|option| option.key == key)
                .unwrap()
                .secret
        };
        assert!(secret("access_token"));
        assert!(secret("client_secret"));
        assert!(!secret("client_id"));
        assert!(!secret("host"));
    }
}
