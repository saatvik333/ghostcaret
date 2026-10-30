//! The benchmark corpora: where each comes from, its license, the pinned
//! files with their SHA-256, and the fixed rule that turns them into samples.
//!
//! Files are downloaded into the data directory on first use and verified on
//! every load; nothing is committed. Samples are taken in a fixed order with
//! fixed counts, so every run replays the same text.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use oft_engine::io::{Author, Message};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use unicode_segmentation::UnicodeSegmentation;

use crate::mail::{self, Mail};
use crate::replay::Sample;
use crate::wiki;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Chat,
    Email,
    Document,
}

/// One pinned file of a corpus.
#[derive(Clone, Debug, Serialize)]
pub struct File {
    pub name: String,
    /// The page title, for wiki pages; empty otherwise.
    pub title: &'static str,
    pub url: String,
    pub sha256: &'static str,
}

/// A corpus as it is described in the report.
#[derive(Debug, Serialize)]
pub struct Spec {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: Kind,
    /// BCP 47 language tag.
    pub language: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
    pub source_url: &'static str,
    /// Why this corpus, and why its license allows this use.
    pub why: &'static str,
    /// The fixed rule that picks the samples.
    pub sampling: &'static str,
    /// The app the text is typed into: its id decides the category.
    pub app_id: &'static str,
    pub app_name: &'static str,
    #[serde(skip)]
    files: fn() -> Vec<File>,
    #[serde(skip)]
    parse: fn(&[File], &[Vec<u8>]) -> Vec<Sample>,
}

/// A loaded corpus.
#[derive(Debug)]
pub struct Corpus {
    pub spec: &'static Spec,
    pub files: Vec<File>,
    pub samples: Vec<Sample>,
}

