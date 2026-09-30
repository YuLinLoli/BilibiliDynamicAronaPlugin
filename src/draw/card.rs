//! 动态卡片：把一条动态画成一张竖长图。
//!
//! 布局走两遍：先只累加高度（不带画布的干跑），拿到总高建好画布再原样画一遍。
//! image 的位图必须事先定尺寸，而原插件靠 skia 自动扩画布，所以这两遍是必需的。
//! 尺寸与字号逐档抄自原插件 `draw/` 包（见 [`super::quality`]），块序也是那一套：
//! 作者头部 → 遮挡提示 → 正文 → 图片网格 → 附属卡 → 附加小卡 → 转发原文 → 页脚。
//!
//! 与原插件的已知差别都记在 README：徽标 SVG 不引（用纯色角标代替）、扫掠渐变按线性算、
//! 阴影用多层半透明圆角矩形近似。观感等效，不保证逐像素对齐。

use std::collections::HashMap;
use std::sync::Arc;

use image::{ImageBuffer, RgbaImage};

use super::canvas::{Canvas, Color, Corners};
use super::quality::Quality;
use super::text::{EmojiSet, Flow, Span, Typeface};
use super::theme::Theme;
use crate::bili::model::{Attached, Dynamic, Node};

/// 画一张卡要的全部素材。绘图层不碰网络，缺图就按缺图的样子画——
/// 不能因为一张封面下不下来就不推这条动态。
#[derive(Default)]
pub struct Art {
    /// 头像：key 是 B 站给的 face 地址
    pub faces: HashMap<String, RgbaImage>,
    /// 正文图与各种封面：key 是原图地址（不带缩放后缀的那串）
    pub pictures: HashMap<String, RgbaImage>,
    /// B 站自定义表情：key 是 `[笑]` 这种带方括号的原文
    pub emojis: EmojiSet,
}

impl Art {
    pub fn face(&self, url: &str) -> Option<&RgbaImage> {
        self.faces.get(url)
    }

    pub fn picture(&self, url: &str) -> Option<&RgbaImage> {
        self.pictures.get(url)
    }
}

/// 一张卡的外观参数。全部持有所有权，绘图时不必再跟借用期较劲。
pub struct Style {
    pub quality: Quality,
    pub theme: Theme,
    pub face: Arc<Typeface>,
    /// 整图背景的渐变停靠点（1~N 个）
    pub background: Vec<Color>,
    /// 顶部徽章条
    pub badge: bool,
    /// 徽章条右边那一格显示动态 id
    pub show_id: bool,
    pub footer: String,
    /// 有值就在作者行右侧画二维码（`card_ornament: QrCode`）
    pub qr: Option<String>,
}

/// 一条动态的完整卡片，返回 PNG 字节
pub fn dynamic_card(dynamic: &Dynamic, art: &Art, style: &Style) -> Vec<u8> {
    let height = measure(dynamic, art, style).ceil() as u32;
    let mut canvas = Canvas::new(style.quality.image_width, height, Color::rgb(255, 255, 255));
    paint_background(&mut canvas, &style.background);
    let mut board = Board {
        style,
        art,
        canvas: Some(canvas),
    };
    board.whole(dynamic);
    match board.canvas.take() {
        Some(done) => done.to_png(),
        None => Vec::new(),
    }
}

/// 只量高不画：建画布之前得先知道总高
fn measure(dynamic: &Dynamic, art: &Art, style: &Style) -> f32 {
    Board {
        style,
        art,
        canvas: None,
    }
    .whole(dynamic)
}

/// 一遍绘制。`canvas` 是 None 就只累加坐标。
struct Board<'a> {
    style: &'a Style,
    art: &'a Art,
    canvas: Option<Canvas>,
}

impl<'a> Board<'a> {
    /// 干跑一遍某个块，只问它占多高（画底板之前必须先知道）
    fn probe<F>(&self, run: F) -> f32
    where
        F: FnOnce(&mut Board) -> f32,
    {
        let mut quiet = Board {
            style: self.style,
            art: self.art,
            canvas: None,
        };
        run(&mut quiet)
    }

    /// 整图：上下留 cardMargin，顶部徽章条，中间一张主卡
    fn whole(&mut self, dynamic: &Dynamic) -> f32 {
        let q = self.style.quality;
        let style = self.style;
        let badge_h = q.badge_height_for(style.badge) as f32;
        let card_x = q.card_margin;
        let card_w = q.card_width();
        let top = q.card_margin as f32 + badge_h;
        let body = self.probe(|quiet| quiet.card(card_x, card_w, top, dynamic, false));
        self.card_background(card_x, card_w, top, body + q.card_padding as f32);
        if style.badge {
            self.badges(card_x, top - badge_h, &dynamic.kind, dynamic.id);
        }
        let used = self.card(card_x, card_w, top, dynamic, false);
        top + used + q.card_margin as f32
    }

