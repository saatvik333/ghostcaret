//! Loaded faces, the primary regular and bold faces, and per-character
//! fallback, all loaded on first use.

use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use memmap2::Mmap;
use swash::shape::partition::{SelectedFont, Selector};
use swash::text::cluster::{CharCluster, Emoji, Status};
use swash::{CacheKey, FontRef, Setting, tag_from_bytes};

use crate::system::{self, BOLD, Located, REGULAR};

pub(crate) type FaceId = u16;

pub(crate) struct Face {
    map: Mmap,
    offset: u32,
    key: CacheKey,
    /// `wght` axis value for variable fonts.
    pub wght: Option<f32>,
    /// Draw with a synthetic emboldening because no bold face exists.
    pub synthetic_bold: bool,
    source: (PathBuf, u32),
}

impl Face {
    fn open(located: &Located) -> Option<Face> {
        let file = File::open(&located.path).ok()?;
        // SAFETY: the mapping is read-only and only ever read through swash,
        // which bounds-checks every table access. System font files are
        // replaced (new inode) rather than truncated in place by package
        // managers, so the mapped pages stay valid while the face lives.
        let map = unsafe { Mmap::map(&file) }.ok()?;
        let font = FontRef::from_index(&map, located.index as usize)?;
        let offset = font.offset;
        Some(Face {
            map,
            offset,
            key: CacheKey::new(),
            wght: located.wght,
            synthetic_bold: false,
            source: (located.path.clone(), located.index),
        })
    }

    pub fn font(&self) -> FontRef<'_> {
        FontRef {
            data: &self.map,
            offset: self.offset,
            key: self.key,
        }
    }

    /// Variation settings to apply when shaping and scaling this face.
    pub fn variations(&self) -> Option<Setting<f32>> {
        self.wght.map(|value| Setting {
            tag: tag_from_bytes(b"wght"),
            value,
        })
    }

    fn maps(&self, ch: char) -> bool {
        self.font().charmap().map(ch) != 0
    }
}

#[derive(Default)]
pub(crate) struct Fonts {
    faces: Vec<Arc<Face>>,
    loaded: bool,
    regular: Option<FaceId>,
    bold: Option<FaceId>,
    /// Faces found for characters the primary face lacks, in the order found;
    /// tried before asking the system again.
    fallbacks: Vec<FaceId>,
    /// Characters already looked up in the system, by whether a colour face
    /// was wanted; `None` when no face has it.
    looked_up: HashMap<(char, bool), Option<FaceId>>,
}

impl Fonts {
    /// Loads the primary faces on first call.
    pub fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        self.regular = self.first_openable(&system::locate(REGULAR, None, false), REGULAR);
        let bold = self.first_openable(&system::locate(BOLD, None, false), BOLD);
        self.bold = bold.or_else(|| {
            // Embolden the regular face when the system has no bold.
            let regular = self.regular?;
            let mut face = Face::open(&self.located(regular))?;
            face.synthetic_bold = true;
            Some(self.push(face))
        });
    }

    pub fn face(&self, id: FaceId) -> &Arc<Face> {
        &self.faces[id as usize]
    }

    pub fn primary(&self, bold: bool) -> Option<FaceId> {
        if bold { self.bold } else { self.regular }
    }

    fn located(&self, id: FaceId) -> Located {
        let face = self.face(id);
        Located {
            path: face.source.0.clone(),
            index: face.source.1,
            wght: face.wght,
        }
    }

    fn push(&mut self, face: Face) -> FaceId {
        self.faces.push(Arc::new(face));
        (self.faces.len() - 1) as FaceId
    }

    fn first_openable(&mut self, candidates: &[Located], weight: f32) -> Option<FaceId> {
        candidates.iter().find_map(|located| {
            let mut face = self.find_or_open(located)?;
            if weight >= BOLD
                && located.wght.is_none()
                && face_weight(&self.faces[face as usize]) < 600
            {
                // A regular face returned for a bold request.
                let mut synthetic = Face::open(located)?;
                synthetic.synthetic_bold = true;
                face = self.push(synthetic);
            }
            Some(face)
        })
    }

    fn find_or_open(&mut self, located: &Located) -> Option<FaceId> {
        let existing = self.faces.iter().position(|f| {
            f.source.0 == located.path
                && f.source.1 == located.index
                && f.wght == located.wght
                && !f.synthetic_bold
        });
        match existing {
            Some(i) => Some(i as FaceId),
            None => Face::open(located).map(|face| self.push(face)),
        }
    }

    /// A face for `ch` when the primary face lacks it, or a colour face for
    /// an emoji.
    fn fallback_for(&mut self, ch: char, bold: bool, color: bool) -> Option<FaceId> {
        if let Some(&found) = self.looked_up.get(&(ch, color)) {
            return found;
        }
        let weight = if bold { BOLD } else { REGULAR };
        let found = system::locate(weight, Some(ch), color)
            .iter()
            .find_map(|located| {
                self.find_or_open(located)
                    .filter(|&id| self.face(id).maps(ch))
            });
        if let Some(id) = found
            && !color
            && !self.fallbacks.contains(&id)
        {
            self.fallbacks.push(id);
        }
        self.looked_up.insert((ch, color), found);
        found
    }
}

