mod common;

use common::*;
use oft_cloud::SentLog;
use oft_engine::complete::{
    ChatMessage, CompleteError, Completer, GenerateRequest, Generator, Role,
};
use serde_json::json;

fn ev(name: &str, data: serde_json::Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn text_delta(text: &str) -> String {
    ev(
        "content_block_delta",
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
    )
}

fn start() -> String {
    ev(
        "message_start",
        json!({"type":"message_start","message":{"id":"m","role":"assistant","content":[]}}),
    ) + &ev(
        "content_block_start",
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
    ) + &ev("ping", json!({"type":"ping"}))
}

fn stop() -> String {
    ev(
        "content_block_stop",
        json!({"type":"content_block_stop","index":0}),
    ) + &ev(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":"stop_sequence"}}),
    ) + &ev("message_stop", json!({"type":"message_stop"}))
}

#[test]
fn request_shape_streaming_and_tag_handling_over_split_chunks() {
    let stream = start()
        + &text_delta("Sure <contin")
        + &text_delta("uation>ng t")
        + &text_delta("o</conti")
        + &text_delta("nuation> trailing")
        + &stop();
    let mock = Mock::script(vec![Reply::sse_split(&stream, &[11, 5, 64, 2])]);
    let log = SentLog::default();
    let p = anthropic(&mock, "claude-haiku-4-5-20251001", &log);
    let mut stop_rule = Never::default();

    let done = p
        .complete(&request("I am goi"), &mut stop_rule, &cancel())
        .unwrap();

    let req = &mock.requests()[0];
    assert_eq!(
        (req.method.as_str(), req.path.as_str()),
        ("POST", "/v1/messages")
    );
    assert_eq!(req.header("x-api-key"), Some(KEY));
    assert_eq!(req.header("anthropic-version"), Some("2023-06-01"));
    assert!(req.header("authorization").is_none());
    let body = req.json();
    assert_eq!(body["model"], "claude-haiku-4-5-20251001");
    assert_eq!(
        (body["max_tokens"].clone(), body["stream"].clone()),
        (json!(24), json!(true))
    );
    assert_eq!(body["stop_sequences"], json!(["</continuation>"]));
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({"type":"ephemeral"})
    );
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "no assistant prefill");
    assert_eq!(messages[0]["role"], "user");
    assert!(
        messages[0]["content"]
            .as_str()
            .unwrap()
            .ends_with("<before_caret>I am goi</before_caret>")
    );
    for absent in ["temperature", "top_p", "top_k", "output_config", "thinking"] {
        assert!(body.get(absent).is_none(), "{absent}");
    }

    let c = &done.candidates[0];
    assert_eq!(c.text, "ng to");
    assert!(c.token_probs.is_empty() && c.token_ends.is_empty());
    assert!(!p.caps().token_probs);
    assert_eq!(stop_rule.seen.last().unwrap().0, "ng to");
    assert!(stop_rule.seen.iter().all(|(_, prob)| prob.is_none()));
    assert_key_absent(&mock, &log);
}

#[test]
fn newest_models_get_low_effort_and_room_to_think_and_a_rejection_is_learned() {
    let mock = Mock::start(|req, _| {
        if req.json().get("output_config").is_some() {
            Reply::status(
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"output_config.effort: not supported for this model"}}"#,
            )
        } else {
            Reply::sse_split(
                &(start() + &text_delta("<continuation> ok</continuation>") + &stop()),
                &[1000],
            )
        }
    });
    let p = anthropic(&mock, "claude-opus-5-5", &SentLog::default());
    let done = p
        .complete(&request("a"), &mut Never::default(), &cancel())
        .unwrap();
    assert_eq!(done.candidates[0].text, " ok");
    let posts = mock.posts();
    assert_eq!(posts.len(), 2);
    let first = posts[0].json();
    assert_eq!(
        (first["output_config"].clone(), first["max_tokens"].clone()),
        (json!({"effort":"low"}), json!(24 + 1024))
    );
    let second = posts[1].json();
    assert!(second.get("output_config").is_none());
    assert_eq!(second["max_tokens"], json!(24));

    p.complete(&request("a"), &mut Never::default(), &cancel())
        .unwrap();
    assert!(
        mock.posts()[2].json().get("output_config").is_none(),
        "learned"
    );
}

#[test]
fn thinking_blocks_and_tool_blocks_are_not_text() {
    let stream = ev(
        "message_start",
        json!({"type":"message_start","message":{}}),
    ) + &ev(
        "content_block_start",
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
    ) + &ev(
        "content_block_delta",
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}),
    ) + &ev(
        "content_block_delta",
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"x"}}),
    ) + &text_delta("<continuation> fine</continuation>")
        + &stop();
    let mock = Mock::script(vec![Reply::sse(&[&stream])]);
    let p = anthropic(&mock, "claude-opus-5-5", &SentLog::default());
    assert_eq!(
        p.complete(&request("a"), &mut Never::default(), &cancel())
            .unwrap()
            .candidates[0]
            .text,
        " fine"
    );
}

