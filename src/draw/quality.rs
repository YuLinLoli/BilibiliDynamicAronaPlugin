//! 四档画质的字面值。
//!
//! 原插件的 `DynamicImageQuality` 不是等比缩放，`1500w` 那一档明显偏离比例，所以四档各写一份字面值，
//! 逐个抄自 `draw/DynamicImageQuality.kt`。配置 `bili-image.quality` 认 `800w / 1000w / 1200w / 1500w`。

/// 一档画质的全部尺寸与字号（像素与像素当量的字号）
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quality {
    pub name: &'static str,
    pub image_width: u32,
    pub card_margin: u32,
    pub card_padding: u32,
    pub card_arc: u32,
    pub name_font: f32,
    pub title_font: f32,
    pub sub_title_font: f32,
    pub desc_font: f32,
    pub content_font: f32,
    pub footer_font: f32,
    pub card_outline: u32,
    pub draw_outline: u32,
    pub face_size: u32,
    pub face_inflate: u32,
    pub pendant_size: u32,
    pub verify_icon: u32,
    pub ornament_height: u32,
    pub badge_height: u32,
    pub badge_padding: u32,
    pub badge_arc: u32,
    pub line_space: u32,
    pub draw_space: u32,
    pub content_space: u32,
    pub additional_card_height: u32,
}

const W800: Quality = Quality {
    name: "800w",
    image_width: 800,
    card_margin: 20,
    card_padding: 20,
    card_arc: 10,
    name_font: 30.0,
    title_font: 26.0,
    sub_title_font: 22.0,
    desc_font: 20.0,
    content_font: 26.0,
    footer_font: 22.0,
    card_outline: 2,
    draw_outline: 2,
    face_size: 64,
    face_inflate: 5,
    pendant_size: 112,
    verify_icon: 20,
    ornament_height: 90,
    badge_height: 36,
    badge_padding: 5,
    badge_arc: 5,
    line_space: 8,
    draw_space: 10,
    content_space: 10,
    additional_card_height: 90,
};

const W1000: Quality = Quality {
    name: "1000w",
    image_width: 1000,
    card_margin: 30,
    card_padding: 30,
    card_arc: 15,
    name_font: 36.0,
    title_font: 32.0,
    sub_title_font: 28.0,
    desc_font: 26.0,
    content_font: 32.0,
    footer_font: 28.0,
    card_outline: 3,
    draw_outline: 3,
    face_size: 80,
    face_inflate: 10,
    pendant_size: 140,
    verify_icon: 30,
    ornament_height: 125,
    badge_height: 45,
    badge_padding: 8,
    badge_arc: 8,
    line_space: 11,
    draw_space: 15,
    content_space: 12,
    additional_card_height: 130,
};

const W1200: Quality = Quality {
    name: "1200w",
    image_width: 1200,
    card_margin: 40,
    card_padding: 40,
    card_arc: 20,
    name_font: 42.0,
    title_font: 38.0,
    sub_title_font: 34.0,
    desc_font: 32.0,
    content_font: 38.0,
    footer_font: 34.0,
    card_outline: 4,
    draw_outline: 4,
    face_size: 95,
    face_inflate: 13,
    pendant_size: 170,
    verify_icon: 40,
    ornament_height: 140,
    badge_height: 55,
    badge_padding: 11,
    badge_arc: 11,
    line_space: 14,
    draw_space: 20,
    content_space: 17,
    additional_card_height: 160,
};

const W1500: Quality = Quality {
    name: "1500w",
    image_width: 1500,
    card_margin: 50,
    card_padding: 50,
    card_arc: 30,
    name_font: 51.0,
    title_font: 46.0,
    sub_title_font: 43.0,
    desc_font: 40.0,
    content_font: 47.0,
    footer_font: 43.0,
    card_outline: 6,
    draw_outline: 6,
    face_size: 100,
    face_inflate: 18,
    pendant_size: 190,
    verify_icon: 50,
    ornament_height: 150,
    badge_height: 72,
    badge_padding: 15,
    badge_arc: 16,
    line_space: 20,
    draw_space: 25,
    content_space: 20,
    additional_card_height: 205,
};

impl Quality {
    /// 认不出来的档位退回 1000w：配置写错了不该让卡片画不出来
    pub fn find(name: &str) -> Quality {
        match name.trim().to_ascii_lowercase().as_str() {
            "800w" => W800,
            "1200w" => W1200,
            "1500w" => W1500,
            _ => W1000,
        }
    }

    /// 卡片主体宽：`imageWidth - 2*cardMargin`
    pub fn card_width(&self) -> u32 {
        self.image_width - self.card_margin * 2
    }

    /// 关掉顶部徽章时这一档的高度归零（原插件就是把 badgeHeight 置 0，连带偏移一起收）
    pub fn badge_height_for(&self, enabled: bool) -> u32 {
        if enabled { self.badge_height } else { 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_quality_falls_back_to_1000w() {
        assert_eq!(Quality::find("999w"), Quality::find(""));
        assert_eq!(Quality::find(" 1000W "), W1000);
        assert_eq!(Quality::find("999w").image_width, 1000);
    }

    #[test]
    fn derived_widths_match_the_original_card() {
        // 原插件 1000w 下 cardRect.width=940、cardContentRect.width=880；
        // 内容宽就是 card.rs 排版时那句 `width - card_padding*2`，这里按同一式子核对
        assert_eq!(W1000.card_width(), 940);
        assert_eq!(W1000.card_width() - W1000.card_padding * 2, 880);
        assert_eq!(W800.card_width(), 760);
        assert_eq!(W800.card_width() - W800.card_padding * 2, 720);
    }

    #[test]
    fn every_step_grows_monotonically() {
        let steps = [W800, W1000, W1200, W1500];
        for pair in steps.windows(2) {
            assert!(pair[1].image_width > pair[0].image_width);
            assert!(pair[1].content_font > pair[0].content_font);
            assert!(pair[1].face_size >= pair[0].face_size);
            assert!(pair[1].badge_height > pair[0].badge_height);
        }
    }
}
