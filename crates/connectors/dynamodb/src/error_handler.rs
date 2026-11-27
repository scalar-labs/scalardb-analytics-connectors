//! What a failed DynamoDB request reports, and its classification into the
//! contract's error kinds.
//!
//! The error table matches DynamoDB's error codes; the generic classes follow
//! from the HTTP status and from failures that never reached DynamoDB. The SDK
//! retries throttling, server errors, and transport failures in its standard
//! retry mode before a request fails, so those classes are what remains once
//! retries are exhausted. Only the messages of codes known not to echo the
//! request are returned as the Data Source message.

use std::{error::Error as _, io};

use aws_credential_types::provider::error::CredentialsError;
use aws_sdk_dynamodb::config::http::HttpResponse;
use aws_sdk_dynamodb::error::{ProvideErrorMetadata, SdkError};
use connector_sdk::{ConnectorError, ErrorClass, ErrorHandler, ErrorKind};

/// What a failed DynamoDB request reports.
///
/// It holds no request content and no credentials: only the error code and
/// message DynamoDB returned, or the kind of failure that kept the request
/// from reaching DynamoDB.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// DynamoDB answered with an error.
    Service {
        status: u16,
        /// The error code without its namespace, such as
        /// `ResourceNotFoundException`.
        code: Option<String>,
        message: Option<String>,
    },
    /// The credential chain supplied no credentials.
    Credentials,
    /// The request was not sent or its response not received.
    Transport(TransportKind),
    /// DynamoDB returned a response the SDK cannot read.
    InvalidResponse,
    /// The SDK could not build the request.
    Request,
}

/// The code of a request the credentials may not make.
pub(crate) const ACCESS_DENIED: &str = "AccessDeniedException";
/// The code of a request whose table does not exist.
pub(crate) const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

impl Error {
    /// The DynamoDB error code, if DynamoDB answered with one.
    pub(crate) fn code(&self) -> Option<&str> {
        match self {
            Self::Service { code, .. } => code.as_deref(),
            _ => None,
        }
    }
}

/// How a request failed to reach DynamoDB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportKind {
    Timeout,
    Io,
    Other,
}

impl<E: ProvideErrorMetadata + std::error::Error + 'static> From<SdkError<E, HttpResponse>>
    for Error
{
    fn from(error: SdkError<E, HttpResponse>) -> Self {
        match &error {
            SdkError::ServiceError(context) => Self::Service {
                status: context.raw().status().as_u16(),
                code: context.err().code().map(str::to_owned),
                message: context.err().message().map(str::to_owned),
            },
            SdkError::TimeoutError(_) => Self::Transport(TransportKind::Timeout),
            // The credential chain runs while a request is dispatched, so its
            // failure arrives as a dispatch failure that wraps it.
            SdkError::DispatchFailure(failure)
                if caused_by::<CredentialsError>(
                    failure
                        .as_connector_error()
                        .and_then(|error| error.source()),
                ) =>
            {
                Self::Credentials
            }
            SdkError::DispatchFailure(failure) => Self::Transport(if failure.is_timeout() {
                TransportKind::Timeout
            } else if failure.is_io() {
                TransportKind::Io
            } else {
                TransportKind::Other
            }),
            // A connection that breaks while the body is read fails reading
            // the response, with the I/O error as its cause.
            SdkError::ResponseError(_) if caused_by::<io::Error>(error.source()) => {
                Self::Transport(TransportKind::Io)
            }
            SdkError::ResponseError(_) => Self::InvalidResponse,
            _ => Self::Request,
        }
    }
}

/// Whether `source`, or an error that caused it, is a `T`.
fn caused_by<T: std::error::Error + 'static>(
    mut source: Option<&(dyn std::error::Error + 'static)>,
) -> bool {
    while let Some(error) = source {
        if error.is::<T>() {
            return true;
        }
        source = error.source();
    }
    false
}

/// The DynamoDB connector's error table.
pub struct DynamoDbErrorHandler;

const fn class(kind: ErrorKind, retryable: bool, message: &'static str) -> ErrorClass {
    ErrorClass::new(kind, retryable, message)
}

const REJECTED_CREDENTIALS: ErrorClass = class(
    ErrorKind::Authentication,
    false,
    "DynamoDB rejected the credentials",
);
const THROTTLED: ErrorClass = class(
    ErrorKind::Resource,
    true,
    "DynamoDB kept throttling the requests after the SDK's retries",
);

/// The error codes the table classifies, and their classes.
const ERROR_TABLE: [(&str, ErrorClass); 10] = [
    ("UnrecognizedClientException", REJECTED_CREDENTIALS),
    ("InvalidSignatureException", REJECTED_CREDENTIALS),
    (
        "ExpiredTokenException",
        class(
            ErrorKind::Authentication,
            false,
            "the session token of the credentials expired",
        ),
    ),
    (
        ACCESS_DENIED,
        class(
            ErrorKind::PermissionDenied,
            false,
            "the credentials may not perform the DynamoDB action",
        ),
    ),
    (
        RESOURCE_NOT_FOUND,
        class(
            ErrorKind::DoesNotExist,
            false,
            "the DynamoDB table does not exist",
        ),
    ),
    (
        "ValidationException",
        class(
            ErrorKind::Internal,
            false,
            "DynamoDB rejected the request the connector built",
        ),
    ),
    ("ProvisionedThroughputExceededException", THROTTLED),
    ("ThrottlingException", THROTTLED),
    ("RequestLimitExceeded", THROTTLED),
    (
        "InternalServerError",
        class(ErrorKind::DataSource, true, "DynamoDB failed the request"),
    ),
];

