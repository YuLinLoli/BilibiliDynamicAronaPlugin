//! 绘图主题与配色生成。
//!
//! 三套主题逐字抄自原插件 `data/DynamicImageTheme.kt`；注意 `nameColor` 是粉色 `#FB7299`（作者昵称），
//! `titleColor` 才是深灰 `#313131`，别按字面理解成"标题用品牌色"。
//!
//! 整图背景那层渐变由 `defaultColor` 生成：只给一个色时转 HSB、锁死 S/B，再以色相 ± 步进扩成三段；
//! 给了多个色就逐个原样用（只锁 S/B），不插值。这套规则与原插件一致，用户照老配置迁过来观感不变。

use super::canvas::Color;

/// 卡片会用到的整套颜色
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub id: &'static str,
    pub card_bg: Color,
    /// 描边色串（`;` 分隔），原图是扫掠渐变，这里按线性渐变取用
    pub card_outline: Vec<Color>,
    pub face_outline: Color,
    pub draw_outline: Color,
    /// 作者昵称色
    pub nickname: Color,
    pub title: Color,
    pub sub_title: Color,
    pub desc: Color,
    pub content: Color,
    pub link: Color,
    pub footer: Color,
    pub card_shadow: Color,
    pub small_card_shadow: Color,
    pub main_left_badge: (Color, Color),
    pub main_right_badge: (Color, Color),
    pub sub_left_badge: (Color, Color),
    pub sub_right_badge: (Color, Color),
}

fn c(hex: &str) -> Color {
    Color::parse(hex).unwrap_or_else(|| Color::rgb(0xFF, 0xFF, 0xFF))
}

impl Theme {
    /// 认不出来的主题退回 v3
    pub fn find(name: &str) -> Theme {
        match name.trim() {
            "v2" => v2(),
            "v3RainbowOutline" => rainbow(),
            _ => v3(),
        }
    }

    /// 渐变描边的取色：串里只有一个色时就一直是那个色
    pub fn outline_at(&self, t: f32) -> Color {
        match self.card_outline.len() {
            0 => Color::rgb(255, 255, 255),
            1 => self.card_outline[0],
            count => {
                let position = t.clamp(0.0, 1.0) * (count - 1) as f32;
                let index = position.floor() as usize;
                let next = (index + 1).min(count - 1);
                self.card_outline[index].mix(self.card_outline[next], position - index as f32)
            }
        }
    }
}

fn v3() -> Theme {
    Theme {
        id: "v3",
        card_bg: c("#b4ffffff"),
        card_outline: vec![c("#ffffff")],
        face_outline: c("#a0ffffff"),
        draw_outline: c("#ffffff"),
        nickname: c("#fb7299"),
        title: c("#313131"),
        sub_title: c("#9c9c9c"),
        desc: c("#666666"),
        content: c("#222222"),
        link: c("#178bcf"),
        footer: c("#9c9c9c"),
        card_shadow: c("#46000000"),
        small_card_shadow: c("#1e000000"),
        main_left_badge: (c("#00cbff"), c("#b4ffffff")),
        main_right_badge: (c("#ffffff"), c("#48c7f0")),
        sub_left_badge: (c("#ffffff"), c("#fb7299")),
        sub_right_badge: (c("#ffffff"), c("#48c7f0")),
    }
}

fn v2() -> Theme {
    Theme {
        id: "v2",
        card_bg: c("#c8ffffff"),
        card_outline: vec![c("#ffffff")],
        face_outline: c("#a0ffffff"),
        draw_outline: c("#ffffff"),
        nickname: c("#fb7299"),
        title: c("#313131"),
        sub_title: c("#9c9c9c"),
        desc: c("#666666"),
        content: c("#222222"),
        link: c("#178bcf"),
        footer: c("#9c9c9c"),
        card_shadow: c("#00000000"),
        small_card_shadow: c("#00000000"),
        main_left_badge: (c("#00cbff"), c("#c8ffffff")),
        main_right_badge: (c("#ffffff"), c("#48c7f0")),
        sub_left_badge: (c("#ffffff"), c("#fb7299")),
        sub_right_badge: (c("#ffffff"), c("#48c7f0")),
    }
}

fn rainbow() -> Theme {
    let mut theme = v3();
    theme.id = "v3RainbowOutline";
    theme.card_outline = [
        "#ff0000", "#ff00ff", "#0000ff", "#00ffff", "#00ff00", "#ffff00", "#ff0000",
    ]
    .iter()
    .map(|hex| c(hex))
    .collect();
    theme
}