fn face_weight(face: &Face) -> u16 {
    face.font().attributes().weight().0
}

/// A face chosen for a run of clusters.
pub(crate) struct Chosen {
    pub id: FaceId,
    pub face: Arc<Face>,
}

impl PartialEq for Chosen {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl SelectedFont for Chosen {
    fn font(&self) -> FontRef<'_> {
        self.face.font()
    }
}

/// Picks a face per character cluster: the primary face, then faces already
/// found for other characters, then a system lookup. Never returns `None`
/// while a primary face exists, because the shaper stops at a `None`.
pub(crate) struct FaceSelector<'a> {
    pub fonts: &'a mut Fonts,
    pub bold: bool,
}

impl Selector for FaceSelector<'_> {
    type SelectedFont = Chosen;

    fn select_font(&mut self, cluster: &mut CharCluster) -> Option<Chosen> {
        let primary = self.fonts.primary(self.bold)?;
        let first = cluster.chars().first().map(|c| c.ch);
        let color = match cluster.info().emoji() {
            Emoji::Color => true,
            // Most characters shown as emoji by default live from U+1F000.
            Emoji::Default => first.is_some_and(|ch| ch >= '\u{1F000}'),
            Emoji::None | Emoji::Text => false,
        };
        if color
            && let Some(ch) = first
            && let Some(id) = self.fonts.fallback_for(ch, self.bold, true)
        {
            let charmap = self.fonts.face(id).font().charmap();
            if cluster.map(|ch| charmap.map(ch)) == Status::Complete {
                return Some(self.chosen(id));
            }
        }
        let mut best = None;
        let tried = std::iter::once(primary).chain(self.fonts.fallbacks.iter().copied());
        for id in tried {
            let charmap = self.fonts.face(id).font().charmap();
            match cluster.map(|ch| charmap.map(ch)) {
                Status::Complete => return Some(self.chosen(id)),
                Status::Keep => best = Some(id),
                Status::Discard => {}
            }
        }
        let primary_face = self.fonts.face(primary).clone();
        let missing = cluster
            .chars()
            .iter()
            .filter(|c| !c.ignorable && c.contributes_to_shaping)
            .map(|c| c.ch)
            .find(|&ch| !primary_face.maps(ch));
        if let Some(ch) = missing
            && let Some(id) = self.fonts.fallback_for(ch, self.bold, false)
        {
            let charmap = self.fonts.face(id).font().charmap();
            match cluster.map(|ch| charmap.map(ch)) {
                Status::Complete => return Some(self.chosen(id)),
                Status::Keep => best = Some(id),
                Status::Discard => {}
            }
        }
        Some(self.chosen(best.unwrap_or(primary)))
    }
}

impl FaceSelector<'_> {
    fn chosen(&self, id: FaceId) -> Chosen {
        Chosen {
            id,
            face: self.fonts.face(id).clone(),
        }
    }
}
