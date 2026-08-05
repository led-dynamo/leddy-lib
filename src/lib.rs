#![forbid(unsafe_code)]

mod font5x7;

use leddy_interfaces::{DisplayConfig, ScrollDirection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameBuffer {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl FrameBuffer {
    pub fn new(config: &DisplayConfig) -> Self {
        Self {
            width: usize::from(config.width),
            height: usize::from(config.height),
            pixels: vec![0; config.pixel_count()],
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn clear(&mut self) {
        self.pixels.fill(0);
    }

    pub fn set(&mut self, x: i32, y: i32, value: u8) {
        if x < 0 || y < 0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x < self.width && y < self.height {
            self.pixels[y * self.width + x] = value;
        }
    }

    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.pixels[y * self.width + x]
    }

    pub fn row_major(&self) -> &[u8] {
        &self.pixels
    }

    pub fn serpentine(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(self.pixels.len());
        for y in 0..self.height {
            let row = &self.pixels[y * self.width..(y + 1) * self.width];
            if y % 2 == 0 {
                output.extend_from_slice(row);
            } else {
                output.extend(row.iter().rev().copied());
            }
        }
        output
    }
}

pub fn content_width(text: &str) -> usize {
    let glyphs = text.chars().count();
    glyphs.saturating_mul(6).saturating_sub(1)
}

pub fn scroll_offset(
    elapsed_ms: u64,
    speed_pixels_per_second: f32,
    content_width: usize,
    display_width: usize,
    direction: ScrollDirection,
) -> i32 {
    let travel = content_width.saturating_add(display_width).max(1);
    let moved = ((elapsed_ms as f64 / 1000.0) * f64::from(speed_pixels_per_second)) as usize;
    let phase = moved % travel;
    match direction {
        ScrollDirection::Left => phase as i32 - display_width as i32,
        ScrollDirection::Right => content_width as i32 - phase as i32,
    }
}

pub fn render_text_5x7(frame: &mut FrameBuffer, text: &str, content_offset: i32) {
    frame.clear();
    let y_origin = ((frame.height() as i32 - 7) / 2).max(0);

    for (glyph_index, character) in text.chars().enumerate() {
        let glyph = font5x7::glyph(character);
        let glyph_x = glyph_index as i32 * 6 - content_offset;
        for (column_index, column) in glyph.iter().copied().enumerate() {
            for bit in 0..7 {
                if column & (1 << bit) != 0 {
                    frame.set(glyph_x + column_index as i32, y_origin + bit, 255);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leddy_interfaces::{PixelOrigin, ScrollDirection};

    fn config() -> DisplayConfig {
        DisplayConfig {
            width: 100,
            height: 10,
            brightness: 96,
            serpentine: true,
            origin: PixelOrigin::TopLeft,
        }
    }

    #[test]
    fn message_can_be_wider_than_display() {
        let text = "AN ARBITRARILY LONG LEDDY MESSAGE";
        assert!(content_width(text) > usize::from(config().width));
    }

    #[test]
    fn renderer_writes_pixels_without_overflowing() {
        let mut frame = FrameBuffer::new(&config());
        render_text_5x7(&mut frame, "LEDDY", 0);
        assert_eq!(frame.row_major().len(), 1_000);
        assert!(frame.row_major().iter().any(|pixel| *pixel != 0));
        assert_eq!(frame.serpentine().len(), 1_000);
    }

    #[test]
    fn scroll_is_periodic() {
        let width = content_width("HELLO");
        let first = scroll_offset(0, 20.0, width, 100, ScrollDirection::Left);
        let cycle_ms = ((width + 100) as f32 / 20.0 * 1000.0) as u64;
        let second = scroll_offset(cycle_ms, 20.0, width, 100, ScrollDirection::Left);
        assert_eq!(first, second);
    }
}
