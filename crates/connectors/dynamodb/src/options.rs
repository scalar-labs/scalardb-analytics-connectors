//! The connection options and the SDK configuration loader built from them.

use std::time::Duration;

use aws_config::{BehaviorVersion, ConfigLoader, retry::RetryConfig, timeout::TimeoutConfig};
use aws_sdk_dynamodb::{
    Client,
    config::{Credentials, Region, SharedHttpClient},
};
use aws_smithy_http_client::{
    Builder,
    tls::{Provider, rustls_provider::CryptoMode},
};
use connector_sdk::{
    ConnectionOptions, ConnectorError, ErrorKind, OptionDescriptor, OptionValue, OptionValueType,
};
use http::Uri;

const REGION: &str = "region";
const ENDPOINT: &str = "endpoint";
const ACCESS_KEY_ID: &str = "access_key_id";
const SECRET_ACCESS_KEY: &str = "secret_access_key";
const SESSION_TOKEN: &str = "session_token";

/// The provider name the SDK records for credentials given as options.
const OPTIONS_PROVIDER: &str = "connection options";

/// Every option the connector accepts.
pub(crate) fn descriptors() -> Vec<OptionDescriptor> {
    let option = |key: &str, secret, required, description: &str| OptionDescriptor {
        key: key.to_owned(),
        secret,
        required,
        value_type: OptionValueType::String,
        default: None,
        description: description.to_owned(),
    };
    vec![
        option(
            REGION,
            false,
            true,
            "The AWS Region of the tables, such as `us-east-1`. Requests are signed for it \
             even when `endpoint` is given.",
        ),
        option(
            ENDPOINT,
            false,
            false,
            "The HTTP(S) URL requests are sent to instead of the Region's DynamoDB endpoint, \
             such as a VPC endpoint or DynamoDB Local.",
        ),
        option(
            ACCESS_KEY_ID,
            false,
            false,
            "The access key ID of static credentials, with `secret_access_key`. When omitted, \
             the AWS default credential chain supplies credentials.",
        ),
        option(
            SECRET_ACCESS_KEY,
            true,
            false,
            "The secret access key of static credentials, with `access_key_id`.",
        ),
        option(
            SESSION_TOKEN,
            true,
            false,
            "The session token of temporary static credentials, with `access_key_id` and \
             `secret_access_key`.",
        ),
    ]
}

/// What the client of a Data Source is built from: the SDK's configuration
/// loader and the endpoint the options give.
pub(crate) struct ClientConfig {
    loader: ConfigLoader,
    /// The `endpoint` option. Requests go there, or to the Region's DynamoDB
    /// endpoint without it, never to an endpoint URL that the environment
    /// (`AWS_ENDPOINT_URL`, `AWS_ENDPOINT_URL_DYNAMODB`) or a shared config
    /// profile sets: the registered options alone name the Data Source.
    endpoint: Option<String>,
}

impl ClientConfig {
    /// Validates the options and prepares the configuration.
    ///
    /// Requests go through an HTTPS client on rustls with the ring provider
    /// and the operating system's root certificates, with the SDK's standard
    /// retry mode. `connection_timeout` bounds establishing each connection.
    ///
    /// Errors are `invalid settings` naming the offending option, never a
    /// value.
    pub(crate) fn new(
        options: &ConnectionOptions,
        connection_timeout: Option<Duration>,
    ) -> Result<Self, ConnectorError> {
        let region = plain(options, REGION).ok_or_else(|| missing(REGION))?;
        if !valid_region(region) {
            return Err(invalid(format!(
                "option `{REGION}` must be an AWS Region name, such as `us-east-1`"
            )));
        }
        let endpoint = plain(options, ENDPOINT);
        if endpoint.is_some_and(|endpoint| !valid_endpoint(endpoint)) {
            return Err(invalid(format!(
                "option `{ENDPOINT}` must be an HTTP(S) URL"
            )));
        }
        let mut timeouts = TimeoutConfig::builder();
        if let Some(limit) = connection_timeout {
            timeouts = timeouts.connect_timeout(limit);
        }
        let mut loader = aws_config::defaults(BehaviorVersion::v2026_11_17())
            .region(Region::new(region.to_owned()))
            .http_client(http_client())
            .retry_config(RetryConfig::standard())
            .timeout_config(timeouts.build());
        if let Some(credentials) = credentials(options)? {
            loader = loader.credentials_provider(credentials);
        }
        Ok(Self {
            loader,
            endpoint: endpoint.map(str::to_owned),
        })
    }

