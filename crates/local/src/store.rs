//! The curated model manifest and the store of downloaded models.
//!
//! A download goes to `<file>.part`, resumes from its end with an HTTP Range
//! request, is checked against the manifest's SHA-256, and only then is
//! renamed to its final name. A file under its final name is therefore
//! always verified.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use oft_engine::complete::CancelToken;
use sha2::{Digest, Sha256};

/// Which slot of the product a model fills; provisional until the benchmark
/// picks the small and the standard model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tier {
    Small,
    Standard,
    /// A pure-attention model kept for benchmark comparison.
    Comparison,
}

/// One downloadable model. URLs pin the repository revision, so the file
/// behind a URL never changes under its hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub file_name: &'static str,
    pub size: u64,
    /// Lower-case hex SHA-256 of the file (the Hugging Face LFS oid).
    pub sha256: &'static str,
    pub license: &'static str,
    /// Least system memory to offer the model with.
    pub min_ram_gb: u32,
    pub tier: Tier,
}

macro_rules! hf {
    ($repo:literal, $rev:literal, $file:literal) => {
        concat!(
            "https://huggingface.co/",
            $repo,
            "/resolve/",
            $rev,
            "/",
            $file
        )
    };
}

static CURATED: &[ModelEntry] = &[
    ModelEntry {
        id: "qwen3.5-0.8b-base-q4_k_m",
        name: "Qwen3.5 0.8B Base",
        url: hf!(
            "mradermacher/Qwen3.5-0.8B-Base-i1-GGUF",
            "0adcee13f1a32c38fdbdb93e21d57e76b5640836",
            "Qwen3.5-0.8B-Base.i1-Q4_K_M.gguf"
        ),
        file_name: "Qwen3.5-0.8B-Base.i1-Q4_K_M.gguf",
        size: 529_297_792,
        sha256: "9acb4d284d936bb7f03e618d79f10bc002a4c2087c785b69e696ff7205fffc43",
        license: "Apache-2.0",
        min_ram_gb: 4,
        tier: Tier::Small,
    },
    ModelEntry {
        id: "qwen3.5-2b-base-q4_k_m",
        name: "Qwen3.5 2B Base",
        url: hf!(
            "mradermacher/Qwen3.5-2B-Base-i1-GGUF",
            "209c5e51cc8ceee9b70facbcf36c57d7968ba543",
            "Qwen3.5-2B-Base.i1-Q4_K_M.gguf"
        ),
        file_name: "Qwen3.5-2B-Base.i1-Q4_K_M.gguf",
        size: 1_274_397_056,
        sha256: "e3daa3f6e5ed8de264fc6e4128713ae1eb0c2649ad69b83f97bfc6a6ddca3da8",
        license: "Apache-2.0",
        min_ram_gb: 8,
        tier: Tier::Small,
    },
    ModelEntry {
        id: "qwen3.5-4b-base-q4_k_m",
        name: "Qwen3.5 4B Base",
        url: hf!(
            "mradermacher/Qwen3.5-4B-Base-i1-GGUF",
            "28fac19ebf6e7b388bfd3916994b3abb43a77bf6",
            "Qwen3.5-4B-Base.i1-Q4_K_M.gguf"
        ),
        file_name: "Qwen3.5-4B-Base.i1-Q4_K_M.gguf",
        size: 2_708_804_864,
        sha256: "306f85edfd07bea006c438947d1f16fd476decb9d9aec9ea65e0cecc8671bed3",
        license: "Apache-2.0",
        min_ram_gb: 16,
        tier: Tier::Standard,
    },
    ModelEntry {
        id: "gemma-4-e2b-q4_k_m",
        name: "Gemma 4 E2B",
        url: hf!(
            "mradermacher/gemma-4-E2B-i1-GGUF",
            "a9bf638e53783fc93778f357cc5c672eab5393b1",
            "gemma-4-E2B.i1-Q4_K_M.gguf"
        ),
        file_name: "gemma-4-E2B.i1-Q4_K_M.gguf",
        size: 3_427_862_240,
        sha256: "3cc0e9c2b4bbffea7f3f6cfe1ffd1591f4dafe02c43f78952f1142a24172df65",
        license: "Apache-2.0",
        min_ram_gb: 8,
        tier: Tier::Small,
    },
    ModelEntry {
        id: "gemma-4-e4b-q4_k_m",
        name: "Gemma 4 E4B",
        url: hf!(
            "mradermacher/gemma-4-E4B-i1-GGUF",
            "8d1efb45cd65f46a70ac918584e2f0244117465f",
            "gemma-4-E4B.i1-Q4_K_M.gguf"
        ),
        file_name: "gemma-4-E4B.i1-Q4_K_M.gguf",
        size: 5_335_274_240,
        sha256: "687f4cce47eac6ccfc595fd9517215fb964e5e5be17d4774b25b8add401d6d3c",
        license: "Apache-2.0",
        min_ram_gb: 16,
        tier: Tier::Standard,
    },
    ModelEntry {
        id: "qwen3-1.7b-base-q4_k_m",
        name: "Qwen3 1.7B Base",
        url: hf!(
            "mradermacher/Qwen3-1.7B-Base-i1-GGUF",
            "c11e7b148b8e7ee59a63cc8faa90b842e25e8942",
            "Qwen3-1.7B-Base.i1-Q4_K_M.gguf"
        ),
        file_name: "Qwen3-1.7B-Base.i1-Q4_K_M.gguf",
        size: 1_107_409_248,
        sha256: "b3aac3227132b11e0ea8c818569f27c053ebde24d0b8530f3bd476e7ff33ee20",
        license: "Apache-2.0",
        min_ram_gb: 8,
        tier: Tier::Comparison,
    },
];