    /// 一张卡的主体（不含底板），返回占用高度
    fn card(&mut self, x: u32, width: u32, y: f32, dynamic: &Dynamic, sub: bool) -> f32 {
        let q = self.style.quality;
        let space = q.content_space as f32;
        let content_x = x + q.card_padding;
        let content_w = width - q.card_padding * 2;
        let mut cursor = y;
        cursor += self.author(x, width, cursor, dynamic, sub);
        if let Some(tips) = dynamic.blocked.as_deref().filter(|tips| !tips.is_empty()) {
            cursor += space;
            cursor += self.blocked(content_x, content_w, cursor, tips);
        }
        let body = self.body_spans(dynamic);
        if !body.is_empty() {
            cursor += space;
            cursor += self.text_block(content_x, content_w, cursor, &body, q.content_font, None);
        }
        if !dynamic.images.is_empty() {
            cursor += space;
            cursor += self.pictures(content_x, content_w, cursor, &dynamic.images);
        }
        if let Some(item) = dynamic.attached.as_ref() {
            cursor += space;
            cursor += self.attached(content_x, content_w, cursor, item, !sub && is_wide(item));
        }
        for item in &dynamic.extra {
            cursor += space;
            cursor += self.mini(content_x, content_w, cursor, item);
        }
        if let Some(source) = dynamic.origin.as_deref() {
            cursor += space;
            cursor += self.forward(content_x, content_w, cursor, source);
        }
        if !sub {
            cursor += space;
            cursor += self.footer(content_x, content_w, cursor);
        }
        cursor - y
    }

    /// 卡片底板：阴影 + 半透明底 + 描边（画法见 [`paint_board`]，直播卡共用）
    fn card_background(&mut self, x: u32, width: u32, y: f32, height: f32) {
        if let Some(canvas) = &mut self.canvas {
            paint_board(canvas, self.style, x, width, y, height);
        }
    }

    /// 顶部徽章条：左边「动态 / 转发动态 / 直播」，右边可选动态 id 或房间号
    fn badges(&mut self, x: u32, y: f32, label: &str, id: i64) {
        if let Some(canvas) = &mut self.canvas {
            paint_badges(canvas, self.style, self.art, x, y, label, id);
        }
    }

    /// 作者行：头像、昵称、发布时间，右侧可选二维码
    fn author(&mut self, x: u32, width: u32, y: f32, dynamic: &Dynamic, sub: bool) -> f32 {
        let q = self.style.quality;
        let style = self.style;
        let art = self.art;
        let theme = &style.theme;
        let pad = q.card_padding as f32;
        let height = if sub {
            q.face_size as f32 * 0.6 + pad * 2.0
        } else {
            q.pendant_size as f32
        };
        let face_side = if sub {
            (q.face_size as f32 * 0.6).round()
        } else {
            q.face_size as f32
        };
        let inflate = if sub { 0.0 } else { q.face_inflate as f32 };
        let left = (x as f32 + pad * if sub { 1.5 } else { 1.8 } - inflate).max(x as f32) as u32;
        let top = (y + pad * 1.2 - inflate).max(y) as u32;
        let side = face_side as u32;
        let name_x = (x as f32 + face_side + pad * 3.2).min(x as f32 + width as f32 * 0.55);
        let gap = ((q.pendant_size as f32 - q.name_font - q.sub_title_font) / 3.0).max(4.0);
        let name_y = y + q.name_font + gap * 1.25;
        let time_y = name_y + gap;
        let avatar = art.face(&dynamic.face);
        if let Some(canvas) = &mut self.canvas {
            // 没有装扮挂件时，头像放大一圈并描个白边圆
            canvas.fill_circle(
                left + side / 2,
                top + side / 2,
                side / 2 + inflate as u32 + 1,
                theme.face_outline,
            );
            match avatar {
                Some(found) => {
                    canvas.paste_circle_image(left, top, side / 2 + inflate as u32, found)
                }
                None => canvas.fill_circle(
                    left + side / 2,
                    top + side / 2,
                    side / 2,
                    Color::rgb(0xE3, 0xE5, 0xE7),
                ),
            }
        }
        let stamp = if dynamic.label.is_empty() {
            dynamic.time_text()
        } else {
            format!("{} · {}", dynamic.time_text(), dynamic.label)
        };
        let room = ((x + width) as f32 - name_x).max(40.0);
        if let Some(canvas) = &mut self.canvas {
            let line = style.face.truncate(&dynamic.name, q.name_font, true, room);
            style.face.draw_line(
                canvas,
                name_x,
                name_y,
                &line,
                q.name_font,
                theme.nickname,
                true,
            );
            let when = style.face.truncate(&stamp, q.sub_title_font, false, room);
            style.face.draw_line(
                canvas,
                name_x,
                time_y,
                &when,
                q.sub_title_font,
                theme.sub_title,
                false,
            );
        }
        if sub {
            return height;
        }
        let Some(code) = style.qr.as_deref() else {
            return height;
        };
        let side = q.ornament_height;
        // 原插件让二维码在作者行里垂直居中，1000w 那一档算出来是负的，所以不许它顶出卡片
        let offset = (face_side - side as f32 + q.content_space as f32) / 2.0;
        let at_y = (y + offset.max(0.0)).round() as u32;
        if let Some(image) = qr_image(code, side, theme.title) {
            if let Some(canvas) = &mut self.canvas {
                canvas.paste(
                    x + width.saturating_sub(side + q.card_padding),
                    at_y,
                    &image,
                );
            }
        }
        height
    }

