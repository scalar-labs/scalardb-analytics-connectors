//! OAuth 2.0 grants that obtain an access token from Salesforce.

use std::{
    borrow::Cow,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Url;
use ring::{
    rand::SystemRandom,
    signature::{RSA_PKCS1_SHA256, RsaKeyPair},
};
use rustls_pki_types::{PrivateKeyDer, pem::PemObject};
use serde::Deserialize;

use crate::{
    error::{Error, Result, SourceMessage},
    secret::Secret,
};

/// How the client authenticates to Salesforce.
#[derive(Debug)]
pub enum Credentials {
    /// The OAuth 2.0 refresh token flow, renewing a refresh token obtained
    /// beforehand, for example through the web server flow
    /// ([OAuth 2.0 Refresh Token Flow](https://developer.salesforce.com/docs/platform/mobile-sdk/guide/oauth-refresh-token-flow.html)).
    RefreshToken {
        client_id: String,
        /// Required when the app is configured to require the client secret
        /// for the refresh token flow.
        client_secret: Option<Secret>,
        refresh_token: Secret,
    },
    /// The OAuth 2.0 JWT bearer flow, signing an assertion with the private
    /// key whose certificate the app holds
    /// ([OAuth 2.0 JWT Bearer Flow for Server-to-Server Integration](https://help.salesforce.com/s/articleView?id=sf.remoteaccess_oauth_jwt_flow.htm&type=5)).
    JwtBearer {
        client_id: String,
        /// The Salesforce username the token acts as.
        username: String,
        /// The `aud` claim, such as `https://login.salesforce.com`.
        audience: String,
        /// The RSA private key, PEM-encoded in PKCS #8 or PKCS #1.
        private_key: Secret,
    },
}

/// A grant prepared from [`Credentials`]: a JWT bearer key is parsed once,
/// so an unusable key is reported when the client is created.
pub(crate) enum Grant {
    RefreshToken {
        client_id: String,
        client_secret: Option<Secret>,
        refresh_token: Secret,
    },
    JwtBearer {
        client_id: String,
        username: String,
        audience: String,
        key: Box<RsaKeyPair>,
    },
}

/// How long a JWT bearer assertion is valid. The assertion is sent as soon
/// as it is signed, so it needs only a short lifetime.
const ASSERTION_LIFETIME: Duration = Duration::from_secs(180);

const JWT_BEARER_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

impl Grant {
    pub(crate) fn new(credentials: Credentials) -> Result<Self> {
        Ok(match credentials {
            Credentials::RefreshToken {
                client_id,
                client_secret,
                refresh_token,
            } => Grant::RefreshToken {
                client_id,
                client_secret,
                refresh_token,
            },
            Credentials::JwtBearer {
                client_id,
                username,
                audience,
                private_key,
            } => Grant::JwtBearer {
                client_id,
                username,
                audience,
                key: Box::new(parse_private_key(&private_key)?),
            },
        })
    }

    /// The form fields of a token request for this grant. Secrets are
    /// borrowed rather than copied.
    pub(crate) fn form(&self) -> Result<Vec<(&'static str, Cow<'_, str>)>> {
        Ok(match self {
            Grant::RefreshToken {
                client_id,
                client_secret,
                refresh_token,
            } => {
                let mut form = vec![
                    ("grant_type", Cow::Borrowed("refresh_token")),
                    ("client_id", Cow::Borrowed(client_id.as_str())),
                    ("refresh_token", Cow::Borrowed(refresh_token.expose())),
                ];
                if let Some(secret) = client_secret {
                    form.push(("client_secret", Cow::Borrowed(secret.expose())));
                }
                form
            }
            Grant::JwtBearer {
                client_id,
                username,
                audience,
                key,
            } => vec![
                ("grant_type", Cow::Borrowed(JWT_BEARER_GRANT_TYPE)),
                (
                    "assertion",
                    Cow::Owned(assertion(key, client_id, username, audience)?),
                ),
            ],
        })
    }
}

fn parse_private_key(pem: &Secret) -> Result<RsaKeyPair> {
    let invalid = || Error::Config("the JWT bearer private key is not a usable RSA key".to_owned());
    match PrivateKeyDer::from_pem_slice(pem.expose().as_bytes()).map_err(|_| invalid())? {
        PrivateKeyDer::Pkcs8(der) => RsaKeyPair::from_pkcs8(der.secret_pkcs8_der()),
        PrivateKeyDer::Pkcs1(der) => RsaKeyPair::from_der(der.secret_pkcs1_der()),
        _ => return Err(invalid()),
    }
    .map_err(|_| invalid())
}

/// Builds and signs a JWT bearer assertion with RS256.
fn assertion(key: &RsaKeyPair, client_id: &str, username: &str, audience: &str) -> Result<String> {
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Config("the system clock is before 1970".to_owned()))?
        + ASSERTION_LIFETIME;
    let claims = serde_json::json!({
        "iss": client_id,
        "sub": username,
        "aud": audience,
        "exp": expires.as_secs(),
    });
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string()),
    );
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signing_input.as_bytes(),
        &mut signature,
    )
    .map_err(|_| Error::Config("signing the JWT bearer assertion failed".to_owned()))?;
    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature)
    ))
}

/// A successful response of the token endpoint, `/services/oauth2/token`.
#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    pub(crate) access_token: String,
    pub(crate) instance_url: String,
}

/// An error response of the token endpoint: the OAuth 2.0 `error` code and
/// its description.
#[derive(Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: String,
}

impl TokenResponse {
    pub(crate) fn parse(body: &[u8]) -> Result<(Secret, Url)> {
        let response: TokenResponse = serde_json::from_slice(body).map_err(|error| {
            Error::InvalidResponse(format!("the token response cannot be read: {error}"))
        })?;
        let instance_url = Url::parse(&response.instance_url).map_err(|_| {
            Error::InvalidResponse("the token response holds no valid instance URL".to_owned())
        })?;
        Ok((Secret::new(response.access_token), instance_url))
    }
}

pub(crate) fn authentication_error(status: u16, body: &[u8]) -> Error {
    match serde_json::from_slice::<OAuthError>(body) {
        Ok(error) => Error::Authentication {
            status,
            error: error.error,
            description: SourceMessage::new(error.error_description),
        },
        Err(_) => Error::Authentication {
            status,
            error: "unknown".to_owned(),
            description: SourceMessage::new(String::from_utf8_lossy(body)),
        },
    }
}
