//! Small immediate-mode painter used by the app. Everything is drawn into a
//! tiny-skia pixmap which is then handed to softbuffer.

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

use crate::font::{Clip, Span, TextEngine};
use crate::theme::Rgb;

pub struct Painter<'a> {
    pub pixmap: &'a mut Pixmap,
    pub text: &'a mut TextEngine,
    pub scale: f32,
}

impl<'a> Painter<'a> {
    pub fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgb, alpha: f32) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let Some(rect) = Rect::from_xywh(x, y, w, h) else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(color.color(alpha));
        self.pixmap
            .fill_rect(rect, &paint, Transform::identity(), None);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rounded(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Rgb, alpha: f32) {
        let Some(path) = rounded_path(x, y, w, h, r) else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color(color.color(alpha));
        paint.anti_alias = true;
        self.pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
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
        self.pixmap
            .stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    /// Draw a single line of text. Returns the width used.
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
        self.text
            .draw_line(self.pixmap, x, y_top, &spans, monospace, clip, max_width)
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