    /// 正文的分段：话题、链接、投票这些用链接色，其余用正文色
    fn body_spans<'d>(&self, dynamic: &'d Dynamic) -> Vec<Span<'d>> {
        let theme = &self.style.theme;
        if dynamic.nodes.is_empty() {
            return if dynamic.text.is_empty() {
                Vec::new()
            } else {
                vec![Span::new(&dynamic.text, theme.content)]
            };
        }
        dynamic
            .nodes
            .iter()
            .map(|node| {
                let color = match node {
                    Node::Text(_) | Node::At(_) | Node::Emoji { .. } => theme.content,
                    other => {
                        if other.is_link_like() {
                            theme.link
                        } else {
                            theme.content
                        }
                    }
                };
                Span::new(node.text(), color)
            })
            .collect()
    }

    /// 一块多行文字，返回占用高度
    fn text_block(
        &mut self,
        x: u32,
        width: u32,
        y: f32,
        spans: &[Span],
        font: f32,
        max_lines: Option<usize>,
    ) -> f32 {
        let q = self.style.quality;
        let style = self.style;
        let art = self.art;
        let leading = font + q.line_space as f32;
        let flow = style
            .face
            .layout(spans, font, false, width as f32, max_lines, &art.emojis);
        if let Some(canvas) = &mut self.canvas {
            style.face.draw(
                canvas,
                &flow,
                x as f32,
                y + font,
                font,
                false,
                leading,
                &art.emojis,
            );
        }
        style.face.height(&flow, leading)
    }

    /// 图片网格：1 张按原图比例，2/4 张两列，其余三列
    fn pictures(&mut self, x: u32, width: u32, y: f32, urls: &[String]) -> f32 {
        let q = self.style.quality;
        let style = self.style;
        let art = self.art;
        let shown = urls.len().min(30);
        if shown == 0 {
            return 0.0;
        }
        let space = q.draw_space as f32;
        // 原插件的格子数规则：两张与四张走两列，三张与五张往上走三列，格子里都是正方形
        let (columns, rows) = match shown {
            1 => (1usize, 1usize),
            2 | 4 => (2, shown / 2),
            _ => (3, shown.div_ceil(3)),
        };
        let cell = ((width as f32 - space * (columns - 1) as f32) / columns as f32)
            .floor()
            .max(1.0);
        // 块上下各留一格间距，行间距同值——原插件这里写作 `drawSpace * 行数 - 1`，
        // 那个 `- 1` 是它的笔误，按 `drawSpace * (行数 + 1)` 的口径实现
        let mut bottom = y + space;
        for row in 0..rows {
            let mut row_height = 0.0f32;
            let row_top = bottom;
            for column in 0..columns {
                let index = row * columns + column;
                if index >= shown {
                    break;
                }
                let url = &urls[index];
                let image = art.picture(url);
                let source = image
                    .map(|found| (found.width() as f32, found.height() as f32))
                    .unwrap_or((16.0, 9.0));
                let (box_w, box_h) = if shown == 1 {
                    // 单张按原图比例：够宽就铺满，否则按原宽的两倍，长图封顶两倍宽
                    let wide = if source.0 > width as f32 / 2.0 {
                        width as f32
                    } else {
                        (source.0 * 2.0).min(width as f32)
                    };
                    (wide, (wide * source.1 / source.0.max(1.0)).min(wide * 2.0))
                } else {
                    (cell, cell)
                };
                let long = image
                    .map(|found| found.height() as f32 > found.width() as f32 * 2.0)
                    .unwrap_or(false);
                let animated = url.ends_with(".gif");
                let at_x = x + ((cell + space).round() as u32) * column as u32;
                let at_y = row_top.round() as u32;
                let (w, h) = (box_w.round() as u32, box_h.round() as u32);
                let radius = Corners::all(q.card_arc as f32);
                if let Some(canvas) = &mut self.canvas {
                    canvas.fill_round_rect(
                        at_x,
                        at_y,
                        w,
                        h,
                        radius,
                        Color::rgba(255, 255, 255, 160),
                    );
                    match image {
                        Some(found) => canvas.paste_cover_rounded(at_x, at_y, w, h, radius, found),
                        // 图没下下来留个浅灰框，链接仍在文字里
                        None => canvas.fill_round_rect(
                            at_x,
                            at_y,
                            w,
                            h,
                            radius,
                            Color::rgb(0xEE, 0xF1, 0xF4),
                        ),
                    }
                    canvas.stroke_round_rect(
                        at_x,
                        at_y,
                        w,
                        h,
                        radius,
                        q.draw_outline,
                        style.theme.draw_outline,
                    );
                    if long || animated {
                        let mark = if animated { "动图" } else { "长图" };
                        let font = q.sub_title_font;
                        let badge_w = style.face.measure(mark, font, false, &art.emojis) + 16.0;
                        let badge_h = font + 12.0;
                        canvas.fill_round_rect(
                            at_x + w - badge_w.round() as u32 - 8,
                            at_y + h - badge_h.round() as u32 - 8,
                            badge_w.round() as u32,
                            badge_h.round() as u32,
                            Corners::all(6.0),
                            Color::rgba(0, 0, 0, 130),
                        );
                        style.face.draw_line(
                            canvas,
                            at_x as f32 + box_w - badge_w,
                            row_top + box_h - badge_h - 8.0 + font * 0.8,
                            mark,
                            font,
                            Color::rgb(255, 255, 255),
                            false,
                        );
                    }
                }
                row_height = row_height.max(box_h);
            }
            bottom = row_top + row_height + space;
        }
        (bottom - y).max(0.0)
    }

    /// 附属卡：视频走大卡，其余按小卡
    fn attached(&mut self, x: u32, width: u32, y: f32, item: &Attached, big: bool) -> f32 {
        if big {
            return self.video_card(x, width, y, item);
        }
        self.small_card(x, width, y, item)
    }

    /// 视频大卡：满宽封面 + 底部渐变 + 标题与简介
    fn video_card(&mut self, x: u32, width: u32, y: f32, item: &Attached) -> f32 {
        let q = self.style.quality;
        let style = self.style;
        let art = self.art;
        let theme = &style.theme;
        let pad = q.card_padding as f32;
        let fx = x as f32;
        let cover_h = (width as f32 * 9.0 / 16.0).round();
        let title_leading = q.title_font + q.line_space as f32;
        let desc_leading = q.desc_font + q.line_space as f32;
        let text_w = width as f32 - pad * 2.0;
        let title_flow = style.face.layout(
            &[Span::new(&item.title, theme.title)],
            q.title_font,
            true,
            text_w,
            Some(2),
            &art.emojis,
        );
        let desc_flow = if item.desc.is_empty() {
            Flow::default()
        } else {
            style.face.layout(
                &[Span::new(&item.desc, theme.desc)],
                q.desc_font,
                false,
                text_w,
                Some(3),
                &art.emojis,
            )
        };
        let text_h = style.face.height(&title_flow, title_leading)
            + if desc_flow.rows() == 0 {
                0.0
            } else {
                q.line_space as f32 + style.face.height(&desc_flow, desc_leading)
            };
        let height = cover_h + pad + text_h;
        let cover = art.picture(&item.cover);
        let duration = item.duration.clone();
        let play = item.play.clone();
        if let Some(canvas) = &mut self.canvas {
            let radius = Corners {
                top_left: q.card_arc as f32,
                top_right: q.card_arc as f32,
                bottom_left: 0.0,
                bottom_right: 0.0,
            };
            canvas.fill_round_rect(
                x,
                y.round() as u32,
                width,
                cover_h.round() as u32,
                radius,
                Color::rgb(0x18, 0x1A, 0x1C),
            );
            if let Some(found) = cover {
                canvas.paste_cover_rounded(
                    x,
                    y.round() as u32,
                    width,
                    cover_h.round() as u32,
                    radius,
                    found,
                );
            }
            // 底部两成压一层黑渐变，白字才站得住
            canvas.fill_vertical_gradient(
                x,
                (y + cover_h * 0.8).round() as u32,
                width,
                (cover_h * 0.2).round() as u32,
                Color::rgba(0, 0, 0, 0),
                Color::rgba(0, 0, 0, 150),
            );
            let on_cover = y + cover_h - pad * 0.6;
            if !duration.is_empty() {
                style.face.draw_line(
                    canvas,
                    fx + pad,
                    on_cover,
                    &duration,
                    q.desc_font,
                    Color::rgb(255, 255, 255),
                    false,
                );
            }
            if !play.is_empty() {
                let label = format!("▶ {play}");
                let used = style.face.measure(&label, q.desc_font, false, &art.emojis);
                style.face.draw_line(
                    canvas,
                    fx + width as f32 - used - pad,
                    on_cover,
                    &label,
                    q.desc_font,
                    Color::rgb(255, 255, 255),
                    false,
                );
            }
            let text_y = y + cover_h + pad + q.title_font;
            style.face.draw(
                canvas,
                &title_flow,
                fx + pad,
                text_y,
                q.title_font,
                true,
                title_leading,
                &art.emojis,
            );
            if desc_flow.rows() > 0 {
                style.face.draw(
                    canvas,
                    &desc_flow,
                    fx + pad,
                    text_y + title_flow.rows() as f32 * title_leading,
                    q.desc_font,
                    false,
                    desc_leading,
                    &art.emojis,
                );
            }
        }
        height
    }

    /// 小卡：左边一块封面，右边标题加副标题
    fn small_card(&mut self, x: u32, width: u32, y: f32, item: &Attached) -> f32 {
        let q = self.style.quality;
        let style = self.style;
        let art = self.art;
        let theme = &style.theme;
        let pad = q.card_padding as f32;
        let cover_w = (width as f32 * 0.4).round();
        let title_leading = q.title_font + q.line_space as f32;
        let desc_leading = q.desc_font + q.line_space as f32;
        let text_x = x as f32 + cover_w + q.draw_space as f32;
        let text_w = width as f32 - cover_w - q.draw_space as f32 - pad;
        let title_flow = style.face.layout(
            &[Span::new(&item.title, theme.title)],
            q.title_font,
            false,
            text_w,
            Some(2),
            &art.emojis,
        );
        let desc_flow = if item.desc.is_empty() {
            Flow::default()
        } else {
            style.face.layout(
                &[Span::new(&item.desc, theme.desc)],
                q.desc_font,
                false,
                text_w,
                Some(2),
                &art.emojis,
            )
        };
        let used = style.face.height(&title_flow, title_leading)
            + if desc_flow.rows() == 0 {
                0.0
            } else {
                q.line_space as f32 + style.face.height(&desc_flow, desc_leading)
            };
        let height = (q.additional_card_height as f32).max(used + pad * 2.0);
        let cover = art.picture(&item.cover).cloned();
        let duration = item.duration.clone();
        if let Some(canvas) = &mut self.canvas {
            let radius = Corners::all(q.card_arc as f32);
            canvas.fill_round_rect(
                x,
                y.round() as u32,
                width,
                height.round() as u32,
                radius,
                Color::rgb(0xF0, 0xF2, 0xF5),
            );
            canvas.fill_round_rect(
                x,
                y.round() as u32,
                cover_w.round() as u32,
                height.round() as u32,
                radius,
                Color::rgb(0x18, 0x1A, 0x1C),
            );
            if let Some(found) = cover.as_ref() {
                canvas.paste_cover_rounded(
                    x,
                    y.round() as u32,
                    cover_w.round() as u32,
                    height.round() as u32,
                    radius,
                    found,
                );
            }
            if !duration.is_empty() {
                // 时长角标在封面左下角
                style.face.draw_line(
                    canvas,
                    x as f32 + pad * 0.6,
                    y + height - pad * 0.6,
                    &duration,
                    q.desc_font,
                    Color::rgb(255, 255, 255),
                    false,
                );
            }
            let first = y + (height - used) / 2.0 + q.title_font;
            style.face.draw(
                canvas,
                &title_flow,
                text_x,
                first,
                q.title_font,
                false,
                title_leading,
                &art.emojis,
            );
            if desc_flow.rows() > 0 {
                style.face.draw(
                    canvas,
                    &desc_flow,
                    text_x,
                    first + title_flow.rows() as f32 * title_leading,
                    q.desc_font,
                    false,
                    desc_leading,
                    &art.emojis,
                );
            }
        }
        height
    }

    /// 附加小卡（预约、投票、商品、充电抽奖、合集）：一行标题加一行说明
    fn mini(&mut self, x: u32, width: u32, y: f32, item: &Attached) -> f32 {
        let q = self.style.quality;
        let style = self.style;
        let art = self.art;
        let theme = &style.theme;
        let pad = q.card_padding as f32;
        let title_leading = q.title_font + q.line_space as f32;
        let desc_leading = q.desc_font + q.line_space as f32;
        let head = if item.title.is_empty() {
            item.kind.clone()
        } else {
            format!("{} · {}", item.kind, item.title)
        };
        let title_flow = style.face.layout(
            &[Span::new(&head, theme.title)],
            q.title_font,
            false,
            width as f32 - pad * 2.0,
            Some(2),
            &art.emojis,
        );
        let desc_flow = if item.desc.is_empty() {
            Flow::default()
        } else {
            style.face.layout(
                &[Span::new(&item.desc, theme.desc)],
                q.desc_font,
                false,
                width as f32 - pad * 2.0,
                Some(2),
                &art.emojis,
            )
        };
        let used = style.face.height(&title_flow, title_leading)
            + if desc_flow.rows() == 0 {
                0.0
            } else {
                q.line_space as f32 + style.face.height(&desc_flow, desc_leading)
            };
        let height = used + pad * 1.6;
        if let Some(canvas) = &mut self.canvas {
            canvas.fill_round_rect(
                x,
                y.round() as u32,
                width,
                height.round() as u32,
                Corners::all(q.card_arc as f32),
                Color::rgba(0, 0, 0, 18),
            );
            let first = y + pad * 0.8 + q.title_font;
            style.face.draw(
                canvas,
                &title_flow,
                x as f32 + pad,
                first,
                q.title_font,
                false,
                title_leading,
                &art.emojis,
            );
            if desc_flow.rows() > 0 {
                style.face.draw(
                    canvas,
                    &desc_flow,
                    x as f32 + pad,
                    first + title_flow.rows() as f32 * title_leading,
                    q.desc_font,
                    false,
                    desc_leading,
                    &art.emojis,
                );
            }
        }
        height
    }

    /// 专属与被挡动态的占位块
    fn blocked(&mut self, x: u32, width: u32, y: f32, tips: &str) -> f32 {
        let q = self.style.quality;
        let style = self.style;
        let art = self.art;
        let height = q.additional_card_height as f32 * 0.7;
        let line = style
            .face
            .truncate(tips, q.sub_title_font, false, width as f32 * 0.9);
        if let Some(canvas) = &mut self.canvas {
            canvas.fill_round_rect(
                x,
                y.round() as u32,
                width,
                height.round() as u32,
                Corners::all(q.card_arc as f32),
                Color::rgb(0xE8, 0xEA, 0xEE),
            );
            let used = style
                .face
                .measure(&line, q.sub_title_font, false, &art.emojis);
            style.face.draw_line(
                canvas,
                x as f32 + ((width as f32 - used) / 2.0).max(0.0),
                y + height / 2.0 + q.sub_title_font * 0.35,
                &line,
                q.sub_title_font,
                style.theme.desc,
                false,
            );
        }
        height
    }

    /// 转发的那条原文：内缩一块浅底，内容递归整卡
    fn forward(&mut self, x: u32, width: u32, y: f32, source: &Dynamic) -> f32 {
        let q = self.style.quality;
        let inner_x = x + q.card_padding;
        let inner_w = width - q.card_padding * 2;
        let body = self.probe(|quiet| quiet.card(inner_x, inner_w, y, source, true));
        let height = body + q.card_padding as f32 * 2.0;
        if let Some(canvas) = &mut self.canvas {
            canvas.fill_round_rect(
                x,
                y.round() as u32,
                width,
                height.round() as u32,
                Corners::all(q.card_arc as f32),
                Color::rgb(0xF4, 0xF5, 0xF7),
            );
        }
        self.card(inner_x, inner_w, y + q.card_padding as f32, source, true);
        height
    }

    /// 页脚
    fn footer(&mut self, x: u32, width: u32, y: f32) -> f32 {
        let q = self.style.quality;
        let style = self.style;
        let art = self.art;
        let font = q.footer_font;
        let height = font + q.card_padding as f32 * 0.8;
        if style.footer.is_empty() {
            return height;
        }
        let used = style.face.measure(&style.footer, font, false, &art.emojis);
        if let Some(canvas) = &mut self.canvas {
            style.face.draw_line(
                canvas,
                x as f32 + ((width as f32 - used) / 2.0).max(0.0),
                y + font,
                &style.footer,
                font,
                style.theme.footer,
                false,
            );
        }
        height
    }
}

