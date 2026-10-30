//! Tests against a real model. Ignored by default; run with
//! `OFT_TEST_MODEL=<manifest id or GGUF path> cargo test -p oft-local -- --ignored`.

use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, MutexGuard};

use oft_engine::complete::{
    CancelToken, ChatMessage, CompleteError, Completer, CompletionRequest, GenerateRequest,
    Generator, LengthMode, Role, Segments, StopCheck, StopParams,
};
use oft_engine::io::{AppInfo, FieldId};
use oft_local::store::ModelStore;
use oft_local::{LoadOptions, LocalModel};

fn model_path() -> PathBuf {
    let model = std::env::var("OFT_TEST_MODEL")
        .expect("set OFT_TEST_MODEL to a manifest id or a GGUF path");
    ModelStore::default_dir()
        .map(ModelStore::new)
        .and_then(|s| s.resolve(&model))
        .unwrap_or_else(|| PathBuf::from(model))
}

fn options() -> LoadOptions {
    LoadOptions {
        n_ctx: 2048,
        ..LoadOptions::default()
    }
}

/// One model shared by the tests, used by one test at a time so cache
/// expectations hold.
static MODEL: LazyLock<Mutex<LocalModel>> =
    LazyLock::new(|| Mutex::new(LocalModel::load(&model_path(), options()).expect("model loads")));

fn model() -> MutexGuard<'static, LocalModel> {
    MODEL.lock().unwrap_or_else(|p| p.into_inner())
}

struct NoStop;

impl StopCheck for NoStop {
    fn should_stop(&mut self, _: &str, _: Option<f32>) -> bool {
        false
    }
}

fn request(before: &str, alternatives: u8, max_tokens: u32) -> CompletionRequest {
    CompletionRequest {
        id: 1,
        field: FieldId(1),
        revision: 1,
        segments: Segments {
            app: AppInfo {
                id: "test".into(),
                name: "Notes".into(),
                title: String::new(),
            },
            before: before.into(),
            ..Segments::default()
        },
        stop: StopParams {
            mode: LengthMode::Auto,
            ceiling: LengthMode::Sentence,
            boldness: 0.5,
            paragraph_words: 60,
        },
        max_tokens,
        alternatives,
    }
}

fn complete(model: &LocalModel, before: &str, alternatives: u8, max_tokens: u32) -> Vec<String> {
    model
        .complete(
            &request(before, alternatives, max_tokens),
            &mut NoStop,
            &CancelToken::new(),
        )
        .expect("completion")
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect()
}

#[test]
#[ignore = "needs OFT_TEST_MODEL"]
fn continues_the_text() {
    let m = model();
    let text = &complete(&m, "The capital of France is", 0, 6)[0];
    assert!(text.contains("Paris"), "{text:?}");
}

#[test]
#[ignore = "needs OFT_TEST_MODEL"]
fn completes_a_partial_word_with_only_the_rest() {
    let m = model();
    let text = &complete(&m, "Let's meet for dinner at the new Italian restau", 0, 4)[0];
    assert!(text.starts_with("rant"), "{text:?}");
}

