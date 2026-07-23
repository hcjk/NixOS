use core::fmt;

use limine::framebuffer::{FRAMEBUFFER_RGB, Framebuffer};
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};

const LINE_HEIGHT: usize = 20;
const MARGIN_X: usize = 18;
const MARGIN_Y: usize = 14;

#[derive(Clone, Copy)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Color {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

pub const BACKGROUND: Color = Color::new(8, 18, 32);
pub const FOREGROUND: Color = Color::new(220, 231, 242);
pub const ACCENT: Color = Color::new(54, 211, 153);
pub const INFO: Color = Color::new(96, 165, 250);
pub const MUTED: Color = Color::new(148, 163, 184);
pub const WARNING: Color = Color::new(251, 191, 36);

pub struct Console {
    address: *mut u8,
    width: usize,
    height: usize,
    pitch: usize,
    bytes_per_pixel: usize,
    red_mask_size: u8,
    red_mask_shift: u8,
    green_mask_size: u8,
    green_mask_shift: u8,
    blue_mask_size: u8,
    blue_mask_shift: u8,
    cursor_x: usize,
    cursor_y: usize,
    foreground: Color,
    background: Color,
    enabled: bool,
}

impl Console {
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            address: core::ptr::null_mut(),
            width: 0,
            height: 0,
            pitch: 0,
            bytes_per_pixel: 0,
            red_mask_size: 0,
            red_mask_shift: 0,
            green_mask_size: 0,
            green_mask_shift: 0,
            blue_mask_size: 0,
            blue_mask_shift: 0,
            cursor_x: 0,
            cursor_y: 0,
            foreground: FOREGROUND,
            background: BACKGROUND,
            enabled: false,
        }
    }

    #[must_use]
    pub fn from_framebuffer(framebuffer: &Framebuffer) -> Option<Self> {
        let bpp = usize::from(framebuffer.bpp);
        if framebuffer.memory_model != FRAMEBUFFER_RGB
            || !matches!(bpp, 24 | 32)
            || framebuffer.width == 0
            || framebuffer.height == 0
        {
            return None;
        }
        Some(Self {
            address: framebuffer.address().cast::<u8>(),
            width: usize::try_from(framebuffer.width).ok()?,
            height: usize::try_from(framebuffer.height).ok()?,
            pitch: usize::try_from(framebuffer.pitch).ok()?,
            bytes_per_pixel: bpp / 8,
            red_mask_size: framebuffer.red_mask_size,
            red_mask_shift: framebuffer.red_mask_shift,
            green_mask_size: framebuffer.green_mask_size,
            green_mask_shift: framebuffer.green_mask_shift,
            blue_mask_size: framebuffer.blue_mask_size,
            blue_mask_shift: framebuffer.blue_mask_shift,
            cursor_x: MARGIN_X,
            cursor_y: MARGIN_Y,
            foreground: FOREGROUND,
            background: BACKGROUND,
            enabled: true,
        })
    }

    pub fn clear(&mut self) {
        if !self.enabled {
            return;
        }
        self.fill_rect(0, 0, self.width, self.height, self.background);
        self.cursor_x = MARGIN_X;
        self.cursor_y = MARGIN_Y;
    }

    pub fn draw_header(&mut self) {
        if !self.enabled {
            return;
        }
        self.fill_rect(0, 0, self.width, 5, ACCENT);
        self.fill_rect(0, 5, self.width, 8, Color::new(22, 42, 70));
        self.cursor_y = MARGIN_Y + 5;
    }

    pub fn set_color(&mut self, color: Color) {
        self.foreground = color;
    }

    pub fn reset_color(&mut self) {
        self.foreground = FOREGROUND;
    }

    pub fn backspace(&mut self) {
        if !self.enabled || self.cursor_x <= MARGIN_X {
            return;
        }
        self.cursor_x = self.cursor_x.saturating_sub(Self::character_width());
        self.fill_rect(
            self.cursor_x,
            self.cursor_y,
            Self::character_width(),
            LINE_HEIGHT,
            self.background,
        );
    }

    fn character_width() -> usize {
        get_raster_width(FontWeight::Regular, RasterHeight::Size16)
    }

    fn newline(&mut self) {
        self.cursor_x = MARGIN_X;
        self.cursor_y = self.cursor_y.saturating_add(LINE_HEIGHT);
        if self.cursor_y + LINE_HEIGHT + MARGIN_Y >= self.height {
            self.scroll();
            self.cursor_y = self.height.saturating_sub(LINE_HEIGHT + MARGIN_Y);
        }
    }

    fn write_character(&mut self, character: char) {
        if !self.enabled {
            return;
        }
        match character {
            '\n' => {
                self.newline();
                return;
            }
            '\r' => {
                self.cursor_x = MARGIN_X;
                return;
            }
            '\u{8}' => {
                self.backspace();
                return;
            }
            _ => {}
        }

        let width = Self::character_width();
        if self.cursor_x + width + MARGIN_X >= self.width {
            self.newline();
        }
        let Some(raster) = get_raster(character, FontWeight::Regular, RasterHeight::Size16)
            .or_else(|| get_raster('?', FontWeight::Regular, RasterHeight::Size16))
        else {
            return;
        };
        for (row, pixels) in raster.raster().iter().enumerate() {
            for (column, intensity) in pixels.iter().copied().enumerate() {
                let color = blend(self.background, self.foreground, intensity);
                self.write_pixel(self.cursor_x + column, self.cursor_y + row, color);
            }
        }
        self.cursor_x += raster.width();
    }

    fn scroll(&mut self) {
        if !self.enabled || self.height <= LINE_HEIGHT {
            return;
        }
        let start = LINE_HEIGHT * self.pitch;
        let retained = (self.height - LINE_HEIGHT) * self.pitch;
        for offset in 0..retained {
            // SAFETY: Both addresses lie inside the mapped framebuffer.
            let value = unsafe { self.address.add(start + offset).read_volatile() };
            // SAFETY: The destination is within the first `retained` bytes.
            unsafe { self.address.add(offset).write_volatile(value) };
        }
        self.fill_rect(
            0,
            self.height - LINE_HEIGHT,
            self.width,
            LINE_HEIGHT,
            self.background,
        );
    }

    fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Color) {
        let end_y = y.saturating_add(height).min(self.height);
        let end_x = x.saturating_add(width).min(self.width);
        for py in y..end_y {
            for px in x..end_x {
                self.write_pixel(px, py, color);
            }
        }
    }

    fn write_pixel(&mut self, x: usize, y: usize, color: Color) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = y * self.pitch + x * self.bytes_per_pixel;
        let packed = pack_component(color.red, self.red_mask_size, self.red_mask_shift)
            | pack_component(color.green, self.green_mask_size, self.green_mask_shift)
            | pack_component(color.blue, self.blue_mask_size, self.blue_mask_shift);
        let bytes = packed.to_le_bytes();
        for (index, value) in bytes.iter().copied().take(self.bytes_per_pixel).enumerate() {
            // SAFETY: Offset was calculated from validated framebuffer bounds.
            unsafe { self.address.add(offset + index).write_volatile(value) };
        }
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for character in text.chars() {
            self.write_character(character);
        }
        Ok(())
    }
}

fn pack_component(value: u8, mask_size: u8, shift: u8) -> u32 {
    if mask_size == 0 {
        return 0;
    }
    let maximum = (1_u32 << mask_size.min(8)) - 1;
    ((u32::from(value) * maximum / 255) & maximum) << shift
}

fn blend(background: Color, foreground: Color, intensity: u8) -> Color {
    let alpha = u16::from(intensity);
    let inverse = 255 - alpha;
    Color::new(
        blend_component(background.red, foreground.red, alpha, inverse),
        blend_component(background.green, foreground.green, alpha, inverse),
        blend_component(background.blue, foreground.blue, alpha, inverse),
    )
}

fn blend_component(background: u8, foreground: u8, alpha: u16, inverse: u16) -> u8 {
    let value = (u16::from(background) * inverse + u16::from(foreground) * alpha) / 255;
    u8::try_from(value).unwrap_or(u8::MAX)
}
