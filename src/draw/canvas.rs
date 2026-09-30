//! 卡片画布：一块 RGBA 位图加 handful 个绘图原语。
//!
//! 不用 `imageproc`：需要的只有圆角矩形、alpha 混合和居中裁剪这三件事，
//! 自己写不满一百行，还省掉一个依赖。像素一律越界即忽略（`put` 内部判界），
//! 所以绘图代码不必每处都算 min/max。

use std::io::Cursor;

use image::{ImageBuffer, ImageFormat, Rgba, RgbaImage};

/// 一个 RGBA 颜色，通道 0-255
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub [u8; 4]);

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color([r, g, b, 255])
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color([r, g, b, a])
    }

    pub fn alpha(self, a: u8) -> Color {
        Color([self.0[0], self.0[1], self.0[2], a])
    }

    /// `#rrggbb`、`#rgb`、`#aarrggbb`。九位写法沿用 B 站与原插件的口径：**前两位是 alpha**
    pub fn parse(text: &str) -> Option<Color> {
        let head = text.trim().trim_start_matches('#');
        let digits = head
            .chars()
            .map(|ch| ch.to_digit(16))
            .collect::<Option<Vec<u32>>>()?;
        let pair = |index: usize| (digits[index] * 16 + digits[index + 1]) as u8;
        match digits.len() {
            3 => {
                let short = |index: usize| (digits[index] * 17) as u8;
                Some(Color::rgb(short(0), short(1), short(2)))
            }
            6 => Some(Color::rgb(pair(0), pair(2), pair(4))),
            8 => Some(Color::rgba(pair(2), pair(4), pair(6), pair(0))),
            _ => None,
        }
    }

    /// 按 t(0.0~1.0) 在两色之间线性插值
    pub fn mix(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let at = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Color([
            at(self.0[0], other.0[0]),
            at(self.0[1], other.0[1]),
            at(self.0[2], other.0[2]),
            at(self.0[3], other.0[3]),
        ])
    }

    /// HSV 里的 H/S/B 换色：颜色生成器按 UID 取色相用得上
    pub fn from_hsb(hue: f32, saturation: f32, brightness: f32) -> Color {
        let h = (hue / 60.0).rem_euclid(6.0);
        let (s, v) = (saturation.clamp(0.0, 1.0), brightness.clamp(0.0, 1.0));
        let c = v * s;
        let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
        let m = v - c;
        let (r, g, b) = match h as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        Color::rgb(
            ((r + m) * 255.0).round() as u8,
            ((g + m) * 255.0).round() as u8,
            ((b + m) * 255.0).round() as u8,
        )
    }
}

/// 圆角半径：四个角同一值，或指定某几个角
#[derive(Clone, Copy, Debug)]
pub struct Corners {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_left: f32,
    pub bottom_right: f32,
}

impl Corners {
    pub const fn all(radius: f32) -> Corners {
        Corners {
            top_left: radius,
            top_right: radius,
            bottom_left: radius,
            bottom_right: radius,
        }
    }

    pub const fn none() -> Corners {
        Corners {
            top_left: 0.0,
            top_right: 0.0,
            bottom_left: 0.0,
            bottom_right: 0.0,
        }
    }
}

/// 一张卡片就是一块位图
pub struct Canvas {
    image: RgbaImage,
}

impl Canvas {
    pub fn new(width: u32, height: u32, background: Color) -> Canvas {
        Canvas {
            image: ImageBuffer::from_pixel(width.max(1), height.max(1), Rgba(background.0)),
        }
    }

    pub fn width(&self) -> u32 {
        self.image.width()
    }

    pub fn height(&self) -> u32 {
        self.image.height()
    }

    /// 不判界的写入，公开方法都走它
    #[inline]
    fn put(&mut self, x: u32, y: u32, pixel: Rgba<u8>) {
        if x < self.image.width() && y < self.image.height() {
            *self.image.get_pixel_mut(x, y) = pixel;
        }
    }

