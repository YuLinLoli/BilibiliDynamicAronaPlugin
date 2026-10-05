//! 推送：模板取哪一套、发给谁、@全体 怎么挂、条与条之间怎么节流。
//!
//! 渲染本身在 [`template`]（纯函数，可单测）。这一层只管"对着一个目标把一条动态发出去"：
//! * 模板名先看目标自己的覆盖（`/bili template` 写进 `store.yml` 的那张表），
//!   没覆盖才用配置里的默认模板名；两张表都查不到时退回内置兜底串，不至于静默不推。
//! * 群键 `g<号>` / 好友键 `f<号>` 还原成框架的 [`MessageTarget`]，发送走
//!   [`arona::bot::Bot::send`]（它绑的是本插件所属的那套框架实例）。
//! * @全体 有两种挂法，由 `bili.push.atAllPlus` 决定：默认单独发一条，
//!   写 `PLUS_END` 就挂在最后一条消息尾部。直播结束通知不 @全体。
//! * 每条消息之间歇 `messageInterval` 毫秒，目标之间歇 `pushInterval` 毫秒——
//!   原插件是队列消费，这里等价地在推送循环里 sleep。

pub(crate) mod template;

use std::io::Cursor;
use std::time::Duration;

use arona::bot::Bot;
use arona::plugin::PluginContext;
use arona::runtime::message::{MessageSegment, MessageTarget, OutgoingMessage};
use image::ImageFormat;

use crate::config::{BiliConfig, ForwardCard, SECTION, TEMPLATE_SECTION, TemplateConfig};
use crate::store::{self, TargetState};
use template::Vals;

/// 推的是哪一类东西：决定用哪张模板表、@全体 认哪个种类名
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dynamic,
    Live,
    LiveClose,
}

impl Kind {
    /// @全体 那张表里的种类名
    pub fn label(self) -> &'static str {
        match self {
            Kind::Dynamic => "动态",
            Kind::Live | Kind::LiveClose => "直播",
        }
    }

    /// `TargetState.templates` 里覆盖用的键
    fn template_key(self) -> &'static str {
        match self {
            Kind::Dynamic => "dynamic",
            Kind::Live => "live",
            Kind::LiveClose => "liveClose",
        }
    }

    /// 查不到任何模板时用的兜底串（等价于原插件的 SimpleMsg）
    fn fallback(self) -> &'static str {
        match self {
            Kind::Dynamic => "{draw}\r{content}\r{images}",
            Kind::Live => "{draw}\r{name} 正在直播：{title}\n{link}",
            Kind::LiveClose => "{name} 的直播已结束\n房间号：{rid}\n{link}",
        }
    }

    /// 直播结束那一档只走字面替换：不拆多条、不支持合并转发、也不 @全体
    fn expandable(self) -> bool {
        self != Kind::LiveClose
    }
}

/// 一次推送要的全部材料：一类东西 + 渲染用的字段与图片
pub struct Post {
    pub kind: Kind,
    pub vals: Vals,
    pub supply: template::Supply,
}

impl Post {
    pub fn dynamic(vals: Vals, draw: Option<Vec<u8>>, images: Vec<Vec<u8>>) -> Post {
        Post {
            kind: Kind::Dynamic,
            vals,
            supply: template::Supply::for_dynamic(draw, images),
        }
    }

    pub fn live(vals: Vals, draw: Option<Vec<u8>>, cover: Option<Vec<u8>>) -> Post {
        Post {
            kind: Kind::Live,
            vals,
            supply: template::Supply::for_live(draw, cover),
        }
    }

    pub fn live_close(vals: Vals) -> Post {
        Post {
            kind: Kind::LiveClose,
            vals,
            supply: template::Supply::default(),
        }
    }

    /// @全体 判类要的两样东西：订阅者 uid 与内容种类。
    /// 种类取模板里的 `{type}`（动态可能是"视频/音乐/专栏/转发动态"），
    /// 因为 @全体 的档位是按具体内容匹配的，不是按"这是动态还是直播"。
    pub fn at_all_args(&self) -> (i64, &str) {
        (
            self.vals.uid(),
            self.vals.get("type").unwrap_or_else(|| self.kind.label()),
        )
    }
}

