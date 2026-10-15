//! 直播卡片：开播那条推送用的图。
//!
//! 版式照原插件 `draw/LiveDraw.kt`：一条作者行（头像、房间标题、主播名与开播时间）压一张等宽封面，
//! 顶部徽章条左边写「直播」、右边写房间号。原插件在这张卡上把挂件与认证强制传成 null，
//! 所以头像必走"放大一圈再描个白边圆"那一支，这里就不留分支了。
//!
//! 与动态卡不同，直播卡每一块的高度都能事先算出来（没有递归子卡），所以一遍画完，不必先干跑量高。
//! 直播结束**不画图**——原插件那时只发文字模板，见 `push`。

use super::canvas::{Canvas, Color, Corners};
use super::card::{Art, Style, paint_background, paint_badges, paint_board, qr_image};
use super::text::Span;
use crate::bili::model::LiveInfo;

/// 封面那一段占多高。原插件按 `卡宽 × 图高 / 图宽 − 半个描边` 铺，
/// 图没下下来时按请求缩放时用的 16:10 留位，卡片不该塌成一条。
fn cover_height(live: &LiveInfo, art: &Art, box_w: f32, outline_half: f32) -> f32 {
    match art.picture(&live.cover) {
        Some(found) if found.width() > 0 => {
            (box_w * found.height() as f32 / found.width() as f32 - outline_half).max(1.0)
        }
        _ => (box_w * 0.625 - outline_half).max(1.0),
    }
}

/// 一张直播卡，返回 PNG 字节
pub fn live_card(live: &LiveInfo, art: &Art, style: &Style) -> Vec<u8> {
    let q = style.quality;
    let theme = &style.theme;
    let pad = q.card_padding as f32;
    let card_x = q.card_margin;
    let card_w = q.card_width();
    let badge_h = q.badge_height_for(style.badge_left || style.badge_right) as f32;
    let outline_half = q.card_outline as f32 / 2.0;
    let box_w = card_w as f32 - outline_half;
    let head_h = q.face_size as f32 + pad * 2.0;
    let cover_h = cover_height(live, art, box_w, outline_half);
    let body = head_h + q.content_space as f32 + cover_h;

    let leading = q.footer_font + q.line_space as f32;
    let footer = if style.footer.is_empty() {
        None
    } else {
        Some(style.face.layout(
            &[Span::new(&style.footer, theme.footer)],
            q.footer_font,
            false,
            card_w as f32,
            None,
            &art.emojis,
        ))
    };
    let footer_h = footer
        .as_ref()
        .map(|lines| lines.rows() as f32 * leading)
        .unwrap_or(0.0);

    let top = q.card_margin as f32 + badge_h;
    let height =
        (top + body + q.card_margin as f32 + super::card::shadow_below(q) + footer_h).ceil() as u32;
    let mut canvas = Canvas::new(q.image_width, height, Color::rgb(255, 255, 255));
    paint_background(&mut canvas, &style.background);
    paint_board(&mut canvas, style, card_x, card_w, top, body);
    if style.badge_left || style.badge_right {
        paint_badges(
            &mut canvas,
            style,
            art,
            card_x,
            top - badge_h,
            "直播",
            live.room_id,
        );
    }

    draw_head(&mut canvas, style, art, live, top);

    // 封面：深底垫一层，图没下来就露出这块深灰
    let radius = Corners::all(q.card_arc as f32);
    let cover_y = (top + head_h + q.content_space as f32).round() as u32;
    let wide = box_w.round() as u32;
    let tall = cover_h.round() as u32;
    canvas.fill_round_rect(
        card_x,
        cover_y,
        wide,
        tall,
        radius,
        Color::rgb(0x18, 0x1A, 0x1C),
    );
    if let Some(found) = art.picture(&live.cover) {
        canvas.paste_cover_rounded(card_x, cover_y, wide, tall, radius, found);
    }
    canvas.stroke_round_rect(
        card_x,
        cover_y,
        wide,
        tall,
        radius,
        q.draw_outline,
        theme.draw_outline,
    );

    if let Some(lines) = footer.as_ref() {
        // 页脚顶边贴在卡片底板下方半个 cardMargin 处（原插件口径）
        let first = top + body + q.card_margin as f32 / 2.0 + q.footer_font;
        style.face.draw(
            &mut canvas,
            lines,
            card_x as f32,
            first,
            q.footer_font,
            false,
            leading,
            &art.emojis,
        );
    }
    canvas.to_png()
}

