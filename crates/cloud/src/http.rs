//! Blocking HTTPS with a keep-alive pool.
//!
//! ureq 3 over rustls: synchronous like the rest of the engine, pools
//! connections per host, and lets a connector wrap the socket. That wrapper is
//! how a cancelled request stops waiting on the network immediately instead of
//! at the next byte (see [`Interruptible`]). reqwest would add a tokio runtime
//! to the process for no gain.

use std::cell::RefCell;
use std::fmt;
use std::io::{self, Read};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ghostcaret_engine::complete::CancelToken;
use serde_json::Value;
use ureq::Agent;
use ureq::config::Config;
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::time::Duration as UreqDuration;
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport,
};

use crate::error::ProviderError;
use crate::limit::RateLimiter;
use crate::sse::{Flow, SseEvent, SseParser};
use crate::wire::{Chunk, Decoder, Step, error_message};

const READ_BUFFER: usize = 16 * 1024;
/// Most bytes accepted from one response.
const MAX_RESPONSE: u64 = 8 << 20;
/// Most of an error body worth keeping.
const MAX_ERROR_BODY: u64 = 64 << 10;
/// How often a blocked read looks at the cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(40);
/// A pooled connection older than this may have been closed by the server.
const IDLE_REUSE: Duration = Duration::from_secs(25);

/// Connection settings of one provider.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub connect_timeout: Duration,
}

