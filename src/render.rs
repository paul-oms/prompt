//! Small immediate-mode painter used by the app.
//!
//! A [`Canvas`] is either a tiny-skia pixmap (CPU fallback, screenshots) or an
//! ordered list of GPU instances. The painter API is identical for both, so the
//! app's drawing code does not care which renderer is active.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform};

use crate::font::{Clip, Span, TextEngine};
use crate::gpu::{self, GlyphAtlas, Instance};
use crate::theme::Rgb;

pub struct Canvas {
    width: u32,
    height: u32,
    pixmap: Option<Pixmap>,
    scene: Vec<Instance>,
    atlas: Option<Rc<RefCell<GlyphAtlas>>>,
}

impl Canvas {
    /// Create a canvas for the active backend. `atlas` is `Some` when the GPU
    /// renderer is in use.
    pub fn new(width: u32, height: u32, atlas: Option<Rc<RefCell<GlyphAtlas>>>) -> Option<Self> {
        let (pixmap, scene) = match &atlas {
            Some(_) => (None, Vec::new()),
            None => (Some(Pixmap::new(width, height)?), Vec::new()),
        };
        Some(Self {
            width,
            height,
            pixmap,
            scene,
            atlas,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn scene(&self) -> &[Instance] {
        &self.scene
    }

    pub fn pixmap(&self) -> Option<&Pixmap> {
        self.pixmap.as_ref()
    }

    /// Clear the canvas to a solid colour.
    pub fn fill(&mut self, color: Rgb) {
        if let Some(pixmap) = &mut self.pixmap {
            pixmap.fill(color.solid());
        } else {
            self.scene.clear();
            self.scene.push(gpu::rect(
                0.0,
                0.0,
                self.width as f32,
                self.height as f32,
                0.0,
                0.0,
                color,
                1.0,
            ));
        }
    }

    /// Composite another canvas over this one. `alpha` is used by the terminal
    /// reveal animation.
    pub fn compose(&mut self, other: &Canvas, alpha: f32) {
        if let Some(canvas) = &mut self.pixmap {
            let Some(cache) = &other.pixmap else {
                return;
            };
            if alpha >= 0.999 {
                canvas.data_mut().copy_from_slice(cache.data());
            } else if alpha > 0.0 {
                let paint = PixmapPaint {
                    opacity: alpha,
                    ..PixmapPaint::default()
                };
                canvas.draw_pixmap(0, 0, cache.as_ref(), &paint, Transform::identity(), None);
            }
        } else if alpha >= 0.999 {
            self.scene.extend_from_slice(&other.scene);
        } else if alpha > 0.0 {
            for instance in &other.scene {
                let mut instance = *instance;
                instance.color[3] *= alpha;
                self.scene.push(instance);
            }
        }
    }

    pub fn save_png(&self, path: &Path) -> bool {
        self.pixmap
            .as_ref()
            .is_some_and(|pixmap| pixmap.save_png(path).is_ok())
    }
}

pub struct Painter<'a> {
    pub canvas: &'a mut Canvas,
    pub text: &'a mut TextEngine,
    pub scale: f32,
}

impl<'a> Painter<'a> {
    pub fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgb, alpha: f32) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        if let Some(pixmap) = &mut self.canvas.pixmap {
            let Some(rect) = Rect::from_xywh(x, y, w, h) else {
                return;
            };
            let mut paint = Paint::default();
            paint.set_color(color.color(alpha));
            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        } else {
            self.canvas
                .scene
                .push(gpu::rect(x, y, w, h, 0.0, 0.0, color, alpha));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rounded(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Rgb, alpha: f32) {
        if let Some(pixmap) = &mut self.canvas.pixmap {
            let Some(path) = rounded_path(x, y, w, h, r) else {
                return;
            };
            let mut paint = Paint::default();
            paint.set_color(color.color(alpha));
            paint.anti_alias = true;
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        } else {
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
            self.canvas
                .scene
                .push(gpu::rect(x, y, w, h, r, 0.0, color, alpha));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rounded_stroke(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        width: f32,
        color: Rgb,
        alpha: f32,
    ) {
        if let Some(pixmap) = &mut self.canvas.pixmap {
            let Some(path) = rounded_path(x, y, w, h, r) else {
                return;
            };
            let mut paint = Paint::default();
            paint.set_color(color.color(alpha));
            paint.anti_alias = true;
            let stroke = Stroke {
                width,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        } else {
            if w <= 0.0 || h <= 0.0 || width <= 0.0 {
                return;
            }
            let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
            self.canvas
                .scene
                .push(gpu::rect(x, y, w, h, r, width, color, alpha));
        }
    }

    /// Draw a single line of text. Returns the width used.
    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &mut self,
        x: f32,
        y_top: f32,
        text: &str,
        color: Rgb,
        bold: bool,
        monospace: Option<f32>,
        clip: Clip,
        max_width: f32,
    ) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let spans = [Span {
            text: text.to_string(),
            color,
            bold,
            italic: false,
        }];
        self.spans(x, y_top, &spans, monospace, clip, max_width)
    }

    /// Draw pre-styled spans. Used by the terminal grid and syntax highlighting.
    pub fn spans(
        &mut self,
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
        if let Some(pixmap) = &mut self.canvas.pixmap {
            return self
                .text
                .draw_line(pixmap, x, y_top, spans, monospace, clip, max_width);
        }

        let (glyphs, width) = self.text.layout_line(x, y_top, spans, monospace, max_width);
        let Some(atlas) = self.canvas.atlas.clone() else {
            return width;
        };
        for (glyph, color) in glyphs {
            let (gx, gy, w, h, uv, is_color) = {
                let Some(image) = self.text.glyph_image(glyph.cache_key) else {
                    continue;
                };
                let Some(entry) = atlas.borrow_mut().intern(glyph.cache_key, image) else {
                    continue;
                };
                (
                    glyph.x as f32 + image.placement.left as f32,
                    glyph.y as f32 - image.placement.top as f32,
                    entry.w as f32,
                    entry.h as f32,
                    [entry.u0, entry.v0, entry.u1, entry.v1],
                    entry.is_color,
                )
            };
            self.canvas
                .scene
                .push(gpu::glyph(gx, gy, w, h, uv, color, 1.0, clip, is_color));
        }
        width
    }
}

fn rounded_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pb = PathBuilder::new();
    if r <= 0.5 {
        pb.push_rect(Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }
    let k = r * 0.447_715_48; // circle approximation constant (1 - 0.5522847)
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}