pub static CORPORA: &[Spec] = &[
    Spec {
        id: "chat-en",
        name: "MultiWOZ 2.2 (test split, file 001)",
        kind: Kind::Chat,
        language: "en",
        license: "MIT",
        license_url: "https://raw.githubusercontent.com/budzianowski/multiwoz/fe0c8e65cfcd8462bd33c86e35f21addc84ca82b/LICENSE",
        source_url: "https://github.com/budzianowski/multiwoz",
        why: "Written chat between two people (a tourist and a clerk, Wizard-of-Oz collection) with every earlier turn available as the visible conversation. MIT permits any use. Task-oriented and public since 2018, so it is formulaic and likely in the models' training data: absolute savings are optimistic, comparisons between settings still hold.",
        sampling: "Dialogues sorted by id; the first 4 turns of each of the first 15 dialogues, both speakers, each typed with the earlier turns as the conversation (60 samples).",
        app_id: "slack",
        app_name: "Slack",
        files: || {
            vec![File {
                name: "dialogues_001.json".into(),
                title: "",
                url: "https://raw.githubusercontent.com/budzianowski/multiwoz/fe0c8e65cfcd8462bd33c86e35f21addc84ca82b/data/MultiWOZ_2.2/test/dialogues_001.json".into(),
                sha256: "d6f43876cf130fdb2dfa8f96bc056b0995354137f02122e004925d01264ed386",
            }]
        },
        parse: parse_multiwoz,
    },
    Spec {
        id: "email-en",
        name: "Apache Software Foundation public mailing lists, September 2026",
        kind: Kind::Email,
        language: "en",
        license: "ASF Public Forum Archive Policy: posts are \"deemed to be published by the sender of that communication and made public without conditions\". No license grant; read locally, never redistributed.",
        license_url: "https://www.apache.org/foundation/public-archives.html",
        source_url: "https://lists.apache.org/",
        why: "Real person-to-person email written after the models' training cutoff, with the message being answered (from the archive, or quoted in the reply) as the conversation. No openly licensed email corpus exists: Enron has no license and is memorized, AESLC is non-commercial, BC3 needs registration. The ASF publishes its lists without conditions.",
        sampling: "dev@community, users@kafka, users@tomcat and dev@airflow, September 2026, in mbox order; automated mail, votes, announcements, advisories, code and messages with links (links are pasted, not typed) skipped; the author's own text (quotes, attribution, footer and the signature block after the name removed, hard wraps joined) with 12 to 180 words; the first 8 per list (32 samples).",
        app_id: "thunderbird",
        app_name: "Thunderbird",
        files: || {
            [
                (
                    "dev",
                    "community.apache.org",
                    "6f3fbbc888ce3534058e6ca7e390eef0dbbddbea21fb004c4ab5ae7cdfac60ab",
                ),
                (
                    "users",
                    "kafka.apache.org",
                    "f1ab2e1e9422b571893cac77922abfe67ce7f9952486959c89fe54dd1ac9dd1c",
                ),
                (
                    "users",
                    "tomcat.apache.org",
                    "9fb1b331575169ee33a1b8c8aa34e119263b7df4d62185b4e7b282dfc006a46c",
                ),
                (
                    "dev",
                    "airflow.apache.org",
                    "dd148f6afdbac173d19cd72a3696a83e877bc4a63dc6ffbf68761696645dfe75",
                ),
            ]
            .into_iter()
            .map(|(list, domain, sha256)| File {
                name: format!("{list}@{domain}-2026-09.mbox"),
                url: format!(
                    "https://lists.apache.org/api/mbox.lua?list={list}&domain={domain}&d=2026-09"
                ),
                title: "",
                sha256,
            })
            .collect()
        },
        parse: parse_mail,
    },
    Spec {
        id: "document-en",
        name: "English Wikinews articles, January to April 2026",
        kind: Kind::Document,
        language: "en",
        license: "CC BY 4.0 (Wikinews content published after 2024-12-16)",
        license_url: "https://en.wikinews.org/wiki/Wikinews:Copyright",
        source_url: "https://en.wikinews.org/",
        why: "Edited news prose written after the models' training cutoff; Wikinews closed in May 2026, so the set is static. CC BY 4.0 permits any use with attribution. Each revision is pinned by its id.",
        sampling: "Published articles in revision-id order; per article the first prose paragraph of 30 or more words that follows another paragraph, cut at the last sentence end within 120 words, typed after the title and up to two earlier paragraphs; the first 20 articles that have one (20 samples).",
        app_id: "libreoffice-writer",
        app_name: "LibreOffice Writer",
        files: || {
            WIKINEWS
                .iter()
                .map(|&(oldid, title, sha256)| File {
                    name: format!("{oldid}.wiki"),
                    title,
                    url: format!("https://en.wikinews.org/w/index.php?oldid={oldid}&action=raw"),
                    sha256,
                })
                .collect()
        },
        parse: parse_wiki,
    },
    Spec {
        id: "document-de",
        name: "German Wikipedia articles created in October 2026",
        kind: Kind::Document,
        language: "de",
        license: "CC BY-SA 4.0 (Wikipedia text)",
        license_url: "https://foundation.wikimedia.org/wiki/Policy:Terms_of_Use",
        source_url: "https://de.wikipedia.org/",
        why: "Encyclopedic German prose written after the models' training cutoff, for a language besides English. CC BY-SA 4.0 permits any use with attribution; share-alike does not apply because nothing is redistributed. Articles flagged for deletion or quality review (one of them for suspected AI writing) were left out.",
        sampling: "The pinned revisions in the listed order; per article the same paragraph rule as document-en (16 samples at most).",
        app_id: "libreoffice-writer",
        app_name: "LibreOffice Writer",
        files: || {
            WIKIPEDIA_DE
                .iter()
                .map(|&(oldid, title, sha256)| File {
                    name: format!("{oldid}.wiki"),
                    title,
                    url: format!("https://de.wikipedia.org/w/index.php?oldid={oldid}&action=raw"),
                    sha256,
                })
                .collect()
        },
        parse: parse_wiki,
    },
];

