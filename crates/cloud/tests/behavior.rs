//! Behaviour shared by every provider, shown on the OpenAI-compatible one:
//! cancellation, stopping early, deadlines, limits and connection reuse.

mod common;

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use common::*;
use ghostcaret_cloud::SentLog;
use ghostcaret_engine::complete::{
    CancelToken, ChatMessage, CompleteError, Completer, GenerateRequest, Generator, Role, StopCheck,
};

fn raw_stream(texts: &[&str]) -> Reply {
    let pieces: Vec<String> = texts.iter().map(|t| raw_delta(t)).collect();
    Reply::sse(&pieces.iter().map(String::as_str).collect::<Vec<_>>())
}

/// Cancels when the first token arrives.
struct CancelOnFirst(CancelToken);

impl StopCheck for CancelOnFirst {
    fn should_stop(&mut self, _: &str, _: Option<f32>) -> bool {
        self.0.cancel();
        false
    }
}

#[test]
fn cancelling_while_the_server_is_silent_returns_promptly_and_drops_the_connection() {
    let mock = Mock::script(vec![Reply::Silent]);
    let p = openai(&mock, None, &SentLog::default());
    let token = cancel();
    let canceller = {
        let token = token.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            token.cancel();
        })
    };
    let started = Instant::now();
    let err = p
        .complete(&request("a"), &mut Never::default(), &token)
        .unwrap_err();
    canceller.join().unwrap();
    assert_eq!(err, CompleteError::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
    mock.wait_for("the server to see the hang-up", |m| m.hung_up() == 1);
}

#[test]
fn cancelling_between_chunks_stops_reading_and_drops_the_connection() {
    let token = cancel();
    let mock = Mock::script(vec![
        raw_stream(&["one", " two", " three"]).then(Part::HoldUntilClosed),
    ]);
    let p = openai(&mock, None, &SentLog::default());
    let err = p
        .complete(&request("a"), &mut CancelOnFirst(token.clone()), &token)
        .unwrap_err();
    assert_eq!(err, CompleteError::Cancelled);
    mock.wait_for("the server to see the hang-up", |m| m.hung_up() == 1);
}

#[test]
fn a_cancelled_request_is_not_sent() {
    let mock = Mock::script(vec![raw_stream(&["x"])]);
    let p = openai(&mock, None, &SentLog::default());
    let token = cancel();
    token.cancel();
    assert_eq!(
        p.complete(&request("a"), &mut Never::default(), &token)
            .unwrap_err(),
        CompleteError::Cancelled
    );
    assert!(mock.requests().is_empty());
}

#[test]
fn the_stop_rule_ends_generation_and_the_connection() {
    struct StopAtSecond(usize);
    impl StopCheck for StopAtSecond {
        fn should_stop(&mut self, _: &str, _: Option<f32>) -> bool {
            self.0 += 1;
            self.0 == 2
        }
    }
    let mock = Mock::script(vec![
        raw_stream(&["one", " two", " three", " four"]).then(Part::HoldUntilClosed),
    ]);
    let p = openai(&mock, None, &SentLog::default());
    let done = p
        .complete(&request("a "), &mut StopAtSecond(0), &cancel())
        .unwrap();
    assert_eq!(
        done.candidates[0].text, "one two",
        "text after the stopping chunk is dropped"
    );
    mock.wait_for("the server to see the hang-up", |m| m.hung_up() == 1);
}

