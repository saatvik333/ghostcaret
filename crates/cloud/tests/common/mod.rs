//! A scriptable HTTP/1.1 server on localhost, and helpers to build requests.
//!
//! Written on `std::net` rather than a framework because the tests need what
//! frameworks hide: bytes delivered in chosen pieces, a connection held open
//! while the client decides to leave, and counts of connections and hang-ups.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ghostcaret_cloud::{SentLog, Tuning, provider_with};
use ghostcaret_engine::complete::{
    CancelToken, CompletionRequest, LengthMode, Segments, StopCheck, StopParams,
};
use ghostcaret_engine::io::FieldId;
use ghostcaret_engine::settings::{ProviderConfig, ProviderKind};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Req {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("body is not JSON ({e}): {}", self.body))
    }
}

pub enum Part {
    /// Written as one chunk and flushed.
    Send(Vec<u8>),
    Sleep(Duration),
    /// Keep the connection open until the client closes it.
    HoldUntilClosed,
}

pub enum Reply {
    Json {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    },
    Sse(Vec<Part>),
    /// Accept the request and say nothing until the client leaves.
    Silent,
}

impl Reply {
    pub fn json(body: impl Into<String>) -> Reply {
        Reply::status(200, body)
    }

    pub fn status(status: u16, body: impl Into<String>) -> Reply {
        Reply::Json {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Reply {
        if let Reply::Json { headers, .. } = &mut self {
            headers.push((name.into(), value.into()));
        }
        self
    }

    /// Each string is delivered on its own, 5 ms apart, so the client sees
    /// the pieces separately.
    pub fn sse(pieces: &[&str]) -> Reply {
        let mut parts = Vec::new();
        for p in pieces {
            parts.push(Part::Send(p.as_bytes().to_vec()));
            parts.push(Part::Sleep(Duration::from_millis(5)));
        }
        Reply::Sse(parts)
    }

    /// The whole stream cut into pieces of the given byte sizes (repeating),
    /// wherever that falls: mid-line, mid-event, mid-character.
    pub fn sse_split(stream: &str, sizes: &[usize]) -> Reply {
        let mut parts = Vec::new();
        let mut rest = stream.as_bytes();
        let mut sizes = sizes.iter().copied().cycle();
        while !rest.is_empty() {
            let (head, tail) = rest.split_at(sizes.next().unwrap().clamp(1, rest.len()));
            parts.push(Part::Send(head.to_vec()));
            parts.push(Part::Sleep(Duration::from_millis(2)));
            rest = tail;
        }
        Reply::Sse(parts)
    }

    /// Adds a part at the end of an SSE reply.
    pub fn then(mut self, part: Part) -> Reply {
        if let Reply::Sse(parts) = &mut self {
            parts.push(part);
        }
        self
    }
}

type Handler = dyn Fn(&Req, usize) -> Reply + Send + Sync;

struct State {
    requests: Mutex<Vec<Req>>,
    connections: AtomicUsize,
    hung_up: AtomicUsize,
    stop: AtomicBool,
    handler: Box<Handler>,
}

pub struct Mock {
    pub url: String,
    state: Arc<State>,
}

impl Mock {
    /// `handler` gets each request and its 0-based position among all requests.
    pub fn start(handler: impl Fn(&Req, usize) -> Reply + Send + Sync + 'static) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(State {
            requests: Mutex::new(Vec::new()),
            connections: AtomicUsize::new(0),
            hung_up: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            handler: Box::new(handler),
        });
        let shared = state.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                if shared.stop.load(SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                shared.connections.fetch_add(1, SeqCst);
                let shared = shared.clone();
                thread::spawn(move || serve(stream, &shared));
            }
        });
        Mock { url, state }
    }

    /// Replies from a fixed list, one per request; the last repeats.
    pub fn script(replies: Vec<Reply>) -> Mock {
        let replies = Mutex::new(replies.into_iter().collect::<VecDeque<_>>());
        Mock::start(move |_, _| {
            let mut replies = replies.lock().unwrap();
            if replies.len() > 1 {
                replies.pop_front().unwrap()
            } else {
                clone_reply(replies.front().expect("empty script"))
            }
        })
    }

    pub fn requests(&self) -> Vec<Req> {
        self.state.requests.lock().unwrap().clone()
    }

    /// Requests that were not warm-up probes.
    pub fn posts(&self) -> Vec<Req> {
        self.requests()
            .into_iter()
            .filter(|r| r.method == "POST")
            .collect()
    }

    pub fn connections(&self) -> usize {
        self.state.connections.load(SeqCst)
    }

    /// Times a client closed a connection while a reply was unfinished.
    pub fn hung_up(&self) -> usize {
        self.state.hung_up.load(SeqCst)
    }

    /// Waits for a condition that another thread makes true.
    pub fn wait_for(&self, what: &str, cond: impl Fn(&Mock) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !cond(self) {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            thread::yield_now();
            thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.state.stop.store(true, SeqCst);
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
    }
}

