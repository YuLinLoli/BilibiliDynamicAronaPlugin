//! 模板渲染：把 `{name}` 这类占位符展开成一条条待发消息。
//!
//! 语义逐条抄自原插件 `tasker/SendTasker.kt`，三条路径各有口径：
//! * 动态（`buildMsg`）与直播（`buildLiveMsg`）：按 `\{([a-z]+)}` 扫描替换，
//!   所以键名只认小写字母——`{startTime}` 写在这里不会被识别，会原样留在文本里。
//! * 直播结束（`buildCloseMsg`）：不走正则，按固定的九个键做字面替换，键名可以带大写，
//!   产物只有**一条纯文本消息**（原插件那里既不拆多条也不支持合并转发）。
//! * 模板里字面 `\n` 与真换行等价（当换行），字面 `\r` 与真回车等价（当"拆成两条消息"的分隔）；
//!   `{>>}…{<<}` 之间的内容收成一张合并转发卡片。
//! * 认不出的小写键保留原插件那句 `[不支持的类型: xxx]`。

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use arona::runtime::message::{ForwardMessage, MessageSegment, OutgoingMessage};

use super::Ready;
use crate::bili::model::{Dynamic, LiveInfo, format_duration, format_time};
use crate::config::ForwardCard;

/// 占位符：左花括号 + 若干小写字母 + 右花括号
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{([a-z]+)}").unwrap());
/// 合并转发区间：`{>>}` 到 `{<<}`，跨行也算一段
static FORWARD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\{>>}(.*?)\{<<}").unwrap());

/// 文本类占位符的取值表。图片类占位符（`{draw}` `{images}` `{cover}`）不在这里，
/// 因为它们的产物是消息段而不是文本，见 [`Supply`]。
#[derive(Clone, Debug, Default)]
pub struct Vals(BTreeMap<String, String>);

impl Vals {
    pub(crate) fn fill<const N: usize>(items: [(&str, String); N]) -> Vals {
        let mut map = BTreeMap::new();
        for (key, value) in items {
            map.insert(key.to_string(), value);
        }
        Vals(map)
    }

    /// 测试推送用的那一份字段（`--test-notify` 走真实模板，只是内容是自己编的）
    pub(crate) fn test() -> Vals {
        let now = chrono::Utc::now().timestamp();
        Vals::fill([
            ("name", "bilibili-dynamic-plugin".to_string()),
            ("uid", "0".to_string()),
            ("did", "0".to_string()),
            ("time", format_time(now)),
            ("type", "动态".to_string()),
            (
                "content",
                "这是一条测试推送：能看到就说明模板解析与发送两条路都通了。".to_string(),
            ),
            ("link", "https://t.bilibili.com".to_string()),
            ("links", "https://t.bilibili.com".to_string()),
        ])
    }

    /// 一条动态能填的键。`link` 由调用方决定是原链还是短链（`push.toShortLink`）。
    pub fn dynamic(dynamic: &Dynamic, link: String) -> Vals {
        let links = if dynamic.links.is_empty() {
            dynamic.link()
        } else {
            dynamic
                .links
                .iter()
                .map(|item| item.url.clone())
                .collect::<Vec<String>>()
                .join("\n")
        };
        Vals::fill([
            ("name", dynamic.name.clone()),
            ("uid", dynamic.uid.to_string()),
            ("did", dynamic.id.to_string()),
            ("time", dynamic.time_text()),
            ("type", dynamic.type_text.clone()),
            // 判类词单独带一份：@全体 与类型过滤器认的是它，不是上面那个显示词
            ("kind", dynamic.kind.clone()),
            ("content", dynamic.display_text()),
            ("link", link),
            ("links", links),
        ])
    }

    /// 一次开播能填的键。`time` 是检测时刻，与原插件 `LiveMessage.time` 同口径。
    pub fn live(live: &LiveInfo, link: String, time: &str) -> Vals {
        Vals::fill([
            ("name", live.anchor.clone()),
            ("uid", live.uid.to_string()),
            ("rid", live.room_id.to_string()),
            ("time", time.to_string()),
            ("type", "直播".to_string()),
            ("title", live.title.clone()),
            ("area", live.area.clone()),
            ("link", link),
        ])
    }

