//! Just enough of mbox and MIME to read the plain-text body of list mail:
//! message splitting, folded headers, RFC 2047 encoded words, multipart
//! bodies, quoted-printable and base64, and UTF-8, ASCII and Latin-1
//! charsets. Also the cleaning that turns a reply into what its author
//! typed: quoted text, attribution lines and signatures removed, and
//! hard-wrapped lines joined.

/// One message with the headers the corpus needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mail {
    pub message_id: String,
    pub in_reply_to: String,
    pub from: String,
    pub subject: String,
    pub auto_submitted: bool,
    /// The decoded text/plain body; empty when there is none.
    pub body: String,
}

/// Splits an mbox into messages. A message starts at a line beginning with
/// `From ` at the start of the file or after a blank line.
pub fn split_mbox(mbox: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut at = 0;
    let mut blank_before = true;
    while at < mbox.len() {
        let end = mbox[at..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(mbox.len(), |i| at + i + 1);
        let line = &mbox[at..end];
        if blank_before && line.starts_with(b"From ") {
            starts.push(at);
        }
        blank_before = line == b"\n" || line == b"\r\n";
        at = end;
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let end = starts.get(i + 1).copied().unwrap_or(mbox.len());
            // Drop the separator line.
            let body = &mbox[s..end];
            let first = body
                .iter()
                .position(|&b| b == b'\n')
                .map_or(body.len(), |i| i + 1);
            &body[first..]
        })
        .collect()
}

/// Parses one message.
pub fn parse(raw: &[u8]) -> Mail {
    let (headers, body) = split_head(raw);
    let get = |name: &str| {
        header(&headers, name)
            .map(|v| decode_words(&v))
            .unwrap_or_default()
    };
    Mail {
        message_id: get("message-id").trim().to_owned(),
        in_reply_to: get("in-reply-to").trim().to_owned(),
        from: get("from"),
        subject: get("subject"),
        auto_submitted: header(&headers, "auto-submitted")
            .is_some_and(|v| !v.trim().eq_ignore_ascii_case("no")),
        body: plain_text(&headers, body),
    }
}

/// The display name of a `From` header, or the address when it has none.
pub fn display_name(from: &str) -> String {
    let name = match from.find('<') {
        Some(i) => from[..i].trim(),
        None => from.trim(),
    };
    let name = name.trim_matches('"').trim();
    let name = name
        .strip_suffix(" via dev")
        .or_else(|| name.strip_suffix(" via users"))
        .unwrap_or(name);
    if name.is_empty() {
        from.trim().to_owned()
    } else {
        name.to_owned()
    }
}

/// Unfolded `(lower-case name, value)` pairs and the body after them.
fn split_head(raw: &[u8]) -> (Vec<(String, String)>, &[u8]) {
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut at = 0;
    while at < raw.len() {
        let end = raw[at..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(raw.len(), |i| at + i + 1);
        let line = latin1_or_utf8(&raw[at..end]);
        let line = line.trim_end_matches(['\r', '\n']);
        at = end;
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some(last) = headers.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    (headers, &raw[at.min(raw.len())..])
}

fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.clone())
}

/// A header parameter such as `charset` or `boundary`.
fn param(value: &str, name: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().trim_matches('"').to_owned())
    })
}

/// The first text/plain part, decoded.
fn plain_text(headers: &[(String, String)], body: &[u8]) -> String {
    let ctype = header(headers, "content-type").unwrap_or_else(|| "text/plain".into());
    let mime = ctype
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if mime.starts_with("multipart/") {
        let Some(boundary) = param(&ctype, "boundary") else {
            return String::new();
        };
        for part in multipart(body, &boundary) {
            let (h, b) = split_head(part);
            let text = plain_text(&h, b);
            if !text.is_empty() {
                return text;
            }
        }
        return String::new();
    }
    if mime != "text/plain" {
        return String::new();
    }
    let encoding = header(headers, "content-transfer-encoding")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let bytes = match encoding.trim() {
        "quoted-printable" => quoted_printable(body, false),
        "base64" => base64(body),
        _ => body.to_vec(),
    };
    let charset = param(&ctype, "charset").unwrap_or_else(|| "us-ascii".into());
    let text = decode_charset(&bytes, &charset);
    if param(&ctype, "format").is_some_and(|f| f.eq_ignore_ascii_case("flowed")) {
        unflow(
            &text,
            param(&ctype, "delsp").is_some_and(|d| d.eq_ignore_ascii_case("yes")),
        )
    } else {
        text.replace("\r\n", "\n")
    }
}

