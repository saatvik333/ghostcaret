mod common;

use common::*;
use ghostcaret_cloud::SentLog;
use ghostcaret_engine::complete::{
    ChatMessage, CompleteError, Completer, GenerateRequest, Generator, Role,
};
use serde_json::{Value, json};

const HALF: f64 = -std::f64::consts::LN_2;

fn event(text: &str, tokens: &[(&str, f64)]) -> String {
    let chosen: Vec<Value> = tokens
        .iter()
        .map(|(t, lp)| json!({"token":t,"tokenId":1,"logProbability":lp}))
        .collect();
    data(
        &json!({"candidates":[{"content":{"role":"model","parts":[{"text":text}]},"index":0,
            "logprobsResult":{"chosenCandidates":chosen,"topCandidates":[]}}],"usageMetadata":{}})
        .to_string(),
    )
}

#[test]
fn request_shape_header_key_and_logprob_mapping_over_split_chunks() {
    let stream = event("<continuation>", &[("<continuation>", 0.0)])
        + &event(" going", &[(" go", HALF), ("ing", -1.0)])
        + &event(
            " café</continuation>",
            &[(" caf", -0.0), ("é", HALF), ("</continuation>", 0.0)],
        );
    let mock = Mock::script(vec![Reply::sse_split(&stream, &[17, 4, 90, 3])]);
    let log = SentLog::default();
    let p = gemini(&mock, "gemini-2.5-flash", &log);
    let mut stop = Never::default();

    let done = p.complete(&request("I am"), &mut stop, &cancel()).unwrap();

    let req = &mock.requests()[0];
    assert_eq!(
        req.path,
        "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );
    assert_eq!(req.header("x-goog-api-key"), Some(KEY));
    let body = req.json();
    let config = &body["generationConfig"];
    assert_eq!(
        config["maxOutputTokens"],
        json!(24 + 256),
        "thinking tokens count against the limit"
    );
    assert_eq!(config["stopSequences"], json!(["</continuation>"]));
    assert_eq!(
        (
            config["responseLogprobs"].clone(),
            config["logprobs"].clone()
        ),
        (json!(true), json!(1))
    );
    assert_eq!(config["thinkingConfig"], json!({"thinkingBudget":0}));
    assert_eq!(body["contents"].as_array().unwrap().len(), 1);
    assert_eq!(body["contents"][0]["role"], "user");
    assert!(
        body["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("<continuation>")
    );

    let c = &done.candidates[0];
    assert_eq!(c.text, " going café");
    assert_eq!(c.token_ends, [3, 6, 10, 12]);
    let want = [0.5, (-1.0f32).exp(), 1.0, 0.5]; // the closing tag merges into the last token
    for (got, want) in c.token_probs.iter().zip(want) {
        assert!((got - want).abs() < 1e-5, "{got} != {want}");
    }
    assert!(p.caps().token_probs);
    assert_key_absent(&mock, &log);
}

#[test]
fn model_names_with_the_models_prefix_and_older_models_without_thinking_settings() {
    let mock = Mock::script(vec![Reply::sse(&[&event(
        "<continuation> x</continuation>",
        &[],
    )])]);
    let p = gemini(&mock, "models/gemini-2.0-flash", &SentLog::default());
    assert_eq!(
        p.complete(&request("a"), &mut Never::default(), &cancel())
            .unwrap()
            .candidates[0]
            .text,
        " x"
    );
    let req = &mock.requests()[0];
    assert_eq!(
        req.path,
        "/v1beta/models/gemini-2.0-flash:streamGenerateContent?alt=sse"
    );
    let config = &req.json()["generationConfig"];
    assert!(config.get("thinkingConfig").is_none());
    assert_eq!(config["maxOutputTokens"], json!(24));
}

#[test]
fn newer_models_use_a_thinking_level_and_the_default_temperature() {
    let mock = Mock::script(vec![Reply::sse(&[&event(
        "<continuation> x</continuation>",
        &[],
    )])]);
    let p = gemini(&mock, "gemini-3-flash-preview", &SentLog::default());
    p.complete(&request("a"), &mut Never::default(), &cancel())
        .unwrap();
    let config = &mock.requests()[0].json()["generationConfig"];
    assert_eq!(config["thinkingConfig"], json!({"thinkingLevel":"low"}));
    assert!(config.get("temperature").is_none());
}

#[test]
fn rejected_logprobs_and_thinking_settings_are_dropped_once_and_remembered() {
    let mock = Mock::start(|req, _| {
        let config = req.json()["generationConfig"].clone();
        if config.get("responseLogprobs").is_some() {
            return Reply::status(
                400,
                r#"{"error":{"code":400,"message":"Logprobs is not enabled for models/gemini-2.5-flash","status":"INVALID_ARGUMENT"}}"#,
            );
        }
        if config.get("thinkingConfig").is_some() {
            return Reply::status(
                400,
                r#"{"error":{"code":400,"message":"Thinking is not supported by this model","status":"INVALID_ARGUMENT"}}"#,
            );
        }
        Reply::sse(&[&event("<continuation> ok</continuation>", &[])])
    });
    let p = gemini(&mock, "gemini-2.5-flash", &SentLog::default());
    for _ in 0..2 {
        assert_eq!(
            p.complete(&request("a"), &mut Never::default(), &cancel())
                .unwrap()
                .candidates[0]
                .text,
            " ok"
        );
    }
    assert_eq!(
        mock.posts().len(),
        4,
        "two lessons once, then straight through"
    );
    assert!(!p.caps().token_probs);
}

#[test]
fn blocked_prompts_and_errors_are_reported_with_the_providers_words() {
    let mock = Mock::script(vec![Reply::sse(&[&data(
        r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#,
    )])]);
    let p = gemini(&mock, "gemini-2.5-flash", &SentLog::default());
    let err = p
        .complete(&request("a"), &mut Never::default(), &cancel())
        .unwrap_err();
    assert!(
        matches!(&err, CompleteError::Failed(m) if m.contains("SAFETY")),
        "{err:?}"
    );

    let mock = Mock::script(vec![Reply::status(
        400,
        r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT"}}"#,
    )]);
    let p = gemini(&mock, "gemini-2.5-flash", &SentLog::default());
    let err = p.test_connection().unwrap_err();
    assert!(err.to_string().contains("API key not valid"), "{err}");
}

#[test]
fn reply_maps_roles_and_retries_resource_exhaustion() {
    let mock = Mock::script(vec![
        Reply::status(
            429,
            r#"{"error":{"code":429,"message":"quota","status":"RESOURCE_EXHAUSTED"}}"#,
        )
        .with_header("retry-after", "0"),
        Reply::sse(&[&event("Hello", &[]), &event(" there", &[])]),
    ]);
    let p = gemini(&mock, "gemini-2.0-flash", &SentLog::default());
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
        temperature: 0.5,
    };
    let mut deltas = Vec::new();
    assert_eq!(
        p.generate(&request, &cancel(), &mut |d| deltas.push(d.to_owned()))
            .unwrap(),
        "Hello there"
    );
    assert_eq!(deltas, ["Hello", " there"]);
    let body = mock.posts()[1].json();
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be kind");
    let roles: Vec<_> = body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["user", "model", "user"]);
    assert_eq!(body["generationConfig"]["temperature"], json!(0.5));
    assert!(body["generationConfig"].get("responseLogprobs").is_none());
}

#[test]
fn model_list_filters_to_generating_models_and_follows_page_tokens() {
    let mock = Mock::start(|req, _| {
        if req.path.contains("pageToken=next%2F1") {
            Reply::json(
                r#"{"models":[{"name":"models/gemini-c","displayName":"Gemini C","supportedGenerationMethods":["generateContent"]}]}"#,
            )
        } else {
            Reply::json(
                r#"{"models":[{"name":"models/gemini-a","displayName":"Gemini A","supportedGenerationMethods":["generateContent"]},
                    {"name":"models/text-embedding","supportedGenerationMethods":["embedContent"]}],"nextPageToken":"next/1"}"#,
            )
        }
    });
    let p = gemini(&mock, "gemini-a", &SentLog::default());
    let models = p.list_models().unwrap();
    assert_eq!(
        models
            .iter()
            .map(|m| (m.id.as_str(), m.name.as_str()))
            .collect::<Vec<_>>(),
        [("gemini-a", "Gemini A"), ("gemini-c", "Gemini C")]
    );
    assert!(
        mock.requests().iter().all(|r| !r.path.contains(KEY)),
        "the key goes in a header, never the URL"
    );
}