/// 这个目标该用的模板串。返回 (模板名, 模板串)，名字只为出问题时好写日志。
fn pick_template(ctx: &PluginContext, state: &TargetState, kind: Kind) -> (String, String) {
    let config = ctx.config::<TemplateConfig>(TEMPLATE_SECTION).get();
    let table = match kind {
        Kind::Dynamic => &config.dynamic_push,
        Kind::Live => &config.live_push,
        Kind::LiveClose => &config.live_close,
    };
    let default_name = match kind {
        Kind::Dynamic => config.default_dynamic_push.clone(),
        Kind::Live => config.default_live_push.clone(),
        Kind::LiveClose => config.default_live_close.clone(),
    };
    let name = state
        .template(kind.template_key())
        .map(str::to_string)
        .unwrap_or(default_name.clone());
    let text = table
        .get(&name)
        .or_else(|| table.get(default_name.as_str()))
        .cloned()
        .unwrap_or_else(|| kind.fallback().to_string());
    (name, text)
}

/// 目标键 → 发送地址
fn target_of(key: &str) -> Option<MessageTarget> {
    let (is_group, id) = store::parse_key(key)?;
    Some(if is_group {
        MessageTarget::Group(id)
    } else {
        MessageTarget::Private(id)
    })
}

/// 转发卡片的外观。框架的转发出口只有 title 一个通道，
/// brief/preview/summary 三项取不到处安放（详见 README 的已知差别）。
fn forward_card(ctx: &PluginContext) -> ForwardCard {
    ctx.config::<TemplateConfig>(TEMPLATE_SECTION)
        .get()
        .forward_card
}

/// 这条推送在这个目标上渲染成哪几条消息（只到渲染为止，不涉及发送与 @全体）
fn render_all(ctx: &PluginContext, state: &TargetState, post: &Post) -> Vec<OutgoingMessage> {
    let (name, source) = pick_template(ctx, state, post.kind);
    let messages = if post.kind.expandable() {
        template::render(&source, &post.vals, &post.supply, &forward_card(ctx))
    } else {
        vec![template::render_close(&source, &post.vals)]
    };
    if messages.is_empty() {
        arona::runtime::log::warning(format!(
            "bilibili：目标「{name}」那一档模板渲染结果是空的，这条没推"
        ));
    }
    messages
}

/// 把一次推送发给一个目标，返回实际发出去的消息条数
pub async fn push_state(ctx: &PluginContext, key: &str, state: &TargetState, post: &Post) -> usize {
    if state.muted {
        return 0;
    }
    let Some(target) = target_of(key) else {
        arona::runtime::log::warning(format!("bilibili：目标键「{key}」认不出是群还是好友，跳过"));
        return 0;
    };
    let push = ctx.config::<BiliConfig>(SECTION).get().push;
    let bot = Bot::of(ctx.framework());
    let mut messages = render_all(ctx, state, post);
    if messages.is_empty() {
        return 0;
    }
    let (uid, kind) = post.at_all_args();
    // @全体 只在群里挂：私聊没有"全体"可 @，挂上去整条会被实现端拒掉
    let at_all = post.kind.expandable() && target.group().is_some() && state.at_all_for(uid, kind);
    // 最后一条是合并转发时不能往后拼段（mirai 那条 `[转发消息]` 的特判）
    let appendable = !matches!(
        messages.last().map(|message| &message.segments),
        Some(segments) if segments.iter().any(|segment| matches!(segment, MessageSegment::Forward { .. }))
    );
    if at_all && !push.at_all_is_single() && appendable {
        let last = messages.len() - 1;
        messages[last].segments.push(MessageSegment::AtAll);
    }
    let mut sent = 0usize;
    if at_all && push.at_all_is_single() {
        bot.send(target, OutgoingMessage::at_all()).await;
        sent += 1;
        pause(push.message_interval).await;
    }
    for message in messages {
        bot.send(target, message).await;
        sent += 1;
        pause(push.message_interval).await;
    }
    sent
}