    /// 直播结束的九个键（字面替换那一路，键名带大写）
    pub fn live_close(live: &LiveInfo, link: String, started_at: i64, ended_at: i64) -> Vals {
        let seconds = if started_at > 0 && ended_at > started_at {
            ended_at - started_at
        } else {
            0
        };
        Vals(BTreeMap::from([
            ("name".to_string(), live.anchor.clone()),
            ("uid".to_string(), live.uid.to_string()),
            ("rid".to_string(), live.room_id.to_string()),
            (
                "startTime".to_string(),
                if started_at > 0 {
                    format_time(started_at)
                } else {
                    String::new()
                },
            ),
            ("endTime".to_string(), format_time(ended_at)),
            ("duration".to_string(), format_duration(seconds)),
            ("title".to_string(), live.title.clone()),
            ("area".to_string(), live.area.clone()),
            ("link".to_string(), link),
        ]))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// 这条推送属于哪个订阅者（`{uid}` 那一格，@全体 的档位按它查）
    pub fn uid(&self) -> i64 {
        self.get("uid")
            .and_then(|raw| raw.parse::<i64>().ok())
            .unwrap_or(0)
    }

    /// 转发节点上的显示名与 QQ 号（原插件用动态作者那一组）
    fn author(&self) -> (String, i64) {
        let name = self.0.get("name").cloned().unwrap_or_default();
        let uin = self
            .0
            .get("uid")
            .and_then(|raw| raw.parse::<i64>().ok())
            .unwrap_or(0);
        (name, uin)
    }
}

/// 图片类占位符的供给。绘图与下载都在调用方做完，这里只把成品交出来。
/// 三份字节都是 [`Ready`]（已过出门前的编码归一），所以渲染一个目标只是引用计数加一，
/// 不再按目标各拷一份卡片、各编一遍附件。
#[derive(Default)]
pub struct Supply {
    /// `{draw}`：卡片 PNG。None 表示没出图，发 [`Supply::draw_missing`] 那句
    pub draw: Option<Ready>,
    /// 没出图时给用户的说明，照抄原插件的两条文案
    pub draw_missing: String,
    /// `{images}`：正文图，一张一个图片段
    pub images: Vec<Ready>,
    /// `{cover}`：直播封面。缺了按原插件那样替换成空串
    pub cover: Option<Ready>,
}

impl Supply {
    /// 动态卡片的供给（没有封面这一说）
    pub fn for_dynamic(draw: Option<Ready>, images: Vec<Ready>) -> Supply {
        Supply {
            draw_missing: if draw.is_none() {
                "[绘制动态失败]".to_string()
            } else {
                "[未找到绘制的动态]".to_string()
            },
            draw,
            images,
            cover: None,
        }
    }

