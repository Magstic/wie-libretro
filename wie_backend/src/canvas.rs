mod lbmp;

use alloc::{borrow::Cow, boxed::Box, string::ToString, vec, vec::Vec};
use core::mem::size_of;

use ab_glyph::{Font, FontRef, ScaleFont};
use bytemuck::{Pod, cast_slice, pod_collect_to_vec};
use image::ImageReader;
use num_traits::{Num, Zero};

use wie_util::{Result, WieError};

use self::lbmp::decode_lbmp;

lazy_static::lazy_static! {
    static ref FONT: FontRef<'static> = FontRef::try_from_slice(include_bytes!("../../fonts/neodgm.ttf")).unwrap();
}

pub enum TextAlignment {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Color {
    pub a: u8,
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

pub trait Image: Send {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn bytes_per_pixel(&self) -> u32;
    fn get_pixel(&self, x: i32, y: i32) -> Color;
    fn get_pixels(&self, x: i32, y: i32, colors: &mut [Color]) {
        for (offset, color) in colors.iter_mut().enumerate() {
            *color = self.get_pixel(x + offset as i32, y);
        }
    }
    fn raw(&self) -> Cow<'_, [u8]>;
    fn colors(&self) -> Vec<Color>;
    fn argb8888(&self) -> Vec<u32> {
        self.colors()
            .into_iter()
            .map(|color| ((color.a as u32) << 24) | ((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32)
            .collect()
    }
}

pub trait ImageBuffer: Send {
    fn put_pixel(&mut self, x: i32, y: i32, color: Color);
    fn put_pixels(&mut self, x: i32, y: i32, width: u32, colors: &[Color]);
}

#[allow(clippy::too_many_arguments)]
pub trait Canvas: Send {
    fn image(&self) -> &dyn Image;
    fn draw(&mut self, dx: i32, dy: i32, w: u32, h: u32, src: &dyn Image, sx: i32, sy: i32, clip: Clip);
    fn draw_line(&mut self, x1: i32, y1: i32, x2: i32, y2: i32, color: Color);
    fn draw_text(&mut self, string: &str, x: i32, y: i32, text_alignment: TextAlignment, color: Color);
    fn draw_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: Color, clip: Clip);
    fn draw_arc(&mut self, x: i32, y: i32, w: u32, h: u32, start_angle: u32, arc_angle: u32, color: Color, clip: Clip);
    fn draw_round_rect(&mut self, x: i32, y: i32, w: u32, h: u32, arc_width: u32, arc_height: u32, color: Color, clip: Clip);
    fn fill_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: Color, clip: Clip);
    fn fill_arc(&mut self, x: i32, y: i32, w: u32, h: u32, start_angle: u32, arc_angle: u32, color: Color, clip: Clip);
    fn fill_round_rect(&mut self, x: i32, y: i32, w: u32, h: u32, arc_width: u32, arc_height: u32, color: Color, clip: Clip);
    fn put_pixel(&mut self, x: i32, y: i32, color: Color);
}

pub trait PixelType: Send {
    type DataType: Copy + Pod + Num + Send;
    fn from_color(color: Color) -> Self::DataType;
    fn to_color(raw: Self::DataType) -> Color;
}

pub struct Rgb332Pixel;

impl PixelType for Rgb332Pixel {
    type DataType = u8;

    fn from_color(color: Color) -> Self::DataType {
        let r = (color.r * 7 + 127) / 255;
        let g = (color.g * 7 + 127) / 255;
        let b = (color.b * 3 + 127) / 255;

        (r << 5) | (g << 2) | b
    }

    fn to_color(raw: Self::DataType) -> Color {
        let r = (raw >> 5) & 0x7;
        let g = (raw >> 2) & 0x7;
        let b = raw & 0x3;

        Color {
            a: 0xff,
            r: r * 36,
            g: g * 36,
            b: b * 85,
        }
    }
}

pub struct Rgb565Pixel;

impl PixelType for Rgb565Pixel {
    type DataType = u16;

    fn from_color(color: Color) -> Self::DataType {
        let r = (color.r as u16) >> 3;
        let g = (color.g as u16) >> 2;
        let b = (color.b as u16) >> 3;

        (r << 11) | (g << 5) | b
    }