/// 卡片底板：阴影 + 半透明底 + 描边。动态卡与直播卡共用。
pub(crate) fn paint_board(
    canvas: &mut Canvas,
    style: &Style,
    x: u32,
    width: u32,
    y: f32,
    height: f32,
) {
    let q = style.quality;
    let theme = &style.theme;
    let radius = q.card_arc as f32;
    let top = y.round() as u32;
    let tall = (height.round() as u32).max(1);
    if theme.card_shadow.0[3] > 0 {
        // blur 近似：向外扩几层，逐层变淡
        let layers = q.card_outline + 3;
        for step in (1..=layers).rev() {
            let grow = step * q.card_outline.max(1);
            let fade = (theme.card_shadow.0[3] as f32 * step as f32 / layers as f32) as u8;
            canvas.fill_round_rect(
                x.saturating_sub(grow),
                top.saturating_add(q.card_outline).saturating_sub(grow),
                width + grow * 2,
                tall + grow * 2,
                Corners::all(radius + grow as f32),
                theme.card_shadow.alpha(fade),
            );
        }
    }
    canvas.fill_round_rect(x, top, width, tall, Corners::all(radius), theme.card_bg);
    canvas.stroke_round_rect(
        x,
        top,
        width,
        tall,
        Corners::all(radius),
        q.card_outline,
        theme.outline_at(0.5),
    );
}

