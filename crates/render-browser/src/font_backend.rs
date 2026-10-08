//! The platform font backend: a real face table, CSS Fonts 4 §5 over it, and a
//! document's `@font-face` faces in the same table.
//!
//! What this file is for, and what it replaced: it used to load exactly one face
//! per fallback group and pick a font purely by glyph coverage, so `font-weight`
//! and `font-style` had nothing to select on and bold was not merely unimplemented
//! but impossible. It now loads every face of every family it knows about, each
//! carrying the `font-weight` and `font-style` it was drawn as, and the
//! selection itself is [`font_matching`], which follows §5 rather than
//! approximating it.
//!
//! # Two kinds of face, one table
//!
//! The *installed* face *metadata* comes from a declared platform table rather
//! than from parsing each font's `name` and `OS/2` tables. That is the same
//! design a platform font-configuration file uses, and it is a deliberate trade:
//! reading the tables would make the metadata follow whatever the file happens to
//! contain, at the cost of a hand-rolled sfnt parser this project would have to
//! audit. The declared table is checkable, reviewable, and has one face per row,
//! so a wrong row is a visible row rather than a subtle parse result.
//!
//! A *document's* faces come from the network, for a document that did not exist
//! when the engine started, and they are in the same [`FaceTable`] rather than in
//! a layer above it. §5.2 and §10.2 make that the specification's own shape: a
//! web font shadows an installed font of the same name, so the two cannot be
//! searched in sequence without the shadowing being decided twice.
//!
//! # How staleness is prevented structurally
//!
//! A face arriving changes what §5 resolves to, so every memo keyed on a request
//! could in principle be stale. Three things make that impossible rather than
//! merely unlikely:
//!
//! 1. The table and the faces it indexes live in **one** [`RwLock`], so no caller
//!    can observe a walk that disagrees with the fonts it names.
//! 2. A change of table **bumps a generation**, and the generation is mixed into
//!    every [`FontInstanceId`] by [`SystemFontBackend::request_key`]. A new
//!    generation therefore mints new ids, so a memo entry or a rasterised glyph
//!    mask from the previous table is *unreachable* rather than merely unused.
//! 3. `resolve` re-walks whenever the id it was handed was minted by an earlier
//!    generation, which follows from (2) and needs no separate check.
//!
//! # The measure/draw contract
//!
//! Both halves of the pipeline resolve a face through the *same* call, on the
//! *same* request, and there is no third path: [`SystemFontBackend`] is the
//! measurer, the shaper, and the mask provider together. A measure/draw
//! mismatch is not prevented by convention here, it is prevented by there being
//! one function.
//!
//! # The face that has not arrived
//!
//! There is no font download timer, so there is no block period, no swap period
//! and no failure period to honour, and `font-display` has nothing to act on - see
//! [`render_core::font_face`] for why that is the specification's own outcome at
//! the only point on the timeline this engine can occupy. What §5.2 settles is
//! what a face that has not arrived *is*: "If the font resources defined for a
//! given face in an `@font-face` rule are either not available or contain invalid
//! font data, then the face should be treated as not present in the family."
//!
//! So the engine's answer is: **the face is not in the table.** Not present with
//! an empty character map, and not present with a "loading" flag. §5.2 then walks
//! on to the next name in `font-family` and, after that, to installed font
//! fallback - which is what §4.8.1 requires when a font is unavailable ("user
//! agents must display the text visibly"), and which paints text rather than
//! hiding it.
//!
//! The measure/draw consistency is structural rather than argued: there is no
//! per-face rendering state, so there is nothing for measurement and painting to
//! disagree about. Both read the one table, at one generation, and the generation
//! is inside the instance id both are given.
//!
//! # Private Use Area characters, and the per-site table that is gone
//!
//! This file used to hold a hardcoded table mapping six private-use codepoints to
//! Latin and CJK letterforms - U+E610 to `×`, U+E613 to `▾`, U+E619 to `↻`,
//! U+E62E to `热` and two more - for one site's icon font. It ran as a pre-step
//! before §5, so that §5.4's Private Use Area rule governed the substituted
//! character rather than the codepoint, and a test pinned the substitution and
//! that ordering.
//!
//! Both were removed, and the reason is that the table was a lie twice over. It
//! was knowledge of one site compiled into the engine, which is the defect
//! `tools/check_site_neutrality.py` looks for in a different shape; and a private
//! use area codepoint has no Unicode meaning at all, so every entry drew a
//! letterform the author never asked for.
//!
//! What serves those codepoints legitimately is the mechanism this round
//! implements. A page's icon font is a `@font-face` rule; the engine fetches it;
//! and §5.4 permits it to be selected, because it says a private-use codepoint
//! "must only match font families named in the `font-family` list that are not
//! generic families", and a document family is named and is not generic. When no
//! such family supplies the glyph, §5's last resort applies and the character is
//! not displayed - which is what §5 says a user agent "should indicate by some
//! means", and which an empty advance already does.
//!
//! The ordering property the previous round worked to establish is therefore now
//! the whole of the behaviour rather than a pre-step's precondition: the
//! codepoint the author wrote is the codepoint §5.4 and §5 see.
//! [`crate::font_matching::is_private_use`] is the one function that decides it.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap};
use std::env;
use std::error::Error;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use fontdue::{Font, FontSettings};
use render_core::layout::{
    FontRequest, FontStyle, FontSynthesis, GenericFamily, NominalFace, PhysicalPoint, TextMeasure,
    TextMeasurer, TextStyle,
};
use render_core::paint::{
    Color, FontInstanceId, GlyphId, GlyphInstance, GlyphMask, GlyphMaskProvider, GlyphRun,
    TextShaper,
};

use crate::chrome::{Canvas, Point, TextPainter};
use crate::font_faces::{DocumentFonts, DocumentRule, ResolvedSource};
use crate::font_matching::{Face, FaceTable, Family, FamilyWalk};

const MAX_CACHED_GLYPHS: usize = 16_384;
type GlyphCache = HashMap<(FontInstanceId, GlyphId, u32), Arc<GlyphMask>>;

/// The pixels `font-size` buys for one point of synthetic bold weight.
///
/// §2.8 describes synthesizing bold as "drawing a thin stroke around each
/// glyph", and this is that stroke: a dilation of the rasterized coverage by one
/// pixel in each direction. It is a real change to the painted mask rather than
/// a re-shaping of the outline, because `fontdue` exposes no outline transform
/// and a mask is what the rasterizer consumes.
const SYNTHETIC_BOLD_DILATION_PX: u32 = 1;

/// One face as the platform provides it: a family, and the files that hold that
/// one face of it.
struct FaceSource {
    /// The family this file provides, as the platform names it.
    family: &'static str,
    /// Every name §5.1 matches for this family. A user agent has to recognise
    /// localised names too (§5.1), and for the shipped platforms a family has
    /// one localised name per platform, so one list carries all of them.
    aliases: &'static [&'static str],
    /// The `<generic-font-family>` keywords this family is the engine's choice
    /// for, in §2.1.5's sense.
    generics: &'static [GenericFamily],
    /// The files to look for, in order.
    ///
    /// One face of one family is one row even when two platforms ship it under
    /// different file names, because §5.2 assembles "the set of font faces in
    /// that family" - a family with two declarations of the same face would be
    /// a family with a duplicate face, not a family with a platform variant.
    files: &'static [&'static str],
    /// Which face of a TrueType collection the file is, or zero for a
    /// single-face file.
    collection_index: u32,
    /// §2.2's weight of the face in the file.
    weight: u16,
    /// §2.4's slant of the face in the file.
    style: FontStyle,
    /// §4.3.3.1's `local()` names for this one face: the `name` table's full font
    /// name (nameID 4) and PostScript name (nameID 6).
    ///
    /// §4.3.3.1 is explicit that `local()` "uniquely identifies a single font
    /// face within a larger family" and is matched against "only the Postscript
    /// name or the full font name in the name table of locally available fonts",
    /// with platform substitutions for a given font name not to be used. So the
    /// names are declared here, per face, for the same reason `family` and
    /// `weight` are: the rasteriser exposes no name table, and §4.3.3.1's rule is
    /// that a `local()` name must not resolve to a *family*.
    ///
    /// A row with no names here is a row `local()` cannot find, and §4.3 says
    /// what happens then: "local font faces that are not found are ignored and the
    /// user agent loads the next font in the list." That is the honest outcome for
    /// a name this table does not carry, and it costs a network request rather
    /// than correctness.
    local_names: &'static [&'static str],
}