/// Parts of a multipart body.
fn multipart<'a>(body: &'a [u8], boundary: &str) -> Vec<&'a [u8]> {
    let delim = format!("--{boundary}");
    let mut parts = Vec::new();
    let mut start: Option<usize> = None;
    let mut at = 0;
    while at < body.len() {
        let end = body[at..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(body.len(), |i| at + i + 1);
        let line = &body[at..end];
        let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
        let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
        if trimmed.starts_with(delim.as_bytes()) {
            if let Some(s) = start {
                parts.push(&body[s..at]);
            }
            if trimmed[delim.len()..].starts_with(b"--") {
                return parts;
            }
            start = Some(end);
        }
        at = end;
    }
    if let Some(s) = start {
        parts.push(&body[s..]);
    }
    parts
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Quoted-printable; `header` also maps `_` to space (RFC 2047 Q).
fn quoted_printable(s: &[u8], header: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'=' if s.get(i + 1) == Some(&b'\n') => i += 2,
            b'=' if s.get(i + 1) == Some(&b'\r') && s.get(i + 2) == Some(&b'\n') => i += 3,
            b'=' => match (
                s.get(i + 1).and_then(|&b| hex(b)),
                s.get(i + 2).and_then(|&b| hex(b)),
            ) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                }
                _ => {
                    out.push(b'=');
                    i += 1;
                }
            },
            b'_' if header => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

