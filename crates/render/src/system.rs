//! Finds system font files: the user's sans-serif face, its bold, and a face
//! for any character the primary face lacks.

use std::path::PathBuf;

/// A face inside a font file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Located {
    pub path: PathBuf,
    /// Face index inside a collection (`.ttc`).
    pub index: u32,
    /// Weight to apply through the `wght` axis when the file is a variable
    /// font; `None` for static faces.
    pub wght: Option<f32>,
}

pub(crate) const REGULAR: f32 = 400.0;
pub(crate) const BOLD: f32 = 700.0;

/// Candidates in preference order for the sans-serif face of `weight` that
/// covers `ch` (any character when `None`), preferring colour faces when
/// `color` is set (emoji). The caller checks coverage, since some platforms
/// can only list files.
pub(crate) fn locate(weight: f32, ch: Option<char>, color: bool) -> Vec<Located> {
    imp::locate(weight, ch, color)
}

#[cfg(target_os = "linux")]
mod imp {
    //! fontconfig, so the user's configured sans-serif family, its
    //! substitutions and fallback order apply, answered from fontconfig's own
    //! cache without scanning font directories.

    use std::ffi::{CStr, OsStr, c_char, c_int, c_uint};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::ptr;
    use std::sync::Once;

    use super::Located;

    #[repr(C)]
    struct FcPattern {
        _private: [u8; 0],
    }
    #[repr(C)]
    struct FcCharSet {
        _private: [u8; 0],
    }
    #[repr(C)]
    struct FcConfig {
        _private: [u8; 0],
    }

    type FcBool = c_int;
    const FC_MATCH_PATTERN: c_int = 0;
    const FC_RESULT_MATCH: c_int = 0;
    // fontconfig weight scale (not OpenType's).
    const FC_WEIGHT_REGULAR: c_int = 80;
    const FC_WEIGHT_BOLD: c_int = 200;

    #[link(name = "fontconfig")]
    unsafe extern "C" {
        fn FcInit() -> FcBool;
        fn FcPatternCreate() -> *mut FcPattern;
        fn FcPatternDestroy(p: *mut FcPattern);
        fn FcPatternAddString(p: *mut FcPattern, object: *const c_char, s: *const u8) -> FcBool;
        fn FcPatternAddInteger(p: *mut FcPattern, object: *const c_char, i: c_int) -> FcBool;
        fn FcPatternAddBool(p: *mut FcPattern, object: *const c_char, b: FcBool) -> FcBool;
        fn FcPatternAddCharSet(
            p: *mut FcPattern,
            object: *const c_char,
            c: *const FcCharSet,
        ) -> FcBool;
        fn FcPatternGetString(
            p: *const FcPattern,
            object: *const c_char,
            n: c_int,
            s: *mut *mut u8,
        ) -> c_int;
        fn FcPatternGetInteger(
            p: *const FcPattern,
            object: *const c_char,
            n: c_int,
            i: *mut c_int,
        ) -> c_int;
        fn FcPatternGetBool(
            p: *const FcPattern,
            object: *const c_char,
            n: c_int,
            b: *mut FcBool,
        ) -> c_int;
        fn FcPatternGetCharSet(
            p: *const FcPattern,
            object: *const c_char,
            n: c_int,
            c: *mut *mut FcCharSet,
        ) -> c_int;
        fn FcCharSetCreate() -> *mut FcCharSet;
        fn FcCharSetDestroy(c: *mut FcCharSet);
        fn FcCharSetAddChar(c: *mut FcCharSet, ucs4: c_uint) -> FcBool;
        fn FcCharSetHasChar(c: *const FcCharSet, ucs4: c_uint) -> FcBool;
        fn FcConfigSubstitute(config: *mut FcConfig, p: *mut FcPattern, kind: c_int) -> FcBool;
        fn FcDefaultSubstitute(p: *mut FcPattern);
        fn FcFontMatch(
            config: *mut FcConfig,
            p: *mut FcPattern,
            result: *mut c_int,
        ) -> *mut FcPattern;
    }

    const FAMILY: &CStr = c"family";
    const WEIGHT: &CStr = c"weight";
    const CHARSET: &CStr = c"charset";
    const FILE: &CStr = c"file";
    const INDEX: &CStr = c"index";
    const VARIABLE: &CStr = c"variable";
    const COLOR: &CStr = c"color";

    /// Owns a pattern and destroys it on drop.
    struct Pattern(*mut FcPattern);

    impl Drop for Pattern {
        fn drop(&mut self) {
            // SAFETY: the pointer came from fontconfig, is non-null and is
            // destroyed exactly once.
            unsafe { FcPatternDestroy(self.0) }
        }
    }