    /// 供文本层混字形：外部只给覆盖率，不碰画布内部
    pub fn blend_pixel(&mut self, x: u32, y: u32, color: Color, coverage: u8) {
        self.blend(x, y, color, coverage);
    }

    /// 按覆盖率混色：alpha 是 0..=255 的覆盖率，常用于字形与阴影
    #[inline]
    fn blend(&mut self, x: u32, y: u32, color: Color, coverage: u8) {
        if coverage == 0 {
            return;
        }
        let Some(pixel) = self.image.get_pixel_checked(x, y) else {
            return;
        };
        let base = pixel.0;
        let t = coverage as f32 / 255.0;
        let at = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        let alpha = (base[3] as f32 + (color.0[3] as f32 * t) * (1.0 - base[3] as f32 / 255.0))
            .round()
            .clamp(0.0, 255.0) as u8;
        self.put(
            x,
            y,
            Rgba([
                at(base[0], color.0[0]),
                at(base[1], color.0[1]),
                at(base[2], color.0[2]),
                alpha,
            ]),
        );
    }

    pub fn fill_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        self.fill_round_rect(x, y, width, height, Corners::none(), color);
    }

    /// 实心圆角矩形（半径 0 就是直角），半透明色走混合
    pub fn fill_round_rect(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        radius: Corners,
        color: Color,
    ) {
        if width == 0 || height == 0 {
            return;
        }
        let opaque = color.0[3] == 255;
        for row in 0..height {
            for column in 0..width {
                if !inside_round_area(column, row, width, height, radius) {
                    continue;
                }
                if opaque {
                    self.put(x + column, y + row, Rgba(color.0));
                } else {
                    self.blend(x + column, y + row, color, color.0[3]);
                }
            }
        }
    }

    /// 圆角边框：落在"外层圆角内 && 内缩框外"的差集上
    // 落点、尺寸、圆角、线宽、颜色本来就该一次说全，包成结构体只是多一层搬运
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_round_rect(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        radius: Corners,
        thickness: u32,
        color: Color,
    ) {
        if width == 0 || height == 0 || thickness == 0 {
            return;
        }
        let t = thickness;
        let (iw, ih) = (width.saturating_sub(t * 2), height.saturating_sub(t * 2));
        let inner = Corners {
            top_left: (radius.top_left - t as f32).max(0.0),
            top_right: (radius.top_right - t as f32).max(0.0),
            bottom_left: (radius.bottom_left - t as f32).max(0.0),
            bottom_right: (radius.bottom_right - t as f32).max(0.0),
        };
        for row in 0..height {
            for column in 0..width {
                if !inside_round_area(column, row, width, height, radius) {
                    continue;
                }
                let hollow = iw > 0
                    && ih > 0
                    && column >= t
                    && column - t < iw
                    && row >= t
                    && row - t < ih
                    && inside_round_area(column - t, row - t, iw, ih, inner);
                if !hollow {
                    self.blend(x + column, y + row, color, color.0[3]);
                }
            }
        }
    }

    /// 竖直渐变填充
    pub fn fill_vertical_gradient(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        top: Color,
        bottom: Color,
    ) {
        for row in 0..height {
            let t = if height <= 1 {
                0.0
            } else {
                row as f32 / (height - 1) as f32
            };
            let color = top.mix(bottom, t);
            for column in 0..width {
                self.blend(x + column, y + row, color, color.0[3]);
            }
        }
    }

    pub fn fill_circle(&mut self, center_x: u32, center_y: u32, radius: u32, color: Color) {
        let r = radius as f32;
        let top = center_y.saturating_sub(radius);
        let bottom = (center_y + radius).min(self.image.height().saturating_sub(1));
        let left = center_x.saturating_sub(radius);
        let right = (center_x + radius).min(self.image.width().saturating_sub(1));
        for row in top..=bottom {
            for column in left..=right {
                let dx = column as f32 - center_x as f32;
                let dy = row as f32 - center_y as f32;
                let distance = (dx * dx + dy * dy).sqrt();
                if distance <= r - 0.5 {
                    self.blend(column, row, color, 255);
                } else if distance <= r + 0.5 {
                    // 边缘按覆盖率抗锯齿
                    let coverage = ((r + 0.5 - distance) * 255.0).round().clamp(0.0, 255.0) as u8;
                    self.blend(column, row, color, coverage);
                }
            }
        }
    }

    /// 头像这种圆形图：按左上角 (left, top) 与直径裁剪贴图
    pub fn paste_circle_image(&mut self, left: u32, top: u32, radius: u32, src: &RgbaImage) {
        let diameter = radius * 2;
        if diameter == 0 {
            return;
        }
        let scaled = resize(src, diameter, diameter);
        let r = radius as f32;
        for row in 0..diameter {
            for column in 0..diameter {
                let dx = column as f32 - r + 0.5;
                let dy = row as f32 - r + 0.5;
                let distance = (dx * dx + dy * dy).sqrt();
                if distance > r + 0.5 {
                    continue;
                }
                let coverage = if distance <= r - 0.5 {
                    255
                } else {
                    ((r + 0.5 - distance) * 255.0).round().clamp(0.0, 255.0) as u8
                };
                let pixel = scaled.get_pixel(column, row).0;
                let alpha = ((pixel[3] as u32 * coverage as u32) / 255) as u8;
                let color = Color::rgba(pixel[0], pixel[1], pixel[2], alpha);
                self.blend(left + column, top + row, color, alpha);
            }
        }
    }

    /// 原样贴图（不缩放），超出画布的部分忽略
    pub fn paste(&mut self, x: u32, y: u32, src: &RgbaImage) {
        self.paste_area(x, y, src, 0, 0, src.width(), src.height());
    }

    // 目标落点两个数、源矩形四个数，这就是贴图本身
    #[allow(clippy::too_many_arguments)]
    pub fn paste_area(
        &mut self,
        x: u32,
        y: u32,
        src: &RgbaImage,
        source_x: u32,
        source_y: u32,
        source_width: u32,
        source_height: u32,
    ) {
        for row in 0..source_height {
            for column in 0..source_width {
                let (sx, sy) = (source_x + column, source_y + row);
                if sx >= src.width() || sy >= src.height() {
                    continue;
                }
                let [sr, sg, sb, sa] = src.get_pixel(sx, sy).0;
                if sa == 0 {
                    continue;
                }
                self.blend(x + column, y + row, Color::rgba(sr, sg, sb, sa), sa);
            }
        }
    }

    /// 居中裁剪后贴进目标框：卡片里的图片都长这样（圆角给 none 就是直角）
    pub fn paste_cover_rounded(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        radius: Corners,
        src: &RgbaImage,
    ) {
        let Some((sw, sh)) = fit_cover(src.width(), src.height(), width, height) else {
            return;
        };
        let scaled = resize(src, sw, sh);
        let offset_x = (sw - width) / 2;
        let offset_y = (sh - height) / 2;
        for row in 0..height {
            for column in 0..width {
                if !inside_round_area(column, row, width, height, radius) {
                    continue;
                }
                let [r, g, b, a] = scaled.get_pixel(offset_x + column, offset_y + row).0;
                if a == 0 {
                    continue;
                }
                self.blend(x + column, y + row, Color::rgba(r, g, b, a), a);
            }
        }
    }

    /// 卡片一律出 PNG：QQ 侧会再压一次，插件这边不必先掉一层质量
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        if self.image.write_to(&mut out, ImageFormat::Png).is_err() {
            return Vec::new();
        }
        out.into_inner()
    }
}

