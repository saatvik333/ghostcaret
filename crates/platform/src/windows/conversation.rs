//! The visible conversation above the focused field, rebuilt from the text
//! elements UI Automation finds on screen.

use oft_engine::io::{Author, Conversation, Message};

use super::place::Px;

/// Text of one on-screen element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub rect: Px,
    pub text: String,
    /// A list or data item: one whole message in most chat apps.
    pub row: bool,
}

/// Most bytes of conversation kept; the newest messages win.
pub const CONVERSATION_MAX: usize = 6000;

/// The messages above `field` and in its column, oldest first. Rows (list
/// items) are messages when the app names them; otherwise text elements
/// that sit close together are joined into one message.
pub fn conversation(items: Vec<Item>, field: Px) -> Conversation {
    let mut above: Vec<Item> = items
        .into_iter()
        .filter(|i| {
            let r = i.rect;
            !i.text.trim().is_empty()
                && r.w > 0
                && r.h > 0
                && r.bottom() <= field.y + field.h / 2
                && r.x < field.right()
                && r.right() > field.x
        })
        .collect();
    above.sort_by_key(|i| (i.rect.y, i.rect.x));
    let rows: Vec<&Item> = above.iter().filter(|i| i.row).collect();
    let texts: Vec<String> = if rows.is_empty() {
        grouped(&above)
    } else {
        rows.iter().map(|i| i.text.trim().to_owned()).collect()
    };
    let mut total = 0;
    let mut kept: Vec<Message> = texts
        .into_iter()
        .rev()
        .take_while(|t| {
            total += t.len();
            total <= CONVERSATION_MAX
        })
        .map(|text| Message {
            author: Author::Unknown,
            text,
        })
        .collect();
    kept.reverse();
    Conversation { messages: kept }
}

/// Joins text elements into blocks: the same line joins with a space, the
/// next line with a line break, and a gap of more than half a line starts a
/// new message.
fn grouped(items: &[Item]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut last: Option<Px> = None;
    for i in items {
        let r = i.rect;
        let text = i.text.trim();
        match last {
            Some(p) if r.y < p.bottom() => {
                let s = out.last_mut().expect("a block is open");
                s.push(' ');
                s.push_str(text);
            }
            Some(p) if r.y - p.bottom() <= p.h / 2 => {
                let s = out.last_mut().expect("a block is open");
                s.push('\n');
                s.push_str(text);
            }
            _ => out.push(text.to_owned()),
        }
        last = Some(match last {
            Some(p) if r.y < p.bottom() => Px {
                h: p.h.max(r.bottom() - p.y),
                ..p
            },
            _ => r,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(x: i32, y: i32, w: i32, text: &str) -> Item {
        Item {
            rect: Px { x, y, w, h: 20 },
            text: text.into(),
            row: false,
        }
    }

    const FIELD: Px = Px {
        x: 300,
        y: 900,
        w: 800,
        h: 40,
    };

    fn texts(c: &Conversation) -> Vec<&str> {
        c.messages.iter().map(|m| m.text.as_str()).collect()
    }

    #[test]
    fn nearby_text_joins_and_gaps_split_messages() {
        let c = conversation(
            vec![
                item(320, 500, 60, "Alice"),
                item(390, 500, 50, "10:02"),
                item(320, 522, 300, "are we still on for Tuesday?"),
                item(320, 600, 60, "Bob"),
                item(320, 622, 200, "yes, 3pm works"),
            ],
            FIELD,
        );
        assert_eq!(
            texts(&c),
            [
                "Alice 10:02\nare we still on for Tuesday?",
                "Bob\nyes, 3pm works"
            ]
        );
        assert!(c.messages.iter().all(|m| m.author == Author::Unknown));
    }

    #[test]
    fn sidebars_text_below_and_empty_elements_are_ignored() {
        let c = conversation(
            vec![
                item(10, 300, 200, "#general"),
                item(320, 950, 200, "Send"),
                item(320, 400, 200, "   "),
                item(320, 700, 200, "hello"),
            ],
            FIELD,
        );
        assert_eq!(texts(&c), ["hello"]);
    }

    #[test]
    fn named_rows_are_messages() {
        let mut rows = vec![
            item(320, 100, 500, "Alice: hi"),
            item(320, 200, 500, "Bob: hey"),
        ];
        rows.iter_mut().for_each(|r| r.row = true);
        rows.push(item(320, 110, 100, "hi"));
        let c = conversation(rows, FIELD);
        assert_eq!(texts(&c), ["Alice: hi", "Bob: hey"]);
    }

    #[test]
    fn the_newest_messages_fit_the_cap() {
        let long = "x".repeat(CONVERSATION_MAX / 2);
        let items = (0..5)
            .map(|i| item(320, 100 * i, 300, &format!("{i}{long}")))
            .collect();
        let c = conversation(items, FIELD);
        assert_eq!(c.messages.len(), 1);
        assert!(c.messages[0].text.starts_with('4'));
        assert!(c.messages.iter().map(|m| m.text.len()).sum::<usize>() <= CONVERSATION_MAX);
    }
}