/// 命令侧的"看一眼"：按这个目标当前生效的模板把渲染结果发出去。
/// 与真实推送同一条渲染路径，只是不看 `muted`、也不挂 @全体——预览是用户主动要的。
pub async fn send_preview(ctx: &PluginContext, key: &str, post: &Post) -> usize {
    let Some(target) = target_of(key) else {
        arona::runtime::log::warning(format!("bilibili：目标键「{key}」认不出是群还是好友"));
        return 0;
    };
    let state = crate::store()
        .read(|data| data.targets.get(key).cloned())
        .unwrap_or_default();
    let messages = render_all(ctx, &state, post);
    let push = ctx.config::<BiliConfig>(SECTION).get().push;
    let bot = Bot::of(ctx.framework());
    let mut sent = 0usize;
    for message in messages {
        bot.send(target, message).await;
        sent += 1;
        pause(push.message_interval).await;
    }
    sent
}

/// 节流用的 sleep：毫秒为 0 时直接跳过，免得白挂起一次
async fn pause(millis: u64) {
    if millis > 0 {
        tokio::time::sleep(Duration::from_millis(millis)).await;
    }
}

/// `--test-notify` 的落点：给每个已登记的目标发一条测试消息。
/// 走的是和真实推送完全相同的路径（模板解析、渲染、发送），只是不绘图——
/// 绘图要下素材、要 cookie，测试推送不该被这些拖住。
pub async fn selftest(ctx: &PluginContext) {
    let keys = crate::store().read(|data| data.targets.keys().cloned().collect::<Vec<String>>());
    if keys.is_empty() {
        arona::runtime::log::warning("bilibili：测试推送没有可发的目标（还没订阅过任何群或好友）");
        return;
    }
    let vals = Vals::test();
    for key in keys {
        let state = crate::store().read(|data| data.targets.get(&key).cloned());
        let Some(state) = state else { continue };
        // draw 给 None、失败文案换成一句人话：测试推送不绘图，别让用户以为绘图坏了
        let supply = template::Supply {
            draw_missing: "（测试推送，不含卡片）".to_string(),
            ..Default::default()
        };
        let post = Post {
            kind: Kind::Dynamic,
            vals: vals.clone(),
            supply,
        };
        push_state(ctx, &key, &state, &post).await;
    }
}

/// PNG 的文件头，用来认出「已经是 PNG，不必再动它」
const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// JPEG 的文件头。只比这三字节：紧随其后那一段是段标记，各家写法不一样。
const JPEG_MAGIC: &[u8] = &[0xff, 0xd8, 0xff];

/// 一张附件最多交出去这么多字节，超了才压质量（2026-10-05 定的这条线）。
const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// 交出去的附件按编码分四支（2026-10-05 从「一律先转 PNG」收窄）：
/// * PNG 原样交——自己画的卡片都在这一支，重编一遍只是白烧 CPU。
/// * **JPEG 原件直发**：无损重编成 PNG 能把它撑到约 2.4 倍，那正是单条消息 73MB
///   打崩对端那一轮的主因之一，而对端本来就认 JPEG。
/// * WebP 这类实现端不认的才转成 PNG（`cache.downloadOriginal=false` 时先要 `…@720w.webp`，
///   编码不能跟着运气走）。
/// * 解不开的原样交：动图保住（见 `a_gif_stays_untouched_because_there_is_no_gif_decoder`），
///   而解不开也就压不动，所以超大的 GIF 仍然照发——宁可大，也不能让整条推送少一张图。
///
/// 前三支里任何一支只要超过 [`MAX_ATTACHMENT_BYTES`] 都再走一次 [`cap_to`]。
pub(crate) fn attachment(bytes: Vec<u8>) -> Vec<u8> {
    if bytes.starts_with(PNG_MAGIC) || bytes.starts_with(JPEG_MAGIC) {
        return cap_to(bytes, MAX_ATTACHMENT_BYTES);
    }
    match image::load_from_memory(&bytes) {
        Ok(decoded) => {
            let mut out = Cursor::new(Vec::new());
            if decoded.write_to(&mut out, ImageFormat::Png).is_err() {
                return cap_to(bytes, MAX_ATTACHMENT_BYTES);
            }
            cap_to(out.into_inner(), MAX_ATTACHMENT_BYTES)
        }
        Err(_) => bytes,
    }
}