/// 等比缩放到刚好盖住框（短边贴合），裁剪贴图用
pub fn fit_cover(
    source_width: u32,
    source_height: u32,
    box_width: u32,
    box_height: u32,
) -> Option<(u32, u32)> {
    if source_width == 0 || source_height == 0 || box_width == 0 || box_height == 0 {
        return None;
    }
    let scale =
        (box_width as f32 / source_width as f32).max(box_height as f32 / source_height as f32);
    Some((
        (source_width as f32 * scale).round().max(1.0) as u32,
        (source_height as f32 * scale).round().max(1.0) as u32,
    ))
}

/// 双线性缩放。`image` 的 `resize` 走的是 Lanczos3，对头像与封面够用且更慢，
/// 卡片里同一张图常被贴多次，这里用一次双线性把结果留着复用。
pub fn resize(src: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    if src.width() == width && src.height() == height {
        return src.clone();
    }
    let mut out = ImageBuffer::from_pixel(width.max(1), height.max(1), Rgba([0, 0, 0, 0]));
    if width == 0 || height == 0 || src.width() == 0 || src.height() == 0 {
        return out;
    }
    let (sw, sh) = (src.width() as f32, src.height() as f32);
    for row in 0..height {
        for column in 0..width {
            let u = ((column as f32 + 0.5) * sw / width as f32 - 0.5).clamp(0.0, sw - 1.0);
            let v = ((row as f32 + 0.5) * sh / height as f32 - 0.5).clamp(0.0, sh - 1.0);
            let (x0, y0) = (u.floor() as u32, v.floor() as u32);
            let (x1, y1) = (
                (x0 + 1).min(src.width() - 1),
                (y0 + 1).min(src.height() - 1),
            );
            let (fx, fy) = (u - x0 as f32, v - y0 as f32);
            let p00 = src.get_pixel(x0, y0).0;
            let p10 = src.get_pixel(x1, y0).0;
            let p01 = src.get_pixel(x0, y1).0;
            let p11 = src.get_pixel(x1, y1).0;
            let lerp = |a: u8, b: u8, c: u8, d: u8| {
                let top = a as f32 + (b as f32 - a as f32) * fx;
                let bottom = c as f32 + (d as f32 - c as f32) * fx;
                (top + (bottom - top) * fy).round().clamp(0.0, 255.0) as u8
            };
            *out.get_pixel_mut(column, row) = Rgba([
                lerp(p00[0], p10[0], p01[0], p11[0]),
                lerp(p00[1], p10[1], p01[1], p11[1]),
                lerp(p00[2], p10[2], p01[2], p11[2]),
                lerp(p00[3], p10[3], p01[3], p11[3]),
            ]);
        }
    }
    out
}

