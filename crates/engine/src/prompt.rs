//! Prompt segments for autocomplete.
//!
//! [`build`] assembles [`Segments`] from the focused field, the settings and
//! the optional context the user enabled. Every string passes the secret
//! filter first and is then cut to its cap on a char boundary, so a request
//! never carries more than the caps below allow (docs/quality.md, "Size of a
//! cloud autocomplete request"). Filtering runs before cutting so a cut can
//! never split a secret into an unrecognisable half. Text before the caret
//! keeps its end; everything else keeps its start, except conversation
//! messages, which keep their most recent part.

use std::borrow::Cow;

use crate::complete::Segments;
use crate::filter::redact;
use crate::io::{AppInfo, Author, Conversation, FieldSnapshot, Message};
use crate::settings::Settings;

/// Bytes of text before the caret, counted back from the caret.
pub const BEFORE_CAP: usize = 3 * 1024;
/// Bytes of text after the caret.
pub const AFTER_CAP: usize = 512;
/// Bytes of the user's about-me text.
pub const ABOUT_ME_CAP: usize = 1024;
/// Bytes of clipboard text.
pub const CLIPBOARD_CAP: usize = 1024;
/// Most recent sent messages used as style samples.
pub const STYLE_SAMPLES: usize = 8;
/// Bytes of each style sample.
pub const STYLE_SAMPLE_CAP: usize = 280;
/// Bytes of all conversation messages together, newest kept first.
pub const CONVERSATION_CAP: usize = 4 * 1024;
/// Bytes of one conversation message, keeping its end.
pub const MESSAGE_CAP: usize = 1024;
/// Bytes of a message author's name.
pub const AUTHOR_CAP: usize = 64;
/// Bytes of the window title.
pub const TITLE_CAP: usize = 256;
/// Bytes of the app id and of the app name.
pub const APP_NAME_CAP: usize = 128;

/// Context besides the field itself.
#[derive(Clone, Copy, Debug, Default)]
pub struct Context<'a> {
    /// The visible conversation; used when conversation context is on.
    pub conversation: Option<&'a Conversation>,
    /// The latest clipboard text; used when clipboard context is on and it
    /// holds no secret.
    pub clipboard: Option<&'a str>,
    /// The user's recent sent messages in this app, oldest first; used when
    /// style memory is on.
    pub style: &'a [String],
    /// Text the continuation must follow after `before`: the visible
    /// suggestion when Show More runs past the buffer.
    pub pending: &'a str,
}

/// Builds the prompt segments for a completion request in `field`.
pub fn build(field: &FieldSnapshot, settings: &Settings, context: &Context<'_>) -> Segments {
    let ctx = &settings.context;
    let before: Cow<'_, str> = if context.pending.is_empty() {
        Cow::Borrowed(&field.before)
    } else {
        Cow::Owned([field.before.as_str(), context.pending].concat())
    };
    let (before, cut) = filtered_end(&before, BEFORE_CAP);
    let clipboard = context
        .clipboard
        .filter(|c| ctx.clipboard && !c.trim().is_empty() && matches!(redact(c), Cow::Borrowed(_)))
        .map(|c| keep_start(c, CLIPBOARD_CAP).to_owned());
    let style = if ctx.style_memory {
        let recent = &context.style[context.style.len().saturating_sub(STYLE_SAMPLES)..];
        recent
            .iter()
            .map(|s| filtered_start(s, STYLE_SAMPLE_CAP))
            .filter(|s| !s.trim().is_empty())
            .collect()
    } else {
        Vec::new()
    };
    let conversation = match context.conversation {
        Some(c) if ctx.conversation => conversation(c),
        _ => Vec::new(),
    };
    Segments {
        app: AppInfo {
            id: filtered_start(&field.app.id, APP_NAME_CAP),
            name: filtered_start(&field.app.name, APP_NAME_CAP),
            title: filtered_start(&field.app.title, TITLE_CAP),
        },
        kind: field.kind,
        multiline: field.multiline,
        about_me: filtered_start(&ctx.about_me, ABOUT_ME_CAP),
        style,
        clipboard: clipboard.into_iter().collect(),
        conversation,
        after: filtered_start(&field.after, AFTER_CAP),
        before,
        before_truncated: field.before_truncated || cut,
    }
}

