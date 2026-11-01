//! End-to-end proof that a CSS `@font-face` rule reaches layout geometry.
//!
//! The document names a real TrueType file through a relative `src`; the
//! installer loads it from the file system and the measurer uses its advances.
//! The fixture's `H` advances one em, while the reference measurer's narrow
//! advance is half an em, so the same three characters are 24px without the
//! face and 48px with it at `font-size: 16px`. That difference is the whole
//! claim: descriptor discovery, URL resolution, local loading, and the
//! pre-layout install hook are all on the path between the two numbers.

#![allow(
    clippy::float_cmp,
    reason = "the fixture's advances are exact integer font units scaled by 16/1000"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use fontdue::{Font, FontSettings};
use render_core::document::{
    Document, DocumentBackends, DocumentRenderOptions, ExternalStyleSheets, FontFaceInstaller,
};
use render_core::font_face::{FontFaceSource, FontSource};
use render_core::layout::{FragmentKind, SimpleTextMeasurer, TextMeasure, TextMeasurer, TextStyle};
use render_core::paint::{NoGlyphMasks, ReferenceTextShaper};
use url::Url;

const HTML: &str = r"<!doctype html><style>
@font-face {
  font-family: 'Render Test Webfont';
  src: url(render-test-webfont.ttf) format(truetype);
}
html, body { margin: 0; padding: 0 }
p { font-family: 'Render Test Webfont'; font-size: 16px; margin: 0; width: 300px }
</style><p>HHH</p>";

const FAMILY: &str = "render test webfont";

/// A measurer that uses the fixture face for the family the document declares
/// and the reference measurer for everything else.
struct FixtureFonts {
    faces: Arc<Mutex<HashMap<String, Arc<Font>>>>,
}

impl TextMeasurer for FixtureFonts {
    fn measure(&self, text: &str, style: TextStyle<'_>) -> TextMeasure {
        let family = style.font.family.to_ascii_lowercase();
        let face = {
            let faces = self.faces.lock().unwrap_or_else(PoisonError::into_inner);
            faces
                .iter()
                .find(|(name, _)| family.contains(name.as_str()))
                .map(|(_, font)| Arc::clone(font))
        };
        let Some(font) = face else {
            return SimpleTextMeasurer.measure(text, style);
        };
        let advance = text
            .chars()
            .map(|character| font.metrics(character, style.font_size).advance_width)
            .sum();
        let (ascent, descent) = font
            .horizontal_line_metrics(style.font_size)
            .map_or((style.font_size * 0.8, style.font_size * 0.2), |metrics| {
                (metrics.ascent, metrics.descent)
            });
        TextMeasure {
            advance,
            ascent,
            descent,
        }
    }
}

/// The production loader's local-media half, reduced to the fixture's shape.
struct LocalInstaller {
    faces: Arc<Mutex<HashMap<String, Arc<Font>>>>,
}

impl FontFaceInstaller for LocalInstaller {
    fn install_font_faces(&self, sources: &[FontFaceSource]) {
        for source in sources {
            for rule in &source.rules {
                let Some(base) = source.base_url.as_ref() else {
                    continue;
                };
                for item in &rule.sources {
                    let FontSource::Url { url, .. } = item else {
                        continue;
                    };
                    let Ok(resolved) = base.join(url) else {
                        continue;
                    };
                    if resolved.scheme() != "file" {
                        continue;
                    }
                    let Ok(path) = resolved.to_file_path() else {
                        continue;
                    };
                    let Ok(bytes) = std::fs::read(path) else {
                        continue;
                    };
                    let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) else {
                        continue;
                    };
                    self.faces
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(rule.family.clone(), Arc::new(font));
                }
            }
        }
    }
}

fn base_url() -> Url {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("index.html");
    Url::from_file_path(&path).expect("the fixture directory has a file URL")
}