#[test]
fn overload_is_transient_for_flow_draft_but_never_retried_for_autocomplete() {
    let overloaded = ev(
        "error",
        json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
    );
    let ok = start() + &text_delta("Draft") + &text_delta(" text") + &stop();
    let mock = Mock::script(vec![Reply::sse(&[&overloaded]), Reply::sse(&[&ok])]);
    let p = anthropic(&mock, "claude-haiku-4-5", &SentLog::default());
    let err = p
        .complete(&request("a"), &mut Never::default(), &cancel())
        .unwrap_err();
    assert_eq!(err, CompleteError::Unavailable("Overloaded".into()));
    assert_eq!(mock.posts().len(), 1);

    let mock = Mock::script(vec![Reply::sse(&[&overloaded]), Reply::sse(&[&ok])]);
    let p = anthropic(&mock, "claude-haiku-4-5", &SentLog::default());
    let request = GenerateRequest {
        id: 1,
        messages: vec![
            ChatMessage {
                role: Role::System,
                content: "be kind".into(),
            },
            ChatMessage {
                role: Role::User,
                content: "hi".into(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "hello".into(),
            },
            ChatMessage {
                role: Role::User,
                content: "again".into(),
            },
        ],
        max_tokens: 100,
        temperature: 0.9,
    };
    assert_eq!(
        p.generate(&request, &cancel(), &mut |_| {}).unwrap(),
        "Draft text"
    );
    let body = mock.posts()[1].json();
    assert_eq!(body["system"], "be kind");
    assert_eq!(
        body["messages"],
        json!([{"role":"user","content":"hi"},{"role":"assistant","content":"hello"},{"role":"user","content":"again"}])
    );
    assert!(
        body.get("temperature").is_none(),
        "sampling parameters are rejected by current models"
    );
}

#[test]
fn http_overload_529_and_rate_limits_are_retried_by_the_generator() {
    let ok = start() + &text_delta("done") + &stop();
    let mock = Mock::script(vec![
        Reply::status(
            529,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ),
        Reply::status(
            429,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow"}}"#,
        )
        .with_header("retry-after", "0"),
        Reply::sse(&[&ok]),
    ]);
    let p = anthropic(&mock, "claude-haiku-4-5", &SentLog::default());
    let request = GenerateRequest {
        id: 1,
        messages: vec![ChatMessage {
            role: Role::User,
            content: "hi".into(),
        }],
        max_tokens: 10,
        temperature: 0.0,
    };
    assert_eq!(
        p.generate(&request, &cancel(), &mut |_| {}).unwrap(),
        "done"
    );
    assert_eq!(mock.posts().len(), 3);
}

#[test]
fn model_list_follows_pagination_and_test_connection_reports_the_provider_text() {
    let mock = Mock::start(|req, _| {
        if req.path.starts_with("/v1/models") {
            if req.path.contains("after_id=claude-b") {
                Reply::json(
                    r#"{"data":[{"id":"claude-c","display_name":"Claude C"}],"has_more":false}"#,
                )
            } else {
                Reply::json(
                    r#"{"data":[{"id":"claude-a","display_name":"Claude A"},{"id":"claude-b","display_name":"Claude B"}],"has_more":true,"last_id":"claude-b"}"#,
                )
            }
        } else {
            Reply::status(
                401,
                r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
            )
        }
    });
    let p = anthropic(&mock, "claude-haiku-4-5", &SentLog::default());
    let models = p.list_models().unwrap();
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["claude-a", "claude-b", "claude-c"]
    );
    assert_eq!(models[2].name, "Claude C");
    assert_eq!(mock.requests()[0].header("x-api-key"), Some(KEY));
    assert!(
        p.test_connection()
            .unwrap_err()
            .to_string()
            .contains("invalid x-api-key")
    );
}

#[test]
fn anthropic_and_gemini_require_a_key() {
    use oft_engine::settings::{ProviderConfig, ProviderKind};
    for kind in [ProviderKind::Anthropic, ProviderKind::Gemini] {
        let config = ProviderConfig {
            id: "p".into(),
            label: "x".into(),
            kind,
            model: "m".into(),
        };
        for key in [None, Some("  ".to_owned())] {
            assert!(matches!(
                oft_cloud::provider(&config, key, SentLog::default()),
                Err(oft_cloud::ProviderError::Config(_))
            ));
        }
    }
}