    /// 直播卡片的供给
    pub fn for_live(draw: Option<Ready>, cover: Option<Ready>) -> Supply {
        Supply {
            draw_missing: if draw.is_none() {
                "[绘制直播图片失败]".to_string()
            } else {
                "[未找到绘制的直播图片]".to_string()
            },
            draw,
            images: Vec::new(),
            cover,
        }
    }
}

/// 动态/直播模板的渲染入口：返回按 `\r` 拆开的若干条消息（空的那几条已被丢掉）。
pub fn render(
    template: &str,
    vals: &Vals,
    supply: &Supply,
    card: &ForwardCard,
) -> Vec<OutgoingMessage> {
    let text = normalize(template);
    let mut out: Vec<OutgoingMessage> = Vec::new();
    let mut cursor = 0usize;
    for found in FORWARD.captures_iter(&text) {
        let whole = found.get(0).expect("整段匹配一定存在").range();
        if whole.start > cursor {
            push_runs(&mut out, &text[cursor..whole.start], vals, supply);
        }
        let mut nodes: Vec<ForwardMessage> = Vec::new();
        let mut inner: Vec<OutgoingMessage> = Vec::new();
        push_runs(&mut inner, &found[1], vals, supply);
        let (name, uin) = vals.author();
        for message in inner {
            nodes.push(ForwardMessage {
                name: name.clone(),
                uin,
                content: message.segments,
            });
        }
        if !nodes.is_empty() {
            out.push(OutgoingMessage::forward(plain(&card.title, vals), nodes));
        }
        cursor = whole.end;
    }
    if cursor < text.len() {
        push_runs(&mut out, &text[cursor..], vals, supply);
    }
    out
}

/// 直播结束模板：只做九个键的字面替换，产物一条纯文本消息。
pub fn render_close(template: &str, vals: &Vals) -> OutgoingMessage {
    let mut text = template.to_string();
    for (key, value) in vals.0.iter() {
        text = text.replace(&format!("{{{key}}}"), value);
    }
    OutgoingMessage::text(text)
}

/// 模板里只出文本时用这条（转发卡片的标题、预览）：图片占位按空处理。
pub fn plain(template: &str, vals: &Vals) -> String {
    let text = normalize(template);
    let mut out = String::new();
    // 逐段扫描：TAG 之间的原文照抄，认得的键换成值
    let mut cursor = 0usize;
    for found in TAG.captures_iter(&text) {
        let whole = found.get(0).expect("整段匹配一定存在").range();
        if whole.start > cursor {
            out.push_str(&text[cursor..whole.start]);
        }
        let key = &found[1];
        match vals.get(key) {
            Some(value) => out.push_str(value),
            None if matches!(key, "draw" | "images" | "cover") => {}
            None => out.push_str(&format!("[不支持的类型: {key}]")),
        }
        cursor = whole.end;
    }
    if cursor < text.len() {
        out.push_str(&text[cursor..]);
    }
    out.replace("\\n", "\n")
}

/// 真换行/真回车统一成字面转义，之后所有判断都只认字面量（原插件先做这两次 replace）
fn normalize(template: &str) -> String {
    template.replace('\n', "\\n").replace('\r', "\\r")
}

/// 这一档模板用不用得上那张卡：串里有没有 `{draw}` 这个占位符。
///
/// 走的是与渲染**同一个分词**（[`TAG`]），不是 `contains("{draw}")`——后者会把
/// `\{draw}` 这类没被认成占位符的字面量也算进去，判成"要卡"就多画一张没人收的图。
pub fn uses_draw(template: &str) -> bool {
    let text = normalize(template);
    TAG.captures_iter(&text).any(|found| &found[1] == "draw")
}

/// 一段模板按字面 `\r` 拆成若干条消息
fn push_runs(out: &mut Vec<OutgoingMessage>, chunk: &str, vals: &Vals, supply: &Supply) {
    for run in chunk.split("\\r") {
        let segments = expand(run, vals, supply);
        if !segments.is_empty() {
            out.push(OutgoingMessage::new(segments));
        }
    }
}

/// 一条消息的段序列：文本与图片按模板里出现的先后排
fn expand(run: &str, vals: &Vals, supply: &Supply) -> Vec<MessageSegment> {
    let mut segments: Vec<MessageSegment> = Vec::new();
    let mut pending = String::new();
    let mut cursor = 0usize;
    for found in TAG.captures_iter(run) {
        let whole = found.get(0).expect("整段匹配一定存在").range();
        if whole.start > cursor {
            pending.push_str(&run[cursor..whole.start]);
        }
        let key = &found[1];
        match key {
            "draw" => match &supply.draw {
                Some(bytes) => {
                    flush(&mut segments, &mut pending);
                    segments.push(image(bytes));
                }
                None => pending.push_str(&supply.draw_missing),
            },
            "images" => {
                flush(&mut segments, &mut pending);
                for bytes in &supply.images {
                    segments.push(image(bytes));
                }
            }
            "cover" => {
                if let Some(bytes) = &supply.cover {
                    flush(&mut segments, &mut pending);
                    segments.push(image(bytes));
                }
            }
            _ => match vals.get(key) {
                Some(value) => pending.push_str(value),
                None => pending.push_str(&format!("[不支持的类型: {key}]")),
            },
        }
        cursor = whole.end;
    }
    if cursor < run.len() {
        pending.push_str(&run[cursor..]);
    }
    flush(&mut segments, &mut pending);
    segments
}

/// 攒下的文本变成一段（字面 `\n` 还原成换行；空白串丢掉）
fn flush(segments: &mut Vec<MessageSegment>, pending: &mut String) {
    if pending.trim().is_empty() {
        pending.clear();
        return;
    }
    segments.push(MessageSegment::Text(pending.replace("\\n", "\n")));
    pending.clear();
}

fn image(bytes: &Ready) -> MessageSegment {
    MessageSegment::Image {
        url: None,
        file: None,
        // 框架的出口要的是自有字节，所以这里拷一次；除此之外全程共享
        data: Some(bytes.to_vec()),
        file_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals() -> Vals {
        Vals(BTreeMap::from([
            ("name".to_string(), "测试Up".to_string()),
            ("uid".to_string(), "123".to_string()),
            ("content".to_string(), "正文第一行\n第二行".to_string()),
            ("link".to_string(), "https://t.bilibili.com/1".to_string()),
        ]))
    }

    fn card() -> ForwardCard {
        ForwardCard::default()
    }

    /// 出门用的字节：夹具给的是原始编码，这里过一道 [`Ready`] 那道门（12 个 0 谁都解不开，
    /// 归一那一步会原样交回来）
    fn bytes() -> Ready {
        Ready::of(vec![0u8; 12], None)
    }

    fn text_of(message: &OutgoingMessage) -> String {
        message
            .segments
            .iter()
            .filter_map(|segment| match segment {
                MessageSegment::Text(value) => Some(value.clone()),
                _ => None,
            })
            .collect::<Vec<String>>()
            .join("|")
    }

    fn images_of(message: &OutgoingMessage) -> usize {
        message
            .segments
            .iter()
            .filter(|segment| matches!(segment, MessageSegment::Image { .. }))
            .count()
    }

    /// 第一个图片段里的字节
    fn first_image(message: &OutgoingMessage) -> Vec<u8> {
        message
            .segments
            .iter()
            .find_map(|segment| match segment {
                MessageSegment::Image { data, .. } => data.clone(),
                _ => None,
            })
            .expect("这条消息里没有图片段")
    }

    /// `{images}` 这一支确实过了转码那道门，两支各钉一头。口径 2026-10-05 从「一律 PNG」改成
    /// 「JPEG 原件直发、只有 WebP 这类才转 PNG」——原先这条钉的是 PNG 那头，翻不过来就会把
    /// 单条消息 73MB 那一档请回来。
    #[test]
    fn the_image_supply_passes_the_transcode_gate() {
        fn encoded(format: image::ImageFormat) -> Vec<u8> {
            let mut out = std::io::Cursor::new(Vec::new());
            image::ImageBuffer::from_pixel(3, 2, image::Rgb([10u8, 120, 240]))
                .write_to(&mut out, format)
                .expect("样例编不出来");
            out.into_inner()
        }
        let jpeg = encoded(image::ImageFormat::Jpeg);
        let got = render(
            "{images}",
            &vals(),
            &Supply::for_dynamic(None, vec![Ready::of(jpeg.clone(), None)]),
            &card(),
        );
        assert_eq!(
            first_image(&got[0]),
            jpeg,
            "JPEG 被重编码了——无损转 PNG 能把它撑到约 2.4 倍"
        );

        let webp = encoded(image::ImageFormat::WebP);
        let got = render(
            "{images}",
            &vals(),
            &Supply::for_dynamic(None, vec![Ready::of(webp, None)]),
            &card(),
        );
        assert!(
            first_image(&got[0]).starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
            "WebP 没转成 PNG，交出去的编码又在跟着图床的运气走"
        );
    }

    #[test]
    fn tags_become_text_and_image_segments() {
        let supply = Supply::for_dynamic(Some(bytes()), vec![bytes(), bytes()]);
        let got = render(
            "{name} 更新了{type}\n{content}\n{images}\n{link}",
            &vals(),
            &supply,
            &card(),
        );
        assert_eq!(got.len(), 1, "整串没有 \\r，只该出一条");
        assert_eq!(images_of(&got[0]), 2, "正文图两张该各成一个段");
        let text = text_of(&got[0]);
        assert!(text.starts_with("测试Up 更新了"), "{text}");
        assert!(text.contains("正文第一行\n第二行"), "{text}");
        assert!(text.ends_with("https://t.bilibili.com/1"), "{text}");
    }

    #[test]
    fn literal_backslash_r_splits_into_several_messages() {
        let supply = Supply {
            draw: Some(bytes()),
            draw_missing: String::new(),
            images: Vec::new(),
            cover: None,
        };
        let got = render("{draw}\\r{content}", &vals(), &supply, &card());
        assert_eq!(got.len(), 2);
        assert_eq!(images_of(&got[0]), 1);
        assert_eq!(text_of(&got[1]), "正文第一行\n第二行");
    }

    #[test]
    fn real_newline_and_carriage_return_mean_the_same_as_the_escapes() {
        let supply = Supply::for_dynamic(Some(bytes()), Vec::new());
        let got = render("{draw}\n{content}\r{name}", &vals(), &supply, &card());
        assert_eq!(got.len(), 2, "真回车与字面 \\r 一样拆条");
        assert_eq!(images_of(&got[0]), 1);
        assert!(text_of(&got[0]).contains("正文第一行\n第二行"));
        assert_eq!(text_of(&got[1]), "测试Up");
    }

    /// 默认档必须**只渲染出一条消息**。2026-10-09 之前默认指着自创的 `SimpleMsg`
    /// （`{draw}\r{content}\r{images}`），一条动态被摊成三条——可卡片里本来就画了正文和配图，
    /// 后两条纯属重复。第一次拿真数据推动态时被用户当场看出来。
    /// 串的内容由 `config.rs` 那条逐键逐字面量的单测钉住，这条钉的是"默认档不拆条、不重复发图"。
    #[test]
    fn the_default_templates_render_exactly_one_message() {
        use crate::config::TemplateConfig;
        let config = TemplateConfig::default();

        let dynamic = config.dynamic_push[&config.default_dynamic_push].clone();
        let supply = Supply::for_dynamic(Some(bytes()), vec![bytes(), bytes()]);
        let got = render(&dynamic, &vals(), &supply, &card());
        assert_eq!(got.len(), 1, "默认动态档不该拆条，串是 {dynamic:?}");
        assert_eq!(
            images_of(&got[0]),
            1,
            "那一条里只该有卡片这一张图，配图不该再发一遍，串是 {dynamic:?}"
        );

        let live = config.live_push[&config.default_live_push].clone();
        let supply = Supply::for_live(Some(bytes()), Some(bytes()));
        let got = render(&live, &vals(), &supply, &card());
        assert_eq!(got.len(), 1, "默认开播档不该拆条，串是 {live:?}");
    }

    #[test]
    fn forward_block_becomes_one_card_with_title_rendered() {
        let supply = Supply::for_dynamic(Some(bytes()), Vec::new());
        let template = "{draw}{>>}作者：{name}\nUID：{uid}{<<}\\r{content}";
        let got = render(template, &vals(), &supply, &card());
        assert_eq!(got.len(), 3, "卡片、转发、正文各一条");
        let MessageSegment::Forward {
            title, messages, ..
        } = &got[1].segments[0]
        else {
            panic!("第二条该是合并转发，实际 {:?}", got[1].segments);
        };
        assert_eq!(title, "测试Up 的动态");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].name, "测试Up");
        assert_eq!(messages[0].uin, 123);
        assert!(text_of(&OutgoingMessage::new(messages[0].content.clone())).contains("UID：123"));
    }