fn render(fonts: &FixtureFonts, installer: Option<&dyn FontFaceInstaller>) -> f32 {
    let document = Document::parse(HTML);
    let output = document.render_with_external_style_sheets(
        DocumentRenderOptions::default(),
        DocumentBackends {
            text_measurer: fonts,
            text_shaper: &ReferenceTextShaper,
            glyph_masks: &NoGlyphMasks,
            font_face_installer: installer,
        },
        &base_url(),
        &ExternalStyleSheets::default(),
    );
    output
        .layout
        .fragments
        .iter()
        .find_map(|fragment| match &fragment.kind {
            FragmentKind::Text(data) if data.text == "HHH" => Some(fragment.rect.size.width),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the paragraph's text run is laid out"))
}

#[test]
fn an_external_stylesheet_keeps_its_font_faces_and_base_url() {
    use render_core::document::{DocumentLimits, ExternalStyleSheetKey};
    use render_core::font_face::FontSource;

    struct RecordingInstaller {
        sources: Mutex<Vec<FontFaceSource>>,
    }

    impl FontFaceInstaller for RecordingInstaller {
        fn install_font_faces(&self, sources: &[FontFaceSource]) {
            self.sources
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(sources);
        }
    }

    let document = Document::parse(
        "<!doctype html><link rel='stylesheet' href='/assets/main.css'><p>text</p>",
    );
    let base = Url::parse("https://example.test/page.html").expect("a base URL");
    let discovery = document.discover_author_style_slots(&base, DocumentLimits::default());
    let slot = discovery
        .slots
        .iter()
        .find(|slot| {
            matches!(
                slot.source,
                render_core::document::AuthorStyleSource::External { .. }
            )
        })
        .expect("the link is a discovered external stylesheet");
    let render_core::document::AuthorStyleSource::External {
        resolved_url: Some(url),
        ..
    } = &slot.source
    else {
        panic!("the link resolves against the document");
    };
    let key = ExternalStyleSheetKey::new(slot.owner, url.clone());
    let mut external = ExternalStyleSheets::default();
    external.insert_css(
        key,
        "@font-face { font-family: Ext; src: url(../fonts/ext.ttf) format(truetype) } \
         p { color: red }",
    );

    let installer = RecordingInstaller {
        sources: Mutex::new(Vec::new()),
    };
    let faces = Arc::new(Mutex::new(HashMap::new()));
    let fonts = FixtureFonts {
        faces: Arc::clone(&faces),
    };
    let _ = document.render_with_external_style_sheets(
        DocumentRenderOptions::default(),
        DocumentBackends {
            text_measurer: &fonts,
            text_shaper: &ReferenceTextShaper,
            glyph_masks: &NoGlyphMasks,
            font_face_installer: Some(&installer),
        },
        &base,
        &external,
    );

    let sources = installer
        .sources
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    assert_eq!(sources.len(), 1, "one sheet declares one face");
    assert_eq!(
        sources[0].base_url.as_ref().map(Url::as_str),
        Some("https://example.test/assets/main.css"),
        "a relative src resolves against the stylesheet, not the document"
    );
    assert_eq!(sources[0].rules[0].family, "ext");
    assert!(
        matches!(
            &sources[0].rules[0].sources[0],
            FontSource::Url { url, .. } if url == "../fonts/ext.ttf"
        ),
        "the descriptor keeps the author's URL for §4.3.2 resolution"
    );
}

#[test]
fn a_local_font_face_changes_the_measured_width() {
    let faces = Arc::new(Mutex::new(HashMap::new()));
    let fonts = FixtureFonts {
        faces: Arc::clone(&faces),
    };
    let installer = LocalInstaller {
        faces: Arc::clone(&faces),
    };

    let without = render(&fonts, None);
    let with = render(&fonts, Some(&installer));

    // The reference measurer charges 0.5em per narrow character; the fixture
    // charges 1em. Three characters at 16px are therefore 24px and 48px.
    assert_eq!(without, 24.0, "the reference face's HHH advance");
    assert_eq!(with, 48.0, "the declared face's HHH advance");
    assert!(
        faces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(FAMILY),
        "the installer decoded the file named by the collected @font-face rule"
    );
}