/// 顶部徽章条。`id` 是右边那一格：动态卡给动态 id，直播卡给房间号。
pub(crate) fn paint_badges(
    canvas: &mut Canvas,
    style: &Style,
    art: &Art,
    x: u32,
    y: f32,
    label: &str,
    id: i64,
) {
    let q = style.quality;
    let theme = &style.theme;
    let height = q.badge_height;
    let font = q.sub_title_font;
    let icon = font;
    let text = style.face.measure(label, font, false, &art.emojis);
    let left_w = (text + q.badge_padding as f32 * 8.0 + icon * 1.6).round() as u32;
    let radius = Corners {
        top_left: q.badge_arc as f32,
        top_right: q.badge_arc as f32,
        bottom_left: 0.0,
        bottom_right: 0.0,
    };
    let top = y.round() as u32;
    // 基线：徽章条内垂直居中
    let baseline = y + height as f32 - (height as f32 - font) / 2.0;
    canvas.fill_round_rect(x, top, left_w, height, radius, theme.main_left_badge.1);
    // 徽标是 SVG，这里不引解码器：用品牌色圆角块顶上去
    canvas.fill_round_rect(
        x + q.badge_padding * 2,
        (y + (height as f32 - icon) / 2.0).round() as u32,
        icon.round() as u32,
        icon.round() as u32,
        Corners::all(icon / 4.0),
        theme.main_left_badge.0,
    );
    style.face.draw_line(
        canvas,
        x as f32 + q.badge_padding as f32 * 2.0 + icon * 1.6,
        baseline,
        label,
        font,
        theme.main_left_badge.0,
        true,
    );
    if !style.show_id {
        return;
    }
    let code = id.to_string();
    let width = (style.face.measure(&code, font, false, &art.emojis) + q.badge_padding as f32 * 8.0)
        .round() as u32;
    let at = x + left_w + 2;
    canvas.fill_round_rect(at, top, width, height, radius, theme.main_right_badge.1);
    style.face.draw_line(
        canvas,
        at as f32 + q.badge_padding as f32 * 2.0,
        baseline,
        &code,
        font,
        theme.main_right_badge.0,
        true,
    );
}

