//! Font discovery, shaping and glyph rasterisation.
//!
//! cosmic-text does the hard parts (system font discovery, fallback, shaping,
//! grapheme clusters, emoji). Terminal rows are shaped with a forced monospace
//! advance so every cell lands exactly on the grid, even for CJK and emoji.

use std::collections::HashSet;

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Hinting, Metrics, Shaping, Style, SwashCache, SwashContent,
    Weight, Wrap,
};
use tiny_skia::Pixmap;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use crate::theme::Rgb;

#[derive(Clone, Copy, Debug, Default)]
pub struct Clip {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Clip {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Overlap of two clips. A zero-sized result means nothing is visible.
    pub fn intersect(&self, other: Clip) -> Clip {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.w).min(other.x + other.w);
        let bottom = (self.y + self.h).min(other.y + other.h);
        Clip {
            x,
            y,
            w: (right - x).max(0.0),
            h: (bottom - y).max(0.0),
        }
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        x as f32 >= self.x
            && y as f32 >= self.y
            && (x as f32) < self.x + self.w
            && (y as f32) < self.y + self.h
    }
}

pub struct Span {
    pub text: String,
    pub color: Rgb,
    pub bold: bool,
    pub italic: bool,
}

pub struct TextEngine {
    pub font_system: FontSystem,
    cache: SwashCache,
    pub family: String,
    pub font_size: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub baseline: f32,
}

const PREFERRED_FAMILIES: &[&str] = &[
    "JetBrains Mono",
    "JetBrainsMono Nerd Font",
    "CaskaydiaMono Nerd Font",
    "Cascadia Code",
    "Fira Code",
    "SF Mono",
    "Menlo",
    "Monaco",
    "Hack",
    "Iosevka",
    "Source Code Pro",
    "IBM Plex Mono",
    "Noto Sans Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Consolas",
];

impl TextEngine {
    pub fn new(scale: f32, preferred: &str, size: f32, line_height_scale: f32) -> Self {
        let font_system = FontSystem::new();
        let family = pick_family(&font_system, preferred);
        let font_size = (size * scale).max(6.0);
        let cell_h = (size * line_height_scale * scale).round().max(8.0);

        let mut engine = Self {
            font_system,
            cache: SwashCache::new(),
            family,
            font_size,
            cell_w: 8.0,
            cell_h,
            baseline: cell_h * 0.78,
        };
        engine.measure();
        engine
    }

    fn measure(&mut self) {
        let metrics = Metrics::new(self.font_size, self.cell_h);
        let attrs = Attrs::new()
            .family(family_ref(&self.family))
            .weight(Weight::NORMAL);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(Some(1000.0), None);
        buffer.set_wrap(Wrap::None);
        buffer.set_text("MMMM", &attrs, Shaping::Advanced, None);
        let mut advance = 0.0;
        let mut baseline = self.cell_h * 0.78;
        {
            let mut borrowed = buffer.borrow_with(&mut self.font_system);
            borrowed.shape_until_scroll(true);
            for run in borrowed.layout_runs() {
                baseline = run.line_y;
                for glyph in run.glyphs {
                    advance += glyph.w;
                }
            }
        }
        if advance > 0.0 {
            self.cell_w = (advance / 4.0).round().max(3.0);
        }
        self.baseline = baseline;
    }

