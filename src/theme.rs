use tiny_skia::Color;

/// A plain 8-bit sRGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn hex(v: u32) -> Self {
        Self(
            ((v >> 16) & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            (v & 0xff) as u8,
        )
    }

    pub fn color(self, alpha: f32) -> Color {
        Color::from_rgba8(
            self.0,
            self.1,
            self.2,
            (alpha.clamp(0.0, 1.0) * 255.0) as u8,
        )
    }

    pub fn solid(self) -> Color {
        self.color(1.0)
    }

    pub fn blend(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        Rgb(
            (self.0 as f32 + (other.0 as f32 - self.0 as f32) * t).round() as u8,
            (self.1 as f32 + (other.1 as f32 - self.1 as f32) * t).round() as u8,
            (self.2 as f32 + (other.2 as f32 - self.2 as f32) * t).round() as u8,
        )
    }

    pub fn lerp(self, other: Rgb, t: f32) -> Rgb {
        self.blend(other, t)
    }
}

/// Prompt's opinionated dark theme.
#[derive(Clone, Debug)]
pub struct Theme {
    pub bg: Rgb,
    pub fg: Rgb,
    pub dim: Rgb,
    pub faint: Rgb,
    pub panel: Rgb,
    pub panel_raised: Rgb,
    pub border: Rgb,
    pub accent: Rgb,
    pub green: Rgb,
    pub yellow: Rgb,
    pub red: Rgb,
    pub purple: Rgb,
    pub cyan: Rgb,
    pub selection: Rgb,
    pub cursor: Rgb,
    pub shadow: Rgb,

    /// The 16 ANSI colours used for the emulator palette.
    pub ansi: [Rgb; 16],
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: Rgb::hex(0x11131a),
            fg: Rgb::hex(0xd8dce6),
            dim: Rgb::hex(0x8b92a4),
            faint: Rgb::hex(0x4d5566),
            panel: Rgb::hex(0x191c25),
            panel_raised: Rgb::hex(0x20242f),
            border: Rgb::hex(0x2c3140),
            accent: Rgb::hex(0x7aa2f7),
            green: Rgb::hex(0x9ece6a),
            yellow: Rgb::hex(0xe0af68),
            red: Rgb::hex(0xf7768e),
            purple: Rgb::hex(0xbb9af7),
            cyan: Rgb::hex(0x7dcfff),
            selection: Rgb::hex(0x7aa2f7),
            cursor: Rgb::hex(0x7aa2f7),
            shadow: Rgb::hex(0x05060a),
            ansi: [
                Rgb::hex(0x15161e),
                Rgb::hex(0xf7768e),
                Rgb::hex(0x9ece6a),
                Rgb::hex(0xe0af68),
                Rgb::hex(0x7aa2f7),
                Rgb::hex(0xbb9af7),
                Rgb::hex(0x7dcfff),
                Rgb::hex(0xa9b1d6),
                Rgb::hex(0x414868),
                Rgb::hex(0xff899d),
                Rgb::hex(0xb9f27c),
                Rgb::hex(0xffc777),
                Rgb::hex(0x8db0ff),
                Rgb::hex(0xc7a9ff),
                Rgb::hex(0xa4daff),
                Rgb::hex(0xc0caf5),
            ],
        }
    }
}

impl Theme {
    pub fn ansi_color(&self, index: u8) -> Rgb {
        self.ansi[(index as usize) % 16]
    }

    /// Resolve a palette index (0..=255) to RGB, interpolating the 6x6x6
    /// colour cube and greyscale ramp the same way xterm does.
    pub fn palette_color(&self, index: u8) -> Rgb {
        if index < 16 {
            return self.ansi_color(index);
        }
        if index < 232 {
            let i = index as u32 - 16;
            let steps = [0u8, 95, 135, 175, 215, 255];
            return Rgb(
                steps[(i / 36) as usize],
                steps[((i % 36) / 6) as usize],
                steps[(i % 6) as usize],
            );
        }
        let v = 8 + (index as u32 - 232) * 10;
        Rgb(v as u8, v as u8, v as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_cube_and_greyscale() {
        let t = Theme::default();
        assert_eq!(t.palette_color(16), Rgb(0, 0, 0));
        assert_eq!(t.palette_color(231), Rgb(255, 255, 255));
        assert_eq!(t.palette_color(232), Rgb(8, 8, 8));
        assert_eq!(t.palette_color(255), Rgb(238, 238, 238));
        assert_eq!(t.palette_color(196), Rgb(255, 0, 0));
    }
}