    #[test]
    fn unknown_tag_keeps_the_original_failure_text() {
        let got = render("前面{wat}后面", &vals(), &Supply::default(), &card());
        assert_eq!(text_of(&got[0]), "前面[不支持的类型: wat]后面");
    }

    #[test]
    fn uppercase_tag_is_left_alone_because_the_original_regex_is_a_z_only() {
        let got = render("开播：{startTime}", &vals(), &Supply::default(), &card());
        assert_eq!(text_of(&got[0]), "开播：{startTime}");
    }

    /// 画不画卡只看这一档模板里有没有 `{draw}`。四条边界：
    /// 纯文本档不算数、转发块里的那一个**算**数（块内走的是同一条 `push_runs`，
    /// 带 supply，所以卡会被塞进转发节点里）、大写 `{Draw}` 不算（渲染端根本不认它）、
    /// 真换行与字面换行两种写法都要认（判据与渲染同一个分词，差在这里）。
    #[test]
    fn only_a_template_that_asks_for_the_card_gets_one() {
        assert!(uses_draw("{draw}\\r{content}"));
        assert!(uses_draw("{content}\n{draw}"));
        assert!(uses_draw("{>>}作者：{name}\\r{draw}{<<}"));
        assert!(!uses_draw("{name} 更新了：{content}\\r{images}"));
        assert!(!uses_draw("开播：{startTime}，看 {Draw}"));
        assert!(!uses_draw(""));
    }