/// 把超线的图压到 `limit` 以内：先只降 JPEG 质量（尺寸不动），两档都压不住才连像素降到 0.6 再编一次。
///
/// 压不住就原样交回去，绝不把图弄丢；也绝不让它比进来时更大。转 JPEG 会去掉透明通道——
/// 那是一张 10MB 以上的图能不能发出去的取舍，不在常规路径上。
/// `limit` 收参数是为了让单测拿一个小阈值真把这一支触发到（造一份 10MB 的图当夹具太贵）。
fn cap_to(bytes: Vec<u8>, limit: usize) -> Vec<u8> {
    if bytes.len() <= limit {
        return bytes;
    }
    let Ok(decoded) = image::load_from_memory(&bytes) else {
        return bytes;
    };
    let rgb = decoded.to_rgb8();
    for quality in [80, 60] {
        if let Some(less) = to_jpeg(&rgb, quality)
            && less.len() <= limit
        {
            return less;
        }
    }
    let (w, h) = (rgb.width(), rgb.height());
    let shrunk = image::imageops::resize(
        &rgb,
        ((w as f64 * 0.6).round() as u32).max(1),
        ((h as f64 * 0.6).round() as u32).max(1),
        image::imageops::FilterType::Triangle,
    );
    match to_jpeg(&shrunk, 75) {
        Some(less) if less.len() < bytes.len() => less,
        _ => bytes,
    }
}