    fn to_color(raw: Self::DataType) -> Color {
        let r = ((raw >> 11) & 0x1f) as u8;
        let g = ((raw >> 5) & 0x3f) as u8;
        let b = (raw & 0x1f) as u8;

        let r = ((r as u32 * 255 + 15) / 31) as u8;
        let g = ((g as u32 * 255 + 31) / 63) as u8;
        let b = ((b as u32 * 255 + 15) / 31) as u8;

        Color { a: 0xff, r, g, b }
    }
}

pub struct Rgb8Pixel;

impl PixelType for Rgb8Pixel {
    type DataType = u32;

    fn from_color(color: Color) -> Self::DataType {
        ((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32
    }

    fn to_color(raw: Self::DataType) -> Color {
        let r = ((raw >> 16) & 0xff) as u8;
        let g = ((raw >> 8) & 0xff) as u8;
        let b = (raw & 0xff) as u8;

        Color { a: 0xff, r, g, b }
    }
}

pub struct ArgbPixel;

impl PixelType for ArgbPixel {
    type DataType = u32;

    fn from_color(color: Color) -> Self::DataType {
        ((color.a as u32) << 24) | ((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32
    }

    fn to_color(raw: Self::DataType) -> Color {
        let a = ((raw >> 24) & 0xff) as u8;
        let r = ((raw >> 16) & 0xff) as u8;
        let g = ((raw >> 8) & 0xff) as u8;
        let b = (raw & 0xff) as u8;

        Color { a, r, g, b }
    }
}

pub struct AbgrPixel;

impl PixelType for AbgrPixel {
    type DataType = u32;

    fn from_color(color: Color) -> Self::DataType {
        ((color.a as u32) << 24) | ((color.b as u32) << 16) | ((color.g as u32) << 8) | color.r as u32
    }

    fn to_color(raw: Self::DataType) -> Color {
        let a = ((raw >> 24) & 0xff) as u8;
        let b = ((raw >> 16) & 0xff) as u8;
        let g = ((raw >> 8) & 0xff) as u8;
        let r = (raw & 0xff) as u8;

        Color { a, r, g, b }
    }
}

pub struct VecImageBuffer<T>
where
    T: PixelType,
{
    width: u32,
    height: u32,
    data: Vec<T::DataType>,
}

impl<T> VecImageBuffer<T>
where
    T: PixelType,
{
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![T::DataType::zero(); (width * height) as usize],
        }
    }

    pub fn from_raw(width: u32, height: u32, raw: Vec<T::DataType>) -> Self {
        Self { width, height, data: raw }
    }
}

impl<T> Image for VecImageBuffer<T>
where
    T: PixelType + 'static,
{
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn bytes_per_pixel(&self) -> u32 {
        size_of::<T::DataType>() as u32
    }

    fn get_pixel(&self, x: i32, y: i32) -> Color {
        let raw = self.data[((y as u32) * self.width + (x as u32)) as usize];

        T::to_color(raw)
    }

    fn get_pixels(&self, x: i32, y: i32, colors: &mut [Color]) {
        let offset = y as usize * self.width as usize + x as usize;
        for (raw, color) in self.data[offset..offset + colors.len()].iter().zip(colors) {
            *color = T::to_color(*raw);
        }
    }

    fn raw(&self) -> Cow<'_, [u8]> {
        cast_slice(&self.data).into()
    }

    fn colors(&self) -> Vec<Color> {
        self.data.iter().map(|&x| T::to_color(x)).collect()
    }

    fn argb8888(&self) -> Vec<u32> {
        self.data
            .iter()
            .map(|&raw| {
                let color = T::to_color(raw);
                ((color.a as u32) << 24) | ((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32
            })
            .collect()
    }
}

impl<T> ImageBuffer for VecImageBuffer<T>
where
    T: PixelType + 'static,
{
    fn put_pixel(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 || (x as u32) >= self.width || (y as u32) >= self.height {
            return;
        }

        let raw = T::from_color(color);

        self.data[((y as u32) * self.width + (x as u32)) as usize] = raw;
    }

    fn put_pixels(&mut self, x: i32, y: i32, width: u32, colors: &[Color]) {
        if width == 0 {
            return;
        }

        for (row, colors) in colors.chunks(width as usize).enumerate() {
            let destination_y = y as i64 + row as i64;
            if destination_y < 0 || destination_y >= self.height as i64 {
                continue;
            }

            let source_start = (-(x as i64)).max(0) as usize;
            let source_end = colors.len().min((self.width as i64 - x as i64).max(0) as usize);
            if source_start >= source_end {
                continue;
            }

            let destination_x = x as i64 + source_start as i64;
            let destination_start = destination_y as usize * self.width as usize + destination_x as usize;
            for (destination, color) in self.data[destination_start..destination_start + source_end - source_start]
                .iter_mut()
                .zip(&colors[source_start..source_end])
            {
                *destination = T::from_color(*color);
            }
        }
    }
}

pub struct ImageBufferCanvas<T>
where
    T: ImageBuffer + Image,
{
    image_buffer: T,
}

impl<T> ImageBufferCanvas<T>
where
    T: ImageBuffer + Image,
{
    pub fn new(image_buffer: T) -> Self {
        Self { image_buffer }
    }

    pub fn into_inner(self) -> T {
        self.image_buffer
    }

    fn blend_pixel(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 || (x as u32) >= self.image_buffer.width() || (y as u32) >= self.image_buffer.height() {
            return;
        }
        if color.a == 0 {
            return;
        }
        if color.a == 0xff {
            self.image_buffer.put_pixel(x, y, color);
            return;
        }

        let bg = self.image_buffer.get_pixel(x, y);
        let alpha = color.a as u32;
        let inverse_alpha = 255 - alpha;

        let computed_color = Color {
            a: 0xff,
            r: ((color.r as u32 * alpha + bg.r as u32 * inverse_alpha) / 255) as u8,
            g: ((color.g as u32 * alpha + bg.g as u32 * inverse_alpha) / 255) as u8,
            b: ((color.b as u32 * alpha + bg.b as u32 * inverse_alpha) / 255) as u8,
        };

        self.image_buffer.put_pixel(x, y, computed_color);
    }
}

#[allow(clippy::too_many_arguments)]
impl<T> Canvas for ImageBufferCanvas<T>
where
    T: ImageBuffer + Image,
{
    fn image(&self) -> &dyn Image {
        &self.image_buffer
    }

    fn draw(&mut self, dx: i32, dy: i32, w: u32, h: u32, src: &dyn Image, sx: i32, sy: i32, clip: Clip) {
        let clip_right = clip.x as i64 + clip.width as i64;
        let clip_bottom = clip.y as i64 + clip.height as i64;
        let x_start = 0i64.max(-(sx as i64)).max(-(dx as i64)).max(clip.x as i64 - dx as i64);
        let y_start = 0i64.max(-(sy as i64)).max(-(dy as i64)).max(clip.y as i64 - dy as i64);
        let x_end = (w as i64)
            .min(src.width() as i64 - sx as i64)
            .min(self.image_buffer.width() as i64 - dx as i64)
            .min(clip_right - dx as i64);
        let y_end = (h as i64)
            .min(src.height() as i64 - sy as i64)
            .min(self.image_buffer.height() as i64 - dy as i64)
            .min(clip_bottom - dy as i64);
        if x_start >= x_end || y_start >= y_end {
            return;
        }

        let row_width = (x_end - x_start) as usize;
        let mut source_row = vec![Color { a: 0, r: 0, g: 0, b: 0 }; row_width];
        for row in y_start..y_end {
            let source_y = sy + row as i32;
            let destination_y = dy + row as i32;
            let destination_x = dx + x_start as i32;
            src.get_pixels(sx + x_start as i32, source_y, &mut source_row);

            if source_row.iter().all(|color| color.a == 0xff) {
                self.image_buffer.put_pixels(destination_x, destination_y, row_width as u32, &source_row);
            } else {
                for (column, color) in source_row.iter().enumerate() {
                    self.blend_pixel(destination_x + column as i32, destination_y, *color);
                }
            }
        }
    }

    fn draw_line(&mut self, x1: i32, y1: i32, x2: i32, y2: i32, color: Color) {
        if x1 == x2 && y1 == y2 {
            self.blend_pixel(x1 as _, y1 as _, color);
            return;
        }

        // bresenham's line drawing
        let dx = (x2 - x1).abs();
        let dy = (y2 - y1).abs();
        let sx = if x1 < x2 { 1 } else { -1 };
        let sy = if y1 < y2 { 1 } else { -1 };
        let mut err = dx - dy;

        let mut x = x1;
        let mut y = y1;

        loop {
            self.blend_pixel(x as _, y as _, color);
            if x == x2 && y == y2 {
                break;
            }

            let e2 = 2 * err;
            if e2 > -dy {
                err -= dy;
                x += sx;
            }
            if e2 < dx {
                err += dx;
                y += sy;
            }
        }
    }

    fn draw_text(&mut self, string: &str, x: i32, y: i32, text_alignment: TextAlignment, color: Color) {
        let size = 10.0; // TODO
        let font = FONT.as_scaled(FONT.pt_to_px_scale(size).unwrap());

        let total_width = string.chars().map(|c| font.h_advance(font.scaled_glyph(c).id)).sum::<f32>();
        let x = match text_alignment {
            TextAlignment::Left => x,
            TextAlignment::Center => x - (total_width / 2.0) as i32,
            TextAlignment::Right => x - total_width as i32,
        };

        let mut position = 0.0;
        for c in string.chars() {
            if c.is_control() {
                continue;
            }

            let glyph = font.scaled_glyph(c);
            let h_advance = font.h_advance(glyph.id);

            if let Some(outlined_glyph) = font.outline_glyph(glyph) {
                outlined_glyph.draw(|glyph_x: u32, glyph_y, c| {
                    let bounds = outlined_glyph.px_bounds();
                    self.blend_pixel(
                        x + (glyph_x as f32 + bounds.min.x + position) as i32,
                        y + (glyph_y as f32 + bounds.min.y + size) as i32,
                        Color {
                            a: (c * 255.0) as u8,
                            r: color.r,
                            g: color.g,
                            b: color.b,
                        },
                    )
                });
            }

            position += h_advance;
        }
    }

    fn draw_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: Color, clip: Clip) {
        if w == 0 || h == 0 {
            return;
        }

        self.fill_rect(x, y, w, 1, color, clip);
        if h > 1 {
            self.fill_rect(x, y.saturating_add_unsigned(h - 1), w, 1, color, clip);
        }
        if h > 2 {
            self.fill_rect(x, y.saturating_add(1), 1, h - 2, color, clip);
            if w > 1 {
                self.fill_rect(x.saturating_add_unsigned(w - 1), y.saturating_add(1), 1, h - 2, color, clip);
            }
        }
    }

    fn draw_arc(&mut self, x: i32, y: i32, w: u32, h: u32, _start_angle: u32, _arc_angle: u32, color: Color, clip: Clip) {
        // TODO unimplemented
        self.draw_rect(x, y, w, h, color, clip);
    }

    fn draw_round_rect(&mut self, x: i32, y: i32, w: u32, h: u32, _arc_width: u32, _arc_height: u32, color: Color, clip: Clip) {
        // TODO unimplemented
        self.draw_rect(x, y, w, h, color, clip);
    }

    fn fill_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: Color, clip: Clip) {
        let x_start = (x as i64).max(0).max(clip.x as i64);
        let y_start = (y as i64).max(0).max(clip.y as i64);
        let x_end = (x as i64 + w as i64)
            .min(self.image_buffer.width() as i64)
            .min(clip.x as i64 + clip.width as i64);
        let y_end = (y as i64 + h as i64)
            .min(self.image_buffer.height() as i64)
            .min(clip.y as i64 + clip.height as i64);
        if x_start >= x_end || y_start >= y_end {
            return;
        }

        let row = vec![color; (x_end - x_start) as usize];
        for y in y_start..y_end {
            self.image_buffer.put_pixels(x_start as i32, y as i32, row.len() as u32, &row);
        }
    }

    fn fill_arc(&mut self, x: i32, y: i32, w: u32, h: u32, _start_angle: u32, _arc_angle: u32, color: Color, clip: Clip) {
        // TODO unimplemented
        self.fill_rect(x, y, w, h, color, clip);
    }

    fn fill_round_rect(&mut self, x: i32, y: i32, w: u32, h: u32, _arc_width: u32, _arc_height: u32, color: Color, clip: Clip) {
        // TODO unimplemented
        self.fill_rect(x, y, w, h, color, clip);
    }

    fn put_pixel(&mut self, x: i32, y: i32, color: Color) {
        self.image_buffer.put_pixel(x, y, color)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Clip {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Clip {
    pub fn intersect(&self, other: &Clip) -> Clip {
        let x = self.x.max(other.x) as i64;
        let y = self.y.max(other.y) as i64;
        let right = (self.x as i64 + self.width as i64).min(other.x as i64 + other.width as i64);
        let bottom = (self.y as i64 + self.height as i64).min(other.y as i64 + other.height as i64);

        Clip {
            x: x as i32,
            y: y as i32,
            width: (right - x).max(0) as u32,
            height: (bottom - y).max(0) as u32,
        }
    }
}

pub fn decode_image(data: &[u8]) -> Result<Box<dyn Image>> {
    extern crate std; // XXX

    use std::io::Cursor;

    if data[0] == b'L' && data[1] == b'B' && data[2] == b'M' && data[3] == b'P' {
        return decode_lbmp(data);
    }

    let image = ImageReader::new(Cursor::new(&data))
        .with_guessed_format()
        .map_err(|x| WieError::FatalError(x.to_string()))?
        .decode()
        .map_err(|x| WieError::FatalError(x.to_string()))?;
    let rgba = image.into_rgba8();

    let data = rgba.pixels().flat_map(|x| [x.0[2], x.0[1], x.0[0], x.0[3]]).collect::<Vec<_>>();

    Ok(Box::new(VecImageBuffer::<ArgbPixel>::from_raw(
        rgba.width(),
        rgba.height(),
        pod_collect_to_vec(&data),
    )) as Box<_>)
}

pub fn string_width(string: &str, pt_size: f32) -> f32 {
    let font = FONT.as_scaled(FONT.pt_to_px_scale(pt_size).unwrap());

    string.chars().map(|c| font.h_advance(font.scaled_glyph(c).id)).sum::<f32>()
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use wie_util::Result;

    use crate::canvas::{Clip, Image, ImageBufferCanvas};

    use super::{ArgbPixel, Canvas, Color, VecImageBuffer};

    #[test]
    fn test_canvas() -> Result<()> {
        let image_buffer = VecImageBuffer::<ArgbPixel>::new(10, 10);
        let mut canvas = ImageBufferCanvas::new(image_buffer);

        let clip = Clip {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        canvas.fill_rect(0, 0, 10, 10, Color { r: 0, g: 0, b: 0, a: 255 }, clip);

        let image_buffer = canvas.into_inner();
        let raw = image_buffer.raw();

        assert_eq!(raw.len(), 10 * 10 * 4);
        for i in 0..10 * 10 {
            assert_eq!(raw[i * 4], 0);
            assert_eq!(raw[i * 4 + 1], 0);
            assert_eq!(raw[i * 4 + 2], 0);
            assert_eq!(raw[i * 4 + 3], 255);
        }

        Ok(())
    }

    #[test]
    fn draw_clips_and_blends_without_touching_uncovered_pixels() {
        let mut canvas = ImageBufferCanvas::new(VecImageBuffer::<ArgbPixel>::from_raw(3, 1, vec![0xff0000ff; 3]));
        let source = VecImageBuffer::<ArgbPixel>::from_raw(3, 1, vec![0x00ff0000, 0x80ff0000, 0xffff0000]);

        canvas.draw(
            0,
            0,
            3,
            1,
            &source,
            0,
            0,
            Clip {
                x: 0,
                y: 0,
                width: 3,
                height: 1,
            },
        );

        assert_eq!(
            canvas.image().colors(),
            vec![
                Color {
                    a: 0xff,
                    r: 0,
                    g: 0,
                    b: 0xff
                },
                Color {
                    a: 0xff,
                    r: 128,
                    g: 0,
                    b: 127
                },
                Color {
                    a: 0xff,
                    r: 0xff,
                    g: 0,
                    b: 0
                },
            ]
        );
    }

    #[test]
    fn clip_intersection_is_empty_when_rectangles_do_not_overlap() {
        let result = Clip {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        }
        .intersect(&Clip {
            x: 20,
            y: 20,
            width: 5,
            height: 5,
        });

        assert_eq!(result.width, 0);
        assert_eq!(result.height, 0);
    }

    #[test]
    fn draw_line_includes_both_endpoints() {
        let mut canvas = ImageBufferCanvas::new(VecImageBuffer::<ArgbPixel>::new(3, 1));
        canvas.draw_line(0, 0, 2, 0, Color { a: 0xff, r: 1, g: 2, b: 3 });

        assert_eq!(canvas.image().colors(), vec![Color { a: 0xff, r: 1, g: 2, b: 3 }; 3]);
    }
}