/// The most recent messages within [`CONVERSATION_CAP`], oldest first, each
/// filtered and cut to [`MESSAGE_CAP`] keeping its end.
pub fn conversation(conversation: &Conversation) -> Vec<Message> {
    let mut total = 0;
    let mut kept: Vec<Message> = Vec::new();
    for m in conversation.messages.iter().rev() {
        let (text, _) = filtered_end(&m.text, MESSAGE_CAP);
        if text.trim().is_empty() {
            continue;
        }
        total += text.len();
        if total > CONVERSATION_CAP && !kept.is_empty() {
            break;
        }
        let author = match &m.author {
            Author::Other(name) => Author::Other(filtered_start(name, AUTHOR_CAP)),
            a => a.clone(),
        };
        kept.push(Message { author, text });
    }
    kept.reverse();
    kept
}

/// At most the last `cap` bytes of `s`, and whether anything was cut.
///
/// The cut falls on a grid of `cap / 4` bytes counted from the start of `s`,
/// then moves past the next whitespace when one is that close, so it stays
/// put while text is appended: a local model's cache keeps the prompt prefix
/// instead of re-reading the whole segment on every keystroke. The kept part
/// is between three quarters of `cap` and `cap` long.
pub fn keep_end(s: &str, cap: usize) -> (&str, bool) {
    if s.len() <= cap {
        return (s, false);
    }
    let step = (cap / 4).max(1);
    let mut i = (s.len() - cap).div_ceil(step) * step;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    if let Some((at, c)) = s[i..].char_indices().find(|(_, c)| c.is_whitespace())
        && at < step
        && i + at + c.len_utf8() < s.len()
    {
        i += at + c.len_utf8();
    }
    (&s[i..], true)
}