/// 铺满宽的附属卡：只有视频和番剧这类有横封面
fn is_wide(item: &Attached) -> bool {
    matches!(item.kind.as_str(), "视频" | "番剧" | "合集") && !item.cover.is_empty()
}

/// 背景渐变：按停靠点在纵向上分段铺
pub(crate) fn paint_background(canvas: &mut Canvas, stops: &[Color]) {
    let (width, height) = (canvas.width(), canvas.height());
    match stops {
        [] => {}
        [only] => canvas.fill_rect(0, 0, width, height, *only),
        many => {
            let span = (height + many.len() as u32 - 2) / (many.len() as u32 - 1).max(1);
            for (index, pair) in many.windows(2).enumerate() {
                let top = index as u32 * span;
                if top >= height {
                    break;
                }
                let bottom = (top + span).min(height);
                canvas.fill_vertical_gradient(0, top, width, bottom - top, pair[0], pair[1]);
            }
        }
    }
}

/// 卡片内嵌的二维码：零静区、透明底，前景取主题色
pub(crate) fn qr_image(code: &str, side: u32, color: Color) -> Option<RgbaImage> {
    use qrcode::{Color as Module, QrCode};
    let qr = QrCode::new(code.as_bytes()).ok()?;
    let modules = qr.width();
    if modules == 0 || side == 0 {
        return None;
    }
    let raw = qr.to_colors();
    let mut out = ImageBuffer::from_pixel(side, side, image::Rgba([0, 0, 0, 0]));
    for row in 0..side {
        for column in 0..side {
            let at = (row as usize * modules / side as usize) * modules
                + column as usize * modules / side as usize;
            if matches!(raw.get(at), Some(Module::Dark)) {
                *out.get_pixel_mut(column, row) = image::Rgba(color.0);
            }
        }
    }
    Some(out)
}