/// The models Ghostcaret offers, shipped inside the app.
#[derive(Clone, Copy, Debug)]
pub struct Manifest {
    entries: &'static [ModelEntry],
}

impl Manifest {
    pub fn builtin() -> Manifest {
        Manifest { entries: CURATED }
    }

    pub fn entries(&self) -> &'static [ModelEntry] {
        self.entries
    }

    pub fn get(&self, id: &str) -> Option<&'static ModelEntry> {
        self.entries.iter().find(|e| e.id == id)
    }
}

/// Download progress in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub downloaded: u64,
    pub total: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("cancelled")]
    Cancelled,
    #[error("another download of this model is running")]
    Busy,
    #[error("download failed: {0}")]
    Http(String),
    #[error("the server sent {got} bytes, expected {expected}")]
    SizeMismatch { expected: u64, got: u64 },
    #[error("the downloaded file does not match its checksum")]
    HashMismatch,
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// A model file in the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub path: PathBuf,
    pub size: u64,
    /// The manifest entry, for curated models.
    pub entry: Option<&'static ModelEntry>,
}

/// The directory of downloaded models.
#[derive(Clone, Debug)]
pub struct ModelStore {
    dir: PathBuf,
}

const PART: &str = "part";
/// Progress is reported at most once per this many bytes.
const REPORT_EVERY: u64 = 1 << 20;

impl ModelStore {
    pub fn new(dir: PathBuf) -> ModelStore {
        ModelStore { dir }
    }