    /// Loads the configuration, which sets up the default credential chain
    /// when the options give no credentials, and creates the client.
    pub(crate) async fn client(self) -> Client {
        let shared = self.loader.load().await;
        let mut config = aws_sdk_dynamodb::config::Builder::from(&shared);
        config.set_endpoint_url(self.endpoint);
        Client::from_conf(config.build())
    }
}

/// Whether `region` can name a Region: a DNS label of letters, digits, and
/// `-`, as the SDK requires to build the Region's endpoint and to sign.
fn valid_region(region: &str) -> bool {
    (1..=63).contains(&region.len())
        && !region.starts_with('-')
        && region
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Whether `endpoint` is an HTTP(S) URL with a host, as the SDK parses it
/// when it sends a request.
fn valid_endpoint(endpoint: &str) -> bool {
    endpoint.parse::<Uri>().is_ok_and(|uri| {
        matches!(uri.scheme_str(), Some("http" | "https"))
            && uri.host().is_some_and(|host| !host.is_empty())
    })
}

/// The HTTPS client every request is sent through.
fn http_client() -> SharedHttpClient {
    Builder::new()
        .tls_provider(Provider::Rustls(CryptoMode::Ring))
        .build_https()
}

/// The static credentials the options give, or `None` for the default chain.
fn credentials(options: &ConnectionOptions) -> Result<Option<Credentials>, ConnectorError> {
    let access_key_id = plain(options, ACCESS_KEY_ID);
    let secret_access_key = secret(options, SECRET_ACCESS_KEY);
    let session_token = secret(options, SESSION_TOKEN);
    match (access_key_id, secret_access_key) {
        (Some(access_key_id), Some(secret_access_key)) => Ok(Some(Credentials::new(
            access_key_id,
            secret_access_key,
            session_token.map(str::to_owned),
            None,
            OPTIONS_PROVIDER,
        ))),
        (None, None) if session_token.is_none() => Ok(None),
        (None, None) => Err(invalid(format!(
            "option `{SESSION_TOKEN}` requires `{ACCESS_KEY_ID}` and `{SECRET_ACCESS_KEY}`"
        ))),
        (Some(_), None) => Err(invalid(format!(
            "option `{ACCESS_KEY_ID}` requires `{SECRET_ACCESS_KEY}`"
        ))),
        (None, Some(_)) => Err(invalid(format!(
            "option `{SECRET_ACCESS_KEY}` requires `{ACCESS_KEY_ID}`"
        ))),
    }
}

fn plain<'a>(options: &'a ConnectionOptions, key: &str) -> Option<&'a str> {
    match options.get(key) {
        Some(OptionValue::Plain(value)) => Some(value),
        Some(OptionValue::Secret(_)) | None => None,
    }
}

fn secret<'a>(options: &'a ConnectionOptions, key: &str) -> Option<&'a str> {
    match options.get(key) {
        Some(OptionValue::Secret(value)) => Some(value.expose()),
        Some(OptionValue::Plain(_)) | None => None,
    }
}

fn missing(key: &str) -> ConnectorError {
    invalid(format!("option `{key}` is required"))
}