    /// 判据说"要卡"的串，渲染出来必须真带一个图片段——两边不一致就是判据在骗人，
    /// 会出现"替没人要卡的画了一张"或"该画的没画"
    #[test]
    fn the_draw_verdict_matches_what_render_actually_emits() {
        let with = "{draw}\\r{content}";
        let without = "{name} 更新了：{content}";
        assert!(uses_draw(with) != uses_draw(without));
        let supply = Supply {
            draw: Some(bytes()),
            draw_missing: String::new(),
            images: Vec::new(),
            cover: None,
        };
        let got = render(with, &vals(), &supply, &card());
        let drawn: usize = got.iter().map(images_of).sum();
        assert_eq!(drawn, 1, "判据说要卡，渲染却没把卡片段发出去");
        let got = render(without, &vals(), &supply, &card());
        let drawn: usize = got.iter().map(images_of).sum();
        assert_eq!(drawn, 0, "判据说要不到卡，渲染却塞了图片段");
    }

    #[test]
    fn a_missing_draw_sends_the_failure_wording() {
        let got = render(
            "{draw}",
            &vals(),
            &Supply::for_dynamic(None, Vec::new()),
            &card(),
        );
        assert_eq!(text_of(&got[0]), "[绘制动态失败]");
        let live = Supply {
            draw: Some(bytes()),
            draw_missing: "[未找到绘制的直播图片]".to_string(),
            images: Vec::new(),
            cover: None,
        };
        let got = render("{cover}", &vals(), &live, &card());
        assert!(
            got.is_empty(),
            "封面缺位时原插件替换成空串，空消息不该发出去"
        );
    }

    #[test]
    fn close_template_substitutes_the_nine_keys_only() {
        let live = LiveInfo {
            uid: 123,
            room_id: 456,
            anchor: "测试Up".to_string(),
            title: "标题".to_string(),
            area: "分区".to_string(),
            ..Default::default()
        };
        let vals = Vals::live_close(&live, "https://live.bilibili.com/456".to_string(), 0, 0);
        let got = render_close(
            "{name} 的直播已结束\n房间号：{rid}\n时长：{duration}\n{unsupported}",
            &vals,
        );
        // 字面替换那一路不认得 {unsupported}，也不做拆条——与原插件 buildCloseMsg 一致
        // （MessageSegment 来自框架 crate、没有 PartialEq，所以逐段比文本）
        assert_eq!(got.segments.len(), 1);
        let MessageSegment::Text(text) = &got.segments[0] else {
            panic!("结束通知该就一条文本段");
        };
        assert_eq!(
            text,
            "测试Up 的直播已结束\n房间号：456\n时长：0:00\n{unsupported}"
        );
    }
}