/// English Wikinews revisions: `(oldid, title, SHA-256 of the raw
/// wikitext)`. The interview (5004394) and a two-sentence brief (4973182)
/// from the same period are left out.
const WIKINEWS: &[(u64, &str, &str)] = &[
    (
        4970464,
        "Keyboardist-guitarist of English rock band The Cure dies aged 65",
        "f92c0a8971519a546d03ec184f3f231bbe8ce8a32031d277f679a49aa50fd787",
    ),
    (
        4970466,
        "Residents shelter in place for hours after gas leak outside of Los Angeles",
        "706c840e3ad41281ae0c0ed7bd1459fd3c2717aca47b6060f2ba08dd72aab033",
    ),
    (
        4970954,
        "United States capture Venezuelan president, Nicolás Maduro",
        "14e7efaf0443c219e67f2b8d2e91fa0e2e909c47ab12e27d91d11600e963d78f",
    ),
    (
        4971655,
        "Kyiv street to be named for former Ukrainian parliament chairman Andriy Parubiy, killed in Lviv",
        "b34618590dc879e4b2b80054284807a41c6b8672f023c35f8b60893c543eb455",
    ),
    (
        4971656,
        "\"I came to be the manager ... not the coach\": Manchester United fire head coach Ruben Amorim",
        "7c06ce4688ab2a4d4f593814159f9c83f1fda6875b7091f9745699264de5a7d0",
    ),
    (
        4984990,
        "\"Media and Tenkil\" exhibit opens in Hanau, Germany",
        "47f00a842ecf3ac2af190199dfd56baee3f2595416e793dee2b3f43617ad3838",
    ),
    (
        4986401,
        "Poets gather for reading at Ray's Pizza in New York City",
        "577c0d8830a005ab45c341179a0d211e78dc465d0420e6a6fb9954a68b0ab5d4",
    ),
    (
        4995310,
        "Grateful Dead rhythm guitarist Bob Weir dies, aged 78",
        "93c6bca70cbfa31a5a6a5ff9afa58a11940b4e7c3ff8659b7e935a2aad1c996f",
    ),
    (
        4996365,
        "Moreton Bay illegally evicted homeless campers, Queensland Supreme Court rules",
        "4de85d58434fd9d1029973d51b38e9eb650778b5250d530b25cb033d14bcdf78",
    ),
    (
        4996366,
        "Monta publishes its 2025 EV charger ranking, including hardware performance",
        "6d63f201d51352fecdd9fd34561061df68ab3177e91ce00ba0c3d77e35ca4cbd",
    ),
    (
        4996970,
        "Australia grants refuge to five Iranian women's national soccer team players",
        "413ed15d115e217faba95ac507301e29c08044b7b0d5e86279b237814d0e817f",
    ),
    (
        4997309,
        "Caloundra lose round 2 game in Australian soccer's Football Queensland Premier League 2 competition",
        "cc38549cbe17d8783d4f7243ae39aacaac1a277a7e1ecd47d745e0a75f6e3869",
    ),
    (
        4997443,
        "NSW government proposes rezoning to create residences midway between Sydney and Parramatta",
        "7c435fdf181c7ff458f70958faa55ab8b678e46eeacf8dd6d52d585b5045ea78",
    ),
    (
        5000060,
        "Caloundra win round 3 game in Australian soccer's Football Queensland Premier League 2 competition",
        "b4fb408d32a634eb0992d83d3e791f8e22194223ac490ac4d9c47e97d2b241c3",
    ),
    (
        5000394,
        "Boats carrying Kyoto students capsize off Okinawa, fatalities reported",
        "62ab7bceae7838464b233e80c006204fe2172a192b84c18371a8ea830670c1ed",
    ),
    (
        5000996,
        "\"One Battle After Another\" wins 6 Oscars including Best Picture",
        "dfbb2fadb98533c0281a40b0b1bc288f8a327a9f8c0f82b1e82e958c7c6459b6",
    ),
    (
        5002943,
        "South Korean military halts small firearms drills after stray bullet strikes young girl on playground",
        "eafbea7a61abacda177a69a98d77b74c8ec411a4d3f2150c9675b286b5749e09",
    ),
    (
        5002944,
        "Musique Libre Femmes plays for Women's History Month at Tompkins Square Library in New York City",
        "a9e3b255c196ea4dfa2efb75422b0c3ec118e30fe754bce61fc7c7b4ae863b00",
    ),
    (
        5004376,
        "Homeland Security says Minneapolis woman shot by ICE officer dies",
        "cbdd3197e8b619e3c77dac1af31e4c01def6852237697b6c5c3a691276942b80",
    ),
    (
        5004383,
        "Boy retrieves Nintendo Switch locked away in gun safe, and shoots his father",
        "74d152e0067caff83cf66690092f03a601ee9ff2f7437f60e654d557363d7761",
    ),
    (
        5004396,
        "White House deletes Truth Social post portraying Obamas as apes",
        "2bdd14f9e50073ace2aca169baaa887176840017fba8c726fb523c74958897db",
    ),
    (
        5004397,
        "Caloundra through to next round of 2026 Australia Cup preliminaries",
        "2726c5d5d6d9832d0053dd0ff48add16f726852d8244d366e6d8caadbdc85ab5",
    ),
    (
        5004402,
        "Musical performance presented by Striped Light in Long Island City, New York showcases four performers",
        "2e90441b749ec5a7d185eadb03844f022a735326f7a79b215e468cb576c9b88f",
    ),
    (
        5004410,
        "Green Party wins major by-election in northern England city of Manchester",
        "09b1a5c7014fa9e7d0cf65fe6792c4783555f157cfb2a622599ee8deeefeb48c",
    ),
    (
        5005009,
        "Caloundra wins first game in Australian soccer's Football Queensland Premier League 2 competition",
        "1d3fefd096849e0596bab9fba5ac860af829175a235110562ee9f1dacb1f213f",
    ),
    (
        5005011,
        "US President Donald Trump appears to wear makeup after apparent rash breaks out on neck, reports say",
        "03d22e5398a1f86d7b9a67967d38d3404f5926d0ad0066ce8727cefe65043110",
    ),
    (
        5007001,
        "Australian Minister for Energy lowers fuel standards for 60 days amid rising oil prices, panic buying",
        "97740b60fd0f6bb404bc3200bb4a13324aab7b0beee306b912938a93e422ac24",
    ),
    (
        5007006,
        "Göbeklitepe, Taş Tepeler and life 12,000 years ago exhibition opens in Berlin, Germany",
        "816336517b4add8fd0c134fa3aa8e4dfe15f935cec0b00a43ca876fc824923c0",
    ),
    (
        5010345,
        "Max Scherzer signs deal to return to Toronto Blue Jays",
        "d49d12268e041ea76a4e75b9d35267dddc24332feaa14e80221c3dc8415e56c6",
    ),
    (
        5010348,
        "Randwick City Council opens public consultation about parking near beaches: ''To pay or not to pay?''",
        "9eaef5c3de17800eee55c5a4c659fcd500597dae2b79e6b94f854bccbe81dbf0",
    ),
    (
        5010746,
        "Roblox player uncovers cheating, awarded Guinness World Record",
        "75d8f80e5c353633eeda41367d2d1ea65517c328971369fbaaf9b3000638b03d",
    ),
    (
        5012079,
        "Pope Leo XIV visits four nations in Africa",
        "2751604ff6249a457e12754b7270e852693421e75b1e9e313a8e3a392142fcf0",
    ),
    (
        5013866,
        "Former Major League Baseball pitcher Julio Teherán retires",
        "c0090b350d67bd873276202f7deabe97f5809c6b381b3c06665c286a2b17c272",
    ),
    (
        5013889,
        "United States announces blockade on the Strait of Hormuz",
        "d543cb9acf12a4bea782f3d7094936215f5b1b187149e56f25b697dc922dc964",
    ),
    (
        5013890,
        "US president Donald Trump demands the unconditional surrender of Iran",
        "c9faaa7282d2e84c87f028432e5e53bef5a49e0a8c1cddd7e05054352bcbe843",
    ),
    (
        5013891,
        "PM of Australia sends E-7A Wedgetail and air-to-air missiles to UAE",
        "59e36f68f509d3464ff83b5af0123d70193a058327cd3db9ae283c51a902e51b",
    ),
];

