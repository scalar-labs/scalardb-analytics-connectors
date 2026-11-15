//! The client: configuration, the shared access token, and sending requests.

use std::{fmt, sync::Arc, time::Duration};

use bytes::Bytes;
use reqwest::{StatusCode, Url, header::ACCEPT};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::{
    auth::{Credentials, Grant, TokenResponse, authentication_error},
    describe::{GlobalDescribe, SObjectDescribe},
    error::{ApiError, Error, Result, SourceMessage},
    secret::Secret,
};

/// Settings for one Salesforce org.
#[derive(Debug)]
pub struct ClientConfig {
    /// Where tokens are requested, such as `https://login.salesforce.com` or
    /// the org's My Domain URL.
    pub login_url: Url,
    /// The REST API version, such as `62.0`.
    pub api_version: String,
    pub credentials: Credentials,
    /// Limits establishing each connection; `None` means no limit.
    pub connect_timeout: Option<Duration>,
}

/// A client for one Salesforce org.
///
/// Clones share one HTTP client and one access token. The client creates no
/// async runtime: its futures and streams must be polled on a Tokio runtime,
/// because the HTTP client uses Tokio's I/O and timers. Dropping a future or
/// stream stops its request.
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    http: reqwest::Client,
    token_url: Url,
    api_version: String,
    grant: Grant,
    token: Mutex<Option<Arc<Token>>>,
}

/// An access token and the instance it was issued for.
pub(crate) struct Token {
    access: Secret,
    pub(crate) instance_url: Url,
    /// Counts tokens issued to this client, so that concurrent requests that
    /// all saw one token expire renew it once.
    generation: u64,
}

/// How many times a request is sent again after a retryable failure.
const MAX_RETRIES: u32 = 3;
const FIRST_BACKOFF: Duration = Duration::from_millis(500);

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Client")
            .field("token_url", &self.inner.token_url.as_str())
            .field("api_version", &self.inner.api_version)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Creates a client. It sends nothing until a request is made.
    pub fn new(config: ClientConfig) -> Result<Self> {
        let version_is_valid = config
            .api_version
            .split_once('.')
            .is_some_and(|(major, minor)| {
                [major, minor]
                    .iter()
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
            });
        if !version_is_valid {
            return Err(Error::Config(
                "the API version must be written as <major>.<minor>, such as 62.0".to_owned(),
            ));
        }
        let token_url = config
            .login_url
            .join("/services/oauth2/token")
            .map_err(|_| Error::Config("the login URL cannot have a path".to_owned()))?;
        let mut http = reqwest::Client::builder();
        if let Some(timeout) = config.connect_timeout {
            http = http.connect_timeout(timeout);
        }
        let http = http
            .build()
            .map_err(|error| Error::Config(format!("the HTTP client cannot be built: {error}")))?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                token_url,
                api_version: config.api_version,
                grant: Grant::new(config.credentials)?,
                token: Mutex::new(None),
            }),
        })
    }

    /// Obtains an access token now, so that invalid credentials are reported
    /// before any other request.
    pub async fn authenticate(&self) -> Result<()> {
        self.inner.token().await.map(|_| ())
    }

    /// Lists the objects visible to the user
    /// ([Describe Global](https://developer.salesforce.com/docs/platform/api-rest/guide/resources-describeglobal.html)).
    pub async fn describe_global(&self) -> Result<GlobalDescribe> {
        let body = self
            .get(|token| self.data_url(token, &["sobjects"]))
            .await?;
        parse(&body, "describeGlobal")
    }

    /// Describes one object and its fields
    /// ([sObject Describe](https://developer.salesforce.com/docs/platform/api-rest/guide/resources-sobject-describe.html)).
    pub async fn describe(&self, sobject: &str) -> Result<SObjectDescribe> {
        let body = self
            .get(|token| self.data_url(token, &["sobjects", sobject, "describe"]))
            .await?;
        parse(&body, "describe")
    }

    /// The URL of a REST resource under `/services/data/v<version>/`, the
    /// root of one API version on the instance
    /// ([Versions](https://developer.salesforce.com/docs/platform/api-rest/guide/resources-versions.html)).
    pub(crate) fn data_url(&self, token: &Token, segments: &[&str]) -> Result<Url> {
        let mut url = token.instance_url.clone();
        let version = format!("v{}", self.inner.api_version);
        url.path_segments_mut()
            .map_err(|_| Error::InvalidResponse("the instance URL cannot have a path".to_owned()))?
            .pop_if_empty()
            .extend(["services", "data", version.as_str()])
            .extend(segments);
        Ok(url)
    }

    /// Sends a GET request with the current access token and returns the
    /// body of a successful response.
    ///
    /// `url` builds the request URL for the token in use, because the
    /// instance comes from the token. An expired token is renewed once, and a
    /// retryable failure is retried with backoff.
    pub(crate) async fn get(&self, url: impl Fn(&Token) -> Result<Url>) -> Result<Bytes> {
        let mut token = self.inner.token().await?;
        let mut renewed = false;
        let mut retries = 0;
        loop {
            let response = self
                .inner
                .http
                .get(url(&token)?)
                .bearer_auth(token.access.expose())
                .header(ACCEPT, "application/json")
                .send()
                .await;
            match outcome(response).await {
                Outcome::Success(body) => return Ok(body),
                Outcome::Unauthorized(_) if !renewed => {
                    token = self.inner.renew(token.generation).await?;
                    renewed = true;
                }
                Outcome::Retryable(_) if retries < MAX_RETRIES => {
                    tokio::time::sleep(backoff(retries)).await;
                    retries += 1;
                }
                Outcome::Unauthorized(body) | Outcome::Retryable(body) | Outcome::Failed(body) => {
                    return Err(body.into_error());
                }
            }
        }
    }
}