pub(crate) struct Http {
    agent: Agent,
    headers: Vec<(&'static str, String)>,
    pub limiter: RateLimiter,
    /// When a connection last went back to the pool in a usable state.
    pooled_at: Mutex<Option<Instant>>,
}

impl fmt::Debug for Http {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Header values carry the API key.
        f.debug_struct("Http")
            .field(
                "headers",
                &self.headers.iter().map(|h| h.0).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Http {
    pub fn new(settings: &Settings, headers: Vec<(&'static str, String)>, per_minute: u32) -> Self {
        let config = Config::builder()
            .http_status_as_error(false)
            // Never follow a redirect: the key would travel to wherever it points.
            .max_redirects(0)
            .timeout_connect(Some(settings.connect_timeout))
            .max_idle_connections_per_host(2)
            .max_idle_age(Duration::from_secs(50))
            .user_agent(concat!("ghostcaret/", env!("CARGO_PKG_VERSION")))
            .build();
        let agent = Agent::with_parts(
            config,
            DefaultConnector::default().chain(InterruptConnector),
            DefaultResolver::default(),
        );
        Http {
            agent,
            headers,
            limiter: RateLimiter::new(per_minute),
            pooled_at: Mutex::new(None),
        }
    }

    /// Opens a connection to the host of `url` and parks it in the pool, unless
    /// one was parked recently. No credentials are sent.
    pub fn warm(&self, url: &str, deadline: Instant, cancel: &CancelToken) {
        if self
            .pooled_at
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some_and(|t| t.elapsed() < IDLE_REUSE)
        {
            return;
        }
        let _armed = Armed::new(cancel);
        let request = self
            .agent
            .head(url)
            .config()
            .timeout_global(Some(remaining(deadline)))
            .build();
        // Any answer, even 404, leaves the connection reusable.
        if request.call().is_ok() {
            self.mark_pooled();
        }
    }

    pub fn get_json(
        &self,
        url: &str,
        deadline: Instant,
        cancel: &CancelToken,
    ) -> Result<Value, ProviderError> {
        let _armed = Armed::new(cancel);
        let mut request = self
            .agent
            .get(url)
            .config()
            .timeout_global(Some(remaining(deadline)))
            .build();
        for (name, value) in &self.headers {
            request = request.header(*name, value.as_str());
        }
        let response = request
            .header("Accept", "application/json")
            .call()
            .map_err(|e| transport_error(e, cancel))?;
        let (parts, body) = response.into_parts();
        let bytes = read_all(body.into_with_config().limit(MAX_RESPONSE).reader(), cancel)?;
        check_status(parts.status.as_u16(), &parts.headers, &bytes)?;
        self.mark_pooled();
        serde_json::from_slice(&bytes)
            .map_err(|e| ProviderError::Malformed(format!("invalid JSON: {e}")))
    }

    /// Sends `body` and feeds the answer to `decoder`, handing each decoded
    /// chunk to `on_chunk`. Returns when the answer is complete or `on_chunk`
    /// says stop; stopping and cancelling both drop the connection so the
    /// provider stops generating.
    pub fn stream(
        &self,
        url: &str,
        body: &str,
        deadline: Instant,
        cancel: &CancelToken,
        decoder: &mut dyn Decoder,
        on_chunk: &mut dyn FnMut(Chunk) -> Result<Flow, ProviderError>,
    ) -> Result<(), ProviderError> {
        let _armed = Armed::new(cancel);
        let mut request = self
            .agent
            .post(url)
            .config()
            .timeout_global(Some(remaining(deadline)))
            .build();
        for (name, value) in &self.headers {
            request = request.header(*name, value.as_str());
        }
        let response = request
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream, application/json")
            .send(body)
            .map_err(|e| transport_error(e, cancel))?;
        let (parts, body) = response.into_parts();
        let status = parts.status.as_u16();
        if status >= 300 {
            let bytes = read_all(
                body.into_with_config().limit(MAX_ERROR_BODY).reader(),
                cancel,
            )?;
            return Err(status_error(status, &parts.headers, &bytes));
        }
        let is_stream = body
            .mime_type()
            .is_some_and(|m| m.eq_ignore_ascii_case("text/event-stream"));
        let mime = body.mime_type().unwrap_or("none").to_owned();
        let mut reader = body.into_with_config().limit(MAX_RESPONSE).reader();

        if !is_stream {
            let bytes = read_all(reader, cancel)?;
            if !mime.contains("json") {
                return Err(ProviderError::Malformed(format!(
                    "expected JSON or an event stream, got content type {mime}; is the base URL right?"
                )));
            }
            self.mark_pooled();
            let chunk = decoder.whole(&bytes)?;
            on_chunk(chunk)?;
            return Ok(());
        }

        let mut parser = SseParser::new();
        let mut finished = false;
        let mut sink = |event: SseEvent| -> Result<Flow, ProviderError> {
            if finished {
                return Ok(Flow::Continue);
            }
            match decoder.event(&event)? {
                Step::Idle => Ok(Flow::Continue),
                Step::Chunk(chunk) => on_chunk(chunk),
                Step::Done => {
                    finished = true;
                    Ok(Flow::Continue)
                }
            }
        };
        let mut buf = [0u8; READ_BUFFER];
        loop {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let n = reader.read(&mut buf).map_err(|e| io_error(&e, cancel))?;
            if n == 0 {
                parser.finish(&mut sink)?;
                break;
            }
            if parser.feed(&buf[..n], &mut sink)? == Flow::Stop {
                return Ok(());
            }
        }
        self.mark_pooled();
        Ok(())
    }

    fn mark_pooled(&self) {
        *self.pooled_at.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
    }
}

fn remaining(deadline: Instant) -> Duration {
    // ureq treats a zero timeout as "expire at once", which is what a spent
    // deadline should do.
    deadline.saturating_duration_since(Instant::now())
}

fn read_all(mut reader: impl Read, cancel: &CancelToken) -> Result<Vec<u8>, ProviderError> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| io_error(&e, cancel))?;
    Ok(bytes)
}

fn check_status(
    status: u16,
    headers: &ureq::http::HeaderMap,
    body: &[u8],
) -> Result<(), ProviderError> {
    if status < 300 {
        Ok(())
    } else {
        Err(status_error(status, headers, body))
    }
}

fn status_error(status: u16, headers: &ureq::http::HeaderMap, body: &[u8]) -> ProviderError {
    let message = error_message(body);
    match status {
        429 => ProviderError::RateLimited {
            message,
            retry_after: retry_after(headers),
        },
        408 | 500..=599 => ProviderError::Unavailable(format!("{message} (HTTP {status})")),
        300..=399 => {
            let to = headers
                .get("location")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("another address");
            ProviderError::Rejected {
                status: Some(status),
                message: format!("the server redirects to {to}; use that as the base URL"),
            }
        }
        _ => ProviderError::Rejected {
            status: Some(status),
            message,
        },
    }
}

/// `retry-after-ms`, or `retry-after` in seconds. The HTTP-date form is
/// ignored; backoff covers it.
fn retry_after(headers: &ureq::http::HeaderMap) -> Option<Duration> {
    let number = |name: &str| {
        headers
            .get(name)?
            .to_str()
            .ok()?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0)
    };
    number("retry-after-ms")
        .map(|ms| Duration::from_secs_f64(ms / 1000.0))
        .or_else(|| number("retry-after").map(Duration::from_secs_f64))
}

fn transport_error(e: ureq::Error, cancel: &CancelToken) -> ProviderError {
    if cancel.is_cancelled() {
        return ProviderError::Cancelled;
    }
    match e {
        ureq::Error::Timeout(_) => ProviderError::Unavailable("timed out".into()),
        ureq::Error::BadUri(m) => ProviderError::Config(format!("invalid URL: {m}")),
        ureq::Error::Rustls(_) | ureq::Error::Tls(_) | ureq::Error::Pem(_) => {
            ProviderError::Rejected {
                status: None,
                message: format!("TLS error: {e}"),
            }
        }
        ureq::Error::HostNotFound => ProviderError::Unavailable("host not found".into()),
        ureq::Error::Io(io) => io_error(&io, cancel),
        other => ProviderError::Unavailable(other.to_string()),
    }
}

fn io_error(e: &io::Error, cancel: &CancelToken) -> ProviderError {
    if cancel.is_cancelled() {
        return ProviderError::Cancelled;
    }
    // ureq reports its own errors through io::Error when it is used as a reader.
    if let Some(inner) = e.get_ref().and_then(|i| i.downcast_ref::<ureq::Error>()) {
        if let ureq::Error::Timeout(_) = inner {
            return ProviderError::Unavailable("timed out".into());
        }
        return ProviderError::Unavailable(inner.to_string());
    }
    match e.kind() {
        io::ErrorKind::TimedOut => ProviderError::Unavailable("timed out".into()),
        _ => ProviderError::Unavailable(format!("connection error: {e}")),
    }
}

thread_local! {
    /// The cancel token of the request running on this thread. ureq calls the
    /// transport on the caller's thread, so the transport can see it.
    static ARMED: RefCell<Option<CancelToken>> = const { RefCell::new(None) };
}

struct Armed;

impl Armed {
    fn new(cancel: &CancelToken) -> Armed {
        ARMED.set(Some(cancel.clone()));
        Armed
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        ARMED.set(None);
    }
}

fn armed_and_cancelled() -> bool {
    ARMED.with_borrow(|t| t.as_ref().is_some_and(CancelToken::is_cancelled))
}

/// Makes a blocked socket read wake every [`CANCEL_POLL`] to look at the
/// cancel flag, so cancelling returns within that interval whether the server
/// is silent or mid-stream. Returning an error makes ureq close the socket.
#[derive(Debug)]
struct Interruptible<T: Transport>(T);

impl<T: Transport> Transport for Interruptible<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.0.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.0.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let started = Instant::now();
        loop {
            if armed_and_cancelled() {
                return Err(ureq::Error::Io(io::Error::other("request cancelled")));
            }
            let left = (*timeout.after).saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err(ureq::Error::Timeout(timeout.reason));
            }
            let slice = NextTimeout {
                after: UreqDuration::Exact(left.min(CANCEL_POLL)),
                ..timeout
            };
            match self.0.await_input(slice) {
                Err(ureq::Error::Timeout(_)) => {}
                Err(ureq::Error::Io(e))
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }

    fn is_open(&mut self) -> bool {
        self.0.is_open()
    }

    fn is_tls(&self) -> bool {
        self.0.is_tls()
    }
}

#[derive(Debug)]
struct InterruptConnector;

impl<In: Transport> Connector<In> for InterruptConnector {
    type Out = Interruptible<In>;

    fn connect(
        &self,
        _: &ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(Interruptible))
    }
}
