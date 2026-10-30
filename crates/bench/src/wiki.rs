//! Wikitext to prose paragraphs, for Wikinews and Wikipedia revisions.
//!
//! Keeps running text only: templates (except the `{{w|…}}` link template),
//! references, tables, files, categories, comments and HTML tags go; links
//! keep their label; lists, headings and everything from the first source or
//! reference section on are dropped.

/// Sections where the article's prose ends.
const END_SECTIONS: &[&str] = &[
    "sources",
    "source",
    "references",
    "external links",
    "related news",
    "see also",
    "notes",
    "quellen",
    "einzelnachweise",
    "weblinks",
    "literatur",
    "siehe auch",
    "anmerkungen",
    "werke",
    "schriften",
    "filmografie",
    "diskografie",
    "auszeichnungen",
];

/// Link prefixes whose target is not running text.
const NON_TEXT_LINKS: &[&str] = &[
    "file:",
    "image:",
    "datei:",
    "bild:",
    "category:",
    "kategorie:",
    "media:",
];

/// Prose paragraphs of a wikitext page, in order.
pub fn paragraphs(wikitext: &str) -> Vec<String> {
    let text = strip(wikitext);
    let mut out = Vec::new();
    let mut para = String::new();
    let flush = |para: &mut String, out: &mut Vec<String>| {
        let p = collapse_spaces(para.trim());
        if !p.is_empty() {
            out.push(p);
        }
        para.clear();
    };
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("==") {
            flush(&mut para, &mut out);
            let name = t.trim_matches('=').trim().to_lowercase();
            if END_SECTIONS.contains(&name.as_str()) {
                break;
            }
            continue;
        }
        if t.is_empty()
            || t.starts_with(['*', '#', ':', ';', '|', '!', '{', '}'])
            || t.starts_with("__")
        {
            flush(&mut para, &mut out);
            continue;
        }
        if !para.is_empty() {
            para.push(' ');
        }
        para.push_str(t);
    }
    flush(&mut para, &mut out);
    out
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c == ' ' || c == '\u{a0}' || c == '\t' {
            space = true;
        } else {
            if space && !out.is_empty() && !matches!(c, ',' | '.' | ';' | ':' | ')') {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

/// Removes markup, keeping line structure.
fn strip(s: &str) -> String {
    let s = remove_between(s, "<!--", "-->");
    let s = remove_refs(&s);
    let s = templates(&s);
    let s = tables(&s);
    let s = links(&s);
    let s = external_links(&s);
    let s = s.replace("'''", "").replace("''", "");
    let s = remove_element(&s, "gallery");
    let s = remove_element(&s, "math");
    let s = tags(&s);
    entities(&s)
}

fn remove_between(s: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(open) {
        out.push_str(&rest[..i]);
        match rest[i + open.len()..].find(close) {
            Some(j) => rest = &rest[i + open.len() + j + close.len()..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// `<ref …>…</ref>` and `<ref …/>`.
fn remove_refs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = find_ci(rest, "<ref") {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let Some(gt) = after.find('>') else {
            rest = "";
            break;
        };
        if after[..gt].ends_with('/') {
            rest = &after[gt + 1..];
            continue;
        }
        match find_ci(after, "</ref>") {
            Some(j) => rest = &after[j + "</ref>".len()..],
            None => rest = &after[gt + 1..],
        }
    }
    out.push_str(rest);
    out
}

fn remove_element(s: &str, name: &str) -> String {
    let (open, close) = (format!("<{name}"), format!("</{name}>"));
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = find_ci(rest, &open) {
        out.push_str(&rest[..i]);
        match find_ci(&rest[i..], &close) {
            Some(j) => rest = &rest[i + j + close.len()..],
            None => rest = "",
        }
    }
    out.push_str(rest);
    out
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    (0..h.len().saturating_sub(n.len() - 1)).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// Templates, nested. `{{w|Target|Label}}` and `{{w|Target}}` keep their
/// label; every other template is removed.
fn templates(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    let mut text_start = 0;
    while i + 1 < b.len() {
        if b[i] == b'{' && b[i + 1] == b'{' {
            let mut depth = 0;
            let mut j = i;
            while j + 1 < b.len() {
                if b[j] == b'{' && b[j + 1] == b'{' {
                    depth += 1;
                    j += 2;
                } else if b[j] == b'}' && b[j + 1] == b'}' {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            out.push_str(&s[text_start..i]);
            let inner = &s[i + 2..j.saturating_sub(2).max(i + 2)];
            let mut fields = inner.split('|');
            let name = fields.next().unwrap_or("").trim().to_lowercase();
            if name == "w" {
                // Positional parameters only: {{w|Target|anchor=…|Label}}.
                let mut positional = fields.filter(|f| !f.contains('='));
                let target = positional.next().unwrap_or("");
                out.push_str(templates(positional.next().unwrap_or(target)).trim());
            }
            i = j;
            text_start = j;
        } else {
            i += 1;
        }
    }
    out.push_str(&s[text_start.min(s.len())..]);
    out
}

/// `{| … |}` tables, nested.
fn tables(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0;
    for line in s.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("{|") {
            depth += 1;
        } else if t.starts_with("|}") && depth > 0 {
            depth -= 1;
            continue;
        }
        if depth == 0 {
            out.push_str(line);
        }
    }
    out
}

/// Internal links, nested (file captions hold links): `[[a|b]]` → `b`,
/// `[[a]]` → `a`, files and categories removed.
fn links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("[[") {
        out.push_str(&rest[..i]);
        let b = rest.as_bytes();
        let mut depth = 0;
        let mut j = i;
        while j + 1 < b.len() {
            if b[j] == b'[' && b[j + 1] == b'[' {
                depth += 1;
                j += 2;
            } else if b[j] == b']' && b[j + 1] == b']' {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    break;
                }
            } else {
                j += 1;
            }
        }
        let inner = &rest[i + 2..j.saturating_sub(2).max(i + 2)];
        let lower = inner.trim_start().to_lowercase();
        if !NON_TEXT_LINKS.iter().any(|p| lower.starts_with(p)) {
            let label = inner.rsplit_once('|').map_or(inner, |(_, l)| l);
            let label = if inner.contains('|') {
                label
            } else {
                inner.split('#').next().unwrap_or(inner)
            };
            out.push_str(label);
        }
        rest = &rest[j.min(rest.len())..];
    }
    out.push_str(rest);
    out
}

/// `[http://x label]` → `label`; `[http://x]` → nothing.
fn external_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("[http") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(end) = after.find(']') else {
            out.push_str(&rest[i..]);
            return out;
        };
        if let Some((_, label)) = after[..end].split_once(' ') {
            out.push_str(label.trim());
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Removes HTML tags, keeping their content.
fn tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let is_tag = after.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/');
        match after.find('>') {
            Some(end) if is_tag => rest = &after[end + 1..],
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let decoded = after.find(';').filter(|&e| e <= 8).and_then(|e| {
            let name = &after[..e];
            let c = match name {
                "nbsp" => ' ',
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "ndash" => '–',
                "mdash" => '—',
                "minus" => '−',
                "thinsp" | "ensp" | "emsp" => ' ',
                _ => {
                    let n = name.strip_prefix('#')?;
                    let code = match n.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok()?,
                        None => n.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &after[e + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wikinews_article() {
        let src = "{{Australia}}\n{{date|March 12, 2026}}\n[[File:Burwood.jpg|thumb|left|Burwood in 2020. {{Image source|MDRX}} [[Sydney]]]]\n\nThe government of [[New South Wales]] (NSW) announced plans to rezone {{W|Burwood, New South Wales|Burwood}} North.<ref>x</ref> It is near {{W|Parramatta}}.\n\nA '''second''' ''paragraph'' [http://example.com with a link] and [http://bare.example].\n\n{{haveyoursay}}\n== Sources ==\n* {{source|url=x}}\n";
        assert_eq!(
            paragraphs(src),
            [
                "The government of New South Wales (NSW) announced plans to rezone Burwood North. It is near Parramatta.",
                "A second paragraph with a link and."
            ]
        );
    }

    #[test]
    fn wikipedia_article() {
        let src = "{{Infobox Person\n| Name = {{lang|de|X}}\n}}\n'''Ann''' (* 1950 in [[Berlin]]) war eine [[Deutschland|deutsche]] Malerin.<ref name=\"a\" /> Sie lebte&nbsp;dort.\n\n== Leben ==\nSie studierte <small>Kunst</small>.<!-- note -->\n{| class=\"wikitable\"\n| cell\n|}\n* list item\n\n== Einzelnachweise ==\n<references />\n[[Kategorie:Maler]]\n";
        assert_eq!(
            paragraphs(src),
            [
                "Ann (* 1950 in Berlin) war eine deutsche Malerin. Sie lebte dort.",
                "Sie studierte Kunst."
            ]
        );
    }

    #[test]
    fn numeric_entities() {
        assert_eq!(
            entities("a&#45;b&#x2013;c &unknown; d"),
            "a-b–c &unknown; d"
        );
    }

    #[test]
    fn link_template_skips_named_parameters() {
        assert_eq!(
            templates("{{W|Charging_station|anchor=Direct_current_(DC)|DC}} chargers"),
            "DC chargers"
        );
        assert_eq!(templates("{{w|Sydney CBD}}"), "Sydney CBD");
    }
}
