//! Running a SOQL query as a stream of Arrow batches.

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use futures_core::Stream;
use reqwest::Url;
use serde::Deserialize;

use crate::{
    Client,
    client::Token,
    decode::{self, Record},
    error::{Error, Result},
};

/// One page of a query response: whether it is the last, the
/// `nextRecordsUrl` of the next page, and the records
/// ([Query](https://developer.salesforce.com/docs/platform/api-rest/guide/resources-query.html)).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page<'a> {
    done: bool,
    next_records_url: Option<String>,
    #[serde(borrow)]
    records: Vec<Record<'a>>,
}

/// Where the next page comes from.
enum Cursor {
    /// The query has not been sent.
    Start(String),
    /// The `nextRecordsUrl` of the previous page.
    Next(String),
    Done,
}

impl Client {
    /// Runs `soql` with the Query resource and returns its records as batches
    /// of `schema`, one batch per page that holds records
    /// ([Execute a SOQL Query](https://developer.salesforce.com/docs/platform/api-rest/guide/dome-query.html)).
    ///
    /// Nothing is sent until the stream is first polled. Each field of
    /// `schema` names a field of the query's records and gives its Arrow
    /// type, which must be one the client produces; a schema with another
    /// type is rejected here. The SOQL text never appears in an error.
    pub fn query(
        &self,
        soql: impl Into<String>,
        schema: SchemaRef,
    ) -> Result<impl Stream<Item = Result<RecordBatch>> + Send + 'static> {
        decode::check_schema(&schema)?;
        let client = self.clone();
        Ok(futures_util::stream::unfold(
            Cursor::Start(soql.into()),
            move |cursor| {
                let client = client.clone();
                let schema = schema.clone();
                async move {
                    match client.next_batch(cursor, &schema).await {
                        Ok(Some((batch, cursor))) => Some((Ok(batch), cursor)),
                        Ok(None) => None,
                        Err(error) => Some((Err(error), Cursor::Done)),
                    }
                }
            },
        ))
    }

    /// Fetches pages from `cursor` until one holds records, and decodes it.
    async fn next_batch(
        &self,
        mut cursor: Cursor,
        schema: &SchemaRef,
    ) -> Result<Option<(RecordBatch, Cursor)>> {
        loop {
            let body = match &cursor {
                Cursor::Start(soql) => {
                    self.get(|token| {
                        let mut url = self.data_url(token, &["query"])?;
                        url.query_pairs_mut().append_pair("q", soql);
                        Ok(url)
                    })
                    .await?
                }
                Cursor::Next(next) => self.get(|token| next_page_url(token, next)).await?,
                Cursor::Done => return Ok(None),
            };
            let page: Page<'_> = serde_json::from_slice(&body).map_err(|error| {
                Error::InvalidResponse(format!("the query response cannot be read: {error}"))
            })?;
            cursor = match (page.done, page.next_records_url) {
                (false, Some(next)) => Cursor::Next(next),
                (false, None) => {
                    return Err(Error::InvalidResponse(
                        "a query page that is not the last has no nextRecordsUrl".to_owned(),
                    ));
                }
                (true, _) => Cursor::Done,
            };
            if !page.records.is_empty() {
                return Ok(Some((decode::decode(schema, &page.records)?, cursor)));
            }
        }
    }
}

/// Resolves a `nextRecordsUrl` against the instance, refusing one on another
/// origin so that the access token is never sent elsewhere. Salesforce gives
/// it as a path, requested without parameters
/// ([Execute a SOQL Query](https://developer.salesforce.com/docs/platform/api-rest/guide/dome-query.html)).
fn next_page_url(token: &Token, next: &str) -> Result<Url> {
    let url = token
        .instance_url
        .join(next)
        .map_err(|_| Error::InvalidResponse("the nextRecordsUrl is not a valid URL".to_owned()))?;
    if url.origin() != token.instance_url.origin() {
        return Err(Error::InvalidResponse(
            "the nextRecordsUrl points outside the instance".to_owned(),
        ));
    }
    Ok(url)
}