#[test]
fn the_deadline_bounds_a_slow_provider() {
    let mock = Mock::script(vec![Reply::Silent]);
    let mut t = tuning(&mock);
    t.complete_deadline = Duration::from_millis(300);
    let p = openai_with(&mock, None, &SentLog::default(), t);
    let started = Instant::now();
    let err = p
        .complete(&request("a"), &mut Never::default(), &cancel())
        .unwrap_err();
    assert!(
        matches!(&err, CompleteError::Unavailable(m) if m.contains("timed out")),
        "{err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(mock.posts().len(), 1);
}

#[test]
fn the_client_rate_limit_applies_before_anything_is_sent() {
    let mock = Mock::script(vec![raw_stream(&[" ok"])]);
    let p = openai(&mock, None, &SentLog::default());
    p.set_rate_limit(2);
    for _ in 0..2 {
        p.complete(&request("a"), &mut Never::default(), &cancel())
            .unwrap();
    }
    let err = p
        .complete(&request("a"), &mut Never::default(), &cancel())
        .unwrap_err();
    assert_eq!(err, CompleteError::RateLimited);
    assert_eq!(
        mock.posts().len(),
        2,
        "the limited request never reached the server"
    );

    let draft = GenerateRequest {
        id: 1,
        messages: vec![ChatMessage {
            role: Role::User,
            content: "hi".into(),
        }],
        max_tokens: 5,
        temperature: 0.0,
    };
    let started = Instant::now();
    let err = p.generate(&draft, &cancel(), &mut |_| {}).unwrap_err();
    assert_eq!(
        err,
        CompleteError::RateLimited,
        "a slot would free after the deadline"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn warm_opens_one_connection_that_requests_then_reuse() {
    let mock = Mock::start(|req, _| {
        if req.method == "HEAD" {
            Reply::status(404, "")
        } else {
            raw_stream(&[" ok"])
        }
    });
    let p = openai(&mock, Some(KEY), &SentLog::default());
    p.warm();
    assert_eq!(mock.connections(), 1);
    let head = &mock.requests()[0];
    assert_eq!((head.method.as_str(), head.path.as_str()), ("HEAD", "/"));
    assert!(
        head.header("authorization").is_none(),
        "warming sends no credentials"
    );

    for _ in 0..3 {
        p.complete(&request("a"), &mut Never::default(), &cancel())
            .unwrap();
        p.prefill(&request("a"), &cancel());
    }
    assert_eq!(
        mock.connections(),
        1,
        "one connection served the warm-up and every request"
    );
    assert_eq!(
        mock.requests()
            .iter()
            .filter(|r| r.method == "HEAD")
            .count(),
        1,
        "a live connection needs no warming"
    );
}

#[test]
fn generator_cancel_during_backoff_returns_cancelled_without_resending() {
    let mock = Mock::script(vec![Reply::status(503, "down")]);
    let mut t = tuning(&mock);
    t.backoff = Duration::from_secs(5);
    let p = openai_with(&mock, None, &SentLog::default(), t);
    let token = cancel();
    let canceller = {
        let token = token.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            token.cancel();
        })
    };
    let draft = GenerateRequest {
        id: 1,
        messages: vec![ChatMessage {
            role: Role::User,
            content: "hi".into(),
        }],
        max_tokens: 5,
        temperature: 0.0,
    };
    let started = Instant::now();
    let result = p.generate(&draft, &token, &mut |_| {});
    canceller.join().unwrap();
    assert_eq!(result, Err(CompleteError::Cancelled));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(mock.posts().len(), 1);
}

#[test]
fn the_sent_log_is_bounded_and_never_holds_the_key() {
    let mock = Mock::script(vec![raw_stream(&[" ok"])]);
    let log = SentLog::new(3, 1 << 20);
    let p = openai(&mock, Some(KEY), &log);
    for i in 0..5 {
        p.complete(
            &request(&format!("text {i}")),
            &mut Never::default(),
            &cancel(),
        )
        .unwrap();
    }
    let entries = log.entries();
    assert_eq!(entries.len(), 3);
    assert!(entries[0].body.contains("text 2") && entries[2].body.contains("text 4"));
    assert!(
        entries
            .iter()
            .all(|e| e.provider == "Test" && e.model == "test-model" && e.at_ms > 0)
    );
    assert_key_absent(&mock, &log);

    let tight = SentLog::new(100, 200);
    let p = openai(&mock, Some(KEY), &tight);
    for _ in 0..4 {
        p.complete(
            &request(&"long ".repeat(10)),
            &mut Never::default(),
            &cancel(),
        )
        .unwrap();
    }
    let total: usize = tight.entries().iter().map(|e| e.body.len()).sum();
    assert!(total <= 200, "{total}");
}

#[test]
fn a_provider_is_shared_across_threads() {
    let mock = Mock::script(vec![raw_stream(&[" ok"])]);
    let p = openai(&mock, None, &SentLog::default());
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let p = Arc::clone(&p);
            thread::spawn(move || {
                p.complete(&request("a"), &mut Never::default(), &CancelToken::new())
                    .unwrap()
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap().candidates[0].text, " ok");
    }
}
