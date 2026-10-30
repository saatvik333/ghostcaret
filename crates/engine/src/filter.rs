//! The secret filter. It runs on every prompt segment before a provider sees
//! it and on everything stored in style memory, and replaces each secret with
//! [`REDACTED`]:
//!
//! - provider keys and access tokens: `sk-…` (OpenAI, Anthropic `sk-ant-…`),
//!   `AIza…` (Google), GitHub `ghp_` `gho_` `ghu_` `ghs_` `ghr_` and
//!   `github_pat_`, Slack `xox?-`, AWS `AKIA…` and `ASIA…`, JSON Web Tokens,
//!   and the token after `Bearer`;
//! - the value in `password=`-style pairs (`password`, `passwd`, `passphrase`,
//!   `passcode`, `secret`, `token`, `api_key`, `apikey`, `api-key`,
//!   `private_key`, `access_key`, `secret_key`, also as the last part of a
//!   longer key such as `db_password` or `GITHUB_TOKEN`), with `=` or `:`,
//!   quoted or not;
//! - card numbers of 13 to 19 digits, plain or grouped with single spaces or
//!   hyphens, that pass the Luhn check.
//!
//! One pass over the bytes, no allocation unless something is found, and
//! redacting twice gives the same text as redacting once.

use std::borrow::Cow;

/// What a secret is replaced with.
pub const REDACTED: &str = "[redacted]";