    /// Shape and draw one line of styled text starting at `x`, top at `y_top`.
    /// Returns the laid-out width.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_line(
        &mut self,
        pixmap: &mut Pixmap,
        x: f32,
        y_top: f32,
        spans: &[Span],
        monospace: Option<f32>,
        clip: Clip,
        max_width: f32,
    ) -> f32 {
        if spans.is_empty() {
            return 0.0;
        }
        let metrics = Metrics::new(self.font_size, self.cell_h);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(Some(max_width.max(1.0)), None);
        buffer.set_wrap(Wrap::None);
        buffer.set_tab_width(4);
        buffer.set_hinting(Hinting::Disabled);
        buffer.set_monospace_width(monospace);

        let family = self.family.clone();
        let mut features = cosmic_text::FontFeatures::new();
        features.disable(cosmic_text::FeatureTag::STANDARD_LIGATURES);
        features.disable(cosmic_text::FeatureTag::CONTEXTUAL_LIGATURES);

        let default_attrs = Attrs::new()
            .family(family_ref(&family))
            .font_features(features.clone());
        let rich: Vec<(&str, Attrs)> = spans
            .iter()
            .map(|span| {
                let mut attrs = Attrs::new()
                    .family(family_ref(&family))
                    .color(cosmic_text::Color::rgb(
                        span.color.0,
                        span.color.1,
                        span.color.2,
                    ))
                    .font_features(features.clone());
                if span.bold {
                    attrs = attrs.weight(Weight::BOLD);
                }
                if span.italic {
                    attrs = attrs.style(Style::Italic);
                }
                (span.text.as_str(), attrs)
            })
            .collect();
        buffer.set_rich_text(rich, &default_attrs, Shaping::Advanced, None);

        // cosmic-text's `monospace_width` only snaps each advance to a multiple
        // of the cell width, so rounding errors (and fallback faces chosen for
        // bold/italic) leave glyphs off the grid. Long runs then drift into the
        // next column, which shows up in columnar output like `ls`. Place every
        // glyph on its own cell column instead.
        let columns = monospace.filter(|width| *width > 0.0).and_then(|width| {
            let line_text: String = spans.iter().map(|span| span.text.as_str()).collect();
            // Byte offsets are line-relative once cosmic-text splits the
            // text, so only snap single-line runs.
            if line_text.contains('\n') {
                return None;
            }
            Some(ColumnMap::new(&line_text, width))
        });

        let mut glyphs: Vec<(cosmic_text::PhysicalGlyph, Rgb)> = Vec::new();
        let mut width: f32 = 0.0;
        {
            let mut borrowed = buffer.borrow_with(&mut self.font_system);
            borrowed.shape_until_scroll(true);
            for run in borrowed.layout_runs() {
                width = width.max(run.line_w);
                let line_y = y_top + run.line_y;
                for glyph in run.glyphs {
                    let color = glyph
                        .color_opt
                        .map(|c| Rgb(c.r(), c.g(), c.b()))
                        .unwrap_or(Rgb(255, 255, 255));
                    let glyph_x = match &columns {
                        // Column placement assumes left-to-right text.
                        Some(columns) if !run.rtl => {
                            x + columns.column(glyph.start) as f32 * columns.width
                        }
                        _ => x + glyph.x,
                    };
                    glyphs.push((glyph.physical((glyph_x - glyph.x, line_y), 1.0), color));
                }
            }
        }

        for (glyph, color) in glyphs {
            self.draw_glyph(pixmap, glyph, color, clip);
        }
        width
    }

    fn draw_glyph(
        &mut self,
        pixmap: &mut Pixmap,
        glyph: cosmic_text::PhysicalGlyph,
        color: Rgb,
        clip: Clip,
    ) {
        let Some(image) = self
            .cache
            .get_image(&mut self.font_system, glyph.cache_key)
            .as_ref()
        else {
            return;
        };
        let gx = glyph.x + image.placement.left;
        let gy = glyph.y - image.placement.top;
        let w = image.placement.width as i32;
        let h = image.placement.height as i32;
        if w == 0 || h == 0 {
            return;
        }
        let stride = pixmap.width() as i32;
        let pixmap_h = pixmap.height() as i32;
        let data = pixmap.data_mut();
        match image.content {
            SwashContent::Mask => {
                for row in 0..h {
                    let py = gy + row;
                    if py < 0 || py >= pixmap_h {
                        continue;
                    }
                    for col in 0..w {
                        let px = gx + col;
                        if px < 0 || px >= stride || !clip.contains(px, py) {
                            continue;
                        }
                        let coverage = image.data[(row * w + col) as usize];
                        if coverage == 0 {
                            continue;
                        }
                        let idx = ((py * stride + px) * 4) as usize;
                        blend_mask(&mut data[idx..idx + 4], color, coverage);
                    }
                }
            }
            SwashContent::Color => {
                for row in 0..h {
                    let py = gy + row;
                    if py < 0 || py >= pixmap_h {
                        continue;
                    }
                    for col in 0..w {
                        let px = gx + col;
                        if px < 0 || px >= stride || !clip.contains(px, py) {
                            continue;
                        }
                        let i = ((row * w + col) * 4) as usize;
                        let a = image.data[i + 3];
                        if a == 0 {
                            continue;
                        }
                        let idx = ((py * stride + px) * 4) as usize;
                        let src = [image.data[i], image.data[i + 1], image.data[i + 2], a];
                        blend_rgba(&mut data[idx..idx + 4], src);
                    }
                }
            }
            SwashContent::SubpixelMask => {
                // Subpixel masks are rare with hinting disabled. Treat them as
                // coverage masks using the green channel.
                for row in 0..h {
                    let py = gy + row;
                    if py < 0 || py >= pixmap_h {
                        continue;
                    }
                    for col in 0..w {
                        let px = gx + col;
                        if px < 0 || px >= stride || !clip.contains(px, py) {
                            continue;
                        }
                        let i = ((row * w + col) * 3) as usize;
                        let coverage = image.data[i + 1];
                        if coverage == 0 {
                            continue;
                        }
                        let idx = ((py * stride + px) * 4) as usize;
                        blend_mask(&mut data[idx..idx + 4], color, coverage);
                    }
                }
            }
        }
    }

    pub fn measure_text(&mut self, text: &str, bold: bool) -> f32 {
        let metrics = Metrics::new(self.font_size, self.cell_h);
        let attrs = Attrs::new()
            .family(family_ref(&self.family))
            .weight(if bold { Weight::BOLD } else { Weight::NORMAL });
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(Some(100_000.0), None);
        buffer.set_wrap(Wrap::None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        let mut width: f32 = 0.0;
        {
            let mut borrowed = buffer.borrow_with(&mut self.font_system);
            borrowed.shape_until_scroll(true);
            if let Some(run) = borrowed.layout_runs().next() {
                width = run.line_w;
            }
        }
        width
    }
}