/// 作者行：头像与白边圆、房间标题、主播名加开播时间，右侧可选二维码
fn draw_head(canvas: &mut Canvas, style: &Style, art: &Art, live: &LiveInfo, top: f32) {
    let q = style.quality;
    let theme = &style.theme;
    let pad = q.card_padding as f32;
    let inflate = q.face_inflate as f32;
    let side = q.face_size;
    let x = q.card_margin as f32;
    let left = (x + pad * 1.8 - inflate).max(x) as u32;
    let head_top = (top + pad * 1.2 - inflate).max(top) as u32;
    let ring = side / 2 + inflate as u32;
    canvas.fill_circle(
        left + side / 2,
        head_top + side / 2,
        ring + 1,
        theme.face_outline,
    );
    match art.face(&live.face) {
        Some(found) => canvas.paste_circle_image(left, head_top, ring, found),
        // 直播接口有时不给头像，留个灰圆比空着好看
        None => canvas.fill_circle(
            left + side / 2,
            head_top + side / 2,
            side / 2,
            Color::rgb(0xE3, 0xE5, 0xE7),
        ),
    }

    let text_x = x + side as f32 + pad * 3.0;
    let room = (x + q.card_width() as f32 - text_x).max(40.0);
    let gap = (q.pendant_size as f32 - q.name_font - q.sub_title_font) / 3.0;
    let title = style.face.truncate(&live.title, q.name_font, true, room);
    style.face.draw_line(
        canvas,
        text_x,
        top + gap * 1.25 + q.name_font,
        &title,
        q.name_font,
        theme.title,
        true,
    );
    // 原插件这里就是两个空格，主播名与时间之间不留一个
    let stamp = format!("{}  {}", live.anchor, live.started_text());
    let when = style
        .face
        .truncate(stamp.trim(), q.sub_title_font, false, room);
    style.face.draw_line(
        canvas,
        text_x,
        top + gap * 1.25 + q.name_font + gap * 0.5 + q.sub_title_font,
        &when,
        q.sub_title_font,
        theme.desc,
        false,
    );

    let Some(code) = style.qr.as_deref() else {
        return;
    };
    let side = q.ornament_height;
    // 二维码在原插件里略微顶出作者行上沿，靠 cardPadding 拉回来，不许顶穿卡片
    let offset = (q.face_size as f32 - side as f32 + q.content_space as f32) / 2.0;
    let Some(image) = qr_image(code, side, theme.title) else {
        return;
    };
    let at_x = (x + q.card_width() as f32 - side as f32 - offset.abs()).max(x) as u32;
    let at_y = (top + offset + pad).max(top) as u32;
    canvas.paste(at_x, at_y, &image);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::quality::Quality;
    use crate::draw::text::Typeface;
    use crate::draw::theme::Theme;
    use image::RgbaImage;

    fn style(quality: &str) -> Style {
        Style {
            quality: Quality::find(quality),
            theme: Theme::find("v3"),
            face: Typeface::discover("").unwrap(),
            background: vec![Color::rgb(211, 237, 250)],
            badge_left: true,
            badge_right: true,
            footer: String::new(),
            qr: None,
        }
    }

    fn live() -> LiveInfo {
        LiveInfo {
            uid: 123,
            room_id: 603255,
            anchor: "测试主播".to_string(),
            face: "https://face/1.jpg".to_string(),
            area: "手游".to_string(),
            title: "开播了，来看看".to_string(),
            cover: "https://i/cover.png".to_string(),
            online: 100,
            live_start: 1738470120,
            status: 1,
        }
    }

    #[test]
    fn a_live_card_is_as_wide_as_the_quality() {
        let style = style("1000w");
        let bytes = live_card(&live(), &Art::default(), &style);
        let back = image::load_from_memory(&bytes).unwrap();
        assert_eq!(back.width(), 1000);
        // 没封面素材时按 16:10 留位：作者行 140 + 间距 15 + 封面 586 + 徽章与上下留白
        assert!(
            back.height() > 700 && back.height() < 900,
            "总高不对：{}",
            back.height()
        );
    }

    #[test]
    fn a_cover_of_known_aspect_sets_the_card_height() {
        let style = style("1000w");
        let mut art = Art::default();
        // 2:1 的封面：卡宽 940 半描边之后按比例该是 469 上下
        art.pictures.insert(
            "https://i/cover.png".to_string(),
            RgbaImage::from_pixel(1920, 960, image::Rgba([9, 9, 9, 255])),
        );
        let with_cover = image::load_from_memory(&live_card(&live(), &art, &style)).unwrap();
        let bare = image::load_from_memory(&live_card(&live(), &Art::default(), &style)).unwrap();
        assert!(
            bare.height() > with_cover.height(),
            "16:10 的占位比 2:1 的真封面矮？{} vs {}",
            bare.height(),
            with_cover.height()
        );
    }

    #[test]
    fn a_footer_makes_the_live_card_taller() {
        let mut style = style("1000w");
        let bare = image::load_from_memory(&live_card(&live(), &Art::default(), &style)).unwrap();
        style.footer = "由 Bilibili 动态订阅插件推送".to_string();
        let paged = image::load_from_memory(&live_card(&live(), &Art::default(), &style)).unwrap();
        assert!(
            paged.height() > bare.height(),
            "页脚没占高度：{} vs {}",
            bare.height(),
            paged.height()
        );
    }

    #[test]
    fn a_qr_ornament_does_not_change_the_height() {
        // 二维码坐在作者行的富余里，原插件就是这么排的，不该把卡顶高
        let mut style = style("1000w");
        let bare = image::load_from_memory(&live_card(&live(), &Art::default(), &style)).unwrap();
        style.qr = Some(live().link());
        let coded = image::load_from_memory(&live_card(&live(), &Art::default(), &style)).unwrap();
        assert_eq!(bare.height(), coded.height());
    }

    #[test]
    fn quality_switch_scales_the_live_card() {
        let small =
            image::load_from_memory(&live_card(&live(), &Art::default(), &style("800w"))).unwrap();
        let big =
            image::load_from_memory(&live_card(&live(), &Art::default(), &style("1500w"))).unwrap();
        assert_eq!(small.width(), 800);
        assert_eq!(big.width(), 1500);
        assert!(big.height() > small.height());
    }
}
