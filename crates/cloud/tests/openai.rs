mod common;

use common::*;
use oft_cloud::SentLog;
use oft_engine::complete::{
    ChatMessage, CompleteError, Completer, GenerateRequest, Generator, Role,
};
use serde_json::json;

const HALF: f64 = -std::f64::consts::LN_2;

fn assert_close(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-5, "{a} != {b}");
}

#[test]
fn raw_completion_request_shape_and_logprob_mapping_over_split_chunks() {
    // "é" is two bytes; the sizes below cut the stream inside events, lines and that character.
    let stream = format!(
        "{}{}data: [DONE]\n\n",
        data(&json!({"choices":[{"index":0,"text":"ng to","logprobs":{"tokens":["ng"," to"],"token_logprobs":[HALF, -1.0]}}]}).to_string()),
        data(&json!({"choices":[{"index":0,"text":" thé","logprobs":{"tokens":[" th","é"],"token_logprobs":[-0.0, HALF]}}]}).to_string()),
    );
    let mock = Mock::script(vec![Reply::sse_split(&stream, &[7, 3, 40, 1, 13])]);
    let log = SentLog::default();
    let p = openai(&mock, Some(KEY), &log);
    let mut stop = Never::default();

    let done = p
        .complete(&request("I am goi"), &mut stop, &cancel())
        .unwrap();

    let reqs = mock.requests();
    assert_eq!(
        (reqs[0].method.as_str(), reqs[0].path.as_str()),
        ("POST", "/v1/completions")
    );
    assert_eq!(
        reqs[0].header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
    let body = reqs[0].json();
    assert_eq!(body["model"], "test-model");
    assert!(body["prompt"].as_str().unwrap().ends_with("I am goi"));
    assert_eq!(
        (
            body["max_tokens"].clone(),
            body["stream"].clone(),
            body["logprobs"].clone()
        ),
        (json!(24), json!(true), json!(1))
    );
    assert_eq!(body["stop"], json!(["\n"]));
    assert!(body.get("messages").is_none());

    let c = &done.candidates[0];
    assert_eq!((done.id, c.text.as_str()), (7, "ng to thé"));
    assert_eq!(c.token_ends, [2, 5, 8, 10]);
    for (got, want) in c.token_probs.iter().zip([0.5, (-1.0f32).exp(), 1.0, 0.5]) {
        assert_close(*got, want);
    }
    assert_eq!(stop.seen.len(), 4, "the stop rule runs once per token");
    assert_eq!(stop.seen[1].0, "ng to");
    assert_close(stop.seen[1].1.unwrap(), (-1.0f32).exp());
    assert!(p.caps().token_probs && !p.caps().local);
    assert_eq!(log.entries().len(), 1);
    assert_eq!(
        log.entries()[0].body,
        reqs[0].body,
        "the log holds the exact body"
    );
    assert_key_absent(&mock, &log);
}

#[test]
fn llama_server_style_content_logprobs_work_on_completions() {
    let event = json!({"choices":[{"text":" there","logprobs":{"content":[
        {"token":" the","logprob":HALF,"bytes":[32,116,104,101]},{"token":"re","logprob":-0.0,"bytes":[114,101]}]}}]});
    let mock = Mock::script(vec![Reply::sse(&[
        &data(&event.to_string()),
        "data: [DONE]\n\n",
    ])]);
    let p = openai(&mock, None, &SentLog::default());
    let done = p
        .complete(&request("Hi"), &mut Never::default(), &cancel())
        .unwrap();
    let c = &done.candidates[0];
    assert_eq!(
        (c.text.as_str(), c.token_ends.clone()),
        (" there", vec![4, 6])
    );
    assert!(
        mock.requests()[0].header("authorization").is_none(),
        "no key, no auth header"
    );
}

#[test]
fn missing_completions_endpoint_falls_back_to_chat_and_is_remembered() {
    let mock = Mock::start(|req, _| {
        if req.path.ends_with("/chat/completions") {
            Reply::sse(&[
                &chat_delta("<continuation>"),
                &chat_delta(" there"),
                &chat_delta("</continuation>ignored"),
                "data: [DONE]\n\n",
            ])
        } else {
            Reply::status(404, r#"{"error":{"message":"Not Found"}}"#)
        }
    });
    let log = SentLog::default();
    let p = openai(&mock, Some(KEY), &log);
    for _ in 0..2 {
        let done = p
            .complete(&request("Hi"), &mut Never::default(), &cancel())
            .unwrap();
        assert_eq!(done.candidates[0].text, " there");
        assert!(
            done.candidates[0].token_probs.is_empty(),
            "no logprobs were sent"
        );
    }
    let paths: Vec<_> = mock.requests().into_iter().map(|r| r.path).collect();
    assert_eq!(
        paths,
        [
            "/v1/completions",
            "/v1/chat/completions",
            "/v1/chat/completions"
        ]
    );

    let body = mock.requests()[1].json();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["system", "user"]
    );
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .ends_with("<before_caret>Hi</before_caret>")
    );
    assert_eq!(body["stop"], json!(["</continuation>"]));
    assert_eq!(body["logprobs"], json!(true));
    assert_eq!(body["max_tokens"], json!(24));
    assert_key_absent(&mock, &log);
}