/// Maps byte offsets in a line of text to display columns, so text drawn with a
/// fixed cell width lands exactly on the grid.
struct ColumnMap {
    width: f32,
    /// `(byte offset, display column)` at each grapheme boundary.
    stops: Vec<(usize, usize)>,
}

impl ColumnMap {
    fn new(text: &str, width: f32) -> Self {
        let mut stops = Vec::with_capacity(text.len() + 1);
        stops.push((0, 0));
        let mut offset = 0;
        let mut column = 0;
        for grapheme in text.graphemes(true) {
            offset += grapheme.len();
            column += grapheme_width(grapheme);
            stops.push((offset, column));
        }
        Self { width, stops }
    }

    fn column(&self, byte: usize) -> usize {
        match self.stops.binary_search_by(|probe| probe.0.cmp(&byte)) {
            Ok(index) => self.stops[index].1,
            Err(0) => 0,
            Err(index) => self.stops[index - 1].1,
        }
    }
}

fn grapheme_width(grapheme: &str) -> usize {
    grapheme
        .chars()
        .map(|ch| {
            if ch == '\t' {
                4
            } else {
                UnicodeWidthChar::width(ch).unwrap_or(0)
            }
        })
        .sum()
}

fn family_ref(family: &str) -> Family<'_> {
    if family.eq_ignore_ascii_case("monospace") {
        Family::Monospace
    } else {
        Family::Name(family)
    }
}