/// 整图背景：由 `defaultColor` 那串生成 1~N 个停靠点
pub fn background(
    configured: &str,
    hue_step: f32,
    lock_sb: bool,
    saturation: f32,
    brightness: f32,
) -> Vec<Color> {
    let normalized = configured.replace('；', ";");
    let parts: Vec<&str> = normalized
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let parsed: Vec<Color> = parts.iter().filter_map(|part| Color::parse(part)).collect();
    if parsed.is_empty() {
        return vec![Color::rgb(211, 237, 250), Color::rgb(253, 232, 237)];
    }
    if parsed.len() > 1 {
        return parsed
            .iter()
            .map(|color| lock_color(*color, lock_sb, saturation, brightness))
            .collect();
    }
    // 单色扩三段：色相 +步进、原值、-步进，S/B 按配置锁死
    let one = lock_color(parsed[0], lock_sb, saturation, brightness);
    let (hue, sat, val) = to_hsb(one);
    [hue + hue_step * 2.0, hue + hue_step, hue]
        .into_iter()
        .map(|shift| {
            let mut color = Color::from_hsb(shift.rem_euclid(360.0), sat, val);
            color.0[3] = one.0[3];
            color
        })
        .collect()
}

fn lock_color(color: Color, lock_sb: bool, saturation: f32, brightness: f32) -> Color {
    if !lock_sb {
        return color;
    }
    let (hue, _, _) = to_hsb(color);
    let mut locked = Color::from_hsb(hue, saturation, brightness);
    locked.0[3] = color.0[3];
    locked
}

/// RGB → (色相 0..360, 饱和 0..1, 明度 0..1)
pub fn to_hsb(color: Color) -> (f32, f32, f32) {
    let r = color.0[0] as f32 / 255.0;
    let g = color.0[1] as f32 / 255.0;
    let b = color.0[2] as f32 / 255.0;
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let delta = max - min;
    let hue = if delta <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let saturation = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    (hue.rem_euclid(360.0), saturation, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_theme_is_v3() {
        assert_eq!(Theme::find("v9").id, "v3");
        assert_eq!(Theme::find("").id, "v3");
        assert_eq!(Theme::find("v2").card_bg, c("#c8ffffff"));
    }

    #[test]
    fn nickname_is_pink_not_title_color() {
        // 昵称 #FB7299、标题 #313131：这两个字段名最容易记反
        assert_eq!(Theme::find("v3").nickname, Color::rgb(0xFB, 0x72, 0x99));
        assert_eq!(Theme::find("v3").title, Color::rgb(0x31, 0x31, 0x31));
    }

    #[test]
    fn rainbow_outline_has_seven_stops() {
        let theme = Theme::find("v3RainbowOutline");
        assert_eq!(theme.card_outline.len(), 7);
        assert_eq!(theme.outline_at(0.0), Color::rgb(255, 0, 0));
        assert_eq!(theme.outline_at(1.0), Color::rgb(255, 0, 0));
    }

    #[test]
    fn single_color_grows_to_three_hue_steps() {
        let stops = background("#d3edfa", 30.0, true, 0.25, 1.0);
        assert_eq!(stops.len(), 3);
        // 锁了 S/B 之后三段都该是同一饱和同一明度，只有色相在走
        for stop in &stops {
            let (_, sat, val) = to_hsb(*stop);
            assert!((sat - 0.25).abs() < 0.02, "饱和度没锁住：{stop:?}");
            assert!(val > 0.99, "明度没锁住：{stop:?}");
        }
        assert_ne!(stops[0], stops[2]);
    }

    #[test]
    fn multiple_colors_are_used_as_given() {
        let stops = background("#112233;#445566", 30.0, false, 0.25, 1.0);
        assert_eq!(stops.len(), 2);
        assert_eq!(stops[0], Color::rgb(0x11, 0x22, 0x33));
        // 全角分号也得认：中文输入法下打出来的配置是这个字符
        assert_eq!(
            background("#112233；#445566", 30.0, false, 0.25, 1.0),
            stops
        );
    }

    #[test]
    fn hsb_roundtrip_keeps_hue() {
        let blue = Color::rgb(0x00, 0xCB, 0xFF);
        let (hue, _, _) = to_hsb(blue);
        assert!(
            hue > 190.0 && hue < 200.0,
            "青蓝的色相应在 195 上下，实得 {hue}"
        );
    }
}