#[test]
fn chat_logprobs_map_through_the_continuation_tags() {
    let lp = |token: &str, p: f64| json!({"token":token,"logprob":p});
    let ev = |text: &str, tokens: Vec<serde_json::Value>| {
        data(&json!({"choices":[{"index":0,"delta":{"content":text},"logprobs":{"content":tokens}}]}).to_string())
    };
    let mock = Mock::start(move |req, _| {
        if req.path.ends_with("/chat/completions") {
            Reply::sse(&[
                &ev("<continuation>", vec![lp("<continuation>", 0.0)]),
                &ev(" going", vec![lp(" going", HALF)]),
                &ev("</continuation>", vec![lp("</continuation>", 0.0)]),
            ])
        } else {
            Reply::status(404, "nope")
        }
    });
    let p = openai(&mock, None, &SentLog::default());
    let mut stop = Never::default();
    let done = p.complete(&request("I am"), &mut stop, &cancel()).unwrap();
    let c = &done.candidates[0];
    assert_eq!((c.text.as_str(), c.token_ends.clone()), (" going", vec![6]));
    assert_close(c.token_probs[0], 0.5);
    assert_eq!(
        stop.seen.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
        ["", " going", " going"]
    );
}

#[test]
fn a_logprobs_rejection_is_learned_after_one_resend() {
    let mock = Mock::start(|req, _| {
        if req.path.ends_with("/completions") && !req.path.contains("chat") {
            return Reply::status(404, "nope");
        }
        if req.json().get("logprobs").is_some() {
            return Reply::status(
                400,
                r#"{"error":{"message":"logprobs is not supported for this model"}}"#,
            );
        }
        Reply::sse(&[
            &chat_delta("<continuation> ok</continuation>"),
            "data: [DONE]\n\n",
        ])
    });
    let p = openai(&mock, None, &SentLog::default());
    assert!(p.caps().token_probs);
    for _ in 0..2 {
        assert_eq!(
            p.complete(&request("a"), &mut Never::default(), &cancel())
                .unwrap()
                .candidates[0]
                .text,
            " ok"
        );
    }
    let posts = mock.posts();
    let shapes: Vec<_> = posts
        .iter()
        .map(|r| (r.path.as_str(), r.json().get("logprobs").is_some()))
        .collect();
    assert_eq!(
        shapes,
        [
            ("/v1/completions", true),
            ("/v1/chat/completions", true),
            ("/v1/chat/completions", false),
            ("/v1/chat/completions", false)
        ]
    );
    assert!(!p.caps().token_probs);
}

#[test]
fn the_output_limit_field_name_is_negotiated() {
    let mock = Mock::start(|req, _| {
        if !req.path.contains("chat") {
            return Reply::status(404, "nope");
        }
        if req.json().get("max_tokens").is_some() {
            return Reply::status(
                400,
                r#"{"error":{"message":"Unsupported parameter: 'max_tokens'. Use 'max_completion_tokens' instead."}}"#,
            );
        }
        Reply::sse(&[&chat_delta("<continuation> ok</continuation>")])
    });
    let p = openai(&mock, None, &SentLog::default());
    let done = p
        .complete(&request("a"), &mut Never::default(), &cancel())
        .unwrap();
    assert_eq!(done.candidates[0].text, " ok");
    let last = mock.posts().pop().unwrap().json();
    assert_eq!(last["max_completion_tokens"], json!(24));
    assert!(last.get("max_tokens").is_none());
}

#[test]
fn a_server_that_ignores_stream_still_works() {
    let mock = Mock::script(vec![Reply::json(
        r#"{"choices":[{"text":" world","logprobs":null,"finish_reason":"stop"}]}"#,
    )]);
    let p = openai(&mock, None, &SentLog::default());
    let done = p
        .complete(&request("Hello"), &mut Never::default(), &cancel())
        .unwrap();
    assert_eq!(done.candidates[0].text, " world");
}

#[test]
fn an_empty_continuation_is_no_candidate() {
    let mock = Mock::script(vec![Reply::sse(&[&raw_delta(" \n"), "data: [DONE]\n\n"])]);
    let p = openai(&mock, None, &SentLog::default());
    assert!(
        p.complete(&request("Hello"), &mut Never::default(), &cancel())
            .unwrap()
            .candidates
            .is_empty()
    );
}

