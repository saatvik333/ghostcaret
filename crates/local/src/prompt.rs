//! Prompt templates. Base models get labelled plain-text sections in the
//! stable-first order of docs/architecture.md ("Building a request"), with
//! the text before the caret last so the model continues it. Changing any
//! wording here changes model behaviour: bump [`TEMPLATE_VERSION`] so
//! benchmark results stay comparable.

use std::fmt::Write as _;

use oft_engine::complete::Segments;
use oft_engine::io::Author;

pub const TEMPLATE_VERSION: u32 = 1;

/// Where the parts of a rendered prompt end, as byte offsets into it.
#[derive(Debug, PartialEq, Eq)]
pub struct Layout {
    /// End of the stable sections, which change less often than the text.
    pub stable_end: usize,
    /// Start of the healed tail (the partial word at the caret); the prompt
    /// given to the model ends here.
    pub tail_start: usize,
}

/// Renders `segments` into `out` (cleared first).
pub fn render(segments: &Segments, out: &mut String) -> Layout {
    out.clear();
    let app = segments.app.name.trim();
    let title = segments.app.title.trim();
    match (app.is_empty(), title.is_empty()) {
        (false, false) => {
            let _ = write!(out, "Writing in {app} ({title}).\n\n");
        }
        (false, true) => {
            let _ = write!(out, "Writing in {app}.\n\n");
        }
        (true, false) => {
            let _ = write!(out, "Writing in {title}.\n\n");
        }
        (true, true) => {}
    }
    section(out, "About the writer", [segments.about_me.as_str()]);
    section(
        out,
        "Messages the writer sent earlier",
        segments.style.iter().map(String::as_str),
    );
    section(
        out,
        "Clipboard",
        segments.clipboard.iter().map(String::as_str),
    );
    if !segments.conversation.is_empty() {
        out.push_str("Conversation:\n");
        for m in &segments.conversation {
            let _ = writeln!(out, "{}: {}", author(&m.author), m.text.trim());
        }
        out.push('\n');
    }
    section(out, "Text after the cursor", [segments.after.as_str()]);
    let stable_end = out.len();
    if segments.conversation.is_empty() {
        out.push_str("Text:\n");
    } else {
        out.push_str("Me: ");
    }
    if segments.before_truncated {
        out.push('…');
    }
    let text_start = out.len();
    out.push_str(&segments.before);
    Layout {
        stable_end,
        tail_start: text_start + crate::heal::tail_start(&segments.before),
    }
}

fn section<'a>(out: &mut String, label: &str, items: impl IntoIterator<Item = &'a str>) {
    let mut items = items
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .peekable();
    if items.peek().is_none() {
        return;
    }
    let _ = writeln!(out, "{label}:");
    let mut first = true;
    for item in items {
        if !first {
            out.push_str("---\n");
        }
        first = false;
        out.push_str(item);
        out.push('\n');
    }
    out.push('\n');
}

fn author(author: &Author) -> &str {
    match author {
        Author::Me => "Me",
        Author::Other(name) if !name.trim().is_empty() => name.trim(),
        Author::Other(_) | Author::Unknown => "Someone",
    }
}

/// Transcript format for chat requests on models without a usable chat
/// template. Generation stops at [`PLAIN_STOP`].
pub fn plain_chat(messages: &[oft_engine::complete::ChatMessage]) -> String {
    use oft_engine::complete::Role;
    let mut out = String::new();
    for m in messages {
        let label = match m.role {
            Role::System => {
                out.push_str(m.content.trim());
                out.push_str("\n\n");
                continue;
            }
            Role::User => "User",
            Role::Assistant => "Assistant",
        };
        let _ = write!(out, "{label}: {}\n\n", m.content.trim());
    }
    out.push_str("Assistant:");
    out
}

pub const PLAIN_STOP: &str = "\nUser:";

#[cfg(test)]
mod tests {
    use super::*;
    use oft_engine::complete::Segments;
    use oft_engine::io::{AppInfo, Message};

    fn segments(before: &str) -> Segments {
        Segments {
            app: AppInfo {
                id: "slack".into(),
                name: "Slack".into(),
                title: "#general".into(),
            },
            about_me: "Saatvik, engineer".into(),
            conversation: vec![Message {
                author: Author::Other("Ana".into()),
                text: "Lunch today?".into(),
            }],
            after: String::new(),
            before: before.into(),
            ..Segments::default()
        }
    }

    #[test]
    fn stable_sections_come_first_and_the_text_last() {
        let mut out = String::new();
        let layout = render(&segments("Sure, see you at the caf"), &mut out);
        assert!(out.ends_with("Sure, see you at the caf"));
        assert_eq!(&out[layout.tail_start..], " caf");
        let stable = &out[..layout.stable_end];
        assert!(stable.contains("Saatvik") && stable.contains("Lunch today?"));
        assert!(!stable.contains("Sure"));
    }

    #[test]
    fn typing_inside_a_word_keeps_the_prompt_head() {
        let (mut a, mut b) = (String::new(), String::new());
        let la = render(&segments("see you at the c"), &mut a);
        let lb = render(&segments("see you at the caf"), &mut b);
        assert_eq!(a[..la.tail_start], b[..lb.tail_start]);
    }

    #[test]
    fn empty_sections_are_left_out() {
        let mut out = String::new();
        render(
            &Segments {
                before: "Hi".into(),
                ..Segments::default()
            },
            &mut out,
        );
        assert_eq!(out, "Text:\nHi");
    }

    #[test]
    fn appending_text_only_extends_the_prompt() {
        let (mut a, mut b) = (String::new(), String::new());
        let la = render(&segments("see you"), &mut a);
        let lb = render(&segments("see you at noon"), &mut b);
        assert!(b.starts_with(&a[..la.tail_start]));
        assert_eq!(la.stable_end, lb.stable_end);
    }
}