/// German Wikipedia revisions: `(oldid, title, SHA-256 of the raw
/// wikitext)`.
const WIKIPEDIA_DE: &[(u64, &str, &str)] = &[
    (
        271730413,
        "Helge Hommes",
        "9f4777d4ea871a4c383bbe24993f65f78a44df26885d864692c82fd74ebdae64",
    ),
    (
        271692642,
        "Inge Mühlhofer",
        "bb61fa1c9178f6c6a57fe9671ca768f41b5783b0cc67ae558d467467b735796e",
    ),
    (
        271708342,
        "Flurnamenforschung in Thüringen",
        "09e0b04e9e2822dcc7c19552577de5e9dd1e0207600d493d943591f7d3cfc07d",
    ),
    (
        271730553,
        "Frank Wierke",
        "336c87d29a7e570a187c584b87eaab046cadcadf4bbab2d768d46885a27cf91c",
    ),
    (
        271694001,
        "Gaby Gyr",
        "c5ecc402833c5650a2b06bd0e08f9a237eb707840c40e34649f01decb359990c",
    ),
    (
        271723799,
        "Ute Nyssen",
        "335cb26a1888bd726e840d005bd9300158d0767dc9e0a00527a0bda0c5d19895",
    ),
    (
        271747824,
        "Jobst Bittner",
        "ef2f54bb932527cfb6ddd9d421314757a376578c96ead93c9472ad4df812d04a",
    ),
    (
        271740146,
        "Andreas Schaub",
        "2a9ee305407286d5c83817595e99224b74613978f5da276e87455824d7fd7388",
    ),
    (
        271698832,
        "Monica Menez",
        "a791bd73bb6c1935150bbae459a768719382c58062968be26dadf2d22b7768d5",
    ),
    (
        271695082,
        "Pangram",
        "b7fd959c7b79817059dc6dea83e37418c8b8f9724456d890d860cc120ef604f9",
    ),
    (
        271657540,
        "Wa und Aa",
        "5f8dcf237cc3cebbc309e186c69eb34f6f3e733fd0799a1fd82a7e902ad20124",
    ),
    (
        271651325,
        "Søilen Teater",
        "5630e11139a0beed97e99e6b0a72f186bbe49898dc37b6bb39eb25649fc6b340",
    ),
    (
        271681923,
        "Internationales Leipziger Festival für Dokumentar- und Animationsfilm 2026",
        "a46cbe87d4c5c7d431a55217ee5667fee63285a6cd504532b3365f2075e75029",
    ),
    (
        271714334,
        "Gebirge Venezuelas",
        "b4a27a5f545aa16317a83bd0618d7a83ebcc65a21942cd871da73d273177df03",
    ),
    (
        271683692,
        "Chūzan Seifu",
        "9dff4db036434e298b23f04eedaf31c586b7ea6f3b9a9dc9b0565e6b9f0487d8",
    ),
    (
        271687266,
        "Mojeño",
        "4452ce440bfc7b8dfa08eb9c6c6dccbbaa2e5d43d6258320cd05ae959b82f840",
    ),
];