fn base64(s: &[u8]) -> Vec<u8> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for v in s.iter().filter_map(|&c| value(c)) {
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

fn decode_charset(bytes: &[u8], charset: &str) -> String {
    match charset.to_ascii_lowercase().as_str() {
        "iso-8859-1" | "latin1" | "iso-8859-15" | "windows-1252" | "cp1252" => {
            bytes.iter().map(|&b| char::from(b)).collect()
        }
        _ => latin1_or_utf8(bytes).into_owned(),
    }
}

/// UTF-8 when valid, otherwise Latin-1.
fn latin1_or_utf8(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    match std::str::from_utf8(bytes) {
        Ok(s) => std::borrow::Cow::Borrowed(s),
        Err(_) => std::borrow::Cow::Owned(bytes.iter().map(|&b| char::from(b)).collect()),
    }
}

/// RFC 2047 encoded words (`=?utf-8?q?…?=`, `=?utf-8?b?…?=`); whitespace
/// between adjacent encoded words is dropped.
pub fn decode_words(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    let mut last_encoded = false;
    while let Some(start) = rest.find("=?") {
        let decoded = rest[start + 2..]
            .split_once('?')
            .and_then(|(charset, tail)| {
                let (enc, tail) = tail.split_once('?')?;
                let end = tail.find("?=")?;
                let bytes = match enc {
                    "q" | "Q" => quoted_printable(&tail.as_bytes()[..end], true),
                    "b" | "B" => base64(&tail.as_bytes()[..end]),
                    _ => return None,
                };
                let used = start + 2 + charset.len() + 1 + enc.len() + 1 + end + 2;
                Some((decode_charset(&bytes, charset), used))
            });
        match decoded {
            Some((text, used)) => {
                let between = &rest[..start];
                if !(last_encoded && between.trim().is_empty()) {
                    out.push_str(between);
                }
                out.push_str(&text);
                rest = &rest[used..];
                last_encoded = true;
            }
            None => {
                out.push_str(&rest[..start + 2]);
                rest = &rest[start + 2..];
                last_encoded = false;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Joins format=flowed soft line breaks (RFC 3676).
fn unflow(text: &str, delsp: bool) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let line = line.strip_prefix(' ').unwrap_or(line);
        if line.ends_with(' ') && line != "-- " {
            out.push_str(if delsp { &line[..line.len() - 1] } else { line });
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Lines from this length on are taken as wrapped by the mail client.
const WRAPPED: usize = 60;

fn is_attribution(t: &str) -> bool {
    t.ends_with("wrote:")
        || t.ends_with("escribió:")
        || t.ends_with("schrieb:")
        || (t.starts_with("Am ") && t.contains(" schrieb ") && t.ends_with(':'))
}

/// Where the author's own text ends: the first quotation, attribution,
/// forwarded header, list footer or signature delimiter.
fn own_end(lines: &[&str]) -> usize {
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        let rule = t.len() >= 10 && (t.bytes().all(|b| b == b'-') || t.bytes().all(|b| b == b'_'));
        if t.starts_with('>')
            || is_attribution(t)
            || *line == "--"
            || t.starts_with("- - -")
            || t.starts_with("-----Original Message")
            || rule
            || t.starts_with("Sent from my")
            || (i > 0 && t.starts_with("From: "))
        {
            // A two-line attribution: "On Mon, … <" / "… wrote:".
            if is_attribution(t)
                && i > 0
                && lines[i - 1].trim_start().starts_with("On ")
                && !t.starts_with("On ")
            {
                return i - 1;
            }
            return i;
        }
    }
    lines.len()
}

/// Closing phrases after which only the sender's name is typed; what follows
/// the name is a signature block the mail client inserts.
const SIGN_OFFS: &[&str] = &[
    "thanks",
    "thank you",
    "regards",
    "best",
    "cheers",
    "kind regards",
    "best regards",
    "warm regards",
    "warmest regards",
    "many thanks",
    "sincerely",
    "all the best",
    "viele grüße",
    "gruß",
    "saludos",
];

fn is_sign_off(t: &str) -> bool {
    let t = t.trim_end_matches([',', '!', '.']).trim().to_lowercase();
    SIGN_OFFS.contains(&t.as_str())
}

/// Removes `<https://…>` and `<mailto:…>` annotations that plain-text
/// renderings add after link text.
fn strip_link_annotations(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('>') {
            Some(end) if after.starts_with("http") || after.starts_with("mailto:") => {
                rest = &after[end + 1..]
            }
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn is_list_item(t: &str) -> bool {
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    ["- ", "* ", "• "].iter().any(|m| t.starts_with(m))
        || (digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") ")))
}

/// Whether a line break after `line` came from the sender's mail client
/// wrapping the paragraph rather than from the sender: in a paragraph with a
/// line of [`WRAPPED`] characters or more, every break except after the end
/// of a sentence or clause, after a short line ending in a comma
/// ("Thanks,"), before a list item and after a short one.
fn soft_break(line: &str, next: &str) -> bool {
    !line.ends_with(['.', '!', '?', ':', ';'])
        && (line.len() >= WRAPPED || !line.ends_with(','))
        && !is_list_item(next)
        && !(is_list_item(line) && line.len() < WRAPPED)
}

/// What the author typed: the body up to the first quotation, attribution,
/// forwarded header, footer or signature, ending at the name after a
/// closing phrase, with link annotations removed, hard-wrapped lines joined
/// and paragraphs kept.
pub fn authored(body: &str) -> String {
    let lines: Vec<&str> = body.lines().map(str::trim_end).collect();
    let mut end = own_end(&lines);
    if let Some(close) = lines[..end].iter().rposition(|l| is_sign_off(l)) {
        let name = lines[close + 1..end]
            .iter()
            .position(|l| !l.trim().is_empty());
        end = name.map_or(close + 1, |n| close + 1 + n + 1);
    }
    let mut out = String::new();
    for para in lines[..end]
        .split(|l| l.trim().is_empty())
        .filter(|p| !p.is_empty())
    {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        let wrapped = para.iter().any(|l| l.len() >= WRAPPED);
        for (i, line) in para.iter().enumerate() {
            let t = line.trim();
            if i > 0 {
                let prev = para[i - 1].trim();
                out.push(if wrapped && soft_break(prev, t) {
                    ' '
                } else {
                    '\n'
                });
            }
            out.push_str(t);
        }
    }
    strip_link_annotations(&out.replace('\u{a0}', " "))
}

/// The message a reply quotes, as `(author, text)`: the `>` lines after an
/// attribution, or the text after an Outlook-style `From:`/`Sent:` block.
pub fn quoted(body: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = body.lines().map(str::trim_end).collect();
    let start = own_end(&lines);
    let first = lines.get(start)?.trim();
    if first.starts_with("From: ") || first.starts_with("-----Original Message") {
        let from = lines[start..]
            .iter()
            .find_map(|l| l.trim().strip_prefix("From: "))?;
        let blank = start + lines[start..].iter().position(|l| l.trim().is_empty())?;
        let text = authored(&lines[blank..].join("\n"));
        return (!text.is_empty()).then(|| (display_name(from), text));
    }
    let mut at = start;
    let mut attribution = String::new();
    if !first.starts_with('>') {
        // The attribution may span two lines.
        while let Some(l) = lines.get(at).filter(|_| at < start + 2) {
            attribution.push_str(l.trim());
            attribution.push(' ');
            at += 1;
            if is_attribution(l.trim()) {
                break;
            }
        }
        if !is_attribution(attribution.trim()) {
            // A signature or footer, not a quotation.
            return None;
        }
    }
    let mut unquoted: Vec<&str> = lines[at..]
        .iter()
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| l.starts_with('>') || l.trim().is_empty())
        .map(|l| {
            let l = l.strip_prefix('>').unwrap_or(l);
            l.strip_prefix(' ').unwrap_or(l)
        })
        .collect();
    // Apple Mail quotes its attribution: "> On …, Ann <a@b> wrote:".
    if attribution.is_empty()
        && let Some(i) = unquoted.iter().position(|l| !l.trim().is_empty())
        && is_attribution(unquoted[i].trim())
    {
        attribution = unquoted[i].to_owned();
        unquoted.drain(..=i);
    }
    let text = authored(&unquoted.join("\n"));
    (!text.is_empty()).then(|| (attribution_name(&attribution), text))
}

/// The sender's name in an attribution such as `On Wed, Sep 2, 2026 at
/// 1:39 PM Ann Lee <a@b.org> wrote:`: the words after the last one with a
/// digit (the date or time), without an AM/PM marker or the address.
fn attribution_name(line: &str) -> String {
    let mut s = line.trim();
    if let Some(i) = s.find('<') {
        s = &s[..i];
    }
    let s = s.trim().trim_end_matches(':').trim();
    let s = ["wrote", "escribió", "schrieb"]
        .iter()
        .fold(s, |s, w| s.strip_suffix(w).unwrap_or(s))
        .trim();
    let words: Vec<&str> = s.split_whitespace().collect();
    let from = words
        .iter()
        .rposition(|w| w.contains(|c: char| c.is_ascii_digit()))
        .map_or(0, |i| i + 1);
    let name: Vec<&str> = words[from..]
        .iter()
        .copied()
        .skip_while(|w| {
            matches!(
                w.trim_end_matches(','),
                "AM" | "PM" | "um" | "at" | "schrieb"
            )
        })
        .collect();
    display_name(name.join(" ").trim_matches(','))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_mbox_on_from_lines_after_blank_lines() {
        let mbox = b"From a@x Mon\nSubject: one\n\nbody\nFrom here, not a separator\n\nFrom b@x Tue\nSubject: two\n\nbody2\n";
        let msgs = split_mbox(mbox);
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].starts_with(b"Subject: one"));
        assert!(msgs[1].starts_with(b"Subject: two"));
    }

    #[test]
    fn folded_and_encoded_headers() {
        let raw = b"Subject: =?utf-8?q?=5BPR=5D_Add?=\n =?utf-8?q?_more?=\nFrom: \"Ann B via dev\" <a@b.org>\nMessage-ID: <1@x>\n\nhi\n";
        let m = parse(raw);
        assert_eq!(m.subject, "[PR] Add more");
        assert_eq!(display_name(&m.from), "Ann B");
        assert_eq!(m.message_id, "<1@x>");
        assert_eq!(m.body, "hi\n");
    }

    #[test]
    fn base64_encoded_word_and_body() {
        assert_eq!(decode_words("=?UTF-8?B?SGVsbG8gV8O2cmxk?="), "Hello Wörld");
        let raw = b"Content-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: base64\n\nSGkgdGhlcmU=\n";
        assert_eq!(parse(raw).body, "Hi there");
    }

    #[test]
    fn quoted_printable_body_in_multipart() {
        let raw = b"Content-Type: multipart/alternative; boundary=\"XX\"\n\n--XX\nContent-Type: text/plain; charset=\"utf-8\"\nContent-Transfer-Encoding: quoted-printable\n\nCaf=C3=A9 soft=\nbreak\n--XX\nContent-Type: text/html\n\n<p>no</p>\n--XX--\n";
        assert_eq!(parse(raw).body, "Café softbreak\n");
    }

    #[test]
    fn latin1_charset() {
        let raw = b"Content-Type: text/plain; charset=iso-8859-1\n\nna\xefve\n";
        assert_eq!(parse(raw).body, "naïve\n");
    }

    #[test]
    fn authored_text_stops_at_quotes_and_signatures() {
        let body = "Hi Ann,\n\nYes, it will be included in v4.4.0 as shown.\n\nThanks,\nLuke\n\nOn Wed, Sep 2, 2026 at 1:39 PM Ashish <\nx@y.org> wrote:\n\n> quoted\n";
        assert_eq!(
            authored(body),
            "Hi Ann,\n\nYes, it will be included in v4.4.0 as shown.\n\nThanks,\nLuke"
        );
        assert_eq!(authored("Sure.\n-- \nSally\nVP"), "Sure.");
        assert_eq!(authored("Ok\n> earlier"), "Ok");
        assert_eq!(authored("Ok\nFrom: Someone\nSent: today"), "Ok");
    }

    #[test]
    fn hard_wrapped_lines_are_joined_short_lines_kept() {
        let body = "I am running many Spark 3.3 batch processing jobs on YARN and want to\nknow how to size executors.\nThanks,\nGang";
        assert_eq!(
            authored(body),
            "I am running many Spark 3.3 batch processing jobs on YARN and want to know how to size executors.\nThanks,\nGang"
        );
    }

    #[test]
    fn flowed_text_is_joined() {
        let raw =
            b"Content-Type: text/plain; charset=utf-8; format=flowed\n\nOne soft \nline.\nNext\n";
        assert_eq!(parse(raw).body, "One soft line.\nNext\n");
    }

    #[test]
    fn signature_block_after_the_name_is_dropped() {
        let body = "I prefer the 11am slot.\n\nThanks!\n\nJeff Genender\nVice President RAI\nApache Software Foundation";
        assert_eq!(
            authored(body),
            "I prefer the 11am slot.\n\nThanks!\n\nJeff Genender"
        );
        // A thank-you that opens the message is not a closing phrase.
        assert_eq!(
            authored("Thanks, Alex and Jeff.\n\nWednesday works."),
            "Thanks, Alex and Jeff.\n\nWednesday works."
        );
    }

    #[test]
    fn list_footer_and_link_annotations_are_removed() {
        let body = "See the Release Plan<https://cwiki.example/x> and mail me<mailto:a@b>.\n\n—Rich\n---------------------------------------------------------------------\nTo unsubscribe, e-mail: dev-unsubscribe@x";
        assert_eq!(authored(body), "See the Release Plan and mail me.\n\n—Rich");
    }

    #[test]
    fn quoted_message_after_attribution() {
        let body = "Yes.\n\nOn Wed, Sep 2, 2026 at 1:39 PM Ashish Verma V via users <\nusers@kafka.apache.org> wrote:\n\n> Hi,\n>\n> Is my understanding correct?\n>\n> On Tue, Sep 1, 2026 Bob <b@x> wrote:\n>> older\n";
        assert_eq!(
            quoted(body),
            Some((
                "Ashish Verma V".into(),
                "Hi,\n\nIs my understanding correct?".into()
            ))
        );
        let apple = "Ok\n\n> On Sep 6, 2026, at 11:17 AM, Chia-Ping Tsai <c@x> wrote:\n> \n> Hi Alexander\n";
        // Apple Mail quotes the attribution too.
        assert_eq!(
            quoted(apple),
            Some(("Chia-Ping Tsai".into(), "Hi Alexander".into()))
        );
    }

    #[test]
    fn quoted_outlook_block() {
        let body = "Thanks\n\nFrom: Gergely H <g@x.com>\nSent: 31 August 2026 21:56\nTo: users@kafka\nSubject: Re: x\n\nThe fix ships in 4.4.0.\n";
        assert_eq!(
            quoted(body),
            Some(("Gergely H".into(), "The fix ships in 4.4.0.".into()))
        );
        assert_eq!(quoted("Just me.\n-- \nSig"), None);
    }

    #[test]
    fn attribution_names() {
        assert_eq!(
            attribution_name("On Mon, Aug 24, 2026, at 12:47, Direccion Inicia wrote:"),
            "Direccion Inicia"
        );
        assert_eq!(
            attribution_name("On Sep 6, 2026, at 11:17 AM, Chia-Ping Tsai <c@x> wrote:"),
            "Chia-Ping Tsai"
        );
        assert_eq!(
            attribution_name("On 2026/08/18 08:34:09 Alexander Neubeck wrote:"),
            "Alexander Neubeck"
        );
        assert_eq!(
            attribution_name("El dom, 19 ene 2025, 13:10, Mich T <m@x> escribió:"),
            "Mich T"
        );
        assert_eq!(
            attribution_name(
                "Am 7. September 2026 um 22:57 schrieb \"Mark Thomas\" <markt@apache.org>:"
            ),
            "Mark Thomas"
        );
    }

    #[test]
    fn german_attribution_ends_the_reply() {
        let body = "So it works with 9.0.122?\n\nAm 8. September 2026 um 16:21 schrieb \"Mark Thomas\" <m@a.org>:\n> It is in the notes.\n";
        assert_eq!(authored(body), "So it works with 9.0.122?");
        assert_eq!(
            quoted(body),
            Some(("Mark Thomas".into(), "It is in the notes.".into()))
        );
    }

    #[test]
    fn ragged_wrapping_is_joined_but_list_items_and_sign_offs_kept() {
        let body = "One design question that might come up in this thread or PR is:\nwhy do we need a new persisted state\ninstead of just changing what is_paused does? That would be simpler\nfor users.\n- one\n- two\nThanks,\nAmogh";
        assert_eq!(
            authored(body),
            "One design question that might come up in this thread or PR is:\nwhy do we need a new persisted state instead of just changing what is_paused does? That would be simpler for users.\n- one\n- two\nThanks,\nAmogh"
        );
        // Short lines only: every break is the sender's.
        assert_eq!(
            authored("Alina Kazi (Lead) PMC\nPushmina Kazi (Co-Lead)"),
            "Alina Kazi (Lead) PMC\nPushmina Kazi (Co-Lead)"
        );
    }
}