/// The platform font table, in priority order.
///
/// Order is the whole of the user agent's discretion here: §2.1.5 lets a generic
/// family resolve to more than one installed family and §5.2 requires the choice
/// among equal candidates "must not differ between two elements in the same
/// document", so one documented order answers both.
///
/// `sans-serif`, `serif` and `monospace` resolve to the first family that claims
/// them, which on every platform here is the platform's own default for that
/// category. `system-ui` resolves to the platform's user interface font, which is
/// §2.1.5's whole description of it and is also this engine's initial
/// `font-family` (`FontRequest::INITIAL_FAMILY`).
const PLATFORM_FACES: &[FaceSource] = &[
    // ---- Windows ----------------------------------------------------------
    // Segoe UI is the Windows user interface font, so it is what `system-ui`,
    // `ui-sans-serif` and `sans-serif` resolve to.
    FaceSource {
        family: "Segoe UI",
        aliases: &["segoe ui", "segoe", "segoe ui web"],
        generics: &[
            GenericFamily::SystemUi,
            GenericFamily::UiSansSerif,
            GenericFamily::SansSerif,
        ],
        files: &["segoeui.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &["Segoe UI", "SegoeUI"],
    },
    FaceSource {
        family: "Segoe UI",
        aliases: &["segoe ui", "segoe"],
        generics: &[
            GenericFamily::SystemUi,
            GenericFamily::UiSansSerif,
            GenericFamily::SansSerif,
        ],
        files: &["segoeuib.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Normal,
        local_names: &["Segoe UI Bold", "SegoeUI-Bold"],
    },
    FaceSource {
        family: "Segoe UI",
        aliases: &["segoe ui", "segoe"],
        generics: &[
            GenericFamily::SystemUi,
            GenericFamily::UiSansSerif,
            GenericFamily::SansSerif,
        ],
        files: &["segoeuii.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Italic,
        local_names: &["Segoe UI Italic", "SegoeUI-Italic"],
    },
    FaceSource {
        family: "Segoe UI",
        aliases: &["segoe ui", "segoe"],
        generics: &[
            GenericFamily::SystemUi,
            GenericFamily::UiSansSerif,
            GenericFamily::SansSerif,
        ],
        files: &["segoeuiz.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Italic,
        local_names: &["Segoe UI Bold Italic", "SegoeUI-BoldItalic"],
    },
    // Consolas is the Windows user interface monospace face, so it answers both
    // `ui-monospace` and, ahead of Courier New, `monospace`.
    FaceSource {
        family: "Consolas",
        aliases: &["consolas", "consolas regular"],
        generics: &[GenericFamily::UiMonospace, GenericFamily::Monospace],
        files: &["consola.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &["Consolas", "Consolas"],
    },
    FaceSource {
        family: "Consolas",
        aliases: &["consolas"],
        generics: &[GenericFamily::UiMonospace, GenericFamily::Monospace],
        files: &["consolab.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Normal,
        local_names: &["Consolas Bold", "Consolas-Bold"],
    },
    FaceSource {
        family: "Consolas",
        aliases: &["consolas"],
        generics: &[GenericFamily::UiMonospace, GenericFamily::Monospace],
        files: &["consolai.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Italic,
        local_names: &["Consolas Italic", "Consolas-Italic"],
    },
    FaceSource {
        family: "Consolas",
        aliases: &["consolas"],
        generics: &[GenericFamily::UiMonospace, GenericFamily::Monospace],
        files: &["consolaz.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Italic,
        local_names: &["Consolas Bold Italic", "Consolas-BoldItalic"],
    },
    FaceSource {
        family: "Arial",
        aliases: &["arial", "helvetica", "liberation sans", "arial unicode ms"],
        generics: &[],
        files: &["arial.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &["Arial", "ArialMT"],
    },
    FaceSource {
        family: "Arial",
        aliases: &["arial", "helvetica", "liberation sans"],
        generics: &[],
        files: &["arialbd.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Normal,
        local_names: &["Arial Bold", "Arial-BoldMT"],
    },
    FaceSource {
        family: "Arial",
        aliases: &["arial", "helvetica", "liberation sans"],
        generics: &[],
        files: &["ariali.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Italic,
        local_names: &["Arial Italic", "Arial-ItalicMT"],
    },
    FaceSource {
        family: "Arial",
        aliases: &["arial", "helvetica", "liberation sans"],
        generics: &[],
        files: &["arialz.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Italic,
        local_names: &["Arial Bold Italic", "Arial-BoldItalicMT"],
    },
    FaceSource {
        family: "Tahoma",
        aliases: &["tahoma"],
        generics: &[],
        files: &["tahoma.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &[],
    },
    FaceSource {
        family: "Tahoma",
        aliases: &["tahoma"],
        generics: &[],
        files: &["tahomabd.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Normal,
        local_names: &[],
    },
    FaceSource {
        family: "Times New Roman",
        aliases: &[
            "times new roman",
            "times",
            "liberation serif",
            "nimbus roman",
        ],
        generics: &[GenericFamily::Serif, GenericFamily::UiSerif],
        files: &[
            "times.ttf",
            "/System/Library/Fonts/Supplemental/Times New Roman.ttf",
        ],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &["Times New Roman", "TimesNewRomanPSMT"],
    },
    FaceSource {
        family: "Times New Roman",
        aliases: &["times new roman", "times", "liberation serif"],
        generics: &[GenericFamily::Serif, GenericFamily::UiSerif],
        files: &[
            "timesbd.ttf",
            "/System/Library/Fonts/Supplemental/Times New Roman Bold.ttf",
        ],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Normal,
        local_names: &["Times New Roman Bold", "TimesNewRomanPS-BoldMT"],
    },
    FaceSource {
        family: "Times New Roman",
        aliases: &["times new roman", "times", "liberation serif"],
        generics: &[GenericFamily::Serif, GenericFamily::UiSerif],
        files: &[
            "timesi.ttf",
            "/System/Library/Fonts/Supplemental/Times New Roman Italic.ttf",
        ],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Italic,
        local_names: &["Times New Roman Italic", "TimesNewRomanPS-ItalicMT"],
    },
    FaceSource {
        family: "Times New Roman",
        aliases: &["times new roman", "times", "liberation serif"],
        generics: &[GenericFamily::Serif, GenericFamily::UiSerif],
        files: &["timesbi.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Italic,
        local_names: &[
            "Times New Roman Bold Italic",
            "TimesNewRomanPS-BoldItalicMT",
        ],
    },
    FaceSource {
        family: "Courier New",
        aliases: &["courier new", "courier", "liberation mono", "nimbus mono"],
        generics: &[],
        files: &["cour.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &["Courier New", "CourierNewPSMT"],
    },
    FaceSource {
        family: "Courier New",
        aliases: &["courier new", "courier", "liberation mono"],
        generics: &[],
        files: &["courbd.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Normal,
        local_names: &["Courier New Bold", "CourierNewPS-BoldMT"],
    },
    FaceSource {
        family: "Courier New",
        aliases: &["courier new", "courier", "liberation mono"],
        generics: &[],
        files: &["couri.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Italic,
        local_names: &["Courier New Italic", "CourierNewPS-ItalicMT"],
    },
    FaceSource {
        family: "Courier New",
        aliases: &["courier new", "courier", "liberation mono"],
        generics: &[],
        files: &["courbi.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Italic,
        local_names: &["Courier New Bold Italic", "CourierNewPS-BoldItalicMT"],
    },
    // Microsoft YaHei is the Simplified Chinese face. §2.1.5 keeps the CJK
    // generics (`generic(kai)` and friends) honest by leaving them unresolved
    // here: the engine has no installed family to point them at, and a keyword
    // that resolves to nothing walks on to the next name in the list exactly as
    // §5.2 requires.
    FaceSource {
        family: "Microsoft YaHei",
        aliases: &["microsoft yahei", "微软雅黑", "msyh", "yahei"],
        generics: &[],
        files: &["msyh.ttc"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &[],
    },
    FaceSource {
        family: "Microsoft YaHei",
        aliases: &["microsoft yahei", "微软雅黑", "msyh"],
        generics: &[],
        files: &["msyhbd.ttc"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Normal,
        local_names: &[],
    },
    FaceSource {
        family: "Segoe UI Symbol",
        aliases: &["segoe ui symbol", "segoe ui emoji", "symbol"],
        generics: &[],
        files: &["seguisym.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &[],
    },
    // ---- macOS ------------------------------------------------------------
    FaceSource {
        family: "SF Pro",
        aliases: &[
            "sf pro",
            "sf pro text",
            "sf pro display",
            "helvetica",
            "helvetica neue",
            ".apple systemuifont",
        ],
        generics: &[
            GenericFamily::SystemUi,
            GenericFamily::UiSansSerif,
            GenericFamily::SansSerif,
        ],
        files: &["/System/Library/Fonts/SFNS.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &[],
    },
    FaceSource {
        family: "SF Pro",
        aliases: &[
            "sf pro",
            "sf pro text",
            "sf pro display",
            "helvetica",
            "helvetica neue",
        ],
        generics: &[
            GenericFamily::SystemUi,
            GenericFamily::UiSansSerif,
            GenericFamily::SansSerif,
        ],
        files: &["/System/Library/Fonts/SFNSItalic.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Italic,
        local_names: &[],
    },
    FaceSource {
        family: "Menlo",
        aliases: &["menlo", "sf mono", "monaco", "courier"],
        generics: &[GenericFamily::UiMonospace, GenericFamily::Monospace],
        files: &["/System/Library/Fonts/Menlo.ttc"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &[],
    },
    FaceSource {
        family: "PingFang SC",
        aliases: &["pingfang sc", "苹方-简", "heiti sc", "pingfang"],
        generics: &[],
        files: &["/System/Library/Fonts/PingFang.ttc"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &[],
    },
    FaceSource {
        family: "Arial Unicode MS",
        aliases: &["arial unicode ms", "helvetica"],
        generics: &[],
        files: &["/System/Library/Fonts/Supplemental/Arial Unicode.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &[],
    },
    // ---- Linux ------------------------------------------------------------
    FaceSource {
        family: "DejaVu Sans",
        aliases: &[
            "dejavu sans",
            "dejavusans",
            "verdana",
            "bitstream vera sans",
        ],
        generics: &[
            GenericFamily::SystemUi,
            GenericFamily::UiSansSerif,
            GenericFamily::SansSerif,
        ],
        files: &["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &["DejaVu Sans", "DejaVuSans"],
    },
    FaceSource {
        family: "DejaVu Sans",
        aliases: &["dejavu sans", "dejavusans"],
        generics: &[
            GenericFamily::SystemUi,
            GenericFamily::UiSansSerif,
            GenericFamily::SansSerif,
        ],
        files: &["/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Normal,
        local_names: &["DejaVu Sans Bold", "DejaVuSans-Bold"],
    },
    FaceSource {
        family: "DejaVu Sans",
        aliases: &["dejavu sans", "dejavusans"],
        generics: &[
            GenericFamily::SystemUi,
            GenericFamily::UiSansSerif,
            GenericFamily::SansSerif,
        ],
        files: &["/usr/share/fonts/truetype/dejavu/DejaVuSans-Oblique.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Oblique(12.0),
        local_names: &["DejaVu Sans Oblique", "DejaVuSans-Oblique"],
    },
    FaceSource {
        family: "DejaVu Sans Mono",
        aliases: &["dejavu sans mono", "dejavusansmono"],
        generics: &[GenericFamily::UiMonospace, GenericFamily::Monospace],
        files: &["/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &["DejaVu Sans Mono", "DejaVuSansMono"],
    },
    FaceSource {
        family: "DejaVu Sans Mono",
        aliases: &["dejavu sans mono", "dejavusansmono"],
        generics: &[GenericFamily::UiMonospace, GenericFamily::Monospace],
        files: &["/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Normal,
        local_names: &["DejaVu Sans Mono Bold", "DejaVuSansMono-Bold"],
    },
    FaceSource {
        family: "Liberation Sans",
        aliases: &["liberation sans", "arial", "helvetica"],
        generics: &[GenericFamily::SansSerif],
        files: &["/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &["Liberation Sans", "LiberationSans"],
    },
    FaceSource {
        family: "Liberation Sans",
        aliases: &["liberation sans", "arial", "helvetica"],
        generics: &[GenericFamily::SansSerif],
        files: &["/usr/share/fonts/truetype/liberation2/LiberationSans-Bold.ttf"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Normal,
        local_names: &["Liberation Sans Bold", "LiberationSans-Bold"],
    },
    FaceSource {
        family: "Liberation Serif",
        aliases: &["liberation serif", "times new roman", "times"],
        generics: &[GenericFamily::Serif],
        files: &["/usr/share/fonts/truetype/liberation2/LiberationSerif-Regular.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &["Liberation Serif", "LiberationSerif"],
    },
    FaceSource {
        family: "Liberation Mono",
        aliases: &["liberation mono", "courier new", "courier"],
        generics: &[GenericFamily::Monospace],
        files: &["/usr/share/fonts/truetype/liberation2/LiberationMono-Regular.ttf"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &["Liberation Mono", "LiberationMono"],
    },
    FaceSource {
        family: "Noto Sans CJK SC",
        aliases: &[
            "noto sans cjk sc",
            "noto sans sc",
            "source han sans sc",
            "思源黑体",
        ],
        generics: &[],
        files: &["/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"],
        collection_index: 0,
        weight: 400,
        style: FontStyle::Normal,
        local_names: &[],
    },
    FaceSource {
        family: "Noto Sans CJK SC",
        aliases: &["noto sans cjk sc", "noto sans sc", "source han sans sc"],
        generics: &[],
        files: &["/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc"],
        collection_index: 0,
        weight: 700,
        style: FontStyle::Normal,
        local_names: &[],
    },
];

/// The families installed font fallback visits, in order, as indices into
/// [`PLATFORM_FAMILIES`] grouped by family. §5.2 leaves the choice to the user
/// agent, and the order here is the order the files appear in the table: the
/// platform's primary text face first, then its symbol and CJK faces.
const FALLBACK_FAMILIES: &[&str] = &[
    "Segoe UI",
    "Arial",
    "Segoe UI Symbol",
    "Microsoft YaHei",
    "SF Pro",
    "Menlo",
    "PingFang SC",
    "Arial Unicode MS",
    "DejaVu Sans",
    "DejaVu Sans Mono",
    "Liberation Sans",
    "Noto Sans CJK SC",
];

/// What a [`FontInstanceId`] names: one §5 resolution of one request.
///
/// The id names the *request*, not one face, because §5.2 chooses a face per
/// character and a run whose Latin and CJK characters live in different families
/// is ordinary. Both halves of the pipeline re-resolve the character against
/// this one walk, which is the mechanism that stops them disagreeing.
struct ResolvedInstance {
    /// The families §5 walked, each narrowed to the face it chose and to the
    /// synthesis §2.8 permits for it.
    walk: FamilyWalk,
    /// The generation of the table this walk was made against.
    ///
    /// §5.2 requires the choice "must not differ between two elements in the same
    /// document". A face arriving changes the choice, so a walk and the table it
    /// was made against are pinned together and a walk from an earlier table is
    /// re-made rather than reused. This is what makes a font arriving mid-session
    /// safe instead of a source of layout and paint disagreeing.
    generation: u64,
    /// The request the walk was made for, so a walk found to be stale can be
    /// re-made without the caller having to carry the request alongside it.
    ///
    /// `FontRequest` borrows the family list, which for a real render outlives the
    /// `measure` call that borrowed it, so this is sound for the whole of one
    /// `measure` or `shape_font` call - which is exactly the window in which a
    /// stale walk could be noticed.
    request: RequestSnapshot,
}

/// A [`FontRequest`] that outlives the borrow it was made from, for the one place
/// a walk has to be re-made.
#[derive(Clone)]
struct RequestSnapshot {
    family: String,
    weight: u16,
    style: FontStyle,
    synthesis: FontSynthesis,
}

impl RequestSnapshot {
    fn of(request: &FontRequest<'_>) -> Self {
        Self {
            family: request.family.to_owned(),
            weight: request.weight,
            style: request.style,
            synthesis: request.synthesis,
        }
    }

    fn as_request(&self) -> FontRequest<'_> {
        FontRequest {
            family: &self.family,
            weight: self.weight,
            style: self.style,
            synthesis: self.synthesis,
        }
    }
}

/// The installed faces, which are a property of the machine and never change
/// while the process runs.
struct InstalledFaces {
    /// The loaded fonts, indexed by `loaded[family][face]`.
    loaded: Vec<Vec<Arc<Font>>>,
    /// The declared table, which is the same order.
    families: Vec<Family>,
    /// §4.3.3.1's `local()` names, indexed the same way: `names[family][face]`
    /// is the list of full font names and PostScript names for that one face.
    local_names: Vec<Vec<&'static [&'static str]>>,
    /// The families installed font fallback visits, by index into `families`.
    fallback: Vec<usize>,
}

impl InstalledFaces {
    /// §4.3.3.1: the installed face a `local()` name identifies, if any.
    ///
    /// The match is caseless (§5.1) and against one face's own names, never
    /// against a family: §4.3.3.1 says the argument "uniquely identifies a single
    /// font face within a larger family" and that "Platform substitutions for a
    /// given font name must not be used". So a name that matches a family but no
    /// face's name is not found, which is what stops `local(Segoe UI)` from
    /// quietly picking the regular face of a family when the author meant a
    /// particular one.
    fn local_face(&self, name: &str) -> Option<(usize, usize)> {
        for (family, names) in self.local_names.iter().enumerate() {
            for (face, candidates) in names.iter().enumerate() {
                if candidates
                    .iter()
                    .any(|candidate| render_core::layout::caseless_match(candidate, name))
                {
                    return Some((family, face));
                }
            }
        }
        None
    }
}

/// The table the matcher searches, and the fonts it indexes, as one value.
///
/// They are in one `RwLock` rather than two so that no caller can observe a walk
/// that disagrees with the fonts it names - which would be a measure/draw
/// mismatch by construction rather than by accident.
struct FontState {
    /// The unified table: document families first, then installed.
    table: FaceTable,
    /// The fonts the table indexes, in the same order.
    loaded: Vec<Vec<Arc<Font>>>,
    /// Bumped whenever `table` or `loaded` are replaced, and mixed into every
    /// [`FontInstanceId`], so a memo or a mask from an earlier table cannot be
    /// reached under a later one.
    generation: u64,
}

pub struct SystemFontBackend {
    /// The installed faces. Immutable after `load`, so it needs no lock and the
    /// `local()` lookup it serves is available while a table is being rebuilt.
    installed: InstalledFaces,
    /// The current table.
    state: RwLock<FontState>,
    /// The generation currently published, readable without taking the write lock
    /// so the hot path (which only reads) does not contend with a font arriving.
    ///
    /// The value is written under the state lock and read here, so it is a
    /// *cache* of the generation: a reader that sees a stale one mints an id from
    /// the old table and then finds no resolution for it, which
    /// [`Self::resolve`] handles by re-walking against the current table under the
    /// read lock. The id is therefore always consistent with the walk it is
    /// looked up in.
    published_generation: AtomicU64,
    /// The resolved §5 walk per request, so the per-character path is a
    /// coverage test rather than a search.
    instances: RwLock<HashMap<u64, Arc<ResolvedInstance>>>,
    glyph_cache: Mutex<GlyphCache>,
}

impl SystemFontBackend {
    /// Load every face of every family in the platform table.
    ///
    /// A family whose files are absent is simply not in the table, which is what
    /// §5.2 calls a family that does not exist; the search walks on. A platform
    /// where *no* declared face is present is a different thing: §5's last
    /// resort is a missing-glyph symbol for every character, so the engine would
    /// start and render nothing, which is why this reports it instead.
    ///
    /// # Errors
    ///
    /// Returns an I/O-style error when no supported system font can be found.
    pub fn load() -> Result<Self, Box<dyn Error>> {
        let installed = load_installed_faces();
        if installed.loaded.iter().all(Vec::is_empty) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no supported system font was found",
            )
            .into());
        }
        let table = FaceTable::empty()
            .with_installed_families(installed.families.clone())
            .with_fallback(&installed.fallback);
        let loaded = installed.loaded.clone();
        Ok(Self {
            installed,
            state: RwLock::new(FontState {
                table,
                loaded,
                generation: 1,
            }),
            published_generation: AtomicU64::new(1),
            instances: RwLock::new(HashMap::new()),
            glyph_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Installs a document's `@font-face` faces, replacing any previous document's.
    ///
    /// This is the document boundary, and it is one call: a face is reachable only
    /// through the table published here, and dropping the document means calling
    /// this with an empty [`DocumentFonts`] - or letting the next call replace it.
    /// There is no per-face lifetime to track and no collection a face can be left
    /// behind in.
    pub fn set_document_faces(&self, document: &DocumentFonts) {
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        state.table = self.build_table(document);
        state.loaded = self.build_loaded(document);
        state.generation = state.generation.saturating_add(1);
        self.published_generation
            .store(state.generation, Ordering::Release);
    }

    /// The unified table, so the §5 search can be exercised against the real
    /// platform table as well as against a synthetic one.
    #[must_use]
    pub fn table(&self) -> FaceTable {
        self.with_state(|state| state.table.clone())
    }

    /// How many faces of the platform table were actually found on disk.
    #[must_use]
    pub fn loaded_face_count(&self) -> usize {
        self.installed.loaded.iter().map(Vec::len).sum()
    }

    /// How many of the table's families came from `@font-face` rules.
    #[must_use]
    pub fn document_family_count(&self) -> usize {
        self.with_state(|state| state.table.document_family_count())
    }

    /// The generation currently published, which is part of every
    /// [`FontInstanceId`] this backend mints.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.published_generation.load(Ordering::Acquire)
    }

    /// Runs `body` against the current table and the fonts it indexes, under a
    /// read lock.
    ///
    /// The lock is taken and released around the call rather than returned, so a
    /// caller cannot hold it across a point where it would want the write lock -
    /// which is what makes a deadlock here impossible rather than unlikely. Every
    /// use is a short lookup, and the table and the fonts are read together so no
    /// caller can see a walk that disagrees with the fonts it names.
    fn with_state<R>(&self, body: impl FnOnce(&FontStateSnapshot<'_>) -> R) -> R {
        let state = self.state.read().unwrap_or_else(PoisonError::into_inner);
        body(&FontStateSnapshot {
            table: &state.table,
            loaded: &state.loaded,
            generation: state.generation,
        })
    }

    /// The table and the fonts, and §4.3.3.1's `local()` lookup, for one document.
    ///
    /// §4.3: "When a font is needed the user agent iterates over the set of
    /// references listed, using the first one it can successfully parse and
    /// activate." The two kinds of item are answered in the order the author wrote
    /// them, and a `local()` item is answered first when it comes first - which is
    /// the ordering that stops a network request for a font already installed.
    fn resolve_source(&self, rule: &DocumentRule, document: &DocumentFonts) -> Option<Arc<Font>> {
        for source in &rule.sources {
            match source {
                ResolvedSource::Local(name) => {
                    if let Some((family, face)) = self.installed.local_face(name)
                        && let Some(font) = self
                            .installed
                            .loaded
                            .get(family)
                            .and_then(|faces| faces.get(face))
                    {
                        return Some(Arc::clone(font));
                    }
                }
                ResolvedSource::Url(url) => {
                    if let Some(font) = document.resource(url) {
                        return Some(font);
                    }
                }
            }
        }
        None
    }

    fn build_table(&self, document: &DocumentFonts) -> FaceTable {
        let mut table = FaceTable::empty();
        for family in document.families() {
            let faces = family
                .rules
                .iter()
                .filter_map(|rule| {
                    self.resolve_source(rule, document)
                        .map(|_| (rule.weight, rule.style, Some(rule.range.clone())))
                })
                .collect();
            // The family is added even when no rule resolved, because §5.2's
            // shadowing holds for a family whose faces have not arrived: "If no
            // faces are present for a family defined via @font-face rules, the
            // family should be treated as missing; matching a platform font with
            // the same name must not occur in this case."
            table.push_document_family(&family.name, faces);
        }
        table
            .with_installed_families(self.installed.families.clone())
            .with_fallback(&self.installed.fallback)
    }

    /// The fonts the table indexes: the document's resolved faces first, then the
    /// installed ones.
    ///
    /// The document half is rebuilt rather than borrowed because the table's
    /// indices are the ones the matcher uses, and a face the matcher can name has
    /// to be at the index the table gives it. The installed half is cloned as
    /// `Arc`s, so this is a refcount bump per face and not a second copy of a
    /// font file.
    fn build_loaded(&self, document: &DocumentFonts) -> Vec<Vec<Arc<Font>>> {
        let mut loaded: Vec<Vec<Arc<Font>>> = document
            .families()
            .iter()
            .map(|family| {
                family
                    .rules
                    .iter()
                    .filter_map(|rule| self.resolve_source(rule, document))
                    .collect()
            })
            .collect();
        loaded.extend(self.installed.loaded.iter().cloned());
        loaded
    }

    /// A stable identity for a font request.
    ///
    /// A request is a family list plus three small values, so hashing it is
    /// cheap and the same request always lands on the same [`FontInstanceId`].
    /// That is §5.2's "it must not differ between two elements in the same
    /// document" stated as an identity rather than as a convention, and it is
    /// why the identity survives the per-character walk instead of being minted
    /// per run.
    ///
    /// The generation is part of the key, which is what makes a face arriving safe:
    /// a new table mints new ids, so an id minted against the previous table names
    /// nothing and cannot be resolved to a walk over a table that has since
    /// changed.
    fn request_key(request: &FontRequest<'_>, generation: u64) -> u64 {
        let mut hasher = DefaultHasher::new();
        generation.hash(&mut hasher);
        request.family.hash(&mut hasher);
        request.weight.hash(&mut hasher);
        match request.style {
            FontStyle::Normal => 0_u8.hash(&mut hasher),
            FontStyle::Italic => 1_u8.hash(&mut hasher),
            FontStyle::Oblique(degrees) => {
                2_u8.hash(&mut hasher);
                degrees.to_bits().hash(&mut hasher);
            }
        }
        request.synthesis.weight.hash(&mut hasher);
        request.synthesis.style.hash(&mut hasher);
        // Zero is the id `ReferenceTextShaper` and `NoGlyphMasks` use for "the
        // one nominal face", so it is never handed to a real face.
        match hasher.finish() {
            0 => 1,
            key => key,
        }
    }

    /// The id a request is published under, from the published generation.
    fn instance_id_for(&self, request: &FontRequest<'_>) -> FontInstanceId {
        FontInstanceId(Self::request_key(
            request,
            self.published_generation.load(Ordering::Acquire),
        ))
    }

    /// The §5 resolution of `request`, memoised.
    fn resolve(&self, request: &FontRequest<'_>) -> Arc<ResolvedInstance> {
        let id = self.instance_id_for(request);
        if let Some(existing) = self.lookup(id) {
            return existing;
        }
        // Re-walk against the table as it is *now*, and mint the id from that
        // table's generation rather than the cached one, so the id and the walk
        // agree even if a face arrived between the two reads.
        let (id, resolved) = self.with_state(|state| {
            let id = FontInstanceId(Self::request_key(request, state.generation));
            (
                id,
                Arc::new(ResolvedInstance {
                    walk: state.table.walk(request),
                    generation: state.generation,
                    request: RequestSnapshot::of(request),
                }),
            )
        });
        let mut instances = self
            .instances
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        Arc::clone(instances.entry(id.0).or_insert(resolved))
    }

    fn lookup(&self, id: FontInstanceId) -> Option<Arc<ResolvedInstance>> {
        let instances = self
            .instances
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        instances.get(&id.0).map(Arc::clone)
    }

    fn resolved(&self, id: FontInstanceId) -> Option<Arc<ResolvedInstance>> {
        self.lookup(id)
    }

    /// The font a [`FontInstanceId`] resolves one character to, with the
    /// synthesis §2.8 permits for it.
    ///
    /// This is the only place a face is chosen. Measurement, shaping and mask
    /// rasterisation all reach a face through it, which is what makes a
    /// measure/draw mismatch impossible rather than merely unlikely.
    ///
    /// The face comes back shared rather than borrowed, because the table's read
    /// lock is not held once this returns and a caller must be able to hold the
    /// face while the next character is being resolved. The `Arc` also means a
    /// font document face resolved through a `local()` name and the same font as an
    /// installed face are one allocation.
    /// The face a character no installed face covers is drawn from: the first
    /// loaded face, whose `.notdef` glyph is the box every font carries for a
    /// missing character.
    fn missing_glyph_face(&self) -> Option<Arc<Font>> {
        self.with_state(|state| state.loaded.iter().flatten().next().map(Arc::clone))
    }

    fn face_for(
        &self,
        instance: &ResolvedInstance,
        character: char,
    ) -> Option<(Arc<Font>, bool, f32)> {
        // A face arriving between a caller resolving and asking about a character
        // invalidates the walk: its face indices name the *previous* table, and
        // reading the current one with them would pick a face the walk never
        // chose. So the walk is re-made rather than trusted.
        //
        // This is the only run-time read of the generation, and it is what makes
        // the field worth carrying: mixing the generation into `request_key`
        // already keeps a stale memo *entry* unreachable, and this keeps a stale
        // *walk* from being used by a caller that is holding one.
        if self.with_state(|state| state.generation) != instance.generation {
            let request = instance.request.as_request();
            return self.face_for(&self.resolve(&request), character);
        }
        self.with_state(|state| {
            let matched = state
                .table
                .face_for(&instance.walk, character, |family, face| {
                    has_glyph(state.loaded, family, face, character)
                })?;
            let font = state.loaded.get(matched.family)?.get(matched.face)?;
            Some((Arc::clone(font), matched.embolden, matched.shear_degrees))
        })
    }
}

/// The installed half of the backend's state, as one borrow.
struct FontStateSnapshot<'a> {
    table: &'a FaceTable,
    loaded: &'a [Vec<Arc<Font>>],
    generation: u64,
}

/// Whether the font at `(family, face)` has a glyph for `character`.
///
/// §5.2: "A font is considered to support a given character if (1) the character
/// is contained in the font's character map". The other half of that test -
/// §4.5's `unicode-range` - is applied inside [`FaceTable::face_for`], because it
/// is a property of the table and not of any one font file.
fn has_glyph(loaded: &[Vec<Arc<Font>>], family: usize, face: usize, character: char) -> bool {
    loaded
        .get(family)
        .and_then(|faces| faces.get(face))
        .is_some_and(|font| font.lookup_glyph_index(character) != 0)
}

/// The advance of one character in one face.
///
/// Measurement and shaping both come through here, so the advance a character
/// contributes to a line box and the advance its glyph is painted with are the
/// same number, from the same face, for the same request. §2.8's synthesis does
/// not change it, which is why a synthesized bold run is not re-measured after
/// the fact: a stroke around a glyph does not move the next one.
fn advance_of(font: &Font, character: char, font_size: f32) -> f32 {
    font.metrics(character, font_size).advance_width
}

/// The glyph id that stands for a character no installed face covers. It is
/// glyph 0 in every font: the `.notdef` box, which is what a browser draws for a
/// missing character. Real glyph ids here are codepoints, and U+0000 is never
/// shaped as text (the HTML parser replaces it), so the id is unambiguous.
const MISSING_GLYPH: u32 = 0;

/// The advance of the `.notdef` box of `font`, used for a missing character.
fn missing_advance(font: &Font, font_size: f32) -> f32 {
    font.metrics_indexed(0, font_size).advance_width
}

/// The ascent and descent of a face, falling back to the reference path's
/// nominal values for a font with no line metrics.
fn line_metrics(font: &Font, font_size: f32) -> (f32, f32) {
    font.horizontal_line_metrics(font_size).map_or_else(
        || {
            let face = NominalFace::Proportional;
            (face.ascent_em() * font_size, face.descent_em() * font_size)
        },
        |lines| (lines.ascent, -lines.descent),
    )
}

impl TextMeasurer for SystemFontBackend {
    fn measure(&self, text: &str, style: TextStyle<'_>) -> TextMeasure {
        let instance = self.resolve(&style.font);
        let mut advance = 0.0_f32;
        let mut ascent = 0.0_f32;
        let mut descent = 0.0_f32;
        // An empty run still needs a face for its line metrics, and §5 itself
        // names the character to ask about: "The first available font, used for
        // example in the definition of font-relative lengths such as ex or in
        // the definition of the line-height property, is defined to be the first
        // font for which the character U+0020 (space) is not excluded".
        let characters: Vec<char> = if text.is_empty() {
            vec![' ']
        } else {
            text.chars().collect()
        };
        for character in characters {
            // A character no face has is drawn as the missing-glyph box, with the
            // advance of that box, which is what shaping uses too.
            let (font, character_advance) =
                if let Some((font, _, _)) = self.face_for(&instance, character) {
                    let character_advance = advance_of(&font, character, style.font_size);
                    (font, character_advance)
                } else {
                    let Some(missing) = self.missing_glyph_face() else {
                        continue;
                    };
                    let character_advance = missing_advance(&missing, style.font_size);
                    (missing, character_advance)
                };
            if !text.is_empty() {
                advance += character_advance;
            }
            let (face_ascent, face_descent) = line_metrics(&font, style.font_size);
            ascent = ascent.max(face_ascent);
            descent = descent.max(face_descent);
        }
        if ascent == 0.0 && descent == 0.0 {
            ascent = style.font_size * 0.8;
            descent = style.font_size * 0.2;
        }
        TextMeasure {
            advance,
            ascent,
            descent,
        }
    }
}

impl TextShaper for SystemFontBackend {
    fn shape(&self, text: &str, font_size: f32, origin: PhysicalPoint, color: Color) -> GlyphRun {
        self.shape_font(text, &FontRequest::initial(), font_size, origin, color)
    }

    fn shape_font(
        &self,
        text: &str,
        font: &FontRequest<'_>,
        font_size: f32,
        origin: PhysicalPoint,
        color: Color,
    ) -> GlyphRun {
        let instance = self.resolve(font);
        let mut x = origin.x;
        let mut glyphs = Vec::new();
        for character in text.chars() {
            let (glyph, advance) = if let Some((loaded, _, _)) = self.face_for(&instance, character)
            {
                (
                    GlyphId(character as u32),
                    advance_of(&loaded, character, font_size),
                )
            } else {
                let Some(missing) = self.missing_glyph_face() else {
                    continue;
                };
                (GlyphId(MISSING_GLYPH), missing_advance(&missing, font_size))
            };
            glyphs.push(GlyphInstance {
                glyph,
                position: PhysicalPoint { x, y: origin.y },
                advance,
            });
            x += advance;
        }
        GlyphRun {
            // The run's id is minted from the table it was shaped against, so a
            // rasterised mask can only ever be looked up under the resolution
            // that produced it. A face arriving between two runs gives the second
            // run a different id, so the second run cannot be painted with the
            // first run's glyphs.
            font: self.instance_id_for(font),
            font_size,
            color,
            glyphs,
        }
    }
}

impl GlyphMaskProvider for SystemFontBackend {
    fn mask(&self, font: FontInstanceId, glyph: GlyphId, font_size: f32) -> Option<GlyphMask> {
        self.shared_mask(font, glyph, font_size)
            .map(|mask| (*mask).clone())
    }

    fn shared_mask(
        &self,
        font: FontInstanceId,
        glyph: GlyphId,
        font_size: f32,
    ) -> Option<Arc<GlyphMask>> {
        let key = (font, glyph, font_size.to_bits());
        if let Some(mask) = self.glyph_cache.lock().ok()?.get(&key).map(Arc::clone) {
            return Some(mask);
        }
        // An id that was never shaped has no resolution, and so no mask. A mask
        // is only ever asked for a glyph a run named, and naming a run is what
        // registers the instance.
        let instance = self.resolved(font)?;
        // The codepoint the run named is the codepoint that is looked up. There
        // is no substitution step ahead of §5 any more - see
        // [`is_private_use`] for why the per-site table that used to be here is
        // gone - so §5.4's Private Use Area rule governs the character the author
        // actually wrote.
        let (embolden, shear_degrees, metrics, coverage) = if glyph.0 == MISSING_GLYPH {
            let face = self.missing_glyph_face()?;
            let (metrics, coverage) = face.rasterize_indexed(0, font_size);
            (false, 0.0, metrics, coverage)
        } else {
            let character = char::from_u32(glyph.0)?;
            let (loaded, embolden, shear_degrees) = self.face_for(&instance, character)?;
            let (metrics, coverage) = loaded.rasterize(character, font_size);
            (embolden, shear_degrees, metrics, coverage)
        };
        let width = u32::try_from(metrics.width).ok()?;
        let height = u32::try_from(metrics.height).ok()?;
        let coverage = if embolden {
            dilate_coverage(&coverage, width, height, SYNTHETIC_BOLD_DILATION_PX)
        } else {
            coverage
        };
        let coverage = if shear_degrees.abs() > f32::EPSILON {
            shear_coverage(&coverage, width, height, shear_degrees)?
        } else {
            coverage
        };
        let mask = Arc::new(GlyphMask {
            width,
            height,
            left: metrics.xmin,
            top: metrics
                .ymin
                .checked_add(i32::try_from(metrics.height).ok()?)?,
            coverage,
        });
        let mut cache = self.glyph_cache.lock().ok()?;
        if let Some(cached) = cache.get(&key) {
            return Some(Arc::clone(cached));
        }
        if cache.len() < MAX_CACHED_GLYPHS {
            cache.insert(key, Arc::clone(&mask));
        }
        Some(mask)
    }
}

impl TextPainter for SystemFontBackend {
    /// Measures native chrome text.
    ///
    /// The window frame, the tab strip and the toolbar are not web content: no
    /// cascade reaches them, so they have no `font-family`, `font-weight` or
    /// `font-style` to select on and this trait correctly stays as it is. The
    /// request used is the one an element that declared nothing makes, which is
    /// what makes chrome text the platform's own interface face.
    fn measure(&self, text: &str, size: f32) -> f32 {
        let instance = self.resolve(&FontRequest::initial());
        text.chars()
            .map(|character| {
                self.face_for(&instance, character)
                    .map_or(0.0, |(font, _, _)| advance_of(&font, character, size))
            })
            .sum()
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "native chrome coordinates are finite and bounded by the window"
    )]
    fn paint(&self, canvas: &mut Canvas<'_>, text: &str, origin: Point, size: f32, color: u32) {
        let instance = self.resolve(&FontRequest::initial());
        let mut x = origin.x;
        for character in text.chars() {
            let Some((loaded, embolden, shear_degrees)) = self.face_for(&instance, character)
            else {
                continue;
            };
            let (metrics, coverage) = loaded.rasterize(character, size);
            let (Ok(width), Ok(height)) =
                (u32::try_from(metrics.width), u32::try_from(metrics.height))
            else {
                continue;
            };
            let mut coverage = if embolden {
                dilate_coverage(&coverage, width, height, SYNTHETIC_BOLD_DILATION_PX)
            } else {
                coverage
            };
            if shear_degrees.abs() > f32::EPSILON
                && let Some(sheared) = shear_coverage(&coverage, width, height, shear_degrees)
            {
                coverage = sheared;
            }
            let (ascent, _) = line_metrics(&loaded, size);
            let glyph_height = i32::try_from(metrics.height).unwrap_or(i32::MAX);
            let top = origin.y + ascent - metrics.ymin.saturating_add(glyph_height) as f32;
            canvas.blend_mask(
                x.round() as i32 + metrics.xmin,
                top.round() as i32,
                width,
                &coverage,
                color,
            );
            x += metrics.advance_width;
        }
    }
}

/// §2.8's "drawing a thin stroke around each glyph": a dilation of the
/// rasterized coverage by `radius` pixels in each direction.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "a glyph mask is a few thousand pixels of coverage, far inside isize"
)]
fn dilate_coverage(coverage: &[u8], width: u32, height: u32, radius: u32) -> Vec<u8> {
    let width_usize = usize::try_from(width).unwrap_or(0);
    let height_usize = usize::try_from(height).unwrap_or(0);
    if coverage.len() != width_usize.saturating_mul(height_usize) || radius == 0 {
        return coverage.to_vec();
    }
    let radius = radius as isize;
    #[allow(
        clippy::cast_possible_wrap,
        reason = "a glyph mask row index is far inside isize"
    )]
    let width_signed = width_usize as isize;
    #[allow(
        clippy::cast_possible_wrap,
        reason = "a glyph mask row index is far inside isize"
    )]
    let height_signed = height_usize as isize;
    let mut dilated = vec![0_u8; coverage.len()];
    for y in 0..height_usize {
        #[allow(
            clippy::cast_possible_wrap,
            reason = "a glyph mask row index is far inside isize"
        )]
        let y_signed = y as isize;
        for x in 0..width_usize {
            #[allow(
                clippy::cast_possible_wrap,
                reason = "a glyph mask column index is far inside isize"
            )]
            let x_signed = x as isize;
            let mut best = 0_u8;
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let sample_x = x_signed + dx;
                    let sample_y = y_signed + dy;
                    if sample_x < 0
                        || sample_y < 0
                        || sample_x >= width_signed
                        || sample_y >= height_signed
                    {
                        continue;
                    }
                    let sample = coverage[sample_y as usize * width_usize + sample_x as usize];
                    best = best.max(sample);
                }
            }
            dilated[y * width_usize + x] = best;
        }
    }
    dilated
}

/// §2.8's "geometrical shearing of each glyph": every scanline of the rasterized
/// mask moves sideways by the tangent of the angle times its distance from the
/// baseline.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "a glyph mask is a few thousand pixels and an oblique angle is a small \
              degree count; both are far inside the range the other holds"
)]
fn shear_coverage(coverage: &[u8], width: u32, height: u32, degrees: f32) -> Option<Vec<u8>> {
    if !degrees.is_finite() || degrees.abs() <= f32::EPSILON {
        return Some(coverage.to_vec());
    }
    let width_usize = usize::try_from(width).ok()?;
    let height_usize = usize::try_from(height).ok()?;
    if coverage.len() != width_usize.checked_mul(height_usize)? {
        return Some(coverage.to_vec());
    }
    let tangent = degrees.to_radians().tan();
    let width_signed = width_usize as isize;
    let mut sheared = vec![0_u8; coverage.len()];
    for y in 0..height_usize {
        // Rows are counted from the top of the mask, and §2.4 calls a positive
        // angle a clockwise slant, so the top of the glyph moves right.
        let from_baseline = height_usize.saturating_sub(y) as f32;
        let shift = (tangent * from_baseline).round() as isize;
        for x in 0..width_usize {
            let source = x as isize - shift;
            if source < 0 || source >= width_signed {
                continue;
            }
            sheared[y * width_usize + x] = coverage[y * width_usize + source as usize];
        }
    }
    Some(sheared)
}

/// Loads the installed faces: the declared table, the fonts behind it, and the
/// `local()` names §4.3.3.1 matches against.
fn load_installed_faces() -> InstalledFaces {
    let mut families: Vec<Family> = Vec::new();
    let mut loaded: Vec<Vec<Arc<Font>>> = Vec::new();
    let mut local_names: Vec<Vec<&'static [&'static str]>> = Vec::new();
    let mut family_index: HashMap<&'static str, usize> = HashMap::new();
    for source in PLATFORM_FACES {
        let Ok(bytes) = read_face_file(source) else {
            continue;
        };
        let settings = FontSettings {
            collection_index: source.collection_index,
            ..FontSettings::default()
        };
        let Ok(font) = Font::from_bytes(bytes, settings) else {
            continue;
        };
        let family = if let Some(index) = family_index.get(source.family) {
            *index
        } else {
            families.push(Family {
                name: source.family.to_owned(),
                aliases: source
                    .aliases
                    .iter()
                    .map(|alias| (*alias).to_owned())
                    .collect(),
                generics: source.generics.to_vec(),
                faces: Vec::new(),
            });
            let index = families.len() - 1;
            family_index.insert(source.family, index);
            loaded.push(Vec::new());
            local_names.push(Vec::new());
            index
        };
        families[family]
            .faces
            .push(Face::new(source.weight, source.style));
        loaded[family].push(Arc::new(font));
        local_names[family].push(source.local_names);
    }
    let fallback = FALLBACK_FAMILIES
        .iter()
        .filter_map(|name| family_index.get(name).copied())
        .collect();
    InstalledFaces {
        loaded,
        families,
        local_names,
        fallback,
    }
}

fn read_face_file(source: &FaceSource) -> io::Result<Vec<u8>> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no candidate file");
    for file in source.files {
        let path = Path::new(file);
        let found = if path.is_absolute() {
            fs::read(path)
        } else {
            font_directories()
                .into_iter()
                .find_map(|directory| fs::read(directory.join(file)).ok())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("no font directory holds {file}"),
                    )
                })
        };
        match found {
            Ok(bytes) => return Ok(bytes),
            Err(error) => last = error,
        }
    }
    Err(last)
}

fn font_directories() -> Vec<PathBuf> {
    let mut windows = Vec::new();
    if let Some(windir) = env::var_os("WINDIR") {
        windows.push(Path::new(&windir).join("Fonts"));
    }
    windows.push(PathBuf::from(r"C:\Windows\Fonts"));
    let mut seen = BTreeSet::new();
    windows.retain(|path| seen.insert(path.clone()));
    windows
}

#[cfg(test)]
mod tests {
    use super::{
        FALLBACK_FAMILIES, PLATFORM_FACES, SystemFontBackend, dilate_coverage, has_glyph,
        read_face_file, shear_coverage,
    };
    use crate::font_faces::DocumentFonts;
    use crate::font_matching::is_private_use;
    use fontdue::Font;
    use render_core::layout::{
        FontRequest, FontStyle, GenericFamily, PhysicalPoint, TextMeasurer, TextStyle,
        generic_family, is_wide_character,
    };
    use render_core::paint::{Color, GlyphId, GlyphRun, TextShaper};
    use std::sync::Arc;

    /// The total coverage of a mask, for the assertions below.
    fn ink(coverage: &[u8]) -> u32 {
        coverage.iter().map(|value| u32::from(*value)).sum()
    }

    #[test]
    fn every_platform_family_offers_a_regular_and_a_bold_face() {
        for family in ["Segoe UI", "Arial", "DejaVu Sans", "Liberation Sans"] {
            let faces: Vec<_> = PLATFORM_FACES
                .iter()
                .filter(|source| source.family == family)
                .collect();
            assert!(
                faces.iter().any(|source| source.weight == 400),
                "{family} has no regular face in the platform table"
            );
            assert!(
                faces.iter().any(|source| source.weight == 700),
                "{family} has no bold face in the platform table, so `font-weight: 700` \
                 would have nothing real to select and would depend on synthesis alone"
            );
        }
    }

    #[test]
    fn the_always_mapped_generic_families_resolve_to_a_declared_family() {
        // §2.1.5: "serif must always map to at least one matched font face", and
        // the same sentence for sans-serif and monospace.
        for generic in [
            GenericFamily::Serif,
            GenericFamily::SansSerif,
            GenericFamily::Monospace,
            GenericFamily::SystemUi,
        ] {
            assert!(
                PLATFORM_FACES
                    .iter()
                    .any(|source| source.generics.contains(&generic)),
                "{generic:?} resolves to nothing in the platform table"
            );
        }
    }

    #[test]
    fn system_ui_resolves_to_the_same_family_as_sans_serif() {
        // The engine's initial `font-family` is `system-ui`, so the first family
        // that claims it is what an element that declared no family gets, and
        // that must be the same face `sans-serif` names.
        let claims = |wanted: GenericFamily| {
            PLATFORM_FACES
                .iter()
                .find(|source| source.generics.contains(&wanted))
                .map(|source| source.family)
        };
        assert_eq!(
            claims(GenericFamily::SystemUi),
            claims(GenericFamily::SansSerif),
            "the initial family and `sans-serif` must resolve to the same face"
        );
    }

    #[test]
    fn the_fallback_families_all_exist_in_the_table() {
        for name in FALLBACK_FAMILIES {
            assert!(
                PLATFORM_FACES.iter().any(|source| source.family == *name),
                "installed font fallback names {name}, which the table does not declare"
            );
        }
    }

    #[test]
    fn a_declared_alias_is_never_a_generic_family_keyword() {
        // §2.1.2: a generic keyword written unquoted is classified as a keyword
        // and never as a name, so an alias that spells one is table data nothing
        // can ever reach. It is checked because an unreachable row is a row whose
        // author believed it did something.
        for source in PLATFORM_FACES {
            for alias in source.aliases {
                assert!(
                    generic_family(alias).is_none(),
                    "{} lists the generic keyword {alias} as an alias, which is \
                     unreachable because an unquoted keyword is never a name",
                    source.family
                );
            }
        }
    }

    #[test]
    fn a_family_declares_each_of_its_faces_exactly_once() {
        // §5.2 assembles "the set of font faces in that family", so a family with
        // two declarations of one face has a duplicate face rather than a
        // platform variant. One row per face is what the table's shape enforces.
        for (index, source) in PLATFORM_FACES.iter().enumerate() {
            for other in &PLATFORM_FACES[index + 1..] {
                let duplicated = source.family == other.family
                    && source.weight == other.weight
                    && source.style == other.style;
                assert!(
                    !duplicated,
                    "{} is declared twice at weight {} and style {:?}",
                    source.family, source.weight, source.style
                );
            }
        }
    }

    #[test]
    fn every_declared_file_is_claimed_by_exactly_one_face() {
        for (index, source) in PLATFORM_FACES.iter().enumerate() {
            for other in &PLATFORM_FACES[index + 1..] {
                for file in source.files {
                    assert!(
                        !other.files.contains(file),
                        "{file} is claimed by both {} and {}",
                        source.family,
                        other.family
                    );
                }
            }
        }
    }

    /// A vertical bar one pixel wide, in a mask wide enough that a 45 degree
    /// shear of its top row still lands inside the mask.
    fn vertical_bar() -> Vec<u8> {
        let mut coverage = vec![0_u8; 16 * 4];
        for row in 0..4 {
            coverage[row * 16 + 8] = 255;
        }
        coverage
    }

    #[test]
    fn synthetic_bold_adds_ink_without_changing_the_mask_size() {
        let mut source = vec![0_u8; 16];
        source[9] = 255;
        let dilated = dilate_coverage(&source, 4, 4, 1);
        assert_eq!(dilated.len(), source.len(), "the mask size cannot change");
        assert_eq!(
            dilated.iter().filter(|value| **value > 0).count(),
            9,
            "one pixel of ink dilated by one pixel in each direction is a 3x3 block"
        );
        assert!(
            dilated
                .iter()
                .filter(|value| **value > 0)
                .all(|value| *value == 255),
            "and the block is as dark as the pixel it grew from"
        );
    }

    #[test]
    fn synthetic_bold_takes_the_darkest_neighbour_and_never_sums() {
        let coverage = vec![0_u8, 64, 128, 255, 255, 128, 64, 0];
        let dilated = dilate_coverage(&coverage, 4, 2, 1);
        assert_eq!(
            dilated[0], 255,
            "the darkest neighbour wins, and a maximum cannot exceed the source"
        );
        assert!(
            ink(&dilated) <= 8 * 255,
            "dilation is a maximum, so the total ink cannot exceed one full-coverage mask"
        );
    }

    #[test]
    fn a_zero_angle_shear_is_the_identity() {
        let coverage = vec![1_u8, 2, 3, 4];
        assert_eq!(shear_coverage(&coverage, 2, 2, 0.0), Some(coverage));
    }

    #[test]
    fn a_positive_angle_leans_the_top_of_the_glyph_right() {
        // §2.4: "Positive angles represent a clockwise slant", and CSS's y axis
        // points down, so the top of the glyph moves right and the row nearest
        // the baseline moves least.
        let sheared = shear_coverage(&vertical_bar(), 16, 4, 45.0).expect("a finite angle shears");
        assert_eq!(
            ink(&sheared),
            ink(&vertical_bar()),
            "shearing moves ink, it never adds any"
        );
        assert_eq!(
            sheared[12], 255,
            "the top row, at offset zero, moved four columns right"
        );
        assert_eq!(
            sheared[3 * 16 + 9],
            255,
            "the last row moved one column right"
        );
    }

    #[test]
    fn a_negative_angle_leans_the_top_of_the_glyph_left() {
        let sheared = shear_coverage(&vertical_bar(), 16, 4, -45.0).expect("a finite angle shears");
        assert_eq!(ink(&sheared), ink(&vertical_bar()));
        assert_eq!(
            sheared[4], 255,
            "the top row, at offset zero, moved four columns left"
        );
        assert_eq!(
            sheared[3 * 16 + 7],
            255,
            "the last row moved one column left"
        );
    }

    #[test]
    fn a_shear_that_would_move_ink_off_the_mask_drops_it() {
        let coverage = vec![1_u8; 12];
        let sheared = shear_coverage(&coverage, 4, 3, 80.0).expect("a finite angle shears");
        assert!(
            ink(&sheared) < 12,
            "a steep shear past the mask edge must not wrap"
        );
    }

    #[test]
    fn a_non_finite_angle_is_left_alone_rather_than_producing_garbage() {
        let coverage = vec![7_u8; 4];
        assert_eq!(shear_coverage(&coverage, 2, 2, f32::NAN), Some(coverage));
    }

    /// The replacement for `maps_private_use_site_icons_to_local_glyphs`, and the
    /// reasoning.
    ///
    /// The old test asserted that U+E610 maps to `×`, U+E613 to `▾`, U+E619 to
    /// `↻` and U+E62E to `热`. Each assertion was a claim about one site's icon
    /// font compiled into the engine, and each was also a claim the author never
    /// made: the page asked for a private-use codepoint, and the engine answered
    /// with a letterform from an unrelated script. A private-use codepoint has no
    /// Unicode meaning, so any mapping is an invention, and inventing one is the
    /// project defect that `tools/check_site_neutrality.py` looks for in a
    /// different shape.
    ///
    /// What replaces it is the specification's own answer. §5.4 says a private-use
    /// codepoint "must only match font families named in the `font-family` list
    /// that are not generic families" - which a `@font-face` icon family is - and
    /// §5 says a character no face has is indicated as not displayed. So the
    /// codepoint the author wrote is the codepoint that is looked up, and the only
    /// thing that can give it a glyph is a face the page itself supplied.
    #[test]
    fn a_private_use_codepoint_is_looked_up_as_itself_and_never_as_a_letterform() {
        // The property the previous round's pre-step existed to protect: §5.4's
        // rule governs the character actually looked up. It is now the whole of
        // the behaviour rather than a pre-step's precondition, so the assertion is
        // that the codepoint reaches §5 unchanged.
        for codepoint in [
            '\u{e602}', '\u{e610}', '\u{e613}', '\u{e619}', '\u{e625}', '\u{e62e}',
        ] {
            assert!(
                is_private_use(codepoint),
                "{codepoint:?} is a Private Use Area codepoint, so §5.4 governs it"
            );
        }
        assert!(
            !is_private_use('A') && !is_private_use('×') && !is_private_use('热'),
            "and nothing outside the area is treated as one, so the rule is \
             about the codepoint and not about a list of interesting characters"
        );
        // The five codepoints the old table claimed to know are the ones §5.4 now
        // refuses to match against a generic family, rather than the ones the
        // engine rewrote into Latin and CJK letterforms.
        assert!(
            is_private_use('\u{e62e}') && !is_wide_character('\u{e62e}'),
            "a private-use codepoint is not a CJK wide character either, so the \
             line breaker and the matcher agree about what it is"
        );
    }

    #[test]
    fn a_private_use_codepoint_reaches_a_named_document_family_and_no_generic_one() {
        // §5.4, checked through the real table rather than through a table built
        // for the purpose: the only thing that changed to make icon fonts work is
        // that a document face is now in the table, and §5.4 is what lets it be
        // selected for a Private Use Area codepoint.
        let backend = super::SystemFontBackend::load().expect("the platform table loads");
        // With no document faces at all, a private-use codepoint resolves to
        // nothing: §5.4 forbids a generic family, and the installed fallback is
        // exactly that. The character is then not displayed, which is §5's last
        // resort rather than a substituted letterform.
        let request = FontRequest::of_family("sans-serif");
        let instance = backend.resolve(&request);
        assert!(
            backend.face_for(&instance, '\u{e610}').is_none(),
            "§5.4 forbids a generic family for a Private Use Area codepoint, and \
             §5 then indicates the character is not displayed"
        );
    }

    /// The one thing that must never be true of a real font backend: the width
    /// layout measured a run at and the advances its glyphs are drawn with come
    /// from different faces.
    ///
    /// This is the property `SYNTHETIC_BOLD_DILATION_PX` and the shear exist to
    /// preserve - both change what a glyph looks like and neither changes how much
    /// room it takes - and it is checked per character, not just in total, so a
    /// per-run mismatch cannot hide inside a pair of cancelling ones.
    #[test]
    fn the_face_measured_is_the_face_painted() {
        use render_core::layout::{
            FontRequest, FontStyle, FontSynthesis, TextMeasure, TextMeasurer, TextStyle,
        };
        use render_core::paint::{Color, GlyphRun, TextShaper};

        let backend = super::SystemFontBackend::load().expect("the platform table loads");
        assert!(
            backend.loaded_face_count() > 0,
            "`load` reports a platform with no declared face as an error, so \
             reaching here with none would mean the table and the loader disagree"
        );

        let text = "Rendering AWAY \u{6e32}\u{67d3} 019";
        for (family, weight, style) in [
            ("system-ui", 400_u16, FontStyle::Normal),
            ("sans-serif", 700, FontStyle::Normal),
            ("monospace", 400, FontStyle::Normal),
            ("serif", 700, FontStyle::Italic),
            ("\"No Such Font\", monospace", 700, FontStyle::Normal),
            ("\"No Such Font\", monospace", 900, FontStyle::Oblique(14.0)),
            ("system-ui", 700, FontStyle::Oblique(14.0)),
        ] {
            let request = FontRequest {
                family,
                weight,
                style,
                synthesis: FontSynthesis::default(),
            };
            let measured: TextMeasure = backend.measure(
                text,
                TextStyle {
                    font_size: 24.0,
                    line_height: 28.8,
                    font: request,
                },
            );
            let GlyphRun { glyphs, .. } = backend.shape_font(
                text,
                &request,
                24.0,
                render_core::layout::PhysicalPoint { x: 0.0, y: 0.0 },
                Color::BLACK,
            );
            let drawn: f32 = glyphs.iter().map(|glyph| glyph.advance).sum();
            assert!(
                (measured.advance - drawn).abs() < 1e-3,
                "{family} at weight {weight} and style {style:?} measured {text:?} at \
                 {} but shaped it at {drawn}",
                measured.advance
            );
            // One glyph per character. A character no installed face covers is the
            // missing-glyph box, so every glyph is its own character or that box.
            assert_eq!(
                glyphs.len(),
                text.chars().count(),
                "{family}: a run keeps one glyph per character"
            );
            for (character, glyph) in text.chars().zip(&glyphs) {
                assert!(
                    glyph.glyph.0 == character as u32 || glyph.glyph.0 == super::MISSING_GLYPH,
                    "character {character:?} is painted as glyph {}",
                    glyph.glyph.0
                );
            }
        }
    }

    /// A character no installed face covers is drawn as the missing-glyph box:
    /// it keeps its place in the run, takes the box's advance, and paints ink.
    #[test]
    fn a_character_no_face_covers_is_drawn_as_the_missing_glyph_box() {
        use render_core::layout::{FontRequest, TextMeasurer, TextStyle};
        use render_core::paint::{Color, GlyphMaskProvider, TextShaper};

        let backend = super::SystemFontBackend::load().expect("the platform table loads");
        // A noncharacter that no font maps.
        let text = "A\u{10FFFF}B";
        let request = FontRequest::initial();
        let run = backend.shape_font(
            text,
            &request,
            24.0,
            render_core::layout::PhysicalPoint { x: 0.0, y: 0.0 },
            Color::BLACK,
        );
        assert_eq!(run.glyphs.len(), 3, "the missing character keeps its place");
        assert_eq!(run.glyphs[1].glyph.0, super::MISSING_GLYPH);
        assert!(run.glyphs[1].advance > 0.0, "the box has an advance");

        let measured = backend.measure(
            text,
            TextStyle {
                font_size: 24.0,
                line_height: 28.8,
                font: request,
            },
        );
        let drawn: f32 = run.glyphs.iter().map(|glyph| glyph.advance).sum();
        assert!(
            (measured.advance - drawn).abs() < 1e-3,
            "measured {} but drew {drawn}",
            measured.advance
        );

        let mask = backend
            .mask(
                run.font,
                render_core::paint::GlyphId(super::MISSING_GLYPH),
                24.0,
            )
            .expect("the missing glyph has a mask");
        assert!(
            mask.coverage.iter().any(|coverage| *coverage > 0),
            "the missing-glyph box paints ink"
        );
    }

    // ---- the webfont path, end to end ---------------------------------------

    /// A declared face's file, read off disk, for a document face to be built
    /// from.
    ///
    /// The test uses a *real* font file rather than a synthetic table, because the
    /// claim being checked is that a document face's own metrics are the ones
    /// measured and painted, and a synthetic face has no metrics to be wrong
    /// about. The row is chosen to be a *monospace* face where one is declared,
    /// and the tests do not rely on its advances differing from the platform
    /// fallback's - they compare against the face's own metrics and against the
    /// family index the matcher selected, both of which are exact.
    fn a_decodable_face_file() -> (Vec<u8>, &'static str) {
        for want_monospace in [true, false] {
            for source in PLATFORM_FACES {
                if source.weight != 400
                    || source.style != FontStyle::Normal
                    || source.generics.contains(&GenericFamily::Monospace) != want_monospace
                {
                    continue;
                }
                if let Ok(bytes) = read_face_file(source) {
                    return (bytes, source.family);
                }
            }
        }
        panic!("the platform table declares a regular face on every supported platform");
    }

    /// The index of the family the §5 walk selected for one character, read
    /// through the matcher the pipeline uses.
    ///
    /// Asserting on the index rather than on the advances is what makes
    /// "the document's face was selected" a checkable claim on any platform: which
    /// face answered is a fact about the table, and the advances that face has are
    /// compared against that face's own metrics separately.
    fn selected_family(
        backend: &SystemFontBackend,
        request: &FontRequest<'_>,
        character: char,
    ) -> usize {
        backend.with_state(|state| {
            let instance = backend.resolve(request);
            state
                .table
                .face_for(&instance.walk, character, |family, face| {
                    has_glyph(state.loaded, family, face, character)
                })
                .expect("the walk finds a face for a character the platform covers")
                .family
        })
    }

    /// A stylesheet declaring one document face over `bytes`, as a fetched sheet
    /// would.
    fn document_fonts_with(
        bytes: &[u8],
        family: &str,
        unicode_range: Option<&str>,
    ) -> (DocumentFonts, render_net::Url) {
        let base =
            render_net::Url::parse("https://example.test/page.css").expect("a parseable base URL");
        let url = render_net::Url::parse("https://example.test/fonts/face.ttf")
            .expect("a parseable font URL");
        let range =
            unicode_range.map_or(String::new(), |range| format!("unicode-range: {range}; "));
        let source = format!(
            "@font-face {{ font-family: '{family}'; font-weight: 400; font-style: normal; \
             {range}src: url({url}) format(truetype); }}"
        );
        let (sheet, _problems) = render_core::font_face::parse_font_faces(&source);
        let mut document = DocumentFonts::new();
        document.add_stylesheet(&sheet, &base);
        document
            .install(&url, bytes)
            .expect("a declared face file decodes");
        (document, url)
    }

    /// The advance one character contributes under `request`, in pixels.
    fn advance(backend: &SystemFontBackend, request: &FontRequest<'_>, text: &str) -> f32 {
        backend
            .measure(
                text,
                TextStyle {
                    font_size: 24.0,
                    line_height: 28.8,
                    font: *request,
                },
            )
            .advance
    }

    /// The test of whether this round worked.
    ///
    /// The previous round's `the_face_measured_is_the_face_painted` proved that
    /// measurement and painting reach the same face for an *installed* face. This
    /// is the same assertion for a face that arrived over the network, and it is
    /// the one that matters here: a document face is only real if the advances
    /// the measurer returns are that face's advances.
    ///
    /// It is checked three ways, because "the metrics are the document face's" can
    /// fail in three different ways and only one of them is the interesting one:
    ///
    /// - The run's per-character advances must be the document face's, not a
    ///   nominal fallback's. A font with a distinctly different advance for the
    ///   chosen characters is used, and the measurement is compared against the
    ///   same face's metrics read directly.
    /// - The mask must come from the same face, checked by asking for the glyph's
    ///   mask and comparing its ink to a rasterisation of the same character
    ///   through the same backend.
    /// - A face that is *not* in the document must change the answer, or the first
    ///   two assertions prove nothing.
    #[test]
    fn a_document_face_is_measured_and_painted_with_its_own_metrics() {
        use render_core::paint::GlyphMaskProvider;

        let backend = SystemFontBackend::load().expect("the platform table loads");
        let (bytes, installed_family) = a_decodable_face_file();
        // A family name no installed face answers to, so anything measured under
        // it can only have come from the document.
        let webfont = "Webfont Under Test";
        let (document, _url) = document_fonts_with(&bytes, webfont, None);
        let request = FontRequest::of_family(webfont);
        let origin = PhysicalPoint { x: 0.0, y: 0.0 };

        // Nothing installed: the family does not exist yet, so the request walks
        // on to the initial family and the run is the platform's. Recording which
        // family answered is what makes the later comparison exact.
        assert_eq!(
            backend.document_family_count(),
            0,
            "a fresh backend has no document families at all"
        );
        let before_family = selected_family(&backend, &request, 'H');
        assert!(
            before_family >= backend.document_family_count(),
            "an undeclared family is not a family, so the walk falls through to an \
             installed one (index {before_family} of {} document families)",
            backend.document_family_count()
        );
        let _ = installed_family;

        backend.set_document_faces(&document);

        // Now the family exists, and the run must be laid out with the document
        // face's advances.
        let document_advance = advance(&backend, &request, "Handgloves 019");
        let GlyphRun { glyphs, .. } =
            backend.shape_font("Handgloves 019", &request, 24.0, origin, Color::BLACK);
        let drawn: f32 = glyphs.iter().map(|glyph| glyph.advance).sum();
        assert!(
            (document_advance - drawn).abs() < 1e-3,
            "the run measured at {document_advance} and was shaped at {drawn}, so \
             layout and paint disagree about the same request"
        );
        assert_eq!(
            glyphs.len(),
            "Handgloves 019".chars().count(),
            "every character was given a face, so the document face really is the \
             one that was selected"
        );

        // And the document face's advance is the face's own: the same file, read
        // directly, gives the same numbers. A nominal fallback's advance would
        // differ, which is what makes this an assertion rather than a tautology.
        let face_font = document_face_font(&backend, &request);
        let direct: f32 = "Handgloves 019"
            .chars()
            .map(|character| face_font.metrics(character, 24.0).advance_width)
            .sum();
        assert!(
            (document_advance - direct).abs() < 1e-3,
            "the measurer returned {document_advance} but the downloaded face's own \
             advances for the same characters at the same size total {direct}"
        );
        // Which face that is, is checked by index rather than by number, so the
        // assertion is exact on every platform even when the two faces happen to
        // share an advance for the chosen characters. Together with
        // `before_family >= 0` above, the two range assertions *are* the claim
        // that the selection changed: the family that answered is an installed one
        // before the document's faces are installed and a document one after.
        // Comparing the two indices directly would not, because the document half
        // occupies the leading indices and an installed family that happened to
        // be at the front would share one.
        let after_family = selected_family(&backend, &request, 'H');
        assert!(
            after_family < backend.document_family_count(),
            "the selected family is the document's (index {after_family}), so the \
             advances compared above are the document face's and not a fallback's"
        );

        // The mask comes from the same face: rasterising the same character
        // through the same backend, with no face substituted, gives the same ink.
        let id = SystemFontBackend::instance_id_for(&backend, &request);
        for character in ['H', 'g', '0'] {
            let mask = backend
                .shared_mask(id, GlyphId(character as u32), 24.0)
                .expect("a face that renders the character has a mask for it");
            let (metrics, coverage) = face_font.rasterize(character, 24.0);
            assert_eq!(
                mask.coverage, coverage,
                "the painted mask for {character:?} is not the mask of the \
                 downloaded face"
            );
            assert_eq!(
                (
                    mask.left,
                    mask.top,
                    i32::try_from(mask.width).expect("a glyph fits an i32"),
                    i32::try_from(mask.height).expect("a glyph fits an i32")
                ),
                (
                    metrics.xmin,
                    metrics
                        .ymin
                        .checked_add(i32::try_from(metrics.height).expect("a glyph fits an i32"))
                        .expect("a glyph box fits an i32"),
                    i32::try_from(metrics.width).expect("a glyph fits an i32"),
                    i32::try_from(metrics.height).expect("a glyph fits an i32")
                ),
                "and neither is its box"
            );
        }
    }

    /// The decoded face the §5 walk selected for a request, read from the table
    /// the matcher actually used.
    ///
    /// This is the "reachable through the §5 matcher from the same code path a
    /// real page uses" half of the acceptance test: it goes through `resolve` and
    /// `face_for`, which are the only two functions measurement and painting
    /// share, rather than reaching into the table directly.
    fn document_face_font(backend: &SystemFontBackend, request: &FontRequest<'_>) -> Arc<Font> {
        backend.with_state(|state| {
            let instance = backend.resolve(request);
            assert_eq!(
                instance.generation, state.generation,
                "the walk was made against the table it is being read with"
            );
            let matched = state
                .table
                .face_for(&instance.walk, 'H', |family, face| {
                    has_glyph(state.loaded, family, face, 'H')
                })
                .expect("a request for a document family with a loaded face resolves");
            Arc::clone(&state.loaded[matched.family][matched.face])
        })
    }

    /// A document that declares one face and has not fetched it, which is the
    /// state §5.2's "not available" rule is about.
    ///
    /// Shared by the two tests below so they cannot drift: one checks the pending
    /// state, the other checks that arriving changes the answer, and the second
    /// only means something if the first's state is the one it starts from.
    fn a_late_webfont() -> (
        SystemFontBackend,
        DocumentFonts,
        render_net::Url,
        &'static str,
    ) {
        let base =
            render_net::Url::parse("https://example.test/page.css").expect("a parseable base URL");
        let url = render_net::Url::parse("https://example.test/fonts/late.ttf")
            .expect("a parseable font URL");
        let webfont = "Late Webfont";
        // `font-display: swap` is here on purpose: 259 of the corpus's 287 rules
        // ask for it, and it must not change the outcome, because there is no
        // timeline to run a swap period on.
        let source = format!(
            "@font-face {{ font-family: '{webfont}'; src: url({url}) format(truetype);              font-display: swap; }}"
        );
        let (sheet, _problems) = render_core::font_face::parse_font_faces(&source);
        let mut document = DocumentFonts::new();
        document.add_stylesheet(&sheet, &base);
        let backend = SystemFontBackend::load().expect("the platform table loads");
        (backend, document, url, webfont)
    }

    /// The engine's answer for a face that has not arrived, and the proof that
    /// measurement and painting agree about it.
    ///
    /// The choice is stated in this file's documentation: a face that has not
    /// arrived is **not in the family**, so §5.2 walks on to the next name. There
    /// is no swap period to honour because there is no font download timer, so
    /// every `font-display` value - including the 216 corpus blocks that ask for
    /// `swap` - reaches the same place, which is §3.2's failure period and §4.8.1's
    /// "user agents must display the text visibly".
    ///
    /// What the test pins is that the answer is *one* answer. A face that is
    /// declared but not fetched, and the same face fetched, must differ - or the
    /// first assertion proves nothing - and in the not-yet-arrived state the
    /// measurement and the shaping must be the platform's, character for
    /// character, exactly as if the `@font-face` rule were not there.
    #[test]
    fn a_face_that_has_not_arrived_is_simply_not_there_and_measure_and_paint_agree() {
        let (backend, document, url, webfont) = a_late_webfont();

        // The rule is registered. The resource is not.
        assert_eq!(
            document.rule_count(),
            1,
            "the @font-face rule is a rule: §4.1 requires a family and a src, and \
             both are here"
        );
        assert!(
            !document.has_resource(&url),
            "and the resource has not arrived, which is the state under test"
        );
        assert_eq!(
            document.outstanding_urls(),
            vec![url.clone()],
            "so the URL is the one a fetch is needed for"
        );

        backend.set_document_faces(&document);
        let request = FontRequest::of_family(webfont);
        let text = "Handgloves 019";
        let origin = PhysicalPoint { x: 0.0, y: 0.0 };

        // Not in the family: the walk falls through to the next name, and
        // `font-display: swap` cannot change that, because there is no timeline
        // to run a swap period on.
        let pending_measured = backend.measure(
            text,
            TextStyle {
                font_size: 24.0,
                line_height: 28.8,
                font: request,
            },
        );
        let GlyphRun { glyphs, .. } =
            backend.shape_font(text, &request, 24.0, origin, Color::BLACK);
        let pending_drawn: f32 = glyphs.iter().map(|glyph| glyph.advance).sum();
        assert_eq!(
            glyphs.len(),
            text.chars().count(),
            "§4.8.1 requires the text to be displayed while the font is \
             unavailable, so every character is given a face - the fallback's"
        );
        assert!(
            (pending_measured.advance - pending_drawn).abs() < 1e-3,
            "measure and paint must agree on the fallback: measured {}, shaped {}",
            pending_measured.advance,
            pending_drawn
        );
        assert!(
            pending_measured.advance > 0.0,
            "and the fallback has real advances, so this is a font being used and \
             not a run of zero-width gaps"
        );
        // And it is the platform's face, not a nominal constant: the same request
        // against the family the walk lands on measures identically.
        let fallthrough = FontRequest::initial();
        let platform = advance(&backend, &fallthrough, text);
        assert!(
            (pending_measured.advance - platform).abs() < 1e-3,
            "so the walk really did fall through to the initial family, which is \
             what 'not present in the family' means"
        );
    }

    /// The other half of the state above: a face arriving changes the answer, and
    /// measure and paint still agree afterwards.
    ///
    /// The fact that the answer changed is what makes the pending test a test of a
    /// *state* rather than of the only state the engine is ever in. Which family
    /// answered is checked by index rather than by comparing advances, so the
    /// assertion is exact on every platform even when the two faces happen to
    /// share an advance for the chosen characters.
    #[test]
    fn a_face_arriving_changes_the_answer_and_measure_and_paint_still_agree() {
        let (backend, mut document, url, webfont) = a_late_webfont();
        let request = FontRequest::of_family(webfont);
        let text = "Handgloves 019";
        let origin = PhysicalPoint { x: 0.0, y: 0.0 };
        backend.set_document_faces(&document);

        let pending_family = selected_family(&backend, &request, 'H');
        assert!(
            pending_family >= backend.document_family_count(),
            "while the face has not arrived, the family that answered is an \
             installed one (index {pending_family})"
        );

        let (bytes, _installed_family) = a_decodable_face_file();
        document
            .install(&url, &bytes)
            .expect("a declared face file decodes");
        backend.set_document_faces(&document);

        let GlyphRun { glyphs, .. } =
            backend.shape_font(text, &request, 24.0, origin, Color::BLACK);
        let drawn: f32 = glyphs.iter().map(|glyph| glyph.advance).sum();
        let measured = backend.measure(
            text,
            TextStyle {
                font_size: 24.0,
                line_height: 28.8,
                font: request,
            },
        );
        assert!(
            (measured.advance - drawn).abs() < 1e-3,
            "once the face has arrived, measure and paint still agree: measured \
             {}, shaped {}",
            measured.advance,
            drawn
        );
        let arrived_family = selected_family(&backend, &request, 'H');
        assert!(
            arrived_family < backend.document_family_count(),
            "the family that answers now is the document's (index {arrived_family}), \
             so the arrival changed the selection and not merely the bookkeeping"
        );
    }

    /// A face arriving mid-session cannot make a rasterised mask from before it
    /// reachable under a later request.
    ///
    /// The generation is mixed into every [`FontInstanceId`], so the second
    /// request gets a different id and the first request's mask cannot be found
    /// under it. This is the concrete form of "a memo cannot go stale": without
    /// the generation, a glyph mask cached before a font arrived would be painted
    /// for a run laid out with it, which is a measure/draw mismatch by
    /// construction.
    #[test]
    fn a_face_arriving_mints_new_instance_ids_so_no_mask_can_survive_it() {
        use render_core::paint::GlyphMaskProvider;

        let backend = SystemFontBackend::load().expect("the platform table loads");
        let (bytes, _family) = a_decodable_face_file();
        let request = FontRequest::initial();
        let before = backend.instance_id_for(&request);
        // The memo entry a mask lookup needs is written by a resolve, so the test
        // shapes a run rather than only minting an id: an id on its own is not a
        // resolution, and this is the path a real page takes before any mask is
        // asked for.
        let GlyphRun { .. } = backend.shape_font(
            "Handgloves",
            &request,
            24.0,
            PhysicalPoint { x: 0.0, y: 0.0 },
            Color::BLACK,
        );
        let mask_before = backend
            .shared_mask(before, GlyphId('H' as u32), 24.0)
            .expect("the initial family renders H");
        let generation_before = backend.generation();

        let (document, _url) = document_fonts_with(&bytes, "Arriving Webfont", None);
        backend.set_document_faces(&document);

        assert_ne!(
            backend.generation(),
            generation_before,
            "installing a document's faces publishes a new generation"
        );
        let after = backend.instance_id_for(&request);
        assert_ne!(
            before, after,
            "so the same request mints a new id, which is what makes the old \
             memo entries and the old masks unreachable"
        );
        // The new id has to resolve to a mask of its own, which is only true if
        // the walk behind it was made against the new table.
        let GlyphRun { .. } = backend.shape_font(
            "Handgloves",
            &request,
            24.0,
            PhysicalPoint { x: 0.0, y: 0.0 },
            Color::BLACK,
        );
        let mask_after = backend
            .shared_mask(after, GlyphId('H' as u32), 24.0)
            .expect("the new id resolves to a mask of its own");
        // The mask is the same face here - the request did not name the document's
        // family - but it was reached under a new id, so the engine cannot have
        // reused the old one by accident. What the assertion protects is the
        // *reachability*, and the strongest available statement of that is the id
        // change above.
        assert_eq!(
            mask_after.coverage, mask_before.coverage,
            "and the two ids happen to reach the same face here, because the \
             request did not name the document's family"
        );
    }

    /// A document face is reachable through §5 from the same path a page uses, and
    /// an installed face is reachable in the same page.
    ///
    /// This is the ordinary case and the one that proves the table is unified
    /// rather than layered: `font-family: Webfont, sans-serif` has to give a
    /// character in the document face's `unicode-range` to the document face and
    /// one outside it to an installed face, out of a single walk.
    #[test]
    fn a_document_face_and_an_installed_face_are_selected_in_the_same_page() {
        let backend = SystemFontBackend::load().expect("the platform table loads");
        let (bytes, _family) = a_decodable_face_file();
        // A range that admits exactly one ASCII letter, so the split is
        // unambiguous and does not depend on which font the platform has.
        let (document, _url) = document_fonts_with(&bytes, "Split Webfont", Some("U+48"));
        backend.set_document_faces(&document);

        let request = FontRequest::of_family("\"Split Webfont\", monospace");
        let instance = backend.resolve(&request);
        let table = backend.table();
        let in_range = table
            .face_for(&instance.walk, 'H', |_, _| true)
            .expect("U+0048 is in the face's range");
        assert!(
            in_range.family < table.document_family_count(),
            "U+0048 is inside the declared range, so the document family is \
             selected: family {} is one of the {} document families",
            in_range.family,
            table.document_family_count()
        );
        let out_of_range = table
            .face_for(&instance.walk, 'x', |_, _| true)
            .expect("§5.2 walks on to the next name for a character no slice owns");
        assert!(
            out_of_range.family >= table.document_family_count(),
            "U+0078 is outside it, so the same walk selected an installed family \
             (index {} of {}) - which is the unified table, not two searches",
            out_of_range.family,
            table.families().len()
        );
        // And the font that answered is the document's, for the in-range one.
        let face = document_face_font(&backend, &request);
        let _ = face;
        assert!(
            instance.generation == backend.generation(),
            "and the walk was made against the table it is being read with"
        );
    }

    /// A document's faces are gone once the next document's are installed.
    #[test]
    fn installing_the_next_document_drops_the_previous_documents_faces() {
        let backend = SystemFontBackend::load().expect("the platform table loads");
        let (bytes, _family) = a_decodable_face_file();
        let (first, _url) = document_fonts_with(&bytes, "First Document Font", None);
        backend.set_document_faces(&first);
        let request = FontRequest::of_family("First Document Font");
        assert!(
            document_face_font(&backend, &request).lookup_glyph_index('H') != 0,
            "the first document's face is reachable while it is loaded"
        );

        // Navigating away. §4.1: "Downloaded fonts are only available to documents
        // that reference them"; §10.2: "A Web Font must not be accessible in any
        // other Document from the one which either is associated with the
        // @font-face rule or owns the FontFaceSet."
        backend.set_document_faces(&DocumentFonts::new());
        assert_eq!(
            backend.document_family_count(),
            0,
            "the table has no document families at all, so the previous \
             document's face is not reachable by any index"
        );
        let table = backend.table();
        let instance = backend.resolve(&request);
        assert!(
            table
                .face_for(&instance.walk, 'H', |_, _| true)
                .is_none_or(|matched| matched.family >= table.document_family_count()),
            "and a request for it resolves to an installed face or to nothing, \
             never to the face the previous document downloaded"
        );
    }

    /// One URL fetched twice is one face.
    ///
    /// §4.1 permits caching these ("These restrictions do not affect caching
    /// behavior, fonts are cached the same way other web resources are cached"),
    /// and the corpus needs it: its one multi-entry `src` is five spellings of
    /// the same icon font, and a 10 MB CJK face is worth not duplicating. The
    /// assertion is made on the decoded face's identity, not on a counter, so it
    /// is about the allocation rather than about bookkeeping.
    #[test]
    fn a_url_installed_twice_is_one_face() {
        use std::sync::Arc;
        let (bytes, _family) = a_decodable_face_file();
        let base =
            render_net::Url::parse("https://example.test/page.css").expect("a parseable base URL");
        let url = render_net::Url::parse("https://example.test/fonts/once.ttf")
            .expect("a parseable font URL");
        let source = format!(
            "@font-face {{ font-family: 'Once'; src: url({url}) format(truetype); }} \
             @font-face {{ font-family: 'Once'; src: url({url}) format(truetype); }}"
        );
        let (sheet, _problems) = render_core::font_face::parse_font_faces(&source);
        let mut document = DocumentFonts::new();
        document.add_stylesheet(&sheet, &base);
        assert_eq!(
            document.outstanding_urls(),
            vec![url.clone()],
            "two rules naming one URL are one request, which is the whole of the \
             deduplication"
        );
        assert!(
            document.install(&url, &bytes).is_ok(),
            "and installing it succeeds"
        );
        let first = document.resource(&url).expect("the resource is registered");
        assert!(
            document.install(&url, &bytes).is_ok(),
            "installing it again is not an error"
        );
        let second = document
            .resource(&url)
            .expect("the resource is still registered");
        assert!(
            Arc::ptr_eq(&first, &second),
            "and it is the same allocation, so a re-fetch did not decode a second \
             copy of a face that is already in memory"
        );
    }

    /// §4.3.3.1's `local()` is looked for before any URL, and against one face's
    /// own name rather than against a family.
    #[test]
    fn a_local_name_resolves_to_an_installed_face_and_issues_no_request() {
        // The name comes from a face this platform actually has installed: the
        // declared table also lists faces of other platforms, which are absent
        // here. The assertion is about the mechanism, so it holds for whichever
        // installed face declares a name.
        let backend = SystemFontBackend::load().expect("the platform table loads");
        let installed_name = backend
            .installed
            .local_names
            .iter()
            .flatten()
            .find_map(|candidates| candidates.first().copied());
        let Some(name) = installed_name else {
            // No installed face declares a local() name, so the lookup must miss.
            assert!(
                backend
                    .installed
                    .local_face("rENDER no such face")
                    .is_none(),
                "an unknown local() name resolves to no face"
            );
            return;
        };
        assert!(
            backend.installed.local_face(name).is_some(),
            "§4.3.3.1 matches a local() name against the name table of an \
             installed face, and that face declares {name:?}"
        );

        // A `local()` that names a face this table does not carry is not found,
        // and §4.3 says what happens then: the list walks on to the URL.
        assert!(
            backend
                .installed
                .local_face("No Such Installed Face Name")
                .is_none(),
            "and an unknown name is not found rather than resolved to a family"
        );

        // §4.3.3.1 also forbids resolving a `local()` name to a *family*: the
        // argument "uniquely identifies a single font face within a larger
        // family", and "Platform substitutions for a given font name must not be
        // used". A declared family whose faces carry no `local_names` is exactly
        // that case - it is a family, no face of it has the name, and §4.3 says an
        // unresolvable `local()` is ignored so the list walks on to its URL.
        if let Some(family_only) = PLATFORM_FACES
            .iter()
            .find(|source| {
                source.local_names.is_empty()
                    && !PLATFORM_FACES.iter().any(|other| {
                        other.local_names.iter().any(|candidate| {
                            render_core::layout::caseless_match(candidate, source.family)
                        })
                    })
            })
            .map(|source| source.family)
        {
            assert!(
                backend.installed.local_face(family_only).is_none(),
                "{family_only} is a declared family, but §4.3.3.1's argument names \
                 one *face* and no face of it carries that name, so `local({family_only})` \
                 is not found and the src list walks on to its URL"
            );
        }
    }
}
