#![forbid(unsafe_code)]

mod font5x7;

use leddy_interfaces::{
    DisplayConfig, MessageEnvelope, PixelOrigin, RepeatMode, ScrollDirection, ValidationError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameBuffer {
    width: usize,
    height: usize,
    serpentine: bool,
    origin: PixelOrigin,
    pixels: Vec<u8>,
}

impl FrameBuffer {
    pub fn new(config: &DisplayConfig) -> Self {
        Self {
            width: usize::from(config.width),
            height: usize::from(config.height),
            serpentine: config.serpentine,
            origin: config.origin,
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

    pub fn device_order(&self) -> Vec<u8> {
        let starts_on_right = matches!(
            self.origin,
            PixelOrigin::TopRight | PixelOrigin::BottomRight
        );
        let starts_on_bottom = matches!(
            self.origin,
            PixelOrigin::BottomLeft | PixelOrigin::BottomRight
        );
        let mut output = Vec::with_capacity(self.pixels.len());

        for physical_row in 0..self.height {
            let y = if starts_on_bottom {
                self.height - 1 - physical_row
            } else {
                physical_row
            };
            let row_starts_on_right =
                starts_on_right ^ (self.serpentine && physical_row % 2 == 1);

            for physical_column in 0..self.width {
                let x = if row_starts_on_right {
                    self.width - 1 - physical_column
                } else {
                    physical_column
                };
                output.push(self.get(x, y));
            }
        }

        output
    }
}

pub fn content_width(text: &str) -> usize {
    let glyphs = text.chars().count();
    glyphs.saturating_mul(6).saturating_sub(1)
}

pub fn scroll_cycle_duration_ms(
    speed_pixels_per_second: f32,
    content_width: usize,
    display_width: usize,
) -> Option<u64> {
    if !speed_pixels_per_second.is_finite() || speed_pixels_per_second <= 0.0 {
        return None;
    }

    let travel = content_width.saturating_add(display_width).max(1);
    let duration =
        ((travel as f64 * 1_000.0) / f64::from(speed_pixels_per_second)).ceil();

    if !duration.is_finite() || duration >= u64::MAX as f64 {
        Some(u64::MAX)
    } else {
        Some(duration.max(1.0) as u64)
    }
}

pub fn scroll_offset(
    elapsed_ms: u64,
    speed_pixels_per_second: f32,
    content_width: usize,
    display_width: usize,
    direction: ScrollDirection,
) -> i32 {
    let travel = content_width.saturating_add(display_width).max(1);
    let moved = if speed_pixels_per_second.is_finite() && speed_pixels_per_second > 0.0 {
        ((elapsed_ms as f64 / 1_000.0) * f64::from(speed_pixels_per_second)) as usize
    } else {
        0
    };
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

pub fn render_message_frame(
    config: &DisplayConfig,
    message: &MessageEnvelope,
    elapsed_ms: u64,
) -> Result<Option<FrameBuffer>, ValidationError> {
    config.validate()?;
    message.validate()?;

    let text_width = content_width(&message.text);
    let cycle_duration = scroll_cycle_duration_ms(
        message.speed_pixels_per_second,
        text_width,
        usize::from(config.width),
    )
    .ok_or(ValidationError(
        "scroll speed must be a positive finite number",
    ))?;

    let is_active = match message.repeat {
        RepeatMode::Forever => true,
        RepeatMode::Once => elapsed_ms < cycle_duration,
        RepeatMode::Count(count) => {
            count > 0 && elapsed_ms < cycle_duration.saturating_mul(u64::from(count))
        }
    };
    if !is_active {
        return Ok(None);
    }

    let cycle_elapsed_ms = elapsed_ms % cycle_duration;
    let offset = scroll_offset(
        cycle_elapsed_ms,
        message.speed_pixels_per_second,
        text_width,
        usize::from(config.width),
        message.direction,
    );
    let mut frame = FrameBuffer::new(config);
    render_text_5x7(&mut frame, &message.text, offset);
    Ok(Some(frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> DisplayConfig {
        DisplayConfig {
            width: 100,
            height: 10,
            brightness: 96,
            serpentine: true,
            origin: PixelOrigin::TopLeft,
        }
    }

    fn message(repeat: RepeatMode) -> MessageEnvelope {
        MessageEnvelope {
            id: "message-1".into(),
            text: "HELLO".into(),
            speed_pixels_per_second: 20.0,
            direction: ScrollDirection::Left,
            repeat,
            issued_at_unix_ms: 0,
        }
    }

    fn numbered_frame(origin: PixelOrigin, serpentine: bool) -> FrameBuffer {
        let config = DisplayConfig {
            width: 3,
            height: 2,
            brightness: 96,
            serpentine,
            origin,
        };
        let mut frame = FrameBuffer::new(&config);
        for (index, value) in (1_u8..=6).enumerate() {
            frame.set((index % 3) as i32, (index / 3) as i32, value);
        }
        frame
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
        assert_eq!(frame.device_order().len(), 1_000);
    }

    #[test]
    fn scroll_is_periodic() {
        let width = content_width("HELLO");
        let first = scroll_offset(0, 20.0, width, 100, ScrollDirection::Left);
        let cycle_ms = scroll_cycle_duration_ms(20.0, width, 100).expect("valid speed");
        let second = scroll_offset(cycle_ms, 20.0, width, 100, ScrollDirection::Left);
        assert_eq!(first, second);
    }

    #[test]
    fn invalid_speed_has_no_cycle() {
        assert_eq!(scroll_cycle_duration_ms(0.0, 30, 100), None);
        assert_eq!(scroll_cycle_duration_ms(f32::NAN, 30, 100), None);
    }

    #[test]
    fn device_order_respects_origin_without_serpentine_wiring() {
        assert_eq!(
            numbered_frame(PixelOrigin::TopLeft, false).device_order(),
            vec![1, 2, 3, 4, 5, 6]
        );
        assert_eq!(
            numbered_frame(PixelOrigin::TopRight, false).device_order(),
            vec![3, 2, 1, 6, 5, 4]
        );
        assert_eq!(
            numbered_frame(PixelOrigin::BottomLeft, false).device_order(),
            vec![4, 5, 6, 1, 2, 3]
        );
        assert_eq!(
            numbered_frame(PixelOrigin::BottomRight, false).device_order(),
            vec![6, 5, 4, 3, 2, 1]
        );
    }

    #[test]
    fn device_order_respects_origin_with_serpentine_wiring() {
        assert_eq!(
            numbered_frame(PixelOrigin::TopLeft, true).device_order(),
            vec![1, 2, 3, 6, 5, 4]
        );
        assert_eq!(
            numbered_frame(PixelOrigin::TopRight, true).device_order(),
            vec![3, 2, 1, 4, 5, 6]
        );
        assert_eq!(
            numbered_frame(PixelOrigin::BottomLeft, true).device_order(),
            vec![4, 5, 6, 3, 2, 1]
        );
        assert_eq!(
            numbered_frame(PixelOrigin::BottomRight, true).device_order(),
            vec![6, 5, 4, 1, 2, 3]
        );
    }

    #[test]
    fn once_playback_stops_after_one_cycle() {
        let message = message(RepeatMode::Once);
        let cycle = scroll_cycle_duration_ms(
            message.speed_pixels_per_second,
            content_width(&message.text),
            usize::from(config().width),
        )
        .expect("valid speed");

        assert!(
            render_message_frame(&config(), &message, cycle - 1)
                .expect("valid message")
                .is_some()
        );
        assert!(
            render_message_frame(&config(), &message, cycle)
                .expect("valid message")
                .is_none()
        );
    }

    #[test]
    fn counted_playback_stops_after_requested_cycles() {
        let message = message(RepeatMode::Count(2));
        let cycle = scroll_cycle_duration_ms(
            message.speed_pixels_per_second,
            content_width(&message.text),
            usize::from(config().width),
        )
        .expect("valid speed");

        assert!(
            render_message_frame(&config(), &message, cycle)
                .expect("valid message")
                .is_some()
        );
        assert!(
            render_message_frame(&config(), &message, cycle.saturating_mul(2))
                .expect("valid message")
                .is_none()
        );
    }

    #[test]
    fn forever_playback_remains_active() {
        let message = message(RepeatMode::Forever);
        assert!(
            render_message_frame(&config(), &message, u64::MAX)
                .expect("valid message")
                .is_some()
        );
    }
}