/// `text` with every secret replaced by [`REDACTED`]; borrowed when there is
/// none.
pub fn redact(text: &str) -> Cow<'_, str> {
    let spans = find(text);
    if spans.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (s, e) in spans {
        out.push_str(&text[at..s]);
        out.push_str(REDACTED);
        at = e;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

/// True when `text` holds anything the filter would remove.
pub fn contains_secret(text: &str) -> bool {
    !find(text).is_empty()
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// End of the run of bytes from `i` that satisfy `pred`.
fn run(b: &[u8], i: usize, pred: impl Fn(u8) -> bool) -> usize {
    b[i.min(b.len())..]
        .iter()
        .position(|&c| !pred(c))
        .map_or(b.len(), |n| i + n)
}

fn starts_with_ignore_case(b: &[u8], i: usize, prefix: &[u8]) -> bool {
    b.len() >= i + prefix.len() && b[i..i + prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Bytes that can start a secret or end a key: the first letters of the
/// token prefixes and `bearer`, digits, `=` and `:`. Everything else is
/// skipped with one table lookup.
const TRIGGER: [bool; 256] = {
    let mut t = [false; 256];
    let starts = b"sAgxebB=:0123456789";
    let mut k = 0;
    while k < starts.len() {
        t[starts[k] as usize] = true;
        k += 1;
    }
    t
};

/// Sorted, merged byte spans of every secret in `text`; all on char
/// boundaries because every pattern starts and ends with ASCII.
fn find(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if !TRIGGER[usize::from(c)] {
            i += 1;
            continue;
        }
        let prev = if i == 0 { None } else { Some(b[i - 1]) };
        if c.is_ascii_digit() {
            if prev.is_none_or(|p| !p.is_ascii_digit()) {
                i = card(b, i, &mut spans);
            } else {
                i += 1;
            }
            continue;
        }
        let at_word = prev.is_none_or(|p| !is_token_byte(p));
        let found = match c {
            b'=' | b':' => pair(b, i),
            b'b' | b'B' if at_word => bearer(b, i),
            _ if at_word => token(b, i),
            _ => None,
        };
        match found {
            Some((s, e)) => {
                spans.push((s, e));
                i = e.max(i + 1);
            }
            None => i += 1,
        }
    }
    spans.sort_unstable();
    spans.dedup_by(|next, prev| {
        if next.0 <= prev.1 {
            prev.1 = prev.1.max(next.1);
            true
        } else {
            false
        }
    });
    spans
}

/// A provider key or access token starting at `i`.
fn token(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let rest = &b[i..];
    let after = |prefix: usize, min: usize, pred: fn(u8) -> bool| {
        let end = run(b, i + prefix, pred);
        (end - i - prefix >= min).then_some((i, end))
    };
    let alnum: fn(u8) -> bool = |c| c.is_ascii_alphanumeric();
    let b64url: fn(u8) -> bool = is_token_byte;
    match rest {
        [b's', b'k', b'-', ..] => after(3, 20, is_token_byte),
        [b'A', b'I', b'z', b'a', ..] => after(4, 30, is_token_byte),
        [b'A', b'K' | b'S', b'I', b'A', ..] => {
            let end = run(b, i + 4, |c| c.is_ascii_uppercase() || c.is_ascii_digit());
            (end - i == 20 && b.get(end).is_none_or(|&c| !c.is_ascii_alphanumeric()))
                .then_some((i, end))
        }
        _ if rest.starts_with(b"github_pat_") => {
            after(11, 20, |c| c.is_ascii_alphanumeric() || c == b'_')
        }
        [b'g', b'h', b'p' | b'o' | b'u' | b's' | b'r', b'_', ..] => after(4, 30, alnum),
        [
            b'x',
            b'o',
            b'x',
            b'a' | b'b' | b'p' | b'o' | b's' | b'r' | b'e',
            b'-',
            ..,
        ] => after(5, 10, |c| c.is_ascii_alphanumeric() || c == b'-'),
        [b'e', b'y', b'J', ..] => {
            let header = run(b, i, b64url);
            if header - i < 10 || b.get(header) != Some(&b'.') {
                return None;
            }
            let payload = run(b, header + 1, b64url);
            if payload - header - 1 < 10 || b.get(payload) != Some(&b'.') {
                return None;
            }
            Some((i, run(b, payload + 1, b64url)))
        }
        _ => None,
    }
}

/// The token after `Bearer `.
fn bearer(b: &[u8], i: usize) -> Option<(usize, usize)> {
    if !starts_with_ignore_case(b, i, b"bearer") {
        return None;
    }
    let s = run(b, i + 6, |c| c == b' ' || c == b'\t');
    if s == i + 6 {
        return None;
    }
    let body = run(b, s, |c| {
        c.is_ascii_alphanumeric() || b"-._~+/".contains(&c)
    });
    let e = run(b, body, |c| c == b'=');
    (body - s >= 8).then_some((s, e))
}

/// Key names whose value is secret, also matched as the last part of a longer
/// key (`db_password`).
const KEYS: &[&[u8]] = &[
    b"password",
    b"passwd",
    b"passphrase",
    b"passcode",
    b"secret",
    b"token",
    b"api_key",
    b"apikey",
    b"api-key",
    b"private_key",
    b"access_key",
    b"secret_key",
];

/// The value of a `key=value` or `key: value` pair whose separator is at `i`.
fn pair(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let ident = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.');
    let mut k = i;
    while k > 0 && b[k - 1] == b' ' {
        k -= 1;
    }
    if k > 0 && matches!(b[k - 1], b'"' | b'\'') {
        k -= 1;
    }
    let ks = b[..k].iter().rposition(|&c| !ident(c)).map_or(0, |p| p + 1);
    let key = &b[ks..k];
    let named = KEYS.iter().any(|name| {
        key.len() >= name.len()
            && key[key.len() - name.len()..].eq_ignore_ascii_case(name)
            && (key.len() == name.len()
                || matches!(key[key.len() - name.len() - 1], b'_' | b'-' | b'.'))
    });
    if !named {
        return None;
    }
    let mut s = run(b, i + 1, |c| c == b' ' || c == b'\t');
    let e = match b.get(s) {
        Some(&q @ (b'"' | b'\'')) => {
            s += 1;
            let close = b[s..]
                .iter()
                .position(|&c| c == q || c == b'\n')
                .map(|n| s + n);
            match close {
                Some(close) if b[close] == q => close,
                _ => run(b, s, |c| !c.is_ascii_whitespace()),
            }
        }
        _ => run(b, s, |c| {
            !c.is_ascii_whitespace() && !b",;&\"'<>)]}".contains(&c)
        }),
    };
    (e > s && !b[s..].starts_with(REDACTED.as_bytes())).then_some((s, e))
}

fn luhn(digits: impl DoubleEndedIterator<Item = u8>) -> bool {
    let sum: u32 = digits
        .rev()
        .enumerate()
        .map(|(n, d)| {
            let d = u32::from(d - b'0');
            if n % 2 == 1 {
                if d * 2 > 9 { d * 2 - 9 } else { d * 2 }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// Pushes every card number in the digit run starting at `i` (groups of
/// digits joined by single spaces or hyphens) and returns the run's end, so
/// the run is scanned once.
fn card(b: &[u8], i: usize, spans: &mut Vec<(usize, usize)>) -> usize {
    const MAX_GROUPS: usize = 32;
    let mut groups = [(0usize, 0usize); MAX_GROUPS];
    let mut digits_before = [0usize; MAX_GROUPS + 1];
    let mut n = 0;
    let mut at = i;
    loop {
        let end = run(b, at, |c| c.is_ascii_digit());
        groups[n] = (at, end);
        digits_before[n + 1] = digits_before[n] + end - at;
        n += 1;
        let joined = n < MAX_GROUPS
            && matches!(b.get(end), Some(b' ' | b'-'))
            && b.get(end + 1).is_some_and(u8::is_ascii_digit);
        if !joined {
            break;
        }
        at = end + 1;
    }
    let mut g = 0;
    while g < n {
        let card = (g..n).rev().find(|&last| {
            let window = &groups[g..=last];
            (13..=19).contains(&(digits_before[last + 1] - digits_before[g]))
                && luhn(window.iter().flat_map(|&(s, e)| b[s..e].iter().copied()))
        });
        match card {
            Some(last) => {
                spans.push((groups[g].0, groups[last].1));
                g = last + 1;
            }
            None => g += 1,
        }
    }
    groups[n - 1].1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up token, assembled at run time so the source never contains
    /// anything shaped like a real credential.
    fn fake(prefix: &str, body: &str) -> String {
        [prefix, body].concat()
    }

    #[test]
    fn removes_provider_keys_and_tokens() {
        let body = "abcdefghijklmnopqrstuvwxyz0123456789";
        let keys = [
            fake("sk-", &["proj-", body].concat()),
            fake("sk-", &["ant-api03-", body].concat()),
            fake("AI", &["za", body].concat()),
            fake("gh", &["p_", body].concat()),
            fake("github", &["_pat_", body].concat()),
            fake("xo", "xb-0000000000-abcdefghij"),
            fake("AK", "IAEXAMPLEEXAMPLE00"),
            fake("AS", "IAEXAMPLEEXAMPLE00"),
            fake(
                "ey",
                &["JhZWFkZXItaGVhZGVy.eyJwYXlsb2FkLXBheWxvYWQ.", body].concat(),
            ),
        ];
        for key in keys {
            let text = format!("my key is {key}, keep it safe");
            assert_eq!(
                redact(&text),
                format!("my key is {REDACTED}, keep it safe"),
                "{key}"
            );
        }
    }

    #[test]
    fn keeps_ordinary_words_that_look_like_prefixes() {
        for text in [
            "use sk-learn for this",
            "a task-specific model",
            "AKIA is a prefix",
            "the eyJ thing",
            "ghp_short",
            "bearer of bad news",
            "the password is long",
            "passwordHash=abc",
            "bypass=1",
            "PWD=/home/me",
            "call me at 555-1234",
            "order 1234567890123",
            "",
        ] {
            assert_eq!(redact(text), text);
            assert!(matches!(redact(text), Cow::Borrowed(_)));
        }
    }

    #[test]
    fn removes_bearer_tokens_but_keeps_the_scheme() {
        assert_eq!(
            redact("Authorization: Bearer abc.def-ghi_123=="),
            format!("Authorization: Bearer {REDACTED}")
        );
    }

    #[test]
    fn removes_values_of_secret_pairs() {
        let table = [
            ("password=hunter22", format!("password={REDACTED}")),
            (
                "Password: hunter22 thanks",
                format!("Password: {REDACTED} thanks"),
            ),
            (
                "DB_PASSWORD = 'p@ss word'",
                format!("DB_PASSWORD = '{REDACTED}'"),
            ),
            (
                "{\"api_key\": \"abc123\"}",
                format!("{{\"api_key\": \"{REDACTED}\"}}"),
            ),
            ("?apikey=XYZ&x=1", format!("?apikey={REDACTED}&x=1")),
            ("GITHUB_TOKEN=abc", format!("GITHUB_TOKEN={REDACTED}")),
        ];
        for (text, want) in table {
            assert_eq!(redact(text), want, "{text}");
        }
    }

    #[test]
    fn removes_card_numbers_that_pass_luhn() {
        assert_eq!(
            redact("card 4111 1111 1111 1111 exp"),
            format!("card {REDACTED} exp")
        );
        assert_eq!(
            redact("card 4111-1111-1111-1111"),
            format!("card {REDACTED}")
        );
        assert_eq!(redact("4111111111111111"), REDACTED);
        assert_eq!(
            redact("ref 12 4111 1111 1111 1111"),
            format!("ref 12 {REDACTED}")
        );
        assert_eq!(
            redact("4111111111111111 and 5500005555555559"),
            format!("{REDACTED} and {REDACTED}")
        );
        assert_eq!(
            redact("4111 1111 1111 1111 5500 0055 5555 5559"),
            format!("{REDACTED} {REDACTED}")
        );
        assert_eq!(redact("4111 1111 1111 1112"), "4111 1111 1111 1112");
    }

    mod props {
        use proptest::prelude::*;

        use super::*;

        fn noise() -> impl Strategy<Value = String> {
            proptest::string::string_regex("[ a-zA-Z0-9=:_\\-\"'.,/{}\n]{0,30}|.{0,20}")
                .expect("valid regex")
        }

        fn secret() -> impl Strategy<Value = (String, String)> {
            let key = "[A-Za-z0-9]{30,40}";
            prop_oneof![
                key.prop_map(|k| format!("sk-{k}"))
                    .prop_map(|s| (s.clone(), s)),
                key.prop_map(|k| format!("sk-ant-api03-{k}"))
                    .prop_map(|s| (s.clone(), s)),
                key.prop_map(|k| format!("ghp_{k}"))
                    .prop_map(|s| (s.clone(), s)),
                "[A-Z0-9]{16}"
                    .prop_map(|k| format!("AKIA{k}"))
                    .prop_map(|s| (s.clone(), s)),
                "[a-z]{8,16}".prop_map(|v| (format!("password={v}"), v)),
                "[a-z]{8,16}".prop_map(|v| (format!("Bearer {v}"), v)),
                proptest::collection::vec(0u8..10, 15).prop_map(|mut d| {
                    let check = (0..10)
                        .find(|&c| {
                            d.push(c);
                            let ok = luhn(d.iter().map(|x| x + b'0'));
                            d.pop();
                            ok
                        })
                        .unwrap();
                    d.push(check);
                    let s: String = d.iter().map(|x| char::from(x + b'0')).collect();
                    (s.clone(), s)
                }),
            ]
        }

        proptest! {
            #[test]
            fn never_panics_and_is_idempotent(t in ".{0,200}") {
                let once = redact(&t).into_owned();
                prop_assert_eq!(redact(&once), once.as_str());
            }

            #[test]
            fn noisy_text_is_idempotent(t in noise(), u in noise(), v in noise()) {
                let t = format!("{t}{u}{v}");
                let once = redact(&t).into_owned();
                prop_assert_eq!(redact(&once), once.as_str());
            }

            #[test]
            fn output_never_contains_an_embedded_secret((s, value) in secret(), a in noise(), z in noise()) {
                let text = format!("{a} {s} {z}");
                let out = redact(&text);
                prop_assert!(!out.contains(&value), "{} -> {}", text, out);
                prop_assert!(contains_secret(&text));
            }
        }
    }
}
