use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::chrome::{Canvas, Point, TextPainter};
use fontdue::{Font, FontSettings};
use render_core::layout::{PhysicalPoint, TextMeasure, TextMeasurer, TextStyle};
use render_core::paint::{
    Color, FontInstanceId, GlyphId, GlyphInstance, GlyphMask, GlyphMaskProvider, GlyphRun,
    TextShaper,
};

const MAX_CACHED_GLYPHS: usize = 16_384;
type GlyphCache = HashMap<(u32, u32), Option<GlyphMask>>;

pub struct SystemFontBackend {
    fonts: Vec<Font>,
    glyph_cache: Mutex<GlyphCache>,
}

impl SystemFontBackend {
    /// Load the first usable font in each platform fallback group.
    ///
    /// # Errors
    ///
    /// Returns an I/O-style error when no supported system font can be found.
    pub fn load() -> Result<Self, Box<dyn Error>> {
        let mut fonts = Vec::new();
        for group in system_font_candidate_groups() {
            for path in group {
                let Ok(bytes) = fs::read(&path) else {
                    continue;
                };
                if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                    fonts.push(font);
                    break;
                }
            }
        }
        if fonts.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no supported system font was found",
            )
            .into());
        }
        Ok(Self {
            fonts,
            glyph_cache: Mutex::new(HashMap::new()),
        })
    }

    fn font_for(&self, character: char) -> &Font {
        self.fonts
            .iter()
            .find(|font| font.lookup_glyph_index(character) != 0)
            .unwrap_or(&self.fonts[0])
    }
}

impl TextMeasurer for SystemFontBackend {
    fn measure(&self, text: &str, style: TextStyle) -> TextMeasure {
        let mut advance = 0.0_f32;
        let mut ascent = 0.0_f32;
        let mut descent = 0.0_f32;
        let characters = if text.is_empty() {
            " ".chars()
        } else {
            text.chars()
        };
        for character in characters {
            let font = self.font_for(character);
            if !text.is_empty() {
                advance += font.metrics(character, style.font_size).advance_width;
            }
            if let Some(lines) = font.horizontal_line_metrics(style.font_size) {
                ascent = ascent.max(lines.ascent);
                descent = descent.max(-lines.descent);
            }
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
        let mut x = origin.x;
        let glyphs = text
            .chars()
            .map(|character| {
                let advance = self
                    .font_for(character)
                    .metrics(character, font_size)
                    .advance_width;
                let glyph = GlyphInstance {
                    glyph: GlyphId(character as u32),
                    position: PhysicalPoint { x, y: origin.y },
                    advance,
                };
                x += advance;
                glyph
            })
            .collect();
        GlyphRun {
            font: FontInstanceId(0),
            font_size,
            color,
            glyphs,
        }
    }
}

impl GlyphMaskProvider for SystemFontBackend {
    fn mask(&self, _font: FontInstanceId, glyph: GlyphId, font_size: f32) -> Option<GlyphMask> {
        let key = (glyph.0, font_size.to_bits());
        if let Some(mask) = self.glyph_cache.lock().ok()?.get(&key) {
            return mask.clone();
        }
        let character = char::from_u32(glyph.0)?;
        let (metrics, coverage) = self.font_for(character).rasterize(character, font_size);
        let mask = Some(GlyphMask {
            width: u32::try_from(metrics.width).ok()?,
            height: u32::try_from(metrics.height).ok()?,
            left: metrics.xmin,
            top: metrics
                .ymin
                .checked_add(i32::try_from(metrics.height).ok()?)?,
            coverage,
        });
        let mut cache = self.glyph_cache.lock().ok()?;
        if cache.len() < MAX_CACHED_GLYPHS {
            cache.insert(key, mask.clone());
        }
        mask
    }
}

impl TextPainter for SystemFontBackend {
    fn measure(&self, text: &str, size: f32) -> f32 {
        text.chars()
            .map(|character| {
                self.font_for(character)
                    .metrics(character, size)
                    .advance_width
            })
            .sum()
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "native chrome coordinates are finite and bounded by the window"
    )]
    fn paint(&self, canvas: &mut Canvas<'_>, text: &str, origin: Point, size: f32, color: u32) {
        let mut x = origin.x;
        for character in text.chars() {
            let font = self.font_for(character);
            let (metrics, coverage) = font.rasterize(character, size);
            let ascent = font
                .horizontal_line_metrics(size)
                .map_or(size * 0.8, |lines| lines.ascent);
            let glyph_height = i32::try_from(metrics.height).unwrap_or(i32::MAX);
            let top = origin.y + ascent - metrics.ymin.saturating_add(glyph_height) as f32;
            canvas.blend_mask(
                x.round() as i32 + metrics.xmin,
                top.round() as i32,
                u32::try_from(metrics.width).unwrap_or(0),
                &coverage,
                color,
            );
            x += metrics.advance_width;
        }
    }
}

fn system_font_candidate_groups() -> Vec<Vec<PathBuf>> {
    let mut windows_directories = Vec::new();
    if let Some(windows) = env::var_os("WINDIR") {
        windows_directories.push(Path::new(&windows).join("Fonts"));
    }
    windows_directories.push(PathBuf::from(r"C:\Windows\Fonts"));
    let mut seen = HashSet::new();
    windows_directories.retain(|path| seen.insert(path.clone()));

    let mut primary = Vec::new();
    let mut cjk = Vec::new();
    let mut symbols = Vec::new();
    for directory in windows_directories {
        primary.extend(["segoeui.ttf", "arial.ttf", "tahoma.ttf"].map(|name| directory.join(name)));
        cjk.push(directory.join("msyh.ttc"));
        symbols.push(directory.join("seguisym.ttf"));
    }
    primary.extend([
        PathBuf::from("/System/Library/Fonts/SFNS.ttf"),
        PathBuf::from("/System/Library/Fonts/Helvetica.ttc"),
        PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf"),
    ]);
    cjk.extend([
        PathBuf::from("/Library/Fonts/Arial Unicode.ttf"),
        PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
    ]);
    vec![primary, cjk, symbols]
}