/// The first `cap` bytes of `s`, ending on a char boundary.
pub fn keep_start(s: &str, cap: usize) -> &str {
    if s.len() <= cap {
        return s;
    }
    let mut i = cap;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

/// `s` filtered, then cut to `cap` keeping its start.
pub fn filtered_start(s: &str, cap: usize) -> String {
    keep_start(&redact(s), cap).to_owned()
}

/// `s` filtered, then cut to `cap` keeping its end, and whether it was cut.
pub fn filtered_end(s: &str, cap: usize) -> (String, bool) {
    let filtered = redact(s);
    let (kept, cut) = keep_end(&filtered, cap);
    (kept.to_owned(), cut)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::REDACTED;

    /// A made-up key, assembled at run time so the source never contains
    /// anything shaped like a real credential.
    struct Key;
    impl std::fmt::Display for Key {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("sk-")?;
            f.write_str("ant-api03-abcdefghijklmnopqrstuvwxyz0123")
        }
    }
    const KEY: Key = Key;

    fn field(before: &str, after: &str) -> FieldSnapshot {
        FieldSnapshot {
            before: before.into(),
            after: after.into(),
            app: AppInfo {
                id: "org.telegram.desktop".into(),
                name: "Telegram".into(),
                title: format!("chat {KEY}"),
            },
            ..FieldSnapshot::default()
        }
    }

    fn all_on() -> Settings {
        let mut s = Settings::default();
        s.context.clipboard = true;
        s.context.style_memory = true;
        s.context.about_me = format!("I am Anna. password={KEY}");
        s
    }

    fn texts(s: &Segments) -> Vec<&str> {
        let mut v = vec![
            s.app.id.as_str(),
            &s.app.name,
            &s.app.title,
            &s.about_me,
            &s.after,
            &s.before,
        ];
        v.extend(s.style.iter().map(String::as_str));
        v.extend(s.clipboard.iter().map(String::as_str));
        v.extend(s.conversation.iter().map(|m| m.text.as_str()));
        v
    }

    #[test]
    fn every_segment_is_filtered() {
        let convo = Conversation {
            messages: vec![Message {
                author: Author::Other(format!("bot {KEY}")),
                text: format!("use {KEY} now"),
            }],
        };
        let style = vec![format!("my key {KEY}")];
        let segments = build(
            &field(&format!("here: {KEY} and"), &format!(" then {KEY}")),
            &all_on(),
            &Context {
                conversation: Some(&convo),
                clipboard: Some("hello there"),
                style: &style,
                pending: "",
            },
        );
        for t in texts(&segments) {
            assert!(!t.contains(&KEY.to_string()), "{t}");
        }
        assert!(segments.before.contains(REDACTED));
        assert_eq!(segments.clipboard, ["hello there"]);
        assert_eq!(
            segments.conversation[0].author,
            Author::Other(format!("bot {REDACTED}"))
        );
    }

    #[test]
    fn clipboard_with_a_secret_is_dropped_entirely() {
        let key = KEY.to_string();
        let segments = build(
            &field("a", ""),
            &all_on(),
            &Context {
                clipboard: Some(&key),
                ..Context::default()
            },
        );
        assert!(segments.clipboard.is_empty());
    }

    #[test]
    fn optional_context_follows_the_settings() {
        let convo = Conversation {
            messages: vec![Message {
                author: Author::Me,
                text: "hi".into(),
            }],
        };
        let style = vec!["sent".to_owned()];
        let mut settings = Settings::default();
        settings.context.conversation = false;
        let ctx = Context {
            conversation: Some(&convo),
            clipboard: Some("clip"),
            style: &style,
            pending: "",
        };
        let segments = build(&field("a", ""), &settings, &ctx);
        assert!(
            segments.conversation.is_empty()
                && segments.clipboard.is_empty()
                && segments.style.is_empty()
        );
    }

    #[test]
    fn before_keeps_its_end_and_reports_the_cut() {
        let before = format!("{}tail end", "é".repeat(BEFORE_CAP));
        let segments = build(
            &field(&before, ""),
            &Settings::default(),
            &Context::default(),
        );
        assert!(segments.before.len() <= BEFORE_CAP);
        assert!(segments.before.ends_with("tail end"));
        assert!(segments.before_truncated);
        let segments = build(
            &field("short", ""),
            &Settings::default(),
            &Context {
                pending: " more",
                ..Context::default()
            },
        );
        assert_eq!(segments.before, "short more");
        assert!(!segments.before_truncated);
    }

    #[test]
    fn conversation_keeps_the_newest_messages_within_the_cap() {
        let messages = (0..20)
            .map(|i| Message {
                author: Author::Unknown,
                text: format!("{i} {}", "x".repeat(600)),
            })
            .collect();
        let kept = conversation(&Conversation { messages });
        let total: usize = kept.iter().map(|m| m.text.len()).sum();
        assert!(total <= CONVERSATION_CAP);
        assert!(kept.last().is_some_and(|m| m.text.starts_with("19 ")));
    }

    /// Regression: a cut that slid one char per keystroke made the local
    /// model re-read the whole before-segment every time.
    #[test]
    fn before_cut_stays_put_while_typing() {
        let mut text = "word ".repeat(BEFORE_CAP / 5 + 10);
        let start = |t: &str| t.len() - keep_end(t, BEFORE_CAP).0.len();
        let first = start(&text);
        for _ in 0..100 {
            text.push('x');
            assert_eq!(start(&text), first);
            assert!(keep_end(&text, BEFORE_CAP).0.len() <= BEFORE_CAP);
        }
    }

    mod props {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn caps_hold_on_char_boundaries(before in "[aé😀 ]{0,4000}", after in "[aé😀 ]{0,700}", about in "[aé😀 ]{0,1500}") {
                let mut settings = all_on();
                settings.context.about_me = about;
                let s = build(&field(&before, &after), &settings, &Context::default());
                prop_assert!(s.before.len() <= BEFORE_CAP && before.ends_with(&s.before));
                prop_assert!(s.after.len() <= AFTER_CAP && after.starts_with(&s.after));
                prop_assert!(s.about_me.len() <= ABOUT_ME_CAP);
            }
        }
    }
}