/// Codes whose messages never contain the request: they describe the
/// credentials, permissions, or capacity, not the request's expressions or
/// key values.
const MESSAGE_KEPT: [&str; 9] = [
    "UnrecognizedClientException",
    "InvalidSignatureException",
    "ExpiredTokenException",
    ACCESS_DENIED,
    RESOURCE_NOT_FOUND,
    "ProvisionedThroughputExceededException",
    "ThrottlingException",
    "RequestLimitExceeded",
    "InternalServerError",
];

impl ErrorHandler for DynamoDbErrorHandler {
    type Diagnostic = Error;

    fn classify(diagnostic: &Error) -> Option<ErrorClass> {
        let code = diagnostic.code()?;
        ERROR_TABLE
            .iter()
            .find(|(known, _)| *known == code)
            .map(|(_, class)| *class)
    }
}

/// Builds the error for a failed request, with `class` from the error table
/// when it matched.
pub(crate) fn error(diagnostic: Error, class: Option<ErrorClass>) -> ConnectorError {
    match diagnostic {
        Error::Service {
            status,
            code,
            message,
        } => {
            let class = class.unwrap_or_else(|| service_class(status));
            let message = code
                .as_deref()
                .filter(|code| MESSAGE_KEPT.contains(code))
                .and(message);
            ConnectorError::new(class.kind, class.retryable, class.safe_message)
                .with_data_source_error(code, message)
        }
        Error::Credentials => ConnectorError::new(
            ErrorKind::Authentication,
            false,
            "the AWS credential chain supplied no credentials",
        ),
        Error::Transport(kind) => ConnectorError::new(
            ErrorKind::Connection,
            true,
            match kind {
                TransportKind::Timeout => "the connection to DynamoDB timed out",
                TransportKind::Io => "the connection to DynamoDB broke",
                TransportKind::Other => "no request could be sent to DynamoDB",
            },
        ),
        Error::InvalidResponse => ConnectorError::new(
            ErrorKind::Internal,
            false,
            "DynamoDB returned a response the connector cannot read",
        ),
        Error::Request => ConnectorError::new(
            ErrorKind::Internal,
            false,
            "the SDK could not build the DynamoDB request",
        ),
    }
}

/// The class of a DynamoDB error that the error table does not list.
fn service_class(status: u16) -> ErrorClass {
    match status {
        500.. => class(ErrorKind::DataSource, true, "DynamoDB failed the request"),
        _ => class(ErrorKind::DataSource, false, "DynamoDB refused the request"),
    }
}

#[cfg(test)]
mod tests {
    use aws_sdk_dynamodb::{error::ConnectorError as Dispatch, operation::scan::ScanError};
    use connector_sdk::{ConnectionOptions, Connector, ErrorKind, SecretString};

    use super::*;
    use crate::{DynamoDbConnector, options::ClientConfig};

    /// The orchestrator resolves credentials while dispatching a request,
    /// and wraps what the credential chain returned in the dispatch failure.
    #[test]
    fn a_credential_chain_failure_is_authentication() {
        let chain = CredentialsError::not_loaded("no providers in chain provided credentials");
        let failure = SdkError::<ScanError, HttpResponse>::dispatch_failure(Dispatch::other(
            chain.into(),
            None,
        ));
        let diagnostic = Error::from(failure);
        assert_eq!(diagnostic, Error::Credentials);
        let error = DynamoDbConnector::classify(diagnostic);
        assert_eq!(error.kind(), ErrorKind::Authentication);
        assert!(!error.retryable());
    }

    /// A response whose connection closes before its body ends.
    #[test]
    fn a_connection_that_breaks_while_a_response_is_read_is_a_connection_error() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let _ = stream.read(&mut [0_u8; 8192]);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/x-amz-json-1.0\r\n\
                      content-length: 1000\r\n\r\n{\"Items\":[",
                );
            }
        });
        let options = ConnectionOptions::parse(
            &DynamoDbConnector::descriptor(),
            [
                ("region", "us-east-1"),
                ("endpoint", endpoint.as_str()),
                ("access_key_id", "AKIDEXAMPLE"),
                ("secret_access_key", "secret"),
            ]
            .map(|(key, value)| (key.to_owned(), SecretString::new(value.to_owned()))),
        )
        .unwrap();
        let config = ClientConfig::new(&options, None).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let failure = runtime.block_on(async {
            let client = config.client().await;
            client.scan().table_name("orders").send().await.unwrap_err()
        });
        let diagnostic = Error::from(failure);
        assert_eq!(diagnostic, Error::Transport(TransportKind::Io));
        let error = DynamoDbConnector::classify(diagnostic);
        assert_eq!(error.kind(), ErrorKind::Connection);
        assert!(error.retryable());
    }

    #[test]
    fn messages_that_may_echo_the_request_are_dropped() {
        let error = DynamoDbConnector::classify(Error::Service {
            status: 400,
            code: Some("ValidationException".to_owned()),
            message: Some("Invalid ProjectionExpression: #a0".to_owned()),
        });
        assert_eq!(error.kind(), ErrorKind::Internal);
        assert_eq!(error.data_source_code(), Some("ValidationException"));
        assert_eq!(error.data_source_message(), None);
    }

    #[test]
    fn unlisted_codes_are_classified_by_status() {
        let classify = |status| {
            let error = DynamoDbConnector::classify(Error::Service {
                status,
                code: Some("SomethingNew".to_owned()),
                message: None,
            });
            (error.kind(), error.retryable())
        };
        assert_eq!(classify(400), (ErrorKind::DataSource, false));
        assert_eq!(classify(503), (ErrorKind::DataSource, true));
    }
}