/// 点是否落在圆角矩形内。坐标是**框内相对坐标**（0..width、0..height），不是画布坐标。
/// 判的是像素中心，角上的小方块用该角圆心的距离判，直边段一律算内部。
fn inside_round_area(px: u32, py: u32, width: u32, height: u32, radius: Corners) -> bool {
    if width == 0 || height == 0 || px >= width || py >= height {
        return false;
    }
    let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
    let (w, h) = (width as f32, height as f32);
    let (left, top) = (fx < w / 2.0, fy < h / 2.0);
    let r = if top {
        if left {
            radius.top_left
        } else {
            radius.top_right
        }
    } else if left {
        radius.bottom_left
    } else {
        radius.bottom_right
    };
    if r <= 0.0 {
        return true;
    }
    // 离所属那条横/竖边都超过 r，就是落在直边段上，必在内部
    let (edge_x, edge_y) = (
        if left { fx } else { w - fx },
        if top { fy } else { h - fy },
    );
    if edge_x >= r || edge_y >= r {
        return true;
    }
    let (cx, cy) = (if left { r } else { w - r }, if top { r } else { h - r });
    (fx - cx).powi(2) + (fy - cy).powi(2) <= r * r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_both_notations() {
        assert_eq!(Color::parse("#ff8800"), Some(Color::rgb(255, 136, 0)));
        assert_eq!(Color::parse("f80"), Some(Color::rgb(255, 136, 0)));
        // 每个字节由两个 hex 字符合成，别把 nibble 当字节
        assert_eq!(Color::parse("#FB7299"), Some(Color::rgb(251, 114, 153)));
        // 九位是 B 站口径的 #AARRGGBB：#B4FFFFFF 就是七成不透明的白
        assert_eq!(
            Color::parse("#b4ffffff"),
            Some(Color::rgba(255, 255, 255, 0xB4))
        );
        assert_eq!(Color::parse("不是颜色"), None);
    }

    #[test]
    fn hsb_matches_the_billibili_blue() {
        let color = Color::from_hsb(204.0, 0.66, 0.95);
        // 只验个方向：B 站主色相在蓝色那一侧，红通道必须比蓝通道暗
        assert!(color.0[2] > color.0[0], "{color:?} 不该是红的");
    }

    #[test]
    fn rounded_fill_leaves_the_corners_untouched() {
        let mut canvas = Canvas::new(20, 20, Color::rgb(0, 0, 0));
        canvas.fill_round_rect(0, 0, 20, 20, Corners::all(10.0), Color::rgb(255, 255, 255));
        assert_eq!(canvas.image.get_pixel(0, 0).0[0], 0, "左上角该留在底色上");
        assert_eq!(canvas.image.get_pixel(10, 10).0[0], 255, "圆心必须被填上");
    }

    #[test]
    fn round_corners_govern_only_the_corner_squares() {
        let radius = Corners::all(6.0);
        // 顶边中段离右角顶点 9.5 > 6，必须算内部：上一版拿画布绝对坐标判象限，正是在这里错
        assert!(
            inside_round_area(10, 0, 20, 20, radius),
            "直边段被误判成角外"
        );
        assert!(
            inside_round_area(0, 10, 20, 20, radius),
            "左边段被误判成角外"
        );
        assert!(inside_round_area(10, 10, 20, 20, radius));
        assert!(!inside_round_area(0, 0, 20, 20, radius), "角顶点必须被切掉");
        assert!(
            inside_round_area(3, 3, 20, 20, radius),
            "角方块内的点不该被切"
        );
    }

    #[test]
    fn stroke_paints_the_band_and_leaves_the_hollow() {
        let mut canvas = Canvas::new(20, 20, Color::rgb(0, 0, 0));
        canvas.stroke_round_rect(0, 0, 20, 20, Corners::none(), 2, Color::rgb(255, 255, 255));
        assert_eq!(canvas.image.get_pixel(0, 10).0[0], 255, "顶边该描到");
        assert_eq!(canvas.image.get_pixel(2, 10).0[0], 0, "内缩两像素就该空");
        assert_eq!(canvas.image.get_pixel(10, 10).0[0], 0, "中心不该动");
        assert_eq!(canvas.image.get_pixel(19, 19).0[0], 255, "右下角该描到");
    }

    #[test]
    fn cover_crop_fills_the_box() {
        let src = ImageBuffer::from_pixel(200, 100, Rgba([10, 20, 30, 255]));
        let mut canvas = Canvas::new(50, 50, Color::rgb(0, 0, 0));
        canvas.paste_cover_rounded(0, 0, 50, 50, Corners::none(), &src);
        for (x, y) in [(0u32, 0u32), (49, 0), (0, 49), (49, 49)] {
            assert_eq!(canvas.image.get_pixel(x, y).0[0], 10, "({x},{y}) 没被盖住");
        }
    }

    #[test]
    fn out_of_bounds_draws_are_ignored() {
        let mut canvas = Canvas::new(4, 4, Color::rgb(1, 1, 1));
        canvas.fill_rect(2, 2, 100, 100, Color::rgb(9, 9, 9));
        canvas.paste(3, 3, &ImageBuffer::from_pixel(5, 5, Rgba([7, 7, 7, 255])));
        assert_eq!(canvas.image.get_pixel(3, 3).0, [7, 7, 7, 255]);
        assert_eq!(canvas.width(), 4);
    }

    #[test]
    fn png_roundtrip_keeps_pixels() {
        let mut canvas = Canvas::new(6, 4, Color::rgb(1, 2, 3));
        canvas.fill_rect(1, 1, 2, 2, Color::rgb(200, 100, 50));
        let bytes = canvas.to_png();
        let back = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(back.width(), 6);
        assert_eq!(back.get_pixel(2, 2).0, [200, 100, 50, 255]);
        assert_eq!(back.get_pixel(0, 0).0, [1, 2, 3, 255]);
    }
}
