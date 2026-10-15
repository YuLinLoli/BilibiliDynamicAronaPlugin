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

use super::canvas::{Block, Canvas, Color, Corners};
use super::quality::Quality;
use super::text::{EmojiSet, Flow, Span, Typeface};
use super::theme::Theme;
use crate::bili::model::{Attached, Dynamic, Node};
use crate::translate::Rendered;

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
    /// 顶部徽章条左边那一档（动态类型 / 直播分区）
    pub badge_left: bool,
    /// 徽章条右边那一档显示动态 id（直播卡是房间号）
    pub badge_right: bool,
    pub footer: String,
    /// 有值就在作者行右侧画二维码（`card_ornament: QrCode`）
    pub qr: Option<String>,
}

/// 一条动态的完整卡片，返回 PNG 字节。
/// `translation` 是机翻出来的那段（[`crate::translate`]），有值就接在正文后面；
/// 只影响动态卡，直播卡与转发原文都不贴。
pub fn dynamic_card(
    dynamic: &Dynamic,
    art: &Art,
    style: &Style,
    translation: Option<&Rendered>,
) -> Vec<u8> {
    let height = measure(dynamic, art, style, translation).ceil() as u32;
    let mut canvas = Canvas::new(style.quality.image_width, height, Color::rgb(255, 255, 255));
    paint_background(&mut canvas, &style.background);
    let mut board = Board {
        style,
        art,
        canvas: Some(canvas),
    };
    board.whole(dynamic, translation);
    match board.canvas.take() {
        Some(done) => done.to_png(),
        None => Vec::new(),
    }
}

/// 只量高不画：建画布之前得先知道总高
fn measure(dynamic: &Dynamic, art: &Art, style: &Style, translation: Option<&Rendered>) -> f32 {
    Board {
        style,
        art,
        canvas: None,
    }
    .whole(dynamic, translation)
}

/// 作者行两行的基线：主卡抄 `drawGeneral`（DynamicModuleDraw.kt:518-523），
/// 转发行抄 `drawForward`（同文件 493-498）。
struct AuthorLines {
    name_y: f32,
    time_y: f32,
    /// 转发行是「名字 + 时间」并排在同一条基线上，主行才另起一行
    time_after_name: bool,
}