    pub(super) fn locate(weight: f32, ch: Option<char>, color: bool) -> Vec<Located> {
        static INIT: Once = Once::new();
        // SAFETY: FcInit has no preconditions; later calls pass a null config,
        // which means fontconfig's thread-safe current configuration.
        INIT.call_once(|| unsafe {
            FcInit();
        });
        // SAFETY: every pointer passed below is either a live pattern or
        // charset created here, or a NUL-terminated string constant. Strings
        // read from the match stay valid while `matched` lives, and are copied
        // out before it drops.
        unsafe {
            let pattern = FcPatternCreate();
            if pattern.is_null() {
                return Vec::new();
            }
            let pattern = Pattern(pattern);
            FcPatternAddString(pattern.0, FAMILY.as_ptr(), c"sans-serif".as_ptr().cast());
            let fc_weight = if weight >= super::BOLD {
                FC_WEIGHT_BOLD
            } else {
                FC_WEIGHT_REGULAR
            };
            FcPatternAddInteger(pattern.0, WEIGHT.as_ptr(), fc_weight);
            if color {
                FcPatternAddBool(pattern.0, COLOR.as_ptr(), 1);
            }
            if let Some(ch) = ch {
                let set = FcCharSetCreate();
                if set.is_null() {
                    return Vec::new();
                }
                FcCharSetAddChar(set, ch as c_uint);
                // The pattern keeps its own reference to the set.
                FcPatternAddCharSet(pattern.0, CHARSET.as_ptr(), set);
                FcCharSetDestroy(set);
            }
            FcConfigSubstitute(ptr::null_mut(), pattern.0, FC_MATCH_PATTERN);
            FcDefaultSubstitute(pattern.0);
            let mut result = 0;
            let matched = FcFontMatch(ptr::null_mut(), pattern.0, &mut result);
            if matched.is_null() {
                return Vec::new();
            }
            let matched = Pattern(matched);
            if let Some(ch) = ch {
                // fontconfig always returns its best match; it only helps when
                // the match really has the character.
                let mut set = ptr::null_mut();
                if FcPatternGetCharSet(matched.0, CHARSET.as_ptr(), 0, &mut set) != FC_RESULT_MATCH
                    || FcCharSetHasChar(set, ch as c_uint) == 0
                {
                    return Vec::new();
                }
            }
            let mut file = ptr::null_mut();
            if FcPatternGetString(matched.0, FILE.as_ptr(), 0, &mut file) != FC_RESULT_MATCH
                || file.is_null()
            {
                return Vec::new();
            }
            let path = PathBuf::from(OsStr::from_bytes(CStr::from_ptr(file.cast()).to_bytes()));
            let mut index = 0;
            FcPatternGetInteger(matched.0, INDEX.as_ptr(), 0, &mut index);
            let mut variable = 0;
            FcPatternGetBool(matched.0, VARIABLE.as_ptr(), 0, &mut variable);
            // The upper 16 bits of the index name a variable font's instance;
            // the weight is applied through the axis instead.
            let named_instance = (index as u32) >> 16 != 0;
            vec![Located {
                path,
                index: index as u32 & 0xFFFF,
                wght: (variable != 0 || named_instance).then_some(weight),
            }]
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    //! Windows ships a fixed set of UI fonts; the list covers Latin, Greek and
    //! Cyrillic (Segoe UI), symbols, emoji, Chinese, Japanese, Korean, Indic,
    //! Thai, Hebrew and Arabic scripts.

    use std::path::PathBuf;

    use super::Located;

    const REGULAR: &[(&str, u32)] = &[
        ("segoeui.ttf", 0),
        ("seguisym.ttf", 0),
        ("seguiemj.ttf", 0),
        ("Nirmala.ttc", 0),
        ("Nirmala.ttf", 0),
        ("msyh.ttc", 0),
        ("YuGothM.ttc", 0),
        ("malgun.ttf", 0),
        ("leelawui.ttf", 0),
        ("arial.ttf", 0),
    ];
    const BOLD: &[(&str, u32)] = &[("segoeuib.ttf", 0)];
    const COLOR: &[(&str, u32)] = &[("seguiemj.ttf", 0)];

    pub(super) fn locate(weight: f32, _ch: Option<char>, color: bool) -> Vec<Located> {
        let dir = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let fonts = dir.join("Fonts");
        let bold = weight >= super::BOLD;
        BOLD.iter()
            .filter(|_| bold)
            .chain(COLOR.iter().filter(|_| color))
            .chain(REGULAR)
            .map(|&(file, index)| Located {
                path: fonts.join(file),
                index,
                wght: None,
            })
            .filter(|l| l.path.is_file())
            .collect()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod imp {
    use super::Located;

    pub(super) fn locate(_weight: f32, _ch: Option<char>, _color: bool) -> Vec<Located> {
        Vec::new()
    }
}