pub fn spec(id: &str) -> Option<&'static Spec> {
    CORPORA.iter().find(|c| c.id == id)
}

/// Downloads what is missing, verifies every file, and builds the samples.
pub fn load(spec: &'static Spec, data_dir: &Path) -> Result<Corpus, String> {
    let dir = data_dir.join("corpora").join(spec.id);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let files = (spec.files)();
    let mut contents = Vec::with_capacity(files.len());
    for f in &files {
        contents.push(fetch(f, &dir.join(&f.name))?);
    }
    Ok(Corpus {
        spec,
        samples: (spec.parse)(&files, &contents),
        files,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The file's bytes, from disk when present and intact, else downloaded.
fn fetch(file: &File, path: &PathBuf) -> Result<Vec<u8>, String> {
    if let Ok(bytes) = fs::read(path) {
        if sha256_hex(&bytes) == file.sha256 {
            return Ok(bytes);
        }
        eprintln!("{}: checksum mismatch, downloading again", path.display());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(120)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .user_agent(concat!(
            "ghostcaret-bench/",
            env!("CARGO_PKG_VERSION"),
            " (https://github.com/saatvik333/ghostcaret)"
        ))
        .build()
        .into();
    eprintln!("downloading {}", file.url);
    let response = agent
        .get(&file.url)
        .call()
        .map_err(|e| format!("{}: {e}", file.url))?;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", file.url))?;
    let got = sha256_hex(&bytes);
    if got != file.sha256 {
        return Err(format!(
            "{}: SHA-256 is {got}, expected {}; the source changed, so the corpus must be re-pinned",
            file.url, file.sha256
        ));
    }
    let part = path.with_extension("part");
    fs::write(&part, &bytes).map_err(|e| format!("{}: {e}", part.display()))?;
    fs::rename(&part, path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(bytes)
}

fn words(s: &str) -> usize {
    s.unicode_words().count()
}

const CHAT_DIALOGUES: usize = 15;
const CHAT_TURNS: usize = 4;

fn parse_multiwoz(_: &[File], files: &[Vec<u8>]) -> Vec<Sample> {
    let Ok(Value::Array(mut dialogues)) = serde_json::from_slice::<Value>(&files[0]) else {
        return Vec::new();
    };
    let id = |d: &Value| d["dialogue_id"].as_str().unwrap_or("").to_owned();
    dialogues.sort_by_key(id);
    let mut samples = Vec::new();
    for d in dialogues.iter().take(CHAT_DIALOGUES) {
        let turns: Vec<(&str, &str)> = d["turns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| Some((t["speaker"].as_str()?, t["utterance"].as_str()?.trim())))
            .collect();
        for (k, &(speaker, text)) in turns.iter().enumerate().take(CHAT_TURNS) {
            let conversation = turns[..k]
                .iter()
                .map(|&(s, t)| Message {
                    author: if s == speaker {
                        Author::Me
                    } else {
                        Author::Other(if s == "USER" { "Customer" } else { "Clerk" }.into())
                    },
                    text: t.into(),
                })
                .collect();
            samples.push(Sample {
                id: format!("chat-en/{}/{k}", id(d)),
                title: String::new(),
                conversation,
                before: String::new(),
                text: text.into(),
            });
        }
    }
    samples
}

const MAILS_PER_LIST: usize = 8;
const MAIL_WORDS: std::ops::RangeInclusive<usize> = 12..=180;

/// Mail that people did not write themselves, or that pastes a template.
fn is_automated(m: &Mail) -> bool {
    let from = m.from.to_lowercase();
    let subject = m.subject.to_lowercase();
    m.auto_submitted
        || [
            "git@apache.org",
            "(via github)",
            "jira@",
            "jenkins",
            "notifications@",
            "noreply",
            "no-reply",
        ]
        .iter()
        .any(|s| from.contains(s))
        || [
            "[jira]",
            "[github]",
            "[vote]",
            "[result]",
            "[announce]",
            "cve-",
            "unsubscribe",
            "[pr]",
            "jenkins",
        ]
        .iter()
        .any(|s| subject.contains(s))
        || m.body.contains("This is an automated message")
}

/// Text with code, logs, configuration or links in it.
fn is_code_or_link(text: &str) -> bool {
    ["();", ");", "{", "}", " = ", "::", "=>", "->", "</", "://"]
        .iter()
        .any(|s| text.contains(s))
}

/// The message answered: the parent in the same archive, else the quotation
/// in the reply.
fn answered(m: &Mail, mails: &[Mail]) -> Option<Message> {
    let (author, text) = mails
        .iter()
        .find(|p| !m.in_reply_to.is_empty() && p.message_id == m.in_reply_to)
        .map(|p| (mail::display_name(&p.from), mail::authored(&p.body)))
        .filter(|(_, t)| !t.trim().is_empty())
        .or_else(|| mail::quoted(&m.body))?;
    Some(Message {
        author: Author::Other(if author.is_empty() {
            "Sender".into()
        } else {
            author
        }),
        text,
    })
}

fn parse_mail(_: &[File], files: &[Vec<u8>]) -> Vec<Sample> {
    let mut samples = Vec::new();
    for (f, mbox) in files.iter().enumerate() {
        let mails: Vec<Mail> = mail::split_mbox(mbox)
            .into_iter()
            .map(mail::parse)
            .collect();
        let mut taken = 0;
        for (i, m) in mails.iter().enumerate() {
            if taken == MAILS_PER_LIST {
                break;
            }
            let text = mail::authored(&m.body);
            if is_automated(m) || is_code_or_link(&text) || !MAIL_WORDS.contains(&words(&text)) {
                continue;
            }
            samples.push(Sample {
                id: format!("email-en/{f}/{i}"),
                title: m.subject.split_whitespace().collect::<Vec<_>>().join(" "),
                conversation: answered(m, &mails).into_iter().collect(),
                before: String::new(),
                text,
            });
            taken += 1;
        }
    }
    samples
}

const DOC_SAMPLES: usize = 20;
const DOC_MIN_WORDS: usize = 30;
const DOC_MAX_WORDS: usize = 120;
const DOC_CONTEXT_PARAGRAPHS: usize = 2;

fn parse_wiki(files: &[File], contents: &[Vec<u8>]) -> Vec<Sample> {
    let mut samples = Vec::new();
    for (f, raw) in files.iter().zip(contents) {
        if samples.len() == DOC_SAMPLES {
            break;
        }
        let text = String::from_utf8_lossy(raw);
        let title = f.title.to_owned();
        let paragraphs: Vec<String> = wiki::paragraphs(&text)
            .into_iter()
            .filter(|p| ends_sentence(p))
            .collect();
        let Some(k) = (1..paragraphs.len()).find(|&k| words(&paragraphs[k]) >= DOC_MIN_WORDS)
        else {
            continue;
        };
        let Some(body) = cut_words(&paragraphs[k], DOC_MAX_WORDS) else {
            continue;
        };
        let context = &paragraphs[k.saturating_sub(DOC_CONTEXT_PARAGRAPHS)..k];
        let mut before = String::new();
        if !title.is_empty() {
            before.push_str(&title);
            before.push_str("\n\n");
        }
        for p in context {
            before.push_str(p);
            before.push_str("\n\n");
        }
        samples.push(Sample {
            id: format!("doc/{}", f.name),
            title,
            conversation: Vec::new(),
            before,
            text: body,
        });
    }
    samples
}

fn ends_sentence(p: &str) -> bool {
    p.trim_end()
        .ends_with(['.', '!', '?', '"', '”', '“', '»', '«', ')'])
}

/// `p` up to its last sentence end within `max` words, or `None` when no
/// sentence ends there.
fn cut_words(p: &str, max: usize) -> Option<String> {
    if words(p) <= max {
        return Some(p.to_owned());
    }
    let mut end = None;
    for (i, _) in p.unicode_word_indices().take(max) {
        // A sentence end before word `i`.
        let head = p[..i].trim_end();
        if head.ends_with(['.', '!', '?']) {
            end = Some(head.len());
        }
    }
    end.map(|e| p[..e].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_words_keeps_whole_sentences() {
        assert_eq!(
            cut_words("One two. Three four five.", 10).as_deref(),
            Some("One two. Three four five.")
        );
        assert_eq!(
            cut_words("One two. Three four five.", 4).as_deref(),
            Some("One two.")
        );
        assert_eq!(cut_words("One two three four five.", 3), None);
    }

    #[test]
    fn automated_mail_is_skipped() {
        let bot = Mail {
            from: "x (via GitHub) <git@apache.org>".into(),
            ..Mail::default()
        };
        assert!(is_automated(&bot));
        let vote = Mail {
            subject: "[VOTE] Release 1.0".into(),
            ..Mail::default()
        };
        assert!(is_automated(&vote));
        let person = Mail {
            from: "Ann <a@b.org>".into(),
            subject: "Re: question".into(),
            ..Mail::default()
        };
        assert!(!is_automated(&person));
    }

    #[test]
    fn every_corpus_has_unique_ids_and_pinned_files() {
        let mut ids: Vec<_> = CORPORA.iter().map(|c| c.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), CORPORA.len());
        for c in CORPORA {
            for f in (c.files)() {
                assert_eq!(f.sha256.len(), 64, "{}", f.name);
                assert!(f.url.starts_with("https://"), "{}", f.url);
            }
        }
    }
}