fn clone_reply(r: &Reply) -> Reply {
    match r {
        Reply::Json {
            status,
            headers,
            body,
        } => Reply::Json {
            status: *status,
            headers: headers.clone(),
            body: body.clone(),
        },
        Reply::Sse(parts) => Reply::Sse(
            parts
                .iter()
                .map(|p| match p {
                    Part::Send(b) => Part::Send(b.clone()),
                    Part::Sleep(d) => Part::Sleep(*d),
                    Part::HoldUntilClosed => Part::HoldUntilClosed,
                })
                .collect(),
        ),
        Reply::Silent => Reply::Silent,
    }
}

fn serve(stream: TcpStream, state: &State) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    loop {
        let Some(req) = read_request(&mut reader) else {
            return;
        };
        let index = {
            let mut all = state.requests.lock().unwrap();
            all.push(req.clone());
            all.len() - 1
        };
        if !respond(
            &mut writer,
            &mut reader,
            (state.handler)(&req, index),
            state,
            &req,
        ) {
            return;
        }
    }
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Req> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let mut headers = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line).ok()?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':')?;
        headers.push((k.trim().to_owned(), v.trim().to_owned()));
    }
    let len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    reader.read_exact(&mut body).ok()?;
    Some(Req {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// Returns false when the connection is finished.
fn respond(
    w: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    reply: Reply,
    state: &State,
    req: &Req,
) -> bool {
    let head_only = req.method == "HEAD";
    match reply {
        Reply::Json {
            status,
            headers,
            body,
        } => {
            let mut out = format!("HTTP/1.1 {status} X\r\ncontent-length: {}\r\n", body.len());
            if !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            {
                out.push_str("content-type: application/json\r\n");
            }
            for (k, v) in headers {
                out.push_str(&format!("{k}: {v}\r\n"));
            }
            out.push_str("\r\n");
            if !head_only {
                out.push_str(&body);
            }
            w.write_all(out.as_bytes()).and_then(|()| w.flush()).is_ok()
        }
        Reply::Sse(parts) => {
            if w.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n").is_err() {
                return false;
            }
            for part in parts {
                match part {
                    Part::Send(bytes) => {
                        let mut frame = format!("{:x}\r\n", bytes.len()).into_bytes();
                        frame.extend_from_slice(&bytes);
                        frame.extend_from_slice(b"\r\n");
                        if w.write_all(&frame).and_then(|()| w.flush()).is_err() {
                            state.hung_up.fetch_add(1, SeqCst);
                            return false;
                        }
                    }
                    Part::Sleep(d) => thread::sleep(d),
                    Part::HoldUntilClosed => {
                        wait_closed(reader);
                        state.hung_up.fetch_add(1, SeqCst);
                        return false;
                    }
                }
            }
            w.write_all(b"0\r\n\r\n").and_then(|()| w.flush()).is_ok()
        }
        Reply::Silent => {
            wait_closed(reader);
            state.hung_up.fetch_add(1, SeqCst);
            false
        }
    }
}

fn wait_closed(reader: &mut BufReader<TcpStream>) {
    let _ = reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(10)));
    let mut sink = [0u8; 64];
    while matches!(reader.read(&mut sink), Ok(n) if n > 0) {}
    let _ = reader.get_ref().shutdown(Shutdown::Both);
}