impl Inner {
    /// The current token, requesting one if none was issued yet.
    async fn token(&self) -> Result<Arc<Token>> {
        let mut current = self.token.lock().await;
        if let Some(token) = current.as_ref() {
            return Ok(token.clone());
        }
        let token = Arc::new(self.request_token(0).await?);
        *current = Some(token.clone());
        Ok(token)
    }

    /// Replaces the token of `generation`, which Salesforce rejected. When
    /// another request already replaced it, the newer token is returned.
    async fn renew(&self, generation: u64) -> Result<Arc<Token>> {
        let mut current = self.token.lock().await;
        if let Some(token) = current.as_ref()
            && token.generation != generation
        {
            return Ok(token.clone());
        }
        let token = Arc::new(self.request_token(generation + 1).await?);
        *current = Some(token.clone());
        Ok(token)
    }

    async fn request_token(&self, generation: u64) -> Result<Token> {
        let mut retries = 0;
        loop {
            let response = self
                .http
                .post(self.token_url.clone())
                .header(ACCEPT, "application/json")
                .form(&self.grant.form()?)
                .send()
                .await;
            match outcome(response).await {
                Outcome::Success(body) => {
                    let (access, instance_url) = TokenResponse::parse(&body)?;
                    return Ok(Token {
                        access,
                        instance_url,
                        generation,
                    });
                }
                Outcome::Retryable(_) if retries < MAX_RETRIES => {
                    tokio::time::sleep(backoff(retries)).await;
                    retries += 1;
                }
                Outcome::Unauthorized(body) | Outcome::Retryable(body) | Outcome::Failed(body) => {
                    return Err(match body {
                        Failure::Response { status, body } => {
                            authentication_error(status.as_u16(), &body)
                        }
                        Failure::Transport(error) => error,
                    });
                }
            }
        }
    }
}

fn backoff(retries: u32) -> Duration {
    FIRST_BACKOFF * 2_u32.pow(retries)
}

/// What a sent request came to.
enum Outcome {
    Success(Bytes),
    /// HTTP 401: the session or OAuth token has expired or is invalid
    /// ([Status Codes and Error Responses](https://developer.salesforce.com/docs/platform/api-rest/guide/errorcodes.html)).
    Unauthorized(Failure),
    /// A failure that sending the same request again may not repeat: a
    /// connection that could not be made or timed out, HTTP 502 (Salesforce
    /// Edge could not reach the instance), or HTTP 503 (the server is down for
    /// maintenance or overloaded)
    /// ([Status Codes and Error Responses](https://developer.salesforce.com/docs/platform/api-rest/guide/errorcodes.html)).
    /// HTTP 500, an error within the platform, and 403 `REQUEST_LIMIT_EXCEEDED`,
    /// the org's exhausted API limit, are not retried.
    Retryable(Failure),
    Failed(Failure),
}

enum Failure {
    Response { status: StatusCode, body: Bytes },
    Transport(Error),
}

impl Failure {
    /// The error of a failed REST API request.
    fn into_error(self) -> Error {
        match self {
            Failure::Response { status, body } => Error::Api {
                status: status.as_u16(),
                errors: api_errors(&body),
            },
            Failure::Transport(error) => error,
        }
    }
}

async fn outcome(response: reqwest::Result<reqwest::Response>) -> Outcome {
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            let retryable = error.is_connect() || error.is_timeout();
            let failure = Failure::Transport(Error::transport(error));
            return if retryable {
                Outcome::Retryable(failure)
            } else {
                Outcome::Failed(failure)
            };
        }
    };
    let status = response.status();
    let body = match response.bytes().await {
        Ok(body) => body,
        Err(error) => return Outcome::Retryable(Failure::Transport(Error::transport(error))),
    };
    if status.is_success() {
        return Outcome::Success(body);
    }
    let failure = Failure::Response { status, body };
    if status == StatusCode::UNAUTHORIZED {
        Outcome::Unauthorized(failure)
    } else if status == StatusCode::BAD_GATEWAY || status == StatusCode::SERVICE_UNAVAILABLE {
        Outcome::Retryable(failure)
    } else {
        Outcome::Failed(failure)
    }
}

/// The entries of a REST API error response, each with its `message` and
/// `errorCode` ([Status Codes and Error Responses](https://developer.salesforce.com/docs/platform/api-rest/guide/errorcodes.html));
/// an unreadable body gives none.
fn api_errors(body: &[u8]) -> Vec<ApiError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Entry {
        #[serde(default)]
        error_code: String,
        #[serde(default)]
        message: String,
        #[serde(default)]
        fields: Vec<String>,
    }
    serde_json::from_slice::<Vec<Entry>>(body)
        .unwrap_or_default()
        .into_iter()
        .map(|entry| ApiError {
            error_code: entry.error_code,
            message: SourceMessage::new(entry.message),
            fields: entry.fields,
        })
        .collect()
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(body).map_err(|error| {
        Error::InvalidResponse(format!("the {what} response cannot be read: {error}"))
    })
}
