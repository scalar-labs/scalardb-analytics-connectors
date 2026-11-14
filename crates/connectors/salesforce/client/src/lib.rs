//! A Salesforce REST API client for the Salesforce connector.
//!
//! The client holds what it takes to talk to Salesforce: OAuth 2.0
//! authentication, HTTP with retries, paging, the JSON forms of responses and
//! values, and error responses. It does not interpret what the data means:
//! describe results are returned as Salesforce reports them, and query
//! records are decoded into whatever Arrow schema the caller passes, without
//! changing any value. Deciding types, building SOQL, and classifying errors
//! belong to the connector.
//!
//! The API is asynchronous and creates no runtime; see [`Client`].
//!
//! The Salesforce documentation the client follows:
//!
//! - [OAuth Authorization Flows](https://help.salesforce.com/s/articleView?id=sf.remoteaccess_oauth_flows.htm&type=5),
//!   for the refresh token and JWT bearer flows ([`Credentials`]);
//! - the [REST API Developer Guide](https://developer.salesforce.com/docs/platform/api-rest/guide/intro-rest-resources.html), for
//!   the resources it reads and their responses; and
//! - [Primitive Data Types](https://developer.salesforce.com/docs/atlas.en-us.api.meta/api/primitive_data_types.htm),
//!   for what values of each type mean.

mod auth;
mod client;
mod decode;
mod describe;
mod error;
mod query;
mod secret;

pub use auth::Credentials;
pub use client::{Client, ClientConfig};
pub use describe::{FieldDescribe, GlobalDescribe, SObjectDescribe, SObjectSummary};
pub use error::{ApiError, Error, Result, SourceMessage, TransportKind};
pub use reqwest::Url;
pub use secret::Secret;
