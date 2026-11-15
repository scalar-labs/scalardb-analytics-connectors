//! A mock Salesforce org: the OAuth token endpoint and the REST resources the
//! connector reads, served over HTTP/1.1 on a local port.
//!
//! The org is plain data the fixture changes directly, as an administrator
//! would through Setup. The server lets the fixture observe what the contract
//! asks about the source: how many requests are running, breaking every open
//! connection, and making describe requests for one object wait.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    runtime::Runtime,
    task::AbortHandle,
};

use super::soql;

/// The REST API version the connector uses.
pub const API: &str = "/services/data/v62.0";

/// How many records one query page holds, so that the fixture's reads span
/// several pages.
const PAGE_SIZE: usize = 2;

/// One field of an object, with the describe attributes the connector reads.
#[derive(Clone)]
pub struct Field {
    pub name: &'static str,
    pub field_type: &'static str,
    pub precision: i64,
    pub scale: i64,
    pub nillable: bool,
    pub filterable: bool,
    pub unique: bool,
    pub reference_to: Vec<&'static str>,
}

impl Field {
    /// A nillable, filterable field.
    pub fn new(name: &'static str, field_type: &'static str) -> Self {
        Self {
            name,
            field_type,
            precision: 0,
            scale: 0,
            nillable: true,
            filterable: true,
            unique: false,
            reference_to: Vec::new(),
        }
    }

    /// The `Id` field every object has.
    pub fn id() -> Self {
        Self {
            nillable: false,
            ..Self::new("Id", "id")
        }
    }

    pub fn numeric(
        name: &'static str,
        field_type: &'static str,
        precision: i64,
        scale: i64,
    ) -> Self {
        Self {
            precision,
            scale,
            ..Self::new(name, field_type)
        }
    }

    pub fn required(self) -> Self {
        Self {
            nillable: false,
            ..self
        }
    }

    pub fn unfilterable(self) -> Self {
        Self {
            filterable: false,
            ..self
        }
    }

    pub fn unique(self) -> Self {
        Self {
            unique: true,
            ..self
        }
    }

    pub fn reference(self, targets: &[&'static str]) -> Self {
        Self {
            reference_to: targets.to_vec(),
            ..self
        }
    }

    fn describe(&self) -> Value {
        json!({
            "name": self.name,
            "label": self.name,
            "type": self.field_type,
            "soapType": "xsd:anyType",
            "length": 0,
            "byteLength": 0,
            "precision": self.precision,
            "scale": self.scale,
            "digits": 0,
            "nillable": self.nillable,
            "custom": self.name.ends_with("__c"),
            "calculated": false,
            "caseSensitive": false,
            "filterable": self.filterable,
            "unique": self.unique,
            "externalId": false,
            "idLookup": self.name == "Id",
            "deprecatedAndHidden": false,
            "referenceTo": self.reference_to,
            "relationshipName": null,
            "compoundFieldName": null,
        })
    }
}

/// One record: each field's value as JSON text, such as `"\"abc\""`,
/// `12.50`, or `null`.
pub type Record = BTreeMap<&'static str, String>;

/// How the org answers a query of an object.
#[derive(Clone)]
pub enum QueryBehavior {
    /// The matching records, a page at a time.
    Records,
    /// No answer until the client gives up.
    Wait,
    /// One record per page, without end.
    Endless,
    /// An error response.
    Fail { status: u16, code: &'static str },
}

#[derive(Clone)]
pub struct Object {
    pub fields: Vec<Field>,
    pub records: Vec<Record>,
    pub query: QueryBehavior,
}

impl Object {
    pub fn new(fields: Vec<Field>, records: Vec<Record>) -> Self {
        Self {
            fields,
            records,
            query: QueryBehavior::Records,
        }
    }

    pub fn field_mut(&mut self, name: &str) -> &mut Field {
        self.fields
            .iter_mut()
            .find(|field| field.name == name)
            .expect("the field exists")
    }
}

/// A user the org issues tokens to.
pub struct User {
    pub refresh_token: &'static str,
    /// The objects the user may see; `None` for every object.
    pub objects: Option<Vec<&'static str>>,
    /// Fields field-level security hides from the user, as object and field.
    pub hidden_fields: Vec<(&'static str, &'static str)>,
    /// Objects the user may see but not read: describe and queries answer
    /// `INSUFFICIENT_ACCESS`.
    pub denied: Vec<&'static str>,
    /// Whether Salesforce rejects every token issued to the user, as for a
    /// user whose session was revoked.
    pub revoked: bool,
}

impl User {
    fn sees(&self, object: &str) -> bool {
        self.objects
            .as_ref()
            .is_none_or(|objects| objects.contains(&object))
    }