#[test]
#[ignore = "needs OFT_TEST_MODEL"]
fn probabilities_follow_the_text() {
    let m = model();
    let c = m
        .complete(
            &request("One, two, three, four,", 0, 6),
            &mut NoStop,
            &CancelToken::new(),
        )
        .unwrap();
    let main = &c.candidates[0];
    assert_eq!(main.token_ends.len(), main.token_probs.len());
    assert!(main.token_ends.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(main.token_ends.last().copied(), Some(main.text.len()));
    assert!(main.token_probs.iter().all(|p| (0.0..=1.0).contains(p)));
    assert!(
        main.token_probs[0] > 0.5,
        "counting is predictable: {main:?}"
    );
}

#[test]
#[ignore = "needs OFT_TEST_MODEL"]
fn typing_reuses_the_cache() {
    let m = model();
    let base = "Dear Anna,\n\nThank you for the lovely dinner last night. We should do it again";
    complete(&m, base, 0, 4);
    complete(&m, &format!("{base} s"), 0, 4);
    assert!(
        m.last_timings().decoded_tokens <= 2,
        "{:?}",
        m.last_timings()
    );
    complete(&m, &format!("{base} so"), 0, 4);
    assert_eq!(
        m.last_timings().decoded_tokens,
        0,
        "typing inside a word decodes nothing"
    );
}

#[test]
#[ignore = "needs OFT_TEST_MODEL"]
fn an_edit_gives_the_same_result_as_a_fresh_prompt() {
    let m = model();
    let edited = "My favourite fruits are apples, bananas and";
    let fresh = complete(&m, edited, 0, 6);
    complete(&m, "Unrelated text so the cache starts elsewhere.", 0, 2);
    complete(
        &m,
        "My favourite fruits are apples, bananas and cherries, which I eat every",
        0,
        6,
    );
    // Backspacing over the last words cuts the cache back.
    assert_eq!(complete(&m, edited, 0, 6), fresh);
}

#[test]
#[ignore = "needs OFT_TEST_MODEL"]
fn alternatives_are_distinct() {
    let m = model();
    let texts = complete(&m, "I'm sorry, but I can't make it to the", 2, 4);
    assert!(texts.len() >= 2 && texts.len() <= 3, "{texts:?}");
    for (i, a) in texts.iter().enumerate() {
        assert!(texts[i + 1..].iter().all(|b| a != b), "{texts:?}");
    }
}

#[test]
#[ignore = "needs OFT_TEST_MODEL"]
fn prefill_leaves_little_to_decode() {
    let m = model();
    let req = request(
        "Prefill reads the prompt while the user is still typing the next",
        0,
        4,
    );
    m.prefill(&req, &CancelToken::new());
    m.complete(&req, &mut NoStop, &CancelToken::new()).unwrap();
    assert!(
        m.last_timings().decoded_tokens <= 1,
        "{:?}",
        m.last_timings()
    );
}

#[test]
#[ignore = "needs OFT_TEST_MODEL"]
fn a_cancelled_request_stops() {
    let m = model();
    let cancel = CancelToken::new();
    cancel.cancel();
    let err = m
        .complete(&request("Anything at all", 0, 4), &mut NoStop, &cancel)
        .unwrap_err();
    assert_eq!(err, CompleteError::Cancelled);
}

#[test]
#[ignore = "needs OFT_TEST_MODEL"]
fn a_prompt_longer_than_the_context_is_trimmed() {
    let m = model();
    let long = "The quick brown fox jumps over the lazy dog. ".repeat(400);
    let text = &complete(&m, &long, 0, 4)[0];
    assert!(!text.is_empty());
}

#[test]
#[ignore = "needs OFT_TEST_MODEL"]
fn generation_streams_the_whole_result() {
    let m = model();
    let req = GenerateRequest {
        id: 7,
        messages: vec![
            ChatMessage {
                role: Role::System,
                content: "Reply in one short sentence.".into(),
            },
            ChatMessage {
                role: Role::User,
                content: "Are we still on for lunch tomorrow?".into(),
            },
        ],
        max_tokens: 24,
        temperature: 0.0,
    };
    let mut streamed = String::new();
    let text = m
        .generate(&req, &CancelToken::new(), &mut |d| streamed.push_str(d))
        .unwrap();
    assert!(!text.trim().is_empty());
    assert_eq!(streamed, text);
}

#[test]
#[ignore = "needs OFT_TEST_MODEL"]
fn unloading_makes_the_model_unavailable() {
    let m = LocalModel::load(&model_path(), options()).unwrap();
    assert!(m.is_loaded());
    m.unload();
    assert!(!m.is_loaded());
    let err = m
        .complete(&request("Hello", 0, 2), &mut NoStop, &CancelToken::new())
        .unwrap_err();
    assert!(matches!(err, CompleteError::Unavailable(_)));
}