fn pick_family(font_system: &FontSystem, preferred: &str) -> String {
    let mut available: HashSet<String> = HashSet::new();
    for face in font_system.db().faces() {
        for (family, _) in &face.families {
            available.insert(family.to_lowercase());
        }
    }

    if !preferred.trim().is_empty() {
        let wanted = preferred.trim();
        if wanted.eq_ignore_ascii_case("monospace") {
            return "monospace".into();
        }
        if available.contains(&wanted.to_lowercase()) {
            return wanted.to_string();
        }
        eprintln!("prompt: font '{wanted}' is not installed, choosing a fallback");
    }

    for family in PREFERRED_FAMILIES {
        if available.contains(&family.to_lowercase()) {
            return (*family).to_string();
        }
    }
    "monospace".into()
}

fn blend_mask(dst: &mut [u8], color: Rgb, coverage: u8) {
    let sa = coverage as u32;
    let inv = 255 - sa;
    dst[0] = ((color.0 as u32 * sa + dst[0] as u32 * inv) / 255) as u8;
    dst[1] = ((color.1 as u32 * sa + dst[1] as u32 * inv) / 255) as u8;
    dst[2] = ((color.2 as u32 * sa + dst[2] as u32 * inv) / 255) as u8;
    dst[3] = 255;
}

fn blend_rgba(dst: &mut [u8], src: [u8; 4]) {
    let a = src[3] as u32;
    if a == 255 {
        dst[0] = src[0];
        dst[1] = src[1];
        dst[2] = src[2];
        dst[3] = 255;
        return;
    }
    let inv = 255 - a;
    dst[0] = ((src[0] as u32 * a + dst[0] as u32 * inv) / 255) as u8;
    dst[1] = ((src[1] as u32 * a + dst[1] as u32 * inv) / 255) as u8;
    dst[2] = ((src[2] as u32 * a + dst[2] as u32 * inv) / 255) as u8;
    dst[3] = 255;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Pixmap;

    /// Long runs must land on the cell grid: a drift of even a fraction of a
    /// pixel per glyph pushes columnar output (e.g. `ls`) into the next column.
    #[test]
    fn monospace_glyphs_stay_on_the_grid() {
        let mut engine = TextEngine::new(1.0, "", 14.0, 1.38);
        let cell_w = 8.0;
        let cols = 20;
        let mut pixmap = Pixmap::new(400, 40).unwrap();
        pixmap.fill(tiny_skia::Color::BLACK);
        let spans = vec![Span {
            text: "M".repeat(cols),
            color: Rgb(255, 255, 255),
            bold: false,
            italic: false,
        }];
        engine.draw_line(
            &mut pixmap,
            0.0,
            0.0,
            &spans,
            Some(cell_w),
            Clip::new(0.0, 0.0, 400.0, 40.0),
            400.0,
        );
        let max_x = pixmap
            .data()
            .chunks_exact(4)
            .enumerate()
            .filter(|(_, p)| p[0] > 20 || p[1] > 20 || p[2] > 20)
            .map(|(i, _)| (i % 400) as f32)
            .fold(0.0f32, f32::max);
        assert!(
            max_x < cols as f32 * cell_w + 2.0,
            "text extends to {max_x}px, past the {}th cell",
            cols
        );
    }

    #[test]
    fn draws_cjk_and_emoji() {
        let mut engine = TextEngine::new(1.0, "", 14.0, 1.38);
        let mut pixmap = Pixmap::new(200, 40).unwrap();
        pixmap.fill(tiny_skia::Color::BLACK);
        let spans = vec![Span {
            text: "日本語 🚀".into(),
            color: Rgb(255, 255, 255),
            bold: false,
            italic: false,
        }];
        let clip = Clip::new(0.0, 0.0, 200.0, 40.0);
        engine.draw_line(
            &mut pixmap,
            2.0,
            2.0,
            &spans,
            Some(engine.cell_w),
            clip,
            200.0,
        );
        let lit = pixmap
            .data()
            .chunks_exact(4)
            .filter(|p| p[0] > 20 || p[1] > 20 || p[2] > 20)
            .count();
        assert!(lit > 50, "only {lit} lit pixels; family={}", engine.family);
    }
}