    /// `models` in the OS data directory.
    pub fn default_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "ghostcaret").map(|d| d.data_dir().join("models"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, entry: &ModelEntry) -> PathBuf {
        self.dir.join(entry.file_name)
    }

    fn part_path(&self, entry: &ModelEntry) -> PathBuf {
        self.dir.join(format!("{}.{PART}", entry.file_name))
    }

    pub fn is_installed(&self, entry: &ModelEntry) -> bool {
        fs::metadata(self.path(entry)).is_ok_and(|m| m.is_file() && m.len() == entry.size)
    }

    /// GGUF files in the store, curated or not. Partial downloads are left out.
    pub fn installed(&self) -> io::Result<Vec<Installed>> {
        let manifest = Manifest::builtin();
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let meta = entry.metadata()?;
            if !meta.is_file() || path.extension().is_none_or(|e| e != "gguf") {
                continue;
            }
            let curated = manifest
                .entries()
                .iter()
                .find(|e| e.file_name == entry.file_name());
            if curated.is_some_and(|e| e.size != meta.len()) {
                continue;
            }
            out.push(Installed {
                path,
                size: meta.len(),
                entry: curated,
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// The file for a setting's model: an installed manifest id, or a path
    /// to a GGUF file.
    pub fn resolve(&self, model: &str) -> Option<PathBuf> {
        if let Some(entry) = Manifest::builtin().get(model) {
            return self.is_installed(entry).then(|| self.path(entry));
        }
        let path = Path::new(model);
        path.is_file().then(|| path.to_owned())
    }

    pub fn delete(&self, entry: &ModelEntry) -> io::Result<()> {
        for path in [self.path(entry), self.part_path(entry)] {
            match fs::remove_file(path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }

    /// Downloads `entry` into the store, resuming a partial download, and
    /// returns its path. Blocking; `cancel` is checked between reads and
    /// keeps the partial file for the next attempt.
    pub fn download(
        &self,
        entry: &ModelEntry,
        progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<PathBuf, DownloadError> {
        let path = self.path(entry);
        if self.is_installed(entry) {
            return Ok(path);
        }
        fs::create_dir_all(&self.dir)?;
        let part_path = self.part_path(entry);
        let mut part = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&part_path)?;
        part.try_lock().map_err(|_| DownloadError::Busy)?;
        let mut hasher = Sha256::new();
        let mut have = part.metadata()?.len();
        if have > entry.size {
            part.set_len(0)?;
            have = 0;
        }
        if have > 0 {
            // Resume: the hash covers the bytes already on disk.
            io::copy(&mut (&part).take(have), &mut HashWriter(&mut hasher))?;
        }
        if have < entry.size {
            have = fetch(entry, &mut part, &mut hasher, have, progress, cancel)?;
        }
        if have != entry.size {
            return Err(DownloadError::SizeMismatch {
                expected: entry.size,
                got: have,
            });
        }
        if !hex_eq(&hasher.finalize(), entry.sha256) {
            drop(part);
            fs::remove_file(&part_path)?;
            return Err(DownloadError::HashMismatch);
        }
        part.sync_all()?;
        drop(part);
        fs::rename(&part_path, &path)?;
        Ok(path)
    }
}

/// Appends the rest of `entry` from byte `from` to `part`, feeding `hasher`.
/// Returns the file length reached.
fn fetch(
    entry: &ModelEntry,
    part: &mut File,
    hasher: &mut Sha256,
    from: u64,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<u64, DownloadError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .user_agent(concat!("ghostcaret/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut request = agent.get(entry.url);
    if from > 0 {
        request = request.header("Range", format!("bytes={from}-"));
    }
    let response = request
        .call()
        .map_err(|e| DownloadError::Http(e.to_string()))?;
    let mut have = from;
    if from > 0 && response.status() != 206 {
        // The server ignored the range: start over.
        part.set_len(0)?;
        *hasher = Sha256::new();
        have = 0;
    }
    part.seek(io::SeekFrom::Start(have))?;
    let mut body = response.into_body().into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut reported = have;
    progress(Progress {
        downloaded: have,
        total: entry.size,
    });
    loop {
        if cancel.is_cancelled() {
            part.flush()?;
            return Err(DownloadError::Cancelled);
        }
        let n = match body.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(DownloadError::Http(e.to_string())),
        };
        if have + n as u64 > entry.size {
            return Err(DownloadError::SizeMismatch {
                expected: entry.size,
                got: have + n as u64,
            });
        }
        part.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        have += n as u64;
        if have - reported >= REPORT_EVERY || have == entry.size {
            progress(Progress {
                downloaded: have,
                total: entry.size,
            });
            reported = have;
        }
    }
    part.flush()?;
    Ok(have)
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn hex_eq(digest: &[u8], hex: &str) -> bool {
    hex.len() == digest.len() * 2
        && digest
            .iter()
            .zip(hex.as_bytes().chunks(2))
            .all(|(&b, pair)| {
                std::str::from_utf8(pair)
                    .ok()
                    .and_then(|p| u8::from_str_radix(p, 16).ok())
                    == Some(b)
            })
}

/// SHA-256 of a file as lower-case hex.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut File::open(path)?, &mut HashWriter(&mut hasher))?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_entries_are_well_formed() {
        let manifest = Manifest::builtin();
        let mut ids = std::collections::HashSet::new();
        for e in manifest.entries() {
            assert!(ids.insert(e.id), "duplicate id {}", e.id);
            assert_eq!(e.sha256.len(), 64, "{}", e.id);
            assert!(
                e.sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "{}",
                e.id
            );
            assert!(
                e.url.starts_with("https://huggingface.co/") && e.url.ends_with(e.file_name),
                "{}",
                e.id
            );
            assert!(
                e.file_name.ends_with(".gguf") && e.size > 0 && e.min_ram_gb > 0,
                "{}",
                e.id
            );
            assert!(
                !e.url.contains("/resolve/main/"),
                "{} must pin a revision",
                e.id
            );
            assert_eq!(manifest.get(e.id), Some(e));
        }
    }

    #[test]
    fn hex_comparison() {
        assert!(hex_eq(&[0xab, 0x01], "ab01"));
        assert!(!hex_eq(&[0xab, 0x01], "ab02"));
        assert!(!hex_eq(&[0xab], "ab01"));
        assert!(!hex_eq(&[0xab], "zz"));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("oft-local-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn listing_skips_partial_and_wrong_size_files() {
        let dir = scratch("list");
        let store = ModelStore::new(dir.clone());
        let entry = &Manifest::builtin().entries()[0];
        fs::write(store.part_path(entry), b"partial").unwrap();
        fs::write(dir.join(entry.file_name), b"truncated").unwrap();
        fs::write(dir.join("mine.gguf"), b"GGUF").unwrap();
        fs::write(dir.join("notes.txt"), b"x").unwrap();
        let installed = store.installed().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].path, dir.join("mine.gguf"));
        assert_eq!(installed[0].entry, None);
        assert!(!store.is_installed(entry));
        assert_eq!(store.resolve(entry.id), None);
        let mine = dir.join("mine.gguf");
        assert_eq!(store.resolve(mine.to_str().unwrap()), Some(mine));
        store.delete(entry).unwrap();
        assert!(!store.part_path(entry).exists() && !store.path(entry).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_store_lists_nothing() {
        let store = ModelStore::new(std::env::temp_dir().join("oft-local-store-does-not-exist"));
        assert!(store.installed().unwrap().is_empty());
    }

    /// Downloads the smallest curated model when `OFT_TEST_DOWNLOAD=1`,
    /// interrupting it once to exercise the resume path.
    #[test]
    #[ignore = "downloads 0.5 GB; run with OFT_TEST_DOWNLOAD=1"]
    fn download_resumes_and_verifies() {
        if std::env::var_os("OFT_TEST_DOWNLOAD").is_none() {
            return;
        }
        let dir = scratch("download");
        let store = ModelStore::new(dir.clone());
        let entry = Manifest::builtin()
            .entries()
            .iter()
            .min_by_key(|e| e.size)
            .unwrap();
        let cancel = CancelToken::new();
        let stop_at = entry.size / 3;
        let err = store
            .download(
                entry,
                &mut |p| {
                    if p.downloaded >= stop_at {
                        cancel.cancel();
                    }
                },
                &cancel,
            )
            .unwrap_err();
        assert!(matches!(err, DownloadError::Cancelled));
        let partial = fs::metadata(store.part_path(entry)).unwrap().len();
        assert!(partial >= stop_at && partial < entry.size);
        let mut first = None;
        let path = store
            .download(
                entry,
                &mut |p| _ = first.get_or_insert(p.downloaded),
                &CancelToken::new(),
            )
            .unwrap();
        assert_eq!(first, Some(partial), "resumed from the partial file");
        assert!(store.is_installed(entry));
        assert_eq!(sha256_file(&path).unwrap(), entry.sha256);
        fs::remove_dir_all(&dir).unwrap();
    }
}