#[test]
fn http_errors_map_to_engine_errors_and_autocomplete_never_retries() {
    type Check = fn(&CompleteError) -> bool;
    let cases: [(u16, &str, Check); 5] = [
        (
            401,
            r#"{"error":{"message":"Incorrect API key provided"}}"#,
            |e| matches!(e, CompleteError::Failed(m) if m.contains("Incorrect API key") && m.contains("401")),
        ),
        (429, r#"{"error":{"message":"slow down"}}"#, |e| {
            *e == CompleteError::RateLimited
        }),
        (
            500,
            r#"{"error":{"message":"boom"}}"#,
            |e| matches!(e, CompleteError::Unavailable(m) if m.contains("boom")),
        ),
        (
            503,
            "upstream down",
            |e| matches!(e, CompleteError::Unavailable(m) if m.contains("upstream down")),
        ),
        (
            400,
            r#"{"error":"context length exceeded"}"#,
            |e| matches!(e, CompleteError::Failed(m) if m.contains("context length")),
        ),
    ];
    for (status, body, check) in cases {
        let mock = Mock::script(vec![Reply::status(status, body)]);
        let p = openai(&mock, Some(KEY), &SentLog::default());
        let err = p
            .complete(&request("a"), &mut Never::default(), &cancel())
            .unwrap_err();
        assert!(check(&err), "{status}: {err:?}");
        assert_eq!(mock.posts().len(), 1, "{status}: autocomplete is sent once");
    }
}

#[test]
fn a_web_page_is_reported_as_a_wrong_base_url() {
    let mock = Mock::script(vec![
        Reply::json("<html></html>").with_header("content-type", "text/html"),
    ]);
    let p = openai(&mock, None, &SentLog::default());
    let err = p
        .complete(&request("a"), &mut Never::default(), &cancel())
        .unwrap_err();
    assert!(
        matches!(&err, CompleteError::Failed(m) if m.contains("base URL") && m.contains("text/html")),
        "{err:?}"
    );
}

#[test]
fn redirects_are_not_followed_with_the_key() {
    let mock = Mock::script(vec![
        Reply::status(307, "").with_header("location", "http://127.0.0.1:1/elsewhere"),
    ]);
    let p = openai(&mock, Some(KEY), &SentLog::default());
    let err = p
        .complete(&request("a"), &mut Never::default(), &cancel())
        .unwrap_err();
    assert!(
        matches!(&err, CompleteError::Failed(m) if m.contains("redirects")),
        "{err:?}"
    );
    assert_eq!(mock.posts().len(), 1);
}