    fn sees_field(&self, object: &str, field: &str) -> bool {
        !self.hidden_fields.contains(&(object, field))
    }
}

/// The org and the server's bookkeeping.
pub struct Org {
    pub objects: BTreeMap<&'static str, Object>,
    pub users: Vec<User>,
    /// Objects whose describe requests wait.
    pub blocked: Vec<&'static str>,
    tokens: HashMap<String, usize>,
    /// The records each query locator has left.
    cursors: HashMap<String, (String, Vec<String>)>,
    issued: u64,
    running: usize,
    connections: Vec<AbortHandle>,
}

/// The mock org, serving until dropped.
pub struct Mock {
    pub url: String,
    org: Arc<Mutex<Org>>,
    /// Runs the server; dropping it stops serving.
    _runtime: Runtime,
}

impl Mock {
    pub fn start(objects: BTreeMap<&'static str, Object>, users: Vec<User>) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("the mock's runtime starts");
        let listener = runtime
            .block_on(TcpListener::bind("127.0.0.1:0"))
            .expect("the mock binds a local port");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let org = Arc::new(Mutex::new(Org {
            objects,
            users,
            blocked: Vec::new(),
            tokens: HashMap::new(),
            cursors: HashMap::new(),
            issued: 0,
            running: 0,
            connections: Vec::new(),
        }));
        let server = Server {
            org: Arc::clone(&org),
            url: url.clone(),
        };
        runtime.spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let connection = server.clone();
                let task = tokio::spawn(async move { connection.serve(stream).await });
                lock(&server.org).connections.push(task.abort_handle());
            }
        });
        Self {
            url,
            org,
            _runtime: runtime,
        }
    }

    /// Changes the org.
    pub fn change<T>(&self, change: impl FnOnce(&mut Org) -> T) -> T {
        change(&mut lock(&self.org))
    }

    /// Makes describe requests for `object` wait until the block is dropped.
    pub fn block(&self, object: &'static str) -> Block {
        lock(&self.org).blocked.push(object);
        Block(Arc::clone(&self.org))
    }

    /// How many requests are being answered.
    pub fn running(&self) -> usize {
        lock(&self.org).running
    }

    /// Closes every open connection, as a network failure would.
    pub fn break_connections(&self) {
        let connections = std::mem::take(&mut lock(&self.org).connections);
        for connection in connections {
            connection.abort();
        }
        // An aborted task releases its request when the runtime drops it.
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Makes describe requests wait while alive.
pub struct Block(Arc<Mutex<Org>>);

impl Drop for Block {
    fn drop(&mut self) {
        lock(&self.0).blocked.clear();
    }
}

fn lock(org: &Mutex<Org>) -> MutexGuard<'_, Org> {
    org.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
struct Server {
    org: Arc<Mutex<Org>>,
    url: String,
}

/// One parsed request.
struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    authorization: Option<String>,
    body: String,
}

struct Response {
    status: u16,
    body: String,
}

impl Response {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body: body.to_string(),
        }
    }

    fn api_error(status: u16, code: &str, message: &str) -> Self {
        Self::json(
            status,
            json!([{"errorCode": code, "message": message, "fields": []}]),
        )
    }
}

/// What answering a request needs next.
enum Answer {
    Now(Response),
    /// Wait, then answer the request again.
    Later,
}

/// Counts one request as running while it is alive.
struct Running(Arc<Mutex<Org>>);

impl Running {
    fn new(org: &Arc<Mutex<Org>>) -> Self {
        lock(org).running += 1;
        Self(Arc::clone(org))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        lock(&self.0).running -= 1;
    }
}