/// 编一段 JPEG。质量是这里唯一的旋钮，所以直接交 `JpegEncoder` 而不是 `write_to`（后者不给质量）。
fn to_jpeg(img: &image::RgbImage, quality: u8) -> Option<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(img)
        .ok()?;
    Some(out.into_inner())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{ImageBuffer, ImageFormat, Rgb, Rgba, RgbaImage};

    use super::{JPEG_MAGIC, MAX_ATTACHMENT_BYTES, PNG_MAGIC, attachment, cap_to};

    fn swatch() -> RgbaImage {
        ImageBuffer::from_pixel(3, 2, Rgba([10, 120, 240, 255]))
    }

    /// JPEG 编码器不认 RGBA，样例得按目标格式挑颜色
    fn encoded(format: ImageFormat) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        let done = match format {
            ImageFormat::Jpeg => {
                ImageBuffer::from_pixel(3, 2, Rgb([10u8, 120, 240])).write_to(&mut out, format)
            }
            _ => swatch().write_to(&mut out, format),
        };
        done.expect("样例图编不出来");
        out.into_inner()
    }

    #[test]
    fn webp_becomes_png_while_jpeg_leaves_as_it_came() {
        let webp = encoded(ImageFormat::WebP);
        assert!(!webp.starts_with(PNG_MAGIC), "WebP 样例本身就该不是 PNG");
        let done = attachment(webp);
        assert!(done.starts_with(PNG_MAGIC), "WebP 转完不是 PNG");
        let back = image::load_from_memory(&done).expect("转出来的 PNG 读不回来");
        assert_eq!((back.width(), back.height()), (3, 2), "转完尺寸变了");

        // A 那一支：JPEG 不再无损重编成 PNG（那能撑到约 2.4 倍，是单条 73MB 的主因之一）
        let jpeg = encoded(ImageFormat::Jpeg);
        assert_eq!(
            attachment(jpeg.clone()),
            jpeg,
            "JPEG 原件被重编码了，体积那一档又回来了"
        );
    }

    /// 拿一个小阈值把 [`cap_to`] 真触发到：噪声图压缩率极差，编成 PNG 一定超 20KB，
    /// 而压完必须回到线内且是一段 JPEG。
    #[test]
    fn an_attachment_over_the_cap_comes_back_under_it() {
        let limit = 20 * 1024;
        let mut seed = 0x2545_F491_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed >> 11) as u8
        };
        let noise = image::RgbImage::from_fn(240, 160, |_, _| image::Rgb([next(), next(), next()]));
        let mut raw = Cursor::new(Vec::new());
        noise
            .write_to(&mut raw, ImageFormat::Png)
            .expect("噪声 PNG 编不出来");
        let big = raw.into_inner();
        assert!(big.len() > limit, "夹具没造够：{} 字节", big.len());

        let done = cap_to(big.clone(), limit);
        assert!(
            done.starts_with(JPEG_MAGIC),
            "压完不是 JPEG，说明走的是「压不动照原样交」那一支"
        );
        assert!(
            done.len() <= limit,
            "压完仍 {} 字节，没过 {} 这条线",
            done.len(),
            limit
        );
    }

    /// 压不动的那一支：解不开的字节（GIF 与真·垃圾）不能因为超线就被清空或换掉。
    #[test]
    fn bytes_that_cannot_be_decoded_stay_untouched_even_over_the_cap() {
        let junk = vec![7u8; 40 * 1024];
        assert_eq!(
            cap_to(junk.clone(), 1024),
            junk,
            "解不开的被改动了——那等于把这张图弄丢"
        );
    }

    /// 拿**真素材**复核那条线：`E2E_BIG_IMAGE` 指一张超过 `MAX_ATTACHMENT_BYTES` 的图
    /// （2026-10-05 用的是会员购那轮留档下来的 4268×5999 原件，11,957,242 字节），
    /// 断言它出门时过不了那条线、也没被弄丢。
    /// 标了 `#[ignore]`：这份夹具在仓库外的工装目录里，不该让常规 `cargo test` 依赖它。
    /// 跑法：`E2E_BIG_IMAGE=/path/to/xxx.jpg cargo test --release push:: -- --ignored --nocapture`
    #[test]
    #[ignore = "要一张仓库外的超大原件图当夹具"]
    fn a_real_oversize_attachment_leaves_under_the_cap() {
        let path = std::env::var("E2E_BIG_IMAGE").expect("没给 E2E_BIG_IMAGE，这一条跑不了");
        let big = std::fs::read(&path).unwrap_or_else(|e| panic!("读不到 {path}: {e}"));
        assert!(
            big.len() > MAX_ATTACHMENT_BYTES,
            "夹具没过线：{} 字节",
            big.len()
        );
        let done = attachment(big.clone());
        // 旧口径（一律无损转 PNG）会把它撑到多大——这条是给体积那一档留的对照数，不参与断言
        let mut old_way = Cursor::new(Vec::new());
        image::load_from_memory(&big)
            .expect("夹具读不回来")
            .write_to(&mut old_way, ImageFormat::Png)
            .expect("夹具编不成 PNG");
        eprintln!("旧口径无损转 PNG = {} 字节", old_way.into_inner().len());
        eprintln!(
            "{} 字节 → {} 字节（{}）",
            big.len(),
            done.len(),
            if done.starts_with(JPEG_MAGIC) {
                "JPEG"
            } else if done.starts_with(PNG_MAGIC) {
                "PNG"
            } else {
                "别的"
            }
        );
        assert!(
            done.len() <= MAX_ATTACHMENT_BYTES,
            "超线的真素材没被压回线内：{} 字节",
            done.len()
        );
        assert!(
            image::load_from_memory(&done).is_ok(),
            "压完的图读不回来，那等于弄丢了"
        );
    }

    #[test]
    fn png_and_undecodable_bytes_pass_through() {
        let png = encoded(ImageFormat::Png);
        assert_eq!(attachment(png.clone()), png, "已经是 PNG 的被重编码了");
        let junk = vec![1, 2, 3, 4, 5];
        assert_eq!(
            attachment(junk.clone()),
            junk,
            "解不开的也该交原件，别把图片段清空"
        );
    }

    /// 一份最小的真 GIF：魔数 + 逻辑屏幕描述符 + 一个图像块 + trailer
    fn gif() -> Vec<u8> {
        let mut v = b"GIF89a".to_vec();
        v.extend_from_slice(&[3, 0, 2, 0, 0, 0, 0]);
        v.extend_from_slice(&[0x2C, 0, 0, 0, 0, 3, 0, 2, 0, 0x00]);
        v.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]);
        v.push(0x3B);
        v
    }

    /// 动图原样交这条钉子。前半先确言「这份样例现在解不开」——`Cargo.toml` 没给 `image` 装 gif 解码器，
    /// 所以 GIF 走的是「解不开交原件」那一支，动画保住。哪天有人把 `gif` feature 加上，
    /// 前半会红，逼着他显式决定「要不要把动图压成第一帧」，而不是让转码悄悄吃掉动画。
    #[test]
    fn a_gif_stays_untouched_because_there_is_no_gif_decoder() {
        let raw = gif();
        assert!(
            image::load_from_memory(&raw).is_err(),
            "这份 GIF 现在能被解开了——解码器加上了，动图会被压成第一帧，得显式决定"
        );
        assert_eq!(
            attachment(raw.clone()),
            raw,
            "动图被改写了，它本该原样交出去"
        );
    }
}