fn invalid(message: String) -> ConnectorError {
    ConnectorError::new(ErrorKind::InvalidSettings, false, message)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use aws_credential_types::provider::{
        ProvideCredentials, error::CredentialsError,
        future::ProvideCredentials as CredentialsFuture,
    };
    use aws_runtime::env_config::file::{EnvConfigFileKind, EnvConfigFiles};
    use aws_sdk_dynamodb::{
        config::{
            ConfigBag, Intercept, RuntimeComponents,
            interceptors::BeforeTransmitInterceptorContextRef,
        },
        error::BoxError,
    };
    use connector_sdk::{Connector, SecretString};

    use super::*;
    use crate::{DynamoDbConnector, Error};

    fn options(pairs: &[(&str, &str)]) -> ConnectionOptions {
        ConnectionOptions::parse(
            &DynamoDbConnector::descriptor(),
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), SecretString::new((*value).to_owned())))
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    /// Records the URI of the first request it sees, then stops it before it
    /// is sent.
    #[derive(Clone, Debug, Default)]
    struct Destination(Arc<Mutex<Option<String>>>);

    impl Intercept for Destination {
        fn name(&self) -> &'static str {
            "destination"
        }

        fn read_before_transmit(
            &self,
            context: &BeforeTransmitInterceptorContextRef<'_>,
            _components: &RuntimeComponents,
            _config: &mut ConfigBag,
        ) -> Result<(), BoxError> {
            *self.0.lock().unwrap() = Some(context.request().uri().to_owned());
            Err("the test stops the request".into())
        }
    }

    /// Where a `ListTables` request of a client built from `options` goes,
    /// with a shared config profile that sets its own endpoint URL.
    fn destination(options: &ConnectionOptions) -> String {
        let mut config = ClientConfig::new(options, None).unwrap();
        config.loader = config
            .loader
            .profile_files(
                EnvConfigFiles::builder()
                    .with_contents(
                        EnvConfigFileKind::Config,
                        "[profile test]\nendpoint_url = http://profile.invalid\n",
                    )
                    .build(),
            )
            .profile_name("test");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let destination = Destination::default();
        runtime.block_on(async {
            let client = config.client().await;
            let config = client
                .config()
                .to_builder()
                .interceptor(destination.clone());
            let _ = Client::from_conf(config.build()).list_tables().send().await;
        });
        destination.0.lock().unwrap().take().unwrap()
    }

    #[test]
    fn only_the_endpoint_option_overrides_the_regions_endpoint() {
        let keys = [
            ("region", "us-east-1"),
            ("access_key_id", "AKIDEXAMPLE"),
            ("secret_access_key", "secret"),
        ];
        assert_eq!(
            destination(&options(&keys)),
            "https://dynamodb.us-east-1.amazonaws.com/"
        );
        let mut with_endpoint = keys.to_vec();
        with_endpoint.push(("endpoint", "http://127.0.0.1:8000"));
        assert_eq!(
            destination(&options(&with_endpoint)),
            "http://127.0.0.1:8000/"
        );
    }

    /// A credential chain that finds no credentials.
    #[derive(Debug)]
    struct NoCredentials;

    impl ProvideCredentials for NoCredentials {
        fn provide_credentials<'a>(&'a self) -> CredentialsFuture<'a>
        where
            Self: 'a,
        {
            CredentialsFuture::ready(Err(CredentialsError::not_loaded(
                "no providers in chain provided credentials",
            )))
        }
    }

    #[test]
    fn a_credential_chain_without_credentials_fails_a_request_as_authentication() {
        let mut config = ClientConfig::new(
            &options(&[("region", "us-east-1"), ("endpoint", "http://127.0.0.1:9")]),
            None,
        )
        .unwrap();
        config.loader = config.loader.credentials_provider(NoCredentials);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime.block_on(async {
            let client = config.client().await;
            client.list_tables().send().await.unwrap_err()
        });
        let error = DynamoDbConnector::classify(Error::from(error));
        assert_eq!(error.kind(), ErrorKind::Authentication);
    }
}