impl Server {
    async fn serve(&self, mut stream: TcpStream) {
        let mut buffer = Vec::new();
        while let Some(request) = read_request(&mut stream, &mut buffer).await {
            let running = Running::new(&self.org);
            let response = loop {
                match self.answer(&request) {
                    Answer::Now(response) => break response,
                    Answer::Later => {
                        let mut probe = [0_u8; 1];
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
                            // The client sends nothing while it waits, so a
                            // read ends only when it closes the connection.
                            _ = stream.read(&mut probe) => return,
                        }
                    }
                }
            };
            let head = format!(
                "HTTP/1.1 {} Mock\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                response.status,
                response.body.len()
            );
            if stream.write_all(head.as_bytes()).await.is_err()
                || stream.write_all(response.body.as_bytes()).await.is_err()
            {
                return;
            }
            drop(running);
        }
    }

    fn answer(&self, request: &Request) -> Answer {
        let mut org = lock(&self.org);
        if request.method == "POST" && request.path == "/services/oauth2/token" {
            return Answer::Now(org.token(&request.body, &self.url));
        }
        let Some(user) = request
            .authorization
            .as_deref()
            .and_then(|value| value.strip_prefix("Bearer "))
            .and_then(|token| org.tokens.get(token).copied())
            .filter(|user| !org.users[*user].revoked)
        else {
            return Answer::Now(Response::api_error(
                401,
                "INVALID_SESSION_ID",
                "Session expired or invalid",
            ));
        };
        let Some(resource) = request.path.strip_prefix(API) else {
            return Answer::Now(not_found());
        };
        let segments: Vec<&str> = resource.split('/').skip(1).collect();
        match segments.as_slice() {
            ["sobjects"] => Answer::Now(org.describe_global(user)),
            ["sobjects", name, "describe"] => {
                let name = percent_decode(name);
                if org
                    .blocked
                    .iter()
                    .any(|blocked| blocked.eq_ignore_ascii_case(&name))
                {
                    return Answer::Later;
                }
                Answer::Now(org.describe(user, &name))
            }
            ["query"] => {
                let soql = request
                    .query
                    .iter()
                    .find(|(key, _)| key == "q")
                    .map(|(_, value)| value.as_str())
                    .unwrap_or_default();
                org.query(user, soql)
            }
            ["query", locator] => Answer::Now(org.next_page(locator)),
            _ => Answer::Now(not_found()),
        }
    }
}

fn insufficient_access() -> Response {
    Response::api_error(
        403,
        "INSUFFICIENT_ACCESS",
        "insufficient access rights on object",
    )
}

fn not_found() -> Response {
    Response::api_error(404, "NOT_FOUND", "The requested resource does not exist")
}

impl Org {
    fn token(&mut self, body: &str, url: &str) -> Response {
        let form = parse_form(body);
        let refresh_token = form
            .iter()
            .find(|(key, _)| key == "refresh_token")
            .map(|(_, value)| value.as_str());
        let Some(user) = self
            .users
            .iter()
            .position(|user| Some(user.refresh_token) == refresh_token)
        else {
            return Response::json(
                400,
                json!({"error": "invalid_grant", "error_description": "authentication failure"}),
            );
        };
        self.issued += 1;
        let access_token = format!("access-{}", self.issued);
        self.tokens.insert(access_token.clone(), user);
        Response::json(
            200,
            json!({"access_token": access_token, "instance_url": url, "token_type": "Bearer"}),
        )
    }

    /// The object `name` resolves to for `user`, ignoring case as Salesforce
    /// does.
    fn visible(&self, user: usize, name: &str) -> Option<(&'static str, &Object)> {
        self.objects
            .iter()
            .find(|(object, _)| object.eq_ignore_ascii_case(name))
            .filter(|(object, _)| self.users[user].sees(object))
            .map(|(object, value)| (*object, value))
    }