#[test]
fn generator_streams_deltas_and_retries_429_then_503_then_succeeds() {
    let mock = Mock::script(vec![
        Reply::status(429, r#"{"error":{"message":"rate limited"}}"#)
            .with_header("retry-after", "0"),
        Reply::status(503, "overloaded"),
        Reply::sse(&[
            &chat_delta("Hel"),
            &chat_delta("lo"),
            &chat_delta(" there"),
            "data: [DONE]\n\n",
        ]),
    ]);
    let log = SentLog::default();
    let p = openai(&mock, Some(KEY), &log);
    let request = GenerateRequest {
        id: 1,
        messages: vec![
            ChatMessage {
                role: Role::System,
                content: "be brief".into(),
            },
            ChatMessage {
                role: Role::User,
                content: "say hi".into(),
            },
        ],
        max_tokens: 50,
        temperature: 0.7,
    };
    let mut deltas = Vec::new();
    let text = p
        .generate(&request, &cancel(), &mut |d| deltas.push(d.to_owned()))
        .unwrap();
    assert_eq!(
        (text.as_str(), deltas),
        (
            "Hello there",
            vec!["Hel".to_owned(), "lo".into(), " there".into()]
        )
    );
    assert_eq!(mock.posts().len(), 3);
    let body = mock.posts()[2].json();
    assert_eq!(
        body["messages"],
        json!([{"role":"system","content":"be brief"},{"role":"user","content":"say hi"}])
    );
    assert_eq!(
        (
            body["max_tokens"].clone(),
            body["temperature"].clone(),
            body["stream"].clone()
        ),
        (json!(50), json!(0.7), json!(true))
    );
    assert_eq!(
        body["temperature"].to_string(),
        "0.7",
        "no float noise in the logged body"
    );
    assert!(body.get("logprobs").is_none() && body.get("stop").is_none());
    assert_eq!(
        log.entries().len(),
        3,
        "every attempt that was sent is logged"
    );
    assert_key_absent(&mock, &log);
}

fn draft() -> GenerateRequest {
    GenerateRequest {
        id: 1,
        messages: vec![ChatMessage {
            role: Role::User,
            content: "hi".into(),
        }],
        max_tokens: 20,
        temperature: 0.5,
    }
}

#[test]
fn generator_gives_up_after_the_attempt_limit_and_never_retries_client_errors() {
    let mock = Mock::script(vec![Reply::status(503, "down")]);
    let p = openai(&mock, None, &SentLog::default());
    let err = p.generate(&draft(), &cancel(), &mut |_| {}).unwrap_err();
    assert!(matches!(err, CompleteError::Unavailable(_)), "{err:?}");
    assert_eq!(mock.posts().len(), 4);

    let mock = Mock::script(vec![Reply::status(
        401,
        r#"{"error":{"message":"bad key"}}"#,
    )]);
    let p = openai(&mock, Some(KEY), &SentLog::default());
    let err = p.generate(&draft(), &cancel(), &mut |_| {}).unwrap_err();
    assert!(
        matches!(&err, CompleteError::Failed(m) if m.contains("bad key")),
        "{err:?}"
    );
    assert_eq!(mock.posts().len(), 1);
}

#[test]
fn generator_does_not_wait_past_its_deadline_for_retry_after() {
    let mock = Mock::script(vec![
        Reply::status(429, "{}").with_header("retry-after", "3600"),
    ]);
    let p = openai(&mock, None, &SentLog::default());
    let started = std::time::Instant::now();
    let err = p.generate(&draft(), &cancel(), &mut |_| {}).unwrap_err();
    assert_eq!(err, CompleteError::RateLimited);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(mock.posts().len(), 1);
}

#[test]
fn generator_does_not_retry_after_text_was_delivered() {
    let mock = Mock::script(vec![Reply::sse(&[
        &chat_delta("Partial"),
        &data(r#"{"error":{"message":"server went away","type":"server_error"}}"#),
    ])]);
    let p = openai(&mock, None, &SentLog::default());
    let mut got = String::new();
    let err = p
        .generate(&draft(), &cancel(), &mut |d| got.push_str(d))
        .unwrap_err();
    assert!(matches!(err, CompleteError::Unavailable(_)), "{err:?}");
    assert_eq!((got.as_str(), mock.posts().len()), ("Partial", 1));
}

#[test]
fn generator_retries_a_stream_that_fails_before_any_text() {
    let mock = Mock::script(vec![
        Reply::sse(&[&data(
            r#"{"error":{"message":"busy","type":"server_error"}}"#,
        )]),
        Reply::sse(&[&chat_delta("fine"), "data: [DONE]\n\n"]),
    ]);
    let p = openai(&mock, None, &SentLog::default());
    assert_eq!(
        p.generate(&draft(), &cancel(), &mut |_| {}).unwrap(),
        "fine"
    );
    assert_eq!(mock.posts().len(), 2);
}

#[test]
fn list_models_returns_the_providers_list_and_test_connection_returns_its_error_text() {
    let mock = Mock::start(|req, _| match req.path.as_str() {
        "/v1/models" => Reply::json(
            r#"{"object":"list","data":[{"id":"gpt-a"},{"id":"gpt-b","name":"B"},{"id":"gpt-a"}]}"#,
        ),
        _ => Reply::status(
            401,
            r#"{"error":{"message":"Incorrect API key provided: sk-***"}}"#,
        ),
    });
    let p = openai(&mock, Some(KEY), &SentLog::default());
    let models = p.list_models().unwrap();
    assert_eq!(
        models
            .iter()
            .map(|m| (m.id.as_str(), m.name.as_str()))
            .collect::<Vec<_>>(),
        [("gpt-a", "gpt-a"), ("gpt-b", "B")]
    );
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
    let err = p.test_connection().unwrap_err();
    assert!(
        err.to_string().contains("Incorrect API key provided"),
        "{err}"
    );
}

#[test]
fn test_connection_succeeds_with_one_minimal_request() {
    let mock = Mock::start(|req, _| {
        if req.path.contains("chat") {
            Reply::sse(&[&chat_delta("ok"), "data: [DONE]\n\n"])
        } else {
            Reply::status(404, "no")
        }
    });
    openai(&mock, None, &SentLog::default())
        .test_connection()
        .unwrap();
    assert_eq!(mock.posts().last().unwrap().json()["max_tokens"], json!(16));
}

#[test]
fn a_key_is_refused_over_plain_http_to_public_hosts() {
    use oft_engine::settings::{ProviderConfig, ProviderKind};
    let config = ProviderConfig {
        id: "p".into(),
        label: "x".into(),
        kind: ProviderKind::OpenAiCompatible {
            base_url: "http://api.example.com/v1".into(),
        },
        model: "m".into(),
    };
    let err = oft_cloud::provider(&config, Some(KEY.into()), SentLog::default()).unwrap_err();
    assert!(err.to_string().contains("plain http"), "{err}");
    assert!(oft_cloud::provider(&config, None, SentLog::default()).is_ok());
}