/// `top` 是作者行在卡片内容里的起始 y
fn author_lines(q: Quality, top: f32, sub: bool) -> AuthorLines {
    if sub {
        let face = q.face_size as f32 * 0.6;
        let name = top + (face - q.name_font) / 2.0 + q.name_font + q.card_padding as f32;
        return AuthorLines {
            name_y: name,
            time_y: name - (q.name_font - q.sub_title_font) / 2.0,
            time_after_name: true,
        };
    }
    // gap 只是作者行里除两行字号之外的余量三等分，它不是行与行的基线间距：
    // 第二行要挪 `subTitleFontSize + space*0.5`，只挪 gap 会让时间行顶进名字行的
    // 下伸区（1000w 那一档差 15 像素，两行直接叠在一起）。
    let gap = ((q.pendant_size as f32 - q.name_font - q.sub_title_font) / 3.0).max(4.0);
    let name = top + q.name_font + gap * 1.25;
    AuthorLines {
        name_y: name,
        time_y: name + q.sub_title_font + gap * 0.5,
        time_after_name: false,
    }
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
    fn whole(&mut self, dynamic: &Dynamic, translation: Option<&Rendered>) -> f32 {
        let q = self.style.quality;
        let style = self.style;
        let badge_h = q.badge_height_for(style.badge_left || style.badge_right) as f32;
        let card_x = q.card_margin;
        let card_w = q.card_width();
        let top = q.card_margin as f32 + badge_h;
        let body = self.probe(|quiet| quiet.card(card_x, card_w, top, dynamic, false, translation));
        self.card_background(card_x, card_w, top, body + q.card_padding as f32);
        if style.badge_left || style.badge_right {
            // 外层左角标固定是「动态」：原插件那一格取 `tag ?: 动态/转发动态`
            // （`DynamicDraw.kt:269`），类型词从不进这一格——它要么在页脚的 {type} 里，
            // 要么在附属卡自己那一格（`major.*.badge.text`）。转发的那格「转发动态」
            // 属于里层原卡，见 [`Board::forward`]。
            self.badges(card_x, top - badge_h, "动态", dynamic.id);
        }
        let used = self.card(card_x, card_w, top, dynamic, false, translation);
        // 底板比内容多留一格 card_padding（顶部那格由作者行自己吃进去），图的高度还要在
        // 底板之外再留一圈 card_margin，并另算出阴影往下伸的那一截——少加任何一样，
        // 卡片都会顶到图的下边或落影挤进留白里。
        top + used + q.card_padding as f32 + q.card_margin as f32 + shadow_below(q)
    }

    /// 一张卡的主体（不含底板），返回占用高度
    fn card(
        &mut self,
        x: u32,
        width: u32,
        y: f32,
        dynamic: &Dynamic,
        sub: bool,
        translation: Option<&Rendered>,
    ) -> f32 {
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
        let body = self.body_spans(dynamic, translation);
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
        if !sub && !self.style.footer.is_empty() {
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
        // 行高、头像边长与外扩、头像左上偏移：主行与转发行各一套，来源见 [`author_lines`]
        let (height, face_side, inflate, face_dx, face_dy) = if sub {
            (
                q.face_size as f32 + pad,
                (q.face_size as f32 * 0.6).round(),
                0.0,
                1.5,
                1.0,
            )
        } else {
            (
                q.pendant_size as f32,
                q.face_size as f32,
                q.face_inflate as f32,
                1.8,
                1.2,
            )
        };
        let left = (x as f32 + pad * face_dx - inflate).max(x as f32) as u32;
        let top = (y + pad * face_dy - inflate).max(y) as u32;
        let side = face_side as u32;
        let name_x = (x as f32 + face_side + pad * if sub { 2.5 } else { 3.2 })
            .min(x as f32 + width as f32 * 0.55);
        let lines = author_lines(q, y, sub);
        let avatar = art.face(&dynamic.face);
        if let Some(canvas) = &mut self.canvas {
            // 没有装扮挂件时，头像放大一圈并描个白边圆。
            // 圆心要跟着 inflate 一起外扩：贴图是从 (left, top) 起画 2*(side/2+inflate) 见方，
            // 它的中心在 left+side/2+inflate，白圆若仍取 left+side/2 就与头像差一个 inflate，
            // 白边会一边宽一边窄（画出来是左上一道白月牙）。
            let center_x = left + side / 2 + inflate as u32;
            let center_y = top + side / 2 + inflate as u32;
            canvas.fill_circle(
                center_x,
                center_y,
                side / 2 + inflate as u32 + 1,
                theme.face_outline,
            );
            match avatar {
                Some(found) => {
                    canvas.paste_circle_image(left, top, side / 2 + inflate as u32, found)
                }
                None => canvas.fill_circle(
                    center_x,
                    center_y,
                    side / 2 + inflate as u32,
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
            let drawn = style.face.draw_line(
                canvas,
                name_x,
                lines.name_y,
                &line,
                q.name_font,
                theme.nickname,
                true,
            );
            let when_x = if lines.time_after_name {
                name_x + drawn + pad
            } else {
                name_x
            };
            let when_room = ((x + width) as f32 - when_x).max(40.0);
            let when = style
                .face
                .truncate(&stamp, q.sub_title_font, false, when_room);
            style.face.draw_line(
                canvas,
                when_x,
                lines.time_y,
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
        if let Some(image) = qr_image(code, side, theme.title)
            && let Some(canvas) = &mut self.canvas
        {
            canvas.paste(
                x + width.saturating_sub(side + q.card_padding),
                at_y,
                &image,
            );
        }
        height
    }

    /// 正文的分段：话题、链接、投票这些用链接色，其余用正文色。
    /// 有机翻译文时按原插件那一步追加两格——分割线在前、译文在后，都走正文色。
    fn body_spans<'d>(
        &self,
        dynamic: &'d Dynamic,
        translation: Option<&'d Rendered>,
    ) -> Vec<Span<'d>> {
        let theme = &self.style.theme;
        let mut spans = if dynamic.nodes.is_empty() {
            if dynamic.text.is_empty() {
                Vec::new()
            } else {
                vec![Span::new(&dynamic.text, theme.content)]
            }
        } else {
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
        };
        if let Some(done) = translation {
            spans.push(Span::new(&done.cut_line, theme.content));
            spans.push(Span::new(&done.text, theme.content));
        }
        spans
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
        // 站方那格徽章垫在卡上面：下面所有落点都按抬高后的 y 算，块高再把它加回去
        let badge_h = card_badge_height(style, item);
        let y = y + badge_h;
        let cover = art.picture(&item.cover);
        // 封面高度按**这张封面自己的比例**给：原插件是
        // `videoCoverHeight = cardContentRect.width * coverImg.height / coverImg.width`
        // （`DynamicMajorDraw.kt:213`）。写死 16:9 会把竖屏投稿裁掉一大截；
        // 图没下下来时退回原插件请求缩略图用的那档 0.625。
        // 专栏是另一套：原插件按**固定比例**裁（`articleCoverHeight = 卡宽 × 0.35`，
        // 三宫格那档是 0.23166，`DynamicMajorDraw.kt:683`），不看封面自己的长短
        let cover_h = if item.kind == "专栏" {
            (width as f32 * 0.35).round()
        } else {
            match cover {
                Some(found) if found.width() > 0 => {
                    (width as f32 * found.height() as f32 / found.width() as f32).round()
                }
                _ => (width as f32 * 0.625).round(),
            }
        };
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
        let height = badge_h + cover_h + pad + text_h;
        let duration = item.duration.clone();
        let play = item.play.clone();
        if let Some(canvas) = &mut self.canvas {
            // 底板先落，封面与文字再往上压：原插件这一族也是同一个 drawCard
            paint_inner_board(
                canvas,
                style,
                x,
                y.round() as u32,
                width,
                (height - badge_h).round() as u32,
                inner_radius(q, badge_h > 0.0),
            );
            if badge_h > 0.0 {
                paint_card_badge(
                    canvas,
                    style,
                    art,
                    Block {
                        x,
                        y: y.round() as u32,
                        width,
                        height: (height - badge_h).round() as u32,
                        radius: inner_radius(q, true),
                    },
                    &item.badge,
                    style.theme.sub_left_badge,
                    None,
                );
            }
            // 封面的四角必须与卡同一套：徽章压着左上角时这里也得是直角，
            // 否则圆掉的封面角会把下面那层卡体白底露出来（用户圈的缺口）
            let radius = inner_radius(q, badge_h > 0.0);
            // 封面往里缩 COVER_INSET，四周就露出一圈卡体白边。原插件缩的是 1 像素
            // （`videoCardRect.inflate(-1)`），2026-10-16 用户要看 10 像素的观感——
            // 只改 [`COVER_INSET`] 这一个数，下面那几处都以画出来的这块为准，别各自算
            let inset = COVER_INSET;
            let (cx, cy) = (x + inset, y.round() as u32 + inset);
            let (cw, ch) = (
                width.saturating_sub(inset * 2),
                (cover_h.round() as u32).saturating_sub(inset * 2),
            );
            canvas.fill_round_rect(cx, cy, cw, ch, radius, Color::rgb(0x18, 0x1A, 0x1C));
            if let Some(found) = cover {
                canvas.paste_cover_rounded(cx, cy, cw, ch, radius, found);
            }
            // 底部两成压一层黑渐变，白字才站得住。这一段必须按封面的下角裁圆：它的底边
            // 与封面底边对齐、左右也同边，所以只给下面两角半径就正好接上封面那道弧——
            // 之前这里铺的是直角矩形，圆掉的下角又被糊成了一块方角
            let band_h = (ch as f32 * 0.2).round() as u32;
            canvas.fill_vertical_gradient(
                cx,
                cy + ch - band_h,
                cw,
                band_h,
                Corners {
                    bottom_left: radius.bottom_left,
                    bottom_right: radius.bottom_right,
                    ..Corners::none()
                },
                Color::rgba(0, 0, 0, 0),
                Color::rgba(0, 0, 0, 150),
            );
            let on_cover = cy as f32 + ch as f32 - pad * 0.6;
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
                // 三角不能拼进字符串：所选字体里没有 U+25B6，画出来是一块豆腐
                let used = style.face.measure(&play, q.desc_font, false, &art.emojis);
                let mark = q.desc_font * 0.62;
                let right = cx as f32 + cw as f32 - pad;
                style.face.draw_line(
                    canvas,
                    right - used,
                    on_cover,
                    &play,
                    q.desc_font,
                    Color::rgb(255, 255, 255),
                    false,
                );
                paint_play_glyph(
                    canvas,
                    right - used - mark - pad * 0.5,
                    on_cover - q.desc_font * 0.35 - mark / 2.0,
                    mark,
                    Color::rgb(255, 255, 255),
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
        // 右侧那一格类型标签先占掉宽度，否则两行标题会直接压在标签上
        let label_w = if item.kind.is_empty() {
            0.0
        } else {
            style
                .face
                .measure(&item.kind, q.sub_title_font, false, &art.emojis)
                + q.badge_padding as f32 * 2.0
                + pad
        };
        let text_w = width as f32 - cover_w - q.draw_space as f32 - pad - label_w;
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
        let badge_h = card_badge_height(style, item);
        let y = y + badge_h;
        let height = (q.additional_card_height as f32).max(used + pad * 2.0);
        let cover = art.picture(&item.cover).cloned();
        let duration = item.duration.clone();
        if let Some(canvas) = &mut self.canvas {
            let radius = inner_radius(q, badge_h > 0.0);
            paint_inner_board(
                canvas,
                style,
                x,
                y.round() as u32,
                width,
                height.round() as u32,
                radius,
            );
            if badge_h > 0.0 {
                paint_card_badge(
                    canvas,
                    style,
                    art,
                    Block {
                        x,
                        y: y.round() as u32,
                        width,
                        height: height.round() as u32,
                        radius,
                    },
                    &item.badge,
                    style.theme.sub_left_badge,
                    None,
                );
            }
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
            // 附加卡的类型标签：装扮 / 投票 / 充电抽奖 / 商品…
            // 原插件 `drawAdditionalCard(label, cover, title, desc, …)` 的第一个参数，
            // 画在卡片右侧垂直居中。`parse_additional` 一直把它折进 Attached.kind 了，
            // 只是这里从没画出来过——所以装扮动态的卡片上看不见「装扮」那一格。
            if !item.kind.is_empty() {
                let font = q.sub_title_font;
                let label_text_w = style.face.measure(&item.kind, font, false, &art.emojis);
                let badge_w = label_text_w + q.badge_padding as f32 * 2.0;
                let label_h = font + 12.0;
                let bx = (x as f32 + width as f32 - badge_w - pad * 0.8).max(text_x);
                let by = (y + (height - label_h) / 2.0).max(y);
                canvas.fill_round_rect(
                    bx.round() as u32,
                    by.round() as u32,
                    badge_w.round() as u32,
                    label_h.round() as u32,
                    Corners::all(label_h / 2.0),
                    theme.title,
                );
                style.face.draw_line(
                    canvas,
                    bx + (badge_w - label_text_w) / 2.0,
                    by + label_h / 2.0 + font * 0.36,
                    &item.kind,
                    font,
                    Color::rgb(255, 255, 255),
                    false,
                );
            }
        }
        height + badge_h
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
            paint_inner_board(
                canvas,
                style,
                x,
                y.round() as u32,
                width,
                height.round() as u32,
                inner_radius(q, false),
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
        let pad = q.card_padding as f32;
        let inner_x = x + q.card_padding;
        let inner_w = width - q.card_padding * 2;
        // 转发里那张原卡自己也要有一格角标：原插件走的是同一个 `assembleCard`，
        // 只是 `isForward=true`（`DynamicDraw.kt:266-279`），词固定是「转发动态」、
        // 图标换成 FORWARD、配色仍是 mainLeftBadge。这一格同样带那圈白包边，仍贴着卡顶。
        let badge_h = if self.style.badge_left {
            q.badge_height as f32 + COVER_INSET as f32
        } else {
            0.0
        };
        let top = y + badge_h;
        let body = self.probe(|quiet| quiet.card(inner_x, inner_w, top + pad, source, true, None));
        let height = badge_h + body + pad * 2.0;
        if let Some(canvas) = &mut self.canvas {
            let card = Block {
                x,
                y: top.round() as u32,
                width,
                height: (height - badge_h).round() as u32,
                radius: inner_radius(q, badge_h > 0.0),
            };
            paint_inner_board(
                canvas,
                self.style,
                card.x,
                card.y,
                card.width,
                card.height,
                card.radius,
            );
            if badge_h > 0.0 {
                paint_card_badge(
                    canvas,
                    self.style,
                    self.art,
                    card,
                    "转发动态",
                    self.style.theme.main_left_badge,
                    Some(paint_forward_glyph),
                );
            }
        }
        self.card(inner_x, inner_w, top + pad, source, true, None);
        height
    }

    /// 页脚。空页脚由调用方整块跳过——原插件也是"没有这行字就不接这张图"
    fn footer(&mut self, x: u32, width: u32, y: f32) -> f32 {
        let q = self.style.quality;
        let style = self.style;
        let art = self.art;
        let font = q.footer_font;
        let height = font + q.card_padding as f32 * 0.8;
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

/// 卡片阴影的几何：每一环多厚、铺几环、整体往右下挪多少。
/// 底板与整图高度都从这里取数，别各算各的。
fn shadow_geometry(q: Quality) -> (u32, u32, u32) {
    let band = q.card_outline.max(1);
    (band, q.card_outline + 3, band * 2)
}

/// 阴影往卡片下方伸出多高。整图必须在 card_margin 之外再给它留出这一截，
/// 否则落影挤进留白里，卡片下沿看着就贴着图边（原插件是连续高斯淡出，看不出挤；
/// 我们是一档一档的台阶，24 像素的落影压在 30 像素的边距上就很闷）。
pub(crate) fn shadow_below(q: Quality) -> f32 {
    let (band, layers, shift) = shadow_geometry(q);
    (band * layers + shift) as f32
}

/// 一圈软阴影：按环带铺，每环只画自己那一圈，越往外越淡，整体往右下挪。
///
/// 两处口径：
/// * 原插件是 `Theme.Shadow("#46000000", 6, 6, 25, 0)` 的高斯投影（`DynamicImageTheme.kt:37`），
///   贴边那一环最多只有一半浓度、向外线性收。之前每层都从自己的外沿一路填到本体边，
///   六层全叠在贴着边的第一环上——实测卡片上沿那条 15 像素的带把背景从 251 压到 69，
///   就是用户圈的「大黑边」。
/// * 本体永远在洞内：阴影不许压在半透明卡体上（见 [`Canvas::fill_shadow_ring`]）。
fn paint_shadow(
    canvas: &mut Canvas,
    body: Block,
    color: Color,
    band: u32,
    layers: u32,
    shift: u32,
) {
    if color.0[3] == 0 || band == 0 || layers == 0 {
        return;
    }
    let peak = color.0[3] as f32 * 0.5;
    for step in (1..=layers).rev() {
        let outer = body.grown(step * band, (shift, shift));
        let hole = if step == 1 {
            body
        } else {
            body.grown((step - 1) * band, (shift, shift))
        };
        let fade = (peak * (layers - step + 1) as f32 / layers as f32) as u8;
        canvas.fill_shadow_ring(outer, hole, color.alpha(fade));
    }
}

/// 内卡这一圈白边留多少像素：封面四周各缩这一格，卡上方那格徽章外面也包这一格白。
/// 原插件封面是 `videoCardRect.inflate(-1)`（1 像素），徽章外面那一圈是 `drawBadge` 里
/// `rrect.inflate(1f)` 的小阴影；2026-10-16 用户两处都要 3 像素，所以共用这一个数。
/// **徽章仍然贴在卡片顶边上**——那 3 像素是徽章自己的白包边，不是把它抬起来分离。
const COVER_INSET: u32 = 3;

/// 附属卡上方那一格徽章占多高：站方 `badge.text` 有词、且左徽章整档开着才留这一格。
/// 留的是「药丸本身 + 它外面那圈白包边」，药丸底边照旧压在卡顶。
/// 原插件无条件留 `badgeHeight`（text 为空时会画出一个空药丸），我们不留——见 README 已知差别。
fn card_badge_height(style: &Style, item: &Attached) -> f32 {
    if style.badge_left && !item.badge.is_empty() {
        style.quality.badge_height as f32 + COVER_INSET as f32
    } else {
        0.0
    }
}

/// 卡中卡的四角：压着徽章的那个左上角必须直角（同 `cardBadgeArc` 的语义）。
/// 徽章外面那圈白包边会盖住这个角，但形状仍要照原插件置直角，见 [`paint_card_badge`]。
fn inner_radius(q: Quality, badge_on_top: bool) -> Corners {
    Corners {
        top_left: if badge_on_top { 0.0 } else { q.card_arc as f32 },
        ..Corners::all(q.card_arc as f32)
    }
}

/// 徽章里那一格图标的描法。原插件是 `loadSVG("icon/…")` 直接栅格化，我们没有 SVG
/// 光栅器，一律用线段描（[`paint_tv_glyph`]、[`paint_forward_glyph`]）。
type GlyphPainter = fn(&mut Canvas, f32, f32, f32, Color);

/// 一格药丸徽章的底座：先按包边之外铺阴影，再包一圈白，最后落药丸本体。
/// 外层左、外层右、内卡那格三档共用，包边宽度都是 [`COVER_INSET`]。
fn paint_pill(canvas: &mut Canvas, style: &Style, pill: Block, bg: Color) {
    let (band, layers, shift) = shadow_geometry(style.quality);
    let ring = pill.grown(COVER_INSET, (0, 0));
    paint_shadow(
        canvas,
        ring,
        style.theme.small_card_shadow,
        band,
        layers,
        shift,
    );
    canvas.fill_round_rect(
        ring.x,
        ring.y,
        ring.width,
        ring.height,
        ring.radius,
        style.theme.draw_outline,
    );
    canvas.fill_round_rect(pill.x, pill.y, pill.width, pill.height, pill.radius, bg);
}

/// 卡上方那一格徽章：画在 `card` 矩形正上方、左对齐、下两角直角，卡自己压着它的那个
/// 上角跟着置直角（`cardBadgeArc` 的语义，见 [`inner_radius`]）。
///
/// 两档共用这一个画法，差在配色与图标（图标就是传进来的那个描法）：
/// * 转发里那张原卡：固定「转发动态」，`mainLeftBadge` + [`paint_forward_glyph`]
///   （`DynamicDraw.kt:266-279`，主卡那一格是同一段代码换了图标，走 [`paint_badges`]）；
/// * 附属卡：站方 `badge.text`，`subLeftBadge`，**没有图标**（`badge.icon_url` 实测一直是空的）。
fn paint_card_badge(
    canvas: &mut Canvas,
    style: &Style,
    art: &Art,
    card: Block,
    text: &str,
    palette: (Color, Color),
    glyph: Option<GlyphPainter>,
) {
    let q = style.quality;
    let font = q.sub_title_font;
    let pad = q.badge_padding as f32;
    let height = q.badge_height;
    // 图标边长取 contentFont：原插件把 SVG 渲染成 `contentFontSize` 见方，宽度也按这个
    // 数参与徽章宽公式（`drawBadge` 的 `textLine.width + badgePadding * 8 + icon.width`）
    let icon = glyph.map_or(0.0, |_| q.content_font);
    let width = (style.face.measure(text, font, false, &art.emojis) + pad * 8.0 + icon)
        .round()
        .max(1.0) as u32;
    // 药丸底边压在卡片顶边上（原插件 `drawBadge` 的 TOP_LEFT 配方：下两角直角），
    // 外面再包一圈 `COVER_INSET` 白——那圈白向下也压进卡顶那一格，这就是用户圈的"包边"。
    // 徽章本身不分离，只是被白边裹住；阴影按原插件打在包边之外（`rrect.inflate(1f)`）。
    // 往右让开一格包边：这样白边左沿正好落在卡的左边缘上（不往卡外突出），
    // 药丸本体又与里面那张封面的左边缘同一条竖线（用户 2026-10-16 圈的"对齐一点"）。
    let top = card.y.saturating_sub(height);
    let pill = Block {
        x: card.x + COVER_INSET,
        y: top,
        width,
        height,
        radius: Corners {
            bottom_left: 0.0,
            bottom_right: 0.0,
            ..Corners::all(q.badge_arc as f32)
        },
    };
    paint_pill(canvas, style, pill, palette.1);
    let baseline = top as f32 + height as f32 - (height as f32 - font) / 2.0;
    let mut text_x = pill.x as f32 + pad * 4.0;
    if let Some(paint) = glyph {
        // 有图标时整条往左挪一格 badgePadding，图标落定后文字再让开一个图标宽
        text_x -= pad;
        paint(
            canvas,
            text_x,
            top as f32 + (height as f32 - icon) / 2.0,
            icon,
            palette.0,
        );
        text_x += icon + pad * 2.0;
    }
    style
        .face
        .draw_line(canvas, text_x, baseline, text, font, palette.0, true);
}

/// 卡中卡的底板：与主卡同一档半透明白 + 同一份描边 + 那一圈小阴影。
///
/// 原插件这些块（转发原文卡、附属视频卡、附加小卡）全都调同一个 `drawCard()`
/// （`DynamicDraw.kt:360-377`），填的是主题的 `cardBgColorHex`（v3 = `#B4FFFFFF`）。
/// 所以内卡看着"几乎透明"——透出来的是外卡那一层颜色。我们此前逐块写死实心灰
/// （`#F4F5F7` / `#F0F2F5` / `rgba(0,0,0,18)`），扣在渐变背景上就是一块与主题脱节的死灰。
fn paint_inner_board(
    canvas: &mut Canvas,
    style: &Style,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    radius: Corners,
) {
    let q = style.quality;
    let theme = &style.theme;
    let block = Block {
        x,
        y,
        width,
        height,
        radius,
    };
    let (band, layers, shift) = shadow_geometry(q);
    paint_shadow(canvas, block, theme.small_card_shadow, band, layers, shift);
    canvas.fill_round_rect(x, y, width, height, radius, theme.card_bg);
    canvas.stroke_round_rect(
        x,
        y,
        width,
        height,
        radius,
        q.card_outline,
        theme.outline_at(0.5),
    );
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
    let arc = q.card_arc as f32;
    // 压着徽章的那一个上角必须是直角，否则圆掉的卡片角与方底的徽章之间会豁开一道
    // ——原插件 `cardBadgeArc` 就是照徽章开关摆四个角的（`DynamicDraw.kt:164-168`）
    let radius = Corners {
        top_left: if style.badge_left { 0.0 } else { arc },
        top_right: if style.badge_right { 0.0 } else { arc },
        bottom_left: arc,
        bottom_right: arc,
    };
    let top = y.round() as u32;
    let tall = (height.round() as u32).max(1);
    let card = Block {
        x,
        y: top,
        width,
        height: tall,
        radius,
    };
    let (band, layers, shift) = shadow_geometry(q);
    paint_shadow(canvas, card, theme.card_shadow, band, layers, shift);
    canvas.fill_round_rect(x, top, width, tall, radius, theme.card_bg);
    canvas.stroke_round_rect(
        x,
        top,
        width,
        tall,
        radius,
        q.card_outline,
        theme.outline_at(0.5),
    );
}

/// 顶部徽章条。`label` 是左边那一档（动态类型或「直播」），`id` 是右边那一档：动态卡给动态
/// id，直播卡给房间号。两档各由 `badge_left`/`badge_right` 管，与原插件 `BadgeEnable` 同形。
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
    let pad = q.badge_padding as f32;
    // 图标那一格的边长取 contentFont：原插件把 SVG 渲染成 `contentFontSize` 见方
    // （`DynamicDraw.kt:275`），宽度也按这个数参与徽章宽公式
    let icon = q.content_font;
    let text = style.face.measure(label, font, false, &art.emojis);
    let left_w = (text + pad * 8.0 + icon).round() as u32;
    let radius = Corners {
        top_left: q.badge_arc as f32,
        top_right: q.badge_arc as f32,
        bottom_left: 0.0,
        bottom_right: 0.0,
    };
    let top = y.round() as u32;
    // 基线：徽章条内垂直居中
    let baseline = y + height as f32 - (height as f32 - font) / 2.0;
    if style.badge_left {
        let pill = Block {
            // 与内卡那格同一套：药丸往右让开一格包边，白边左沿正好落在卡的左边缘上
            x: x + COVER_INSET,
            y: top,
            width: left_w,
            height,
            radius,
        };
        paint_pill(canvas, style, pill, theme.main_left_badge.1);
        let icon_x = pill.x as f32 + pad * 3.0;
        paint_tv_glyph(
            canvas,
            icon_x,
            y + (height as f32 - icon) / 2.0,
            icon,
            theme.main_left_badge.0,
        );
        style.face.draw_line(
            canvas,
            icon_x + icon + pad * 2.0,
            baseline,
            label,
            font,
            theme.main_left_badge.0,
            true,
        );
    }
    if !style.badge_right {
        return;
    }
    let code = id.to_string();
    let width = (style.face.measure(&code, font, false, &art.emojis) + pad * 8.0).round() as u32;
    // 两格之间留 2 像素是**包边之外**的净缝：左边那格的包边占掉两格 COVER_INSET，
    // 自己这一格的包边还要再往左占一格，所以偏移是 3 格 + 那 2 像素。
    // 左边那一档关掉时右边这档就顶到条的起点（只让开自己那一格包边），别留一截空白
    let at = x + if style.badge_left {
        left_w + 3 * COVER_INSET + 2
    } else {
        COVER_INSET
    };
    let pill = Block {
        x: at,
        y: top,
        width,
        height,
        radius,
    };
    paint_pill(canvas, style, pill, theme.main_right_badge.1);
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

/// B 站那个小电视：机身描边 + 两根天线 + 两只眼睛。
///
/// 原插件画的是资源里那份徽标 SVG，插件不引 SVG 解码器；但这个标是纯几何图形，
/// 几条线就够——之前用一块品牌色圆角方块顶上去，图上读起来只是「一个色点」。
fn paint_tv_glyph(canvas: &mut Canvas, x: f32, y: f32, side: f32, color: Color) {
    let at = |fx: f32, fy: f32| (x + fx * side, y + fy * side);
    let line = (side * 0.11).round().max(1.0) as u32;
    canvas.fill_line(at(0.26, 0.04), at(0.38, 0.28), side * 0.09, color);
    canvas.fill_line(at(0.74, 0.04), at(0.62, 0.28), side * 0.09, color);
    // 机身只描边，中间留空：填实了就又变回那块被顶替的色块
    let (body_x, body_y) = at(0.05, 0.24);
    canvas.stroke_round_rect(
        body_x.round() as u32,
        body_y.round() as u32,
        (side * 0.9).round().max(3.0) as u32,
        (side * 0.71).round().max(3.0) as u32,
        Corners::all(side * 0.2),
        line,
        color,
    );
    for fx in [0.28, 0.63] {
        let (ex, ey) = at(fx, 0.46);
        canvas.fill_round_rect(
            ex.round() as u32,
            ey.round() as u32,
            (side * 0.1).round().max(1.0) as u32,
            (side * 0.22).round().max(2.0) as u32,
            Corners::all(side * 0.05),
            color,
        );
    }
}

/// 转发图标：原插件那一份是 `icon/FORWARD.svg`——一个圆角框，一支箭头从框里往右上指出去。
/// 我们没有 SVG 光栅器，所以照同一套几何用线段描。
fn paint_forward_glyph(canvas: &mut Canvas, x: f32, y: f32, side: f32, color: Color) {
    let at = |fx: f32, fy: f32| (x + fx * side, y + fy * side);
    let line = (side * 0.11).round().max(1.0) as u32;
    let (box_x, box_y) = at(0.02, 0.34);
    canvas.stroke_round_rect(
        box_x.round() as u32,
        box_y.round() as u32,
        (side * 0.64).round().max(3.0) as u32,
        (side * 0.64).round().max(3.0) as u32,
        Corners::all(side * 0.16),
        line,
        color,
    );
    // 箭头：杆从框内起，头朝右上；两撇按杆的方向对称张开
    let tip = at(0.82, 0.16);
    canvas.fill_line(at(0.30, 0.44), tip, side * 0.10, color);
    canvas.fill_line(tip, at(0.694, 0.432), side * 0.10, color);
    canvas.fill_line(tip, at(0.521, 0.135), side * 0.10, color);
}

/// 播放数前那个向右的三角。字体里没有 U+25B6，拼进字符串会画成一块豆腐，所以逐行描：
/// 每一行都从左边那条竖线起，宽度先加到中间再收到右边的尖。
fn paint_play_glyph(canvas: &mut Canvas, x: f32, y: f32, side: f32, color: Color) {
    let rows = side.round().max(1.0) as u32;
    for row in 0..rows {
        let t = row as f32 / (rows - 1).max(1) as f32;
        let run = (side * (1.0 - (t * 2.0 - 1.0).abs())).max(1.0);
        canvas.fill_rect(
            x.round() as u32,
            (y + row as f32).round() as u32,
            run.round() as u32,
            1,
            color,
        );
    }
}

/// 铺满宽的附属卡：视频、番剧、合集这类横封面，专栏也算——原插件那张专栏卡
/// 就是满宽封面压在标题上面（`DynamicMajorDraw.kt:683-692`）
fn is_wide(item: &Attached) -> bool {
    matches!(item.kind.as_str(), "视频" | "番剧" | "合集" | "专栏") && !item.cover.is_empty()
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
                canvas.fill_vertical_gradient(
                    0,
                    top,
                    width,
                    bottom - top,
                    Corners::none(),
                    pair[0],
                    pair[1],
                );
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
            badge_left: true,
            badge_right: false,
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
            type_text: "动态".to_string(),
            time: 1738470120,
            text: text.to_string(),
            nodes: vec![Node::Text(text.to_string())],
            images: Vec::new(),
            links: Vec::new(),
            attached: None,
            extra: Vec::new(),
            live: None,
            origin: None,
            blocked: None,
            label: String::new(),
        }
    }

    #[test]
    fn a_card_without_any_downloaded_image_still_renders() {
        // 素材一张都没有（下载全失败）也必须出图：不能因为图没下来就不推
        let style = style("1000w");
        let bytes = dynamic_card(&sample("一条纯文字动态"), &Art::default(), &style, None);
        let back = image::load_from_memory(&bytes).unwrap();
        assert_eq!(back.width(), 1000);
        assert!(back.height() > 200, "总高异常：{}", back.height());
    }

    #[test]
    fn higher_quality_makes_a_taller_image() {
        let small = measure(
            &sample("同样一段正文"),
            &Art::default(),
            &style("800w"),
            None,
        );
        let large = measure(
            &sample("同样一段正文"),
            &Art::default(),
            &style("1500w"),
            None,
        );
        assert!(large > small, "{large} 该大于 {small}");
    }

    /// 两档标签都关掉时，那条徽章带的高度要一起收掉（原插件 `BadgeEnable` 就是这个语义）
    #[test]
    fn turning_both_badges_off_shrinks_the_card() {
        let art = Art::default();
        let on = style("1000w");
        let mut both_off = style("1000w");
        both_off.badge_left = false;
        both_off.badge_right = false;
        let body = sample("同样一段正文");
        assert!(
            measure(&body, &art, &both_off, None) < measure(&body, &art, &on, None),
            "两档标签都关了，那条徽章带的高度却没收掉"
        );
        // 只关左边那一档时标签带仍然在，高度不该动——右边那档只是顶到条的起点
        let mut only_right = style("1000w");
        only_right.badge_left = false;
        only_right.badge_right = true;
        assert_eq!(
            measure(&body, &art, &only_right, None),
            measure(&body, &art, &on, None),
            "只关一档时标签带不该消失"
        );
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
            measure(&with_source, &art, &style, None) > measure(&plain, &art, &style, None),
            "转发原文没占高度"
        );
        // 底板要提前探一次高度，递归只能多出一层原文的高，不能失控
        assert!(
            measure(&with_source, &art, &style, None) < measure(&plain, &art, &style, None) * 4.0,
            "递归跑飞了"
        );
    }

    #[test]
    fn the_publish_time_line_clears_the_nickname() {
        for step in ["800w", "1000w", "1200w", "1500w"] {
            let q = Quality::find(step);
            let lines = author_lines(q, 0.0, false);
            assert!(
                lines.time_y - lines.name_y >= q.sub_title_font,
                "{step} 两行基线只差 {}，发布时间会压进昵称那行",
                lines.time_y - lines.name_y
            );
            assert!(
                lines.time_y + q.sub_title_font * 0.25 <= q.pendant_size as f32,
                "{step} 发布时间画到作者行（{}）外面",
                q.pendant_size
            );
        }
        // 转发那一行是名字与时间并排，不另起一行
        let forward = author_lines(Quality::find("1000w"), 0.0, true);
        assert!(forward.time_after_name);
        assert!(forward.time_y <= forward.name_y);
        assert!(
            forward.name_y > 0.0 && forward.name_y < Quality::find("1000w").face_size as f32,
            "转发行的基线该落在头像那一块里"
        );
    }

    /// 阴影不许压在卡片本体上：本体是 71% 半透明白，压上去整张卡会灰成一片
    /// （基准图里卡片是亮的，我们改之前实测是 206,200,199）
    #[test]
    fn the_shadow_stays_outside_the_card_body() {
        let style = style("1000w");
        let q = style.quality;
        let bg = Color::rgb(0x80, 0x90, 0xA0);
        let mut board = Canvas::new(400, 300, bg);
        paint_board(&mut board, &style, 60, 280, 40.0, 200.0);
        let mut body_only = Canvas::new(400, 300, bg);
        body_only.fill_round_rect(
            60,
            40,
            280,
            200,
            Corners::all(q.card_arc as f32),
            style.theme.card_bg,
        );
        assert_eq!(board.pixel(200, 150), body_only.pixel(200, 150));
        // 但卡片外那一圈必须还有阴影，不然等于把整块阴影删了（最外一层只扩到 18 像素）
        assert_ne!(board.pixel(50, 150), bg);
    }

    /// 底板之外还要一圈 card_margin 的干净留白，落影另外再占一截：
    /// 少算任何一格，卡片就顶到图的下边、或阴影把留白填满（用户圈的「图片下方空间小了」）
    #[test]
    fn the_image_keeps_ground_below_the_card_and_its_shadow() {
        let art = Art::default();
        let style = style("1000w");
        let q = style.quality;
        let bytes = dynamic_card(&sample("一段短正文"), &art, &style, None);
        let back = image::load_from_memory(&bytes).unwrap().to_rgba8();
        // 测试样式的背景是这一档纯色，卡片本体与阴影都跟它不同
        let flat = Color::rgb(211, 237, 250);
        let at = |y: u32| Color(back.get_pixel(500, y).0);
        let sum = |c: Color| -> u32 { c.0[..3].iter().map(|v| *v as u32).sum() };
        let last = back.height() - 1;
        for y in [last, last - q.card_margin / 2, last - q.card_margin + 1] {
            assert_eq!(at(y), flat, "第 {y} 行还被阴影占着：图的下边距不够");
        }
        let ground = last - q.card_margin;
        assert_ne!(
            at(ground - 2),
            flat,
            "干净留白之上该是落影，可那里什么都没有"
        );
        let board = ground - shadow_below(q) as u32;
        assert!(
            sum(at(board - 8)) > sum(flat),
            "底板下沿没画到 board={} 那一格",
            board
        );
    }

    /// 视频卡的封面高度跟着这张封面自己的比例走，不再写死 16:9
    /// 专栏那一格的封面比例是站方定死的（卡宽 × 0.35），不看封面自己的长短
    /// （`DynamicMajorDraw.kt:683`）——与投稿视频那条正好相反
    #[test]
    fn an_article_cover_ignores_its_own_aspect() {
        let style = style("1000w");
        let mut item = sample("专栏");
        item.attached = Some(Attached {
            kind: "专栏".to_string(),
            title: "同一档标题".to_string(),
            desc: "同一档简介".to_string(),
            cover: "https://i/portrait.png".to_string(),
            url: "https://www.bilibili.com/read/cv1".to_string(),
            badge: "专栏".to_string(),
            ..Default::default()
        });
        let mut art = Art::default();
        art.pictures.insert(
            "https://i/portrait.png".to_string(),
            RgbaImage::from_pixel(360, 640, image::Rgba([9, 9, 9, 255])),
        );
        art.pictures.insert(
            "https://i/landscape.png".to_string(),
            RgbaImage::from_pixel(640, 360, image::Rgba([9, 9, 9, 255])),
        );
        let tall = measure(&item, &art, &style, None);
        item.attached.as_mut().unwrap().cover = "https://i/landscape.png".to_string();
        assert_eq!(
            measure(&item, &art, &style, None),
            tall,
            "专栏封面跟着原图比例走了"
        );
        // 没封面就退回小卡（左图右文），所以整块要矮一截：这条钉住它走的是大卡那一支
        item.attached.as_mut().unwrap().cover = String::new();
        assert!(
            measure(&item, &art, &style, None) < tall,
            "专栏没走满宽封面那一支"
        );
    }

    #[test]
    fn the_video_cover_keeps_its_own_aspect() {
        let style = style("1000w");
        let q = style.quality;
        let content_w = (q.card_width() - q.card_padding * 2) as f32;
        let mut item = sample("投稿");
        item.attached = Some(Attached {
            kind: "视频".to_string(),
            title: "同一档标题".to_string(),
            cover: "https://i/portrait.png".to_string(),
            url: "https://b23.tv/p".to_string(),
            duration: String::new(),
            play: String::new(),
            desc: String::new(),
            badge: "投稿视频".to_string(),
        });
        let mut art = Art::default();
        art.pictures.insert(
            "https://i/portrait.png".to_string(),
            RgbaImage::from_pixel(360, 640, image::Rgba([9, 9, 9, 255])),
        );
        art.pictures.insert(
            "https://i/landscape.png".to_string(),
            RgbaImage::from_pixel(640, 360, image::Rgba([9, 9, 9, 255])),
        );
        let tall = measure(&item, &art, &style, None);
        item.attached.as_mut().unwrap().cover = "https://i/landscape.png".to_string();
        let wide = measure(&item, &art, &style, None);
        // 两遍之间只有封面比例不同，所以高度差必须正好等于两档封面高之差
        let owed = content_w * (640.0 / 360.0 - 360.0 / 640.0);
        assert!(
            (tall - wide - owed).abs() < 1.0,
            "竖屏与横屏只差 {}，该差 {owed}",
            tall - wide
        );
    }

    /// 站方那格徽章：有词就多留一格、左徽章整档关掉就不留
    #[test]
    fn the_station_badge_gets_its_own_band() {
        let art = Art::default();
        let mut item = sample("投稿");
        item.attached = Some(Attached {
            kind: "视频".to_string(),
            title: "标题".to_string(),
            cover: "https://i/c.png".to_string(),
            url: "https://b23.tv/c".to_string(),
            badge: "投稿视频".to_string(),
            ..Default::default()
        });
        let mut blank = item.clone();
        blank.attached.as_mut().unwrap().badge = String::new();
        let card_style = style("1000w");
        // 那一格留的是「药丸本身 + 它外面那圈白包边」
        let band = card_style.quality.badge_height as f32 + COVER_INSET as f32;
        let gap =
            measure(&item, &art, &card_style, None) - measure(&blank, &art, &card_style, None);
        assert!(
            (gap - band).abs() < 1.0,
            "有徽章那一格该多 {band} 像素，实得 {gap}"
        );
        let mut off = style("1000w");
        off.badge_left = false;
        let quiet = measure(&item, &art, &off, None) - measure(&blank, &art, &off, None);
        assert!(quiet.abs() < 1.0, "左徽章关掉时不该留这一格，实得 {quiet}");
    }

    /// 转发的那张原卡自己占一格角标，外层那一格固定写「动态」不跟类型词走
    /// （用户 2026-10-16 圈的 #182 剩下一半：真卡上外层成了「转发动态」，里层反倒空着）
    #[test]
    fn a_forward_reserves_a_second_badge_band_below_the_outer_one() {
        let art = Art::default();
        let on = style("1000w");
        let mut off = style("1000w");
        off.badge_left = false;
        let band = on.quality.badge_height as f32;
        // 里层那一格是「药丸 + 外面那圈白包边」，外层那一格仍按原插件贴顶不包边
        let inner_band = band + COVER_INSET as f32;

        let plain = sample("转发语");
        let mut nested = sample("转发语");
        nested.origin = Some(Box::new(sample("被转发的那条原文正文")));

        // 关掉左徽章只少外层那一格；开着时转发要多出里层那一格
        let plain_gap = measure(&plain, &art, &on, None) - measure(&plain, &art, &off, None);
        assert!(
            (plain_gap - band).abs() < 1.0,
            "普通动态只该有外层那一格（{band}），实得 {plain_gap}"
        );
        let forward_gap = measure(&nested, &art, &on, None) - measure(&nested, &art, &off, None);
        assert!(
            (forward_gap - (band + inner_band)).abs() < 1.0,
            "转发该有外层 {band} + 里层 {inner_band} 两格，实得 {forward_gap}"
        );

        // 里层那一格是真画了字的：整列里带图标/文字那支青色的行数，转发要明显多于普通
        let cyan_rows = |dynamic: &Dynamic| -> u32 {
            let img = image::load_from_memory(&dynamic_card(dynamic, &art, &on, None))
                .unwrap()
                .to_rgba8();
            (0..img.height())
                .filter(|row| {
                    (0..img.width()).any(|col| {
                        let p = img.get_pixel(col, *row).0;
                        p[2] > 150 && p[1] > 140 && p[0] < 120
                    })
                })
                .count() as u32
        };
        assert!(
            cyan_rows(&nested) > cyan_rows(&plain) * 3 / 2,
            "里层那格没画出字来：普通 {} 行，转发 {} 行",
            cyan_rows(&plain),
            cyan_rows(&nested)
        );
    }

    /// 外层角标不许写类型词：原插件那一格取 `tag ?: 动态/转发动态`，
    /// 「投稿视频」这类词只出现在附属卡自己那格与页脚的 {type} 里
    #[test]
    fn the_outer_badge_ignores_the_type_word() {
        let art = Art::default();
        let card_style = style("1000w");
        let mut video = sample("同一段正文");
        video.type_text = "投稿视频".to_string();
        let mut column = sample("同一段正文");
        column.type_text = "专栏".to_string();
        assert_eq!(
            dynamic_card(&video, &art, &card_style, None),
            dynamic_card(&column, &art, &card_style, None),
            "类型词渗进卡片里了：外层那格两种动态必须一模一样"
        );
    }

    /// 压在封面上那层渐变必须按封面的下角裁圆：方角渐变会把圆掉的角重新糊成一块直角
    /// （用户 2026-10-16 圈的「这里怎么不是圆角？漏了点方角还是灰色的」）
    #[test]
    fn the_scrim_follows_the_cover_corner() {
        let q = Quality::find("1000w");
        let mut canvas = Canvas::new(200, 200, Color::rgb(255, 255, 255));
        let (x, y, w, h) = (20u32, 20u32, 160u32, 160u32);
        let radius = inner_radius(q, false);
        canvas.fill_round_rect(x, y, w, h, radius, Color::rgb(0x18, 0x1A, 0x1C));
        let band = (h as f32 * 0.2).round() as u32;
        canvas.fill_vertical_gradient(
            x,
            y + h - band,
            w,
            band,
            Corners {
                bottom_left: radius.bottom_left,
                bottom_right: radius.bottom_right,
                ..Corners::none()
            },
            Color::rgba(0, 0, 0, 0),
            Color::rgba(0, 0, 0, 150),
        );
        let corner = canvas.pixel(x, y + h - 1).0;
        assert_eq!(
            &corner[..3],
            &[255, 255, 255],
            "封面下角被方角渐变糊住了：{:?}",
            corner
        );
        // 中间那一段仍要吃到渐变，否则白字站不住
        let middle = canvas.pixel(x + w / 2, y + h - 1).0;
        assert!(middle[0] < 0x18, "渐变没压到封面底边：{:?}", middle);
    }

    /// 那一格 tag 是"包边"不是"分离"，而且要与里面的封面对齐：
    /// 药丸底边压在卡顶上、外面裹一圈白，白边左沿正好落在卡左边缘上（不往卡外突出），
    /// 药丸本体则与封面左边缘同一条竖线。
    /// （用户 2026-10-16 两次纠正：「我说3像素包边，谁让你分离了？」「这里对齐一点」）
    #[test]
    fn the_tag_is_wrapped_not_detached() {
        let card_style = style("1000w");
        let q = card_style.quality;
        let bg = Color::rgb(0x80, 0x90, 0xA0);
        let mut canvas = Canvas::new(400, 300, bg);
        let card = Block {
            x: 60,
            y: 120,
            width: 300,
            height: 120,
            radius: inner_radius(q, true),
        };
        paint_card_badge(
            &mut canvas,
            &card_style,
            &Art::default(),
            card,
            "专栏",
            card_style.theme.sub_left_badge,
            None,
        );
        let same = |x: u32, y: u32, want: Color| canvas.pixel(x, y).0[..3] == want.0[..3];
        let white = Color::rgb(255, 255, 255);
        let mid = card.y - q.badge_height / 2;
        assert!(
            !same(card.x - 1, mid, white),
            "包边往卡外突出了：卡左边缘外那一格不该是白的（阴影会到那儿，但白边不会）"
        );
        assert!(
            same(card.x + 1, mid, white),
            "包边没画出来：卡左边缘内侧那一格该是白的"
        );
        assert!(
            same(
                card.x + COVER_INSET + 1,
                mid,
                card_style.theme.sub_left_badge.1
            ),
            "药丸本体没让开那一格包边：它该与封面左边缘同一条竖线"
        );
        assert!(
            same(card.x + 40, card.y - 1, card_style.theme.sub_left_badge.1),
            "药丸与卡顶之间被留出了缝：那一行该是药丸底色"
        );
    }

    /// 阴影向外必须渐淡，且贴边那一环不许叠成硬黑边（用户 2026-10-15 圈的「大黑边」）
    #[test]
    fn the_shadow_tapers_outward() {
        let style = style("1000w");
        let bg = Color::rgb(255, 255, 255);
        let mut canvas = Canvas::new(400, 300, bg);
        paint_board(&mut canvas, &style, 60, 280, 40.0, 200.0);
        let edge = 240; // 卡片底边：40 + 200
        let dark = |y: u32| {
            let pixel = canvas.pixel(200, y).0;
            3 * 255 - pixel[..3].iter().map(|c| *c as u32).sum::<u32>()
        };
        let mut previous: Option<u32> = None;
        for step in 0..16 {
            let now = dark(edge + step);
            if let Some(inside) = previous {
                assert!(
                    now <= inside,
                    "往外第 {step} 环比里面还黑（{now} > {inside}）：阴影全叠在贴边那一圈"
                );
            }
            previous = Some(now);
        }
        // 高斯阴影在形状边缘最多给一半浓度：#46 = 70 → 最深那一环每通道到 35 上下（三通道和 105），
        // 不该是六层叠出来的 160（实测旧画法把背景从 251 压到 69）
        assert!(dark(edge) <= 3 * 45, "贴边那一环深到 {}", dark(edge));
    }

    /// 徽章压着的那一个卡片上角必须是直角：圆角卡片 + 方底徽章之间会豁开一道
    #[test]
    fn the_card_corner_under_an_enabled_badge_is_square() {
        let mut style = style("1000w");
        let bg = Color::rgb(0x80, 0x90, 0xA0);
        let bright = |canvas: &Canvas, x: u32, y: u32| {
            canvas.pixel(x, y).0[..3]
                .iter()
                .map(|c| *c as u32)
                .sum::<u32>()
        };
        style.badge_left = true;
        let mut with = Canvas::new(400, 300, bg);
        paint_board(&mut with, &style, 60, 280, 40.0, 200.0);
        style.badge_left = false;
        let mut without = Canvas::new(400, 300, bg);
        paint_board(&mut without, &style, 60, 280, 40.0, 200.0);
        assert!(
            bright(&without, 61, 41) + 100 < bright(&with, 61, 41),
            "开着左徽章时那个角该画到卡体，关掉才该圆出去"
        );
    }

    /// 角标那格画的是小电视，不是一块实心色块
    #[test]
    fn the_badge_glyph_is_a_tv_not_a_solid_block() {
        let style = style("1000w");
        let ink = style.theme.main_left_badge.0;
        let side = style.quality.sub_title_font;
        let mut canvas = Canvas::new(64, 64, Color::rgb(255, 255, 255));
        paint_tv_glyph(&mut canvas, 8.0, 8.0, side, ink);
        let is_ink =
            |x: f32, y: f32| canvas.pixel((8.0 + x * side) as u32, (8.0 + y * side) as u32) == ink;
        // 机身正中必须是空的，两只眼睛之间那一格也是
        assert!(!is_ink(0.50, 0.60), "机身被填实了，又成那块色点");
        // 机身左沿与天线要有墨
        assert!(is_ink(0.06, 0.60), "机身描边没画出来");
        assert!(is_ink(0.30, 0.10), "天线没画出来");
        // 眼睛
        assert!(is_ink(0.30, 0.52) || is_ink(0.66, 0.52), "两只眼睛都没了");
    }

    /// 卡中卡必须吃主题那层半透明白：改 alpha 就得跟着变（原插件的透明度在 ImageTheme.yml 里）
    #[test]
    fn the_inner_card_uses_the_theme_ground() {
        let card_style = style("1000w");
        let q = card_style.quality;
        let base = Color::rgb(0x80, 0x90, 0xA0);
        let mut painted = Canvas::new(300, 200, base);
        paint_inner_board(
            &mut painted,
            &card_style,
            30,
            30,
            240,
            140,
            Corners::all(q.card_arc as f32),
        );
        let mut ground = Canvas::new(300, 200, base);
        ground.fill_round_rect(
            30,
            30,
            240,
            140,
            Corners::all(q.card_arc as f32),
            card_style.theme.card_bg,
        );
        // 中心那一格只该有主题那层白，不许再掺写死的灰（此前是 #F4F5F7／#F0F2F5／rgba(0,0,0,18)）
        assert_eq!(painted.pixel(150, 100), ground.pixel(150, 100));
        let mut thin = style("1000w");
        thin.theme.card_bg = Color::rgba(255, 255, 255, 40);
        let mut faint = Canvas::new(300, 200, base);
        paint_inner_board(
            &mut faint,
            &thin,
            30,
            30,
            240,
            140,
            Corners::all(q.card_arc as f32),
        );
        assert_ne!(
            faint.pixel(150, 100),
            painted.pixel(150, 100),
            "换了主题 alpha 内卡没反应，说明还是写死的"
        );
    }

    /// 页脚留空时整块跳过：白占一行字高会把卡片底下撑出一段没内容的空白
    #[test]
    fn an_empty_footer_reserves_nothing_below_the_body() {
        let art = Art::default();
        let body = sample("一段短正文");
        let mut quiet = style("1000w");
        quiet.footer = String::new();
        let q = quiet.quality;
        let empty = measure(&body, &art, &quiet, None);
        let owed = q.content_space as f32 + q.footer_font + q.card_padding as f32 * 0.8;
        let with_footer = measure(&body, &art, &style("1000w"), None);
        assert!(
            (with_footer - empty - owed).abs() < 1.0,
            "页脚那一档差 {}，该差 {owed}",
            with_footer - empty
        );
        // 空页脚时整图高 = 上边距 + 徽章 + 作者行 + 间距 + 一行正文 + 底板留白 + 下边距 + 落影
        let expected = q.card_margin as f32
            + q.badge_height as f32
            + q.pendant_size as f32
            + q.content_space as f32
            + (q.content_font + q.line_space as f32)
            + q.card_padding as f32
            + q.card_margin as f32
            + shadow_below(q);
        assert!(
            (empty - expected).abs() < 1.0,
            "空页脚还多占 {} 像素",
            empty - expected
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
            measure(&hidden, &art, &style, None) > measure(&plain, &art, &style, None),
            "遮挡提示条没占高度"
        );
    }

    /// 译文挂在正文后面：同一张卡该比不译时高出一截
    #[test]
    fn a_translation_adds_height_below_the_body() {
        let plain = sample("本日の新活動です");
        let art = Art::default();
        let style = style("1000w");
        let done = Rendered {
            cut_line: "\n\n〓〓〓 翻译 〓〓〓\n".to_string(),
            text: "今日的新活动开始了".to_string(),
        };
        assert!(
            measure(&plain, &art, &style, Some(&done)) > measure(&plain, &art, &style, None),
            "译文没占高度"
        );
    }

    #[test]
    fn image_grid_lays_two_and_three_columns() {
        let style = style("1000w");
        let q = style.quality;
        let width = q.card_width() - q.card_padding * 2;
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
            badge: "投稿视频".to_string(),
        };
        let mut board = Board {
            style: &style,
            art: &Art::default(),
            canvas: None,
        };
        let content = style.quality.card_width() - style.quality.card_padding * 2;
        let big = board.attached(0, content, 0.0, &item, true);
        let small = board.attached(0, content, 0.0, &item, false);
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
        assert!(measure(&item, &Art::default(), &style, None) > 0.0);
    }

    /// 给人眼看版式的出图，不做断言：`cargo test --lib -- --ignored --nocapture card_preview`
    /// 三张分别对着原插件 README 的样式预览——九宫格、视频附件、转发嵌套。
    #[test]
    #[ignore = "只出图给人看版式，没有断言"]
    fn card_preview_writes_every_board_to_temp() {
        let mut style = style("1000w");
        // 背景换成调试配置那一档：defaultColor 单值 #d3edfa 自动扩成三段渐变。
        // 纯白底上根本看不出阴影，人眼验收阴影与徽章融合必须带渐变。
        style.background = crate::draw::theme::background("#d3edfa", 30.0, true, 0.25, 1.0);
        let mut art = Art::default();
        art.faces.insert(
            "https://face/1.jpg".to_string(),
            RgbaImage::from_pixel(200, 200, image::Rgba([240, 200, 190, 255])),
        );
        let palette = [
            image::Rgba([214, 96, 96, 255]),
            image::Rgba([96, 168, 214, 255]),
            image::Rgba([112, 200, 132, 255]),
            image::Rgba([224, 176, 88, 255]),
            image::Rgba([168, 120, 214, 255]),
            image::Rgba([88, 88, 96, 255]),
        ];
        let urls: Vec<String> = palette
            .iter()
            .enumerate()
            .map(|index| format!("https://i/{}.png", index.0))
            .collect();
        for (index, color) in palette.iter().enumerate() {
            art.pictures
                .insert(urls[index].clone(), RgbaImage::from_pixel(400, 400, *color));
        }
        art.pictures.insert(
            "https://i/cover.png".to_string(),
            RgbaImage::from_pixel(640, 400, image::Rgba([40, 44, 52, 255])),
        );
        // 想看 unicode 表情的回退效果：设 ARONA_TWEMOJI_DIR 指向放好 `<码位>.png` 的目录，
        // 那些 png 就是 gather 从 twemoji 拉回来的那一份（地址见 text::twemoji_url）。
        if let Ok(dir) = std::env::var("ARONA_TWEMOJI_DIR") {
            let mut set = HashMap::new();
            for code in ["2764", "1f9dc-200d-2640-fe0f"] {
                let path = std::path::Path::new(&dir).join(format!("{code}.png"));
                if let Ok(raw) = std::fs::read(&path)
                    && let Ok(any) = image::load_from_memory(&raw)
                {
                    set.insert(code.to_string(), any.to_rgba8());
                }
            }
            if !set.is_empty() {
                art.emojis = EmojiSet::new(set);
            }
        }

        let mut grid = sample("这是🧜‍♀️只美人鱼，你敢和她直视10秒吗？❤️");
        grid.images = urls.clone();

        let mut video = sample("翻唱投稿");
        video.attached = Some(Attached {
            kind: "视频".to_string(),
            title: "【全部猫芒】7ki7ki棒棒—派对浪客诸葛孔明【翻唱】".to_string(),
            cover: "https://i/cover.png".to_string(),
            url: "https://b23.tv/x".to_string(),
            duration: "01:29".to_string(),
            play: "1.2万播放".to_string(),
            desc: "ばんばん".to_string(),
            badge: "投稿视频".to_string(),
        });

        let mut nested = sample("２ｗ！？？？？？？？？！");
        nested.origin = Some(Box::new(video.clone()));

        // 用户给的那条「超辉夜姬!粉丝勋章」基准：正文空，只有一张装扮卡
        // （右侧标签原插件按 bizType=231 给「装扮」，见 Dynamic.kt:1294）
        let mut deco = sample("");
        deco.images = Vec::new();
        deco.attached = Some(Attached {
            kind: "装扮".to_string(),
            title: "超辉夜姬!粉丝勋章".to_string(),
            cover: "https://i/cover.png".to_string(),
            url: "https://t.bilibili.com/1257441591087333392".to_string(),
            duration: String::new(),
            play: String::new(),
            desc: "动态卡片".to_string(),
            badge: "装扮".to_string(),
        });

        // 专栏那条：满宽封面（站方定死 0.35 比例）+ 标题 + 三行简介 + 卡上方那一格「专栏」
        let mut article = sample("维护更新已完成，补偿已发放");
        article.images = Vec::new();
        article.attached = Some(Attached {
            kind: "专栏".to_string(),
            title: "《蔚蓝档案》10月15日维护更新说明".to_string(),
            cover: "https://i/cover.png".to_string(),
            url: "https://www.bilibili.com/read/cv53416297".to_string(),
            duration: String::new(),
            play: String::new(),
            desc: "欢迎连接【什亭之箱】，老师。为保证更好的游戏体验，我们计划于 10 月 15 日 14:00 \
                   开始进行维护更新，预计持续 4 小时。"
                .to_string(),
            badge: "专栏".to_string(),
        });

        let out = std::env::temp_dir().join("arona-bili-cards");
        std::fs::create_dir_all(&out).unwrap();
        for (name, item) in [
            ("grid", &grid),
            ("video", &video),
            ("nested", &nested),
            ("deco", &deco),
            ("article", &article),
        ] {
            let file = out.join(format!("{name}.png"));
            std::fs::write(&file, dynamic_card(item, &art, &style, None)).unwrap();
            println!("{name} -> {}", file.display());
        }
    }
}