    fn visible_fields<'a>(&self, user: usize, name: &str, object: &'a Object) -> Vec<&'a Field> {
        object
            .fields
            .iter()
            .filter(|field| self.users[user].sees_field(name, field.name))
            .collect()
    }

    fn describe_global(&self, user: usize) -> Response {
        let sobjects: Vec<Value> = self
            .objects
            .keys()
            .filter(|name| self.users[user].sees(name))
            .map(|name| {
                json!({
                    "name": name,
                    "label": name,
                    "custom": name.ends_with("__c"),
                    "queryable": true,
                    "retrieveable": true,
                    "deprecatedAndHidden": false,
                })
            })
            .collect();
        Response::json(200, json!({"sobjects": sobjects}))
    }

    fn describe(&self, user: usize, name: &str) -> Response {
        let Some((name, object)) = self.visible(user, name) else {
            return not_found();
        };
        if self.users[user].denied.contains(&name) {
            return insufficient_access();
        }
        let fields: Vec<Value> = self
            .visible_fields(user, name, object)
            .iter()
            .map(|field| field.describe())
            .collect();
        Response::json(
            200,
            json!({
                "name": name,
                "label": name,
                "custom": name.ends_with("__c"),
                "queryable": true,
                "fields": fields,
            }),
        )
    }

    fn query(&mut self, user: usize, text: &str) -> Answer {
        let Some(query) = soql::parse(text) else {
            return Answer::Now(Response::api_error(400, "MALFORMED_QUERY", text));
        };
        let Some((name, object)) = self.visible(user, &query.object) else {
            return Answer::Now(Response::api_error(
                400,
                "INVALID_TYPE",
                &format!("sObject type '{}' is not supported.", query.object),
            ));
        };
        if self.users[user].denied.contains(&name) {
            return Answer::Now(insufficient_access());
        }
        let fields = self.visible_fields(user, name, object);
        let field = |wanted: &str| {
            fields
                .iter()
                .find(|field| field.name.eq_ignore_ascii_case(wanted))
        };
        for wanted in query
            .fields
            .iter()
            .chain(query.condition.iter().flat_map(soql::fields))
        {
            if field(wanted).is_none() {
                return Answer::Now(Response::api_error(400, "INVALID_FIELD", text));
            }
        }
        match &object.query {
            QueryBehavior::Records => {}
            QueryBehavior::Wait => return Answer::Later,
            QueryBehavior::Endless => return Answer::Now(endless_page(name)),
            QueryBehavior::Fail { status, code } => {
                return Answer::Now(Response::api_error(*status, code, "the request failed"));
            }
        }
        let types: HashMap<String, &'static str> = fields
            .iter()
            .map(|field| (field.name.to_ascii_lowercase(), field.field_type))
            .collect();
        let records: Vec<String> = object
            .records
            .iter()
            .filter(|record| {
                query
                    .condition
                    .as_ref()
                    .is_none_or(|condition| soql::holds(condition, record, &types))
            })
            .map(|record| render_record(name, record, &query.fields, &fields))
            .collect();
        Answer::Now(self.page(name.to_owned(), records))
    }

    /// The first page of `records`, keeping the rest behind a locator.
    fn page(&mut self, object: String, mut records: Vec<String>) -> Response {
        let total = records.len();
        let rest = records.split_off(records.len().min(PAGE_SIZE));
        let next = if rest.is_empty() {
            None
        } else {
            self.issued += 1;
            let locator = format!("01g{}-{}", self.issued, PAGE_SIZE);
            self.cursors.insert(locator.clone(), (object, rest));
            Some(format!("{API}/query/{locator}"))
        };
        page_response(total, &records, next)
    }

    fn next_page(&mut self, locator: &str) -> Response {
        if locator.starts_with("endless") {
            return endless_page("Endless__c");
        }
        let Some((object, records)) = self.cursors.remove(locator) else {
            return Response::api_error(400, "INVALID_QUERY_LOCATOR", "invalid query locator");
        };
        self.page(object, records)
    }
}

fn page_response(total: usize, records: &[String], next: Option<String>) -> Response {
    let next = next.map_or("null".to_owned(), |url| format!("\"{url}\""));
    Response {
        status: 200,
        body: format!(
            "{{\"totalSize\":{total},\"done\":{},\"nextRecordsUrl\":{next},\"records\":[{}]}}",
            next == "null",
            records.join(",")
        ),
    }
}

fn endless_page(object: &str) -> Response {
    let record =
        format!("{{\"attributes\":{{\"type\":\"{object}\"}},\"Id\":\"a00000000000001AAA\"}}");
    page_response(1, &[record], Some(format!("{API}/query/endless")))
}

/// Writes one record's JSON with each value's own text, so that numbers keep
/// every digit.
fn render_record(object: &str, record: &Record, wanted: &[String], fields: &[&Field]) -> String {
    let mut parts = vec![format!(
        "\"attributes\":{{\"type\":\"{object}\",\"url\":\"{API}/sobjects/{object}/x\"}}"
    )];
    for name in wanted {
        let field = fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(name))
            .expect("checked by the query");
        let raw = record.get(field.name).map_or("null", String::as_str);
        parts.push(format!("\"{}\":{raw}", field.name));
    }
    format!("{{{}}}", parts.join(","))
}

/// Reads one request, or `None` once the client closed the connection.
async fn read_request(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Option<Request> {
    let head_end = loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8(buffer[..head_end].to_vec()).ok()?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?.to_owned();
    let mut length = 0;
    let mut authorization = None;
    for line in lines {
        let (name, value) = line.split_once(':')?;
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.parse().ok()?,
            "authorization" => authorization = Some(value.to_owned()),
            _ => {}
        }
    }
    let body_start = head_end + 4;
    while buffer.len() < body_start + length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = String::from_utf8(buffer[body_start..body_start + length].to_vec()).ok()?;
    buffer.drain(..body_start + length);
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Some(Request {
        method,
        path: path.to_owned(),
        query: parse_form(query),
        authorization,
        body,
    })
}

/// Parses `application/x-www-form-urlencoded` pairs.
fn parse_form(text: &str) -> Vec<(String, String)> {
    text.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        decoded.push(byte);
                        index += 2;
                    }
                    Err(_) => decoded.push(b'%'),
                }
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