// ---- building providers and requests -------------------------------------

pub const KEY: &str = "sk-test-SECRET-key-123";

pub fn tuning(mock: &Mock) -> Tuning {
    Tuning {
        complete_deadline: Duration::from_secs(5),
        generate_deadline: Duration::from_secs(10),
        connect_timeout: Duration::from_secs(2),
        backoff: Duration::from_millis(5),
        endpoint: Some(mock.url.clone()),
        per_minute: 1000,
        ..Tuning::default()
    }
}

pub fn openai(mock: &Mock, key: Option<&str>, log: &SentLog) -> Arc<ghostcaret_cloud::Provider> {
    openai_with(mock, key, log, tuning(mock))
}

pub fn openai_with(
    mock: &Mock,
    key: Option<&str>,
    log: &SentLog,
    tuning: Tuning,
) -> Arc<ghostcaret_cloud::Provider> {
    let config = ProviderConfig {
        id: "p".into(),
        label: "Test".into(),
        kind: ProviderKind::OpenAiCompatible {
            base_url: format!("{}/v1", mock.url),
        },
        model: "test-model".into(),
    };
    provider_with(&config, key.map(str::to_owned), log.clone(), tuning).unwrap()
}

pub fn anthropic(mock: &Mock, model: &str, log: &SentLog) -> Arc<ghostcaret_cloud::Provider> {
    let config = ProviderConfig {
        id: "p".into(),
        label: "Claude".into(),
        kind: ProviderKind::Anthropic,
        model: model.into(),
    };
    provider_with(&config, Some(KEY.into()), log.clone(), tuning(mock)).unwrap()
}

pub fn gemini(mock: &Mock, model: &str, log: &SentLog) -> Arc<ghostcaret_cloud::Provider> {
    let config = ProviderConfig {
        id: "p".into(),
        label: "Gem".into(),
        kind: ProviderKind::Gemini,
        model: model.into(),
    };
    provider_with(&config, Some(KEY.into()), log.clone(), tuning(mock)).unwrap()
}

pub fn request(before: &str) -> CompletionRequest {
    CompletionRequest {
        id: 7,
        field: FieldId(1),
        revision: 1,
        segments: Segments {
            before: before.into(),
            ..Segments::default()
        },
        stop: StopParams {
            mode: LengthMode::Sentence,
            ceiling: LengthMode::Sentence,
            boldness: 0.5,
            paragraph_words: 80,
        },
        max_tokens: 24,
        alternatives: 0,
    }
}

/// A stop rule that never stops, recording what it saw.
#[derive(Default)]
pub struct Never {
    pub seen: Vec<(String, Option<f32>)>,
}

impl StopCheck for Never {
    fn should_stop(&mut self, generated: &str, token_prob: Option<f32>) -> bool {
        self.seen.push((generated.to_owned(), token_prob));
        false
    }
}

pub fn cancel() -> CancelToken {
    CancelToken::new()
}

/// `data: {json}\n\n`.
pub fn data(json: &str) -> String {
    format!("data: {json}\n\n")
}

pub fn chat_delta(text: &str) -> String {
    data(&serde_json::json!({"choices":[{"index":0,"delta":{"content":text}}]}).to_string())
}

pub fn raw_delta(text: &str) -> String {
    data(&serde_json::json!({"choices":[{"index":0,"text":text}]}).to_string())
}

/// No recorded request body or URL may contain the key.
pub fn assert_key_absent(mock: &Mock, log: &SentLog) {
    for e in log.entries() {
        assert!(
            !e.body.contains(KEY) && !e.provider.contains(KEY) && !e.model.contains(KEY),
            "key in log entry"
        );
    }
    for r in mock.requests() {
        assert!(
            !r.path.contains(KEY) && !r.body.contains(KEY),
            "key in URL or body"
        );
    }
}