/// 登录用的二维码：白底黑码带静区，编成 PNG 字节发出去。
/// 卡片里那张是透明底嵌在渐变背景上的，扫码端要求边缘留白，登录这张得单独画。
pub(crate) fn qr_png(code: &str, side: u32) -> Option<Vec<u8>> {
    use qrcode::{Color as Module, QrCode};
    let qr = QrCode::new(code.as_bytes()).ok()?;
    let modules = qr.width();
    if modules == 0 || side < 16 {
        return None;
    }
    let quiet = (4 * side as usize / modules).max(2);
    let span = side as usize + quiet * 2;
    let mut board: RgbaImage =
        ImageBuffer::from_pixel(span as u32, span as u32, image::Rgba([255, 255, 255, 255]));
    let raw = qr.to_colors();
    for row in 0..side as usize {
        for column in 0..side as usize {
            let at = (row * modules / side as usize) * modules + column * modules / side as usize;
            if matches!(raw.get(at), Some(Module::Dark)) {
                *board.get_pixel_mut(column as u32 + quiet as u32, row as u32 + quiet as u32) =
                    image::Rgba([0, 0, 0, 255]);
            }
        }
    }
    let mut out = std::io::Cursor::new(Vec::new());
    if board.write_to(&mut out, image::ImageFormat::Png).is_err() {
        return None;
    }
    Some(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bili::model::LiveInfo;

    fn style(quality: &str) -> Style {
        Style {
            quality: Quality::find(quality),
            theme: Theme::find("v3"),
            face: Typeface::discover("").unwrap(),
            background: vec![Color::rgb(211, 237, 250)],
            badge: true,
            show_id: false,
            footer: "测试页脚".to_string(),
            qr: None,
        }
    }

    fn sample(text: &str) -> Dynamic {
        Dynamic {
            id: 1029210402684141574,
            uid: 123,
            name: "测试Up".to_string(),
            face: "https://face/1.jpg".to_string(),
            kind: "动态".to_string(),
            type_str: "DYNAMIC_TYPE_DRAW".to_string(),
            time: 1738470120,
            text: text.to_string(),
            nodes: vec![Node::Text(text.to_string())],
            images: Vec::new(),
            links: Vec::new(),
            attached: None,
            extra: Vec::new(),
            live: None,
            origin: None,
            top: false,
            only_fans: false,
            blocked: None,
            comment: 0,
            forward_count: 0,
            like: 0,
            label: String::new(),
        }
    }

    #[test]
    fn a_card_without_any_downloaded_image_still_renders() {
        // 素材一张都没有（下载全失败）也必须出图：不能因为图没下来就不推
        let style = style("1000w");
        let bytes = dynamic_card(&sample("一条纯文字动态"), &Art::default(), &style);
        let back = image::load_from_memory(&bytes).unwrap();
        assert_eq!(back.width(), 1000);
        assert!(back.height() > 200, "总高异常：{}", back.height());
    }

    #[test]
    fn higher_quality_makes_a_taller_image() {
        let small = measure(&sample("同样一段正文"), &Art::default(), &style("800w"));
        let large = measure(&sample("同样一段正文"), &Art::default(), &style("1500w"));
        assert!(large > small, "{large} 该大于 {small}");
    }

    #[test]
    fn a_forward_is_taller_than_its_own_text() {
        let plain = sample("转发语");
        let mut with_source = sample("转发语");
        with_source.kind = "转发动态".to_string();
        with_source.origin = Some(Box::new(sample("被转发的那条原文正文")));
        let art = Art::default();
        let style = style("1000w");
        assert!(
            measure(&with_source, &art, &style) > measure(&plain, &art, &style),
            "转发原文没占高度"
        );
        // 底板要提前探一次高度，递归只能多出一层原文的高，不能失控
        assert!(
            measure(&with_source, &art, &style) < measure(&plain, &art, &style) * 4.0,
            "递归跑飞了"
        );
    }

    #[test]
    fn a_blocked_dynamic_adds_the_notice_band() {
        let plain = sample("正文");
        let mut hidden = sample("正文");
        hidden.blocked = Some("你当前不是他的粉丝".to_string());
        let art = Art::default();
        let style = style("1000w");
        assert!(
            measure(&hidden, &art, &style) > measure(&plain, &art, &style),
            "遮挡提示条没占高度"
        );
    }

    #[test]
    fn image_grid_lays_two_and_three_columns() {
        let style = style("1000w");
        let q = style.quality;
        let width = q.content_width();
        let mut art = Art::default();
        for index in 0..5 {
            art.pictures.insert(
                format!("https://i/{index}.png"),
                RgbaImage::from_pixel(100, 100, image::Rgba([9, 9, 9, 255])),
            );
        }
        let urls: Vec<String> = (0..5)
            .map(|index| format!("https://i/{index}.png"))
            .collect();
        let space = q.draw_space as f32;
        let mut board = Board {
            style: &style,
            art: &art,
            canvas: None,
        };
        // 三张走三列一行、五张走三列两行、四张走两列两行，格子都是正方形
        let three = board.pictures(0, width, 0.0, &urls[..3]);
        let five = board.pictures(0, width, 0.0, &urls);
        let four = board.pictures(0, width, 0.0, &urls[..4]);
        let wide = ((width as f32 - space * 2.0) / 3.0).floor();
        let narrow = ((width as f32 - space) / 2.0).floor();
        assert!(
            (three - (wide + space * 2.0)).abs() < 2.0,
            "三列一行的总高不对：{three}"
        );
        assert!(
            (five - (wide * 2.0 + space * 3.0)).abs() < 2.0,
            "三列两行的总高不对：{five}"
        );
        assert!(
            (four - (narrow * 2.0 + space * 3.0)).abs() < 2.0,
            "四张两列的总高不对：{four}"
        );
        assert!(four > five, "两列方格比三列高：四张 {four}，五张 {five}");
    }

    #[test]
    fn an_attached_video_card_is_taller_than_a_small_one() {
        let style = style("1000w");
        let item = Attached {
            kind: "视频".to_string(),
            title: "一个很长很长用来测标题换行的视频标题".to_string(),
            cover: "https://i/cover.png".to_string(),
            url: "https://b23.tv/x".to_string(),
            duration: "12:34".to_string(),
            play: "1.2万播放".to_string(),
            desc: "简介文字".to_string(),
        };
        let mut board = Board {
            style: &style,
            art: &Art::default(),
            canvas: None,
        };
        let big = board.attached(0, style.quality.content_width(), 0.0, &item, true);
        let small = board.attached(0, style.quality.content_width(), 0.0, &item, false);
        assert!(big > small, "大卡 {big} 该比小卡 {small} 高");
    }

    #[test]
    fn a_qr_code_renders_dark_modules_on_a_transparent_ground() {
        let image = qr_image(
            "https://t.bilibili.com/1",
            125,
            Color::rgb(0x31, 0x31, 0x31),
        )
        .unwrap();
        assert_eq!(image.width(), 125);
        let dark = image.pixels().filter(|pixel| pixel[3] > 0).count();
        assert!(dark > 0, "二维码全空");
        assert!(dark < 125 * 125, "二维码全黑，前景背景反了");
    }

    #[test]
    fn live_info_is_not_drawn_by_the_dynamic_card() {
        // 直播卡走 live.rs，这里确认带 live 字段的动态不会因此画歪
        let mut item = sample("开播了");
        item.live = Some(LiveInfo {
            uid: 1,
            room_id: 2,
            anchor: "主播".to_string(),
            title: "标题".to_string(),
            ..Default::default()
        });
        item.kind = "直播".to_string();
        let style = style("1000w");
        assert!(measure(&item, &Art::default(), &style) > 0.0);
    }
}
