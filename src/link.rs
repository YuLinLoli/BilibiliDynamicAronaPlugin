//! 链接解析钩子：看见 B 站地址就取详情、画卡、回在这条消息的来源。
//!
//! 对应原插件 `ListenerTasker` + `ResolveLinkService` 那半棵树。分工照搬：
//! `bili.linkResolve.patterns` 那批识别式只做**预筛**（"这条消息值不值得试"），
//! 真正认类型取 id 走下面这批内置式——与原插件 `matchingRegular` /
//! `matchingInternalRegular` 的两段式一致。触发条件 `at` / `always` / `never`
//! 也在钩子体里现读配置，改了不必重启。
//!
//! 两类地址能画完整的卡（动态卡与直播卡，跟订阅推送同一套版式）：
//! `t.bilibili.com/…`、`…/opus/…`、`live.bilibili.com/…`。
//!
//! 三条能力边界，都记进 README：
//! * **视频与专栏没有专用小卡版式**。原插件的 `drawGeneral` 依赖
//!   `web-interface/view` 与文章详情两个接口，本插件的接口层没铺这两条，
//!   所以这两类只回一行标题式的文本加链接，不假装画出了卡。
//! * **短链还原拿不到 302 目标**。框架的 `runtime::http::send` 只回响应正文，
//!   没有状态码与最终 URL，所以 `b23.tv` 那一类只能抓回页面正文，
//!   从正文里的 B 站正式链接再认一次类型。
//! * **番剧与用户主页没有对应卡片**，同样退回文本；用户名片接口齐全，
//!   所以那一行给的是真名字真签名。

use std::sync::Arc;

use arona::onebot::HookFlow;
use arona::onebot::hooks::{BodyFilter, EventContext, NoticeKind, event_handler};
use arona::plugin::PluginContext;
use arona::runtime::message::{MessageSegment, OutgoingMessage};
use arona::runtime::priority::ListenerPriority;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::bili::api::{Bili, client};
use crate::config::{
    BiliConfig, DEFAULT_LINK_PATTERNS, IMAGE_SECTION, ImageConfig, LinkResolveConfig, SECTION,
};
use crate::push::{self, attachment};
use crate::store;
use crate::tasks;

/// 内置分类式，按它声明的顺序即优先级：短链先，其次是能画完整卡的
/// 动态与直播，再是用户/番剧，最后才是只有裸号的视频与专栏。
static SHORT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:b23\.tv|bili2233\.cn)/([0-9A-Za-z]{1,32})").unwrap());
static DYNAMIC: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:t\.bilibili\.com/(?:dynamic/)?|bilibili\.com/opus/)(\d{6,20})").unwrap()
});
static LIVE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"live\.bilibili\.com/(?:h5/)?(\d{1,12})").unwrap());
static USER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"space\.bilibili\.com/(\d{1,12})(?:\D|$)").unwrap());
static PGC: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\b(ss|ep|md)(\d{1,10})\b").unwrap());
static VIDEO: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(BV[0-9A-Za-z]{10}|av\d{1,9})\b").unwrap());
static ARTICLE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\bcv(\d{1,12})\b").unwrap());
/// 页面里第一条指向 B 站的正式链接——短链还原全靠它
static TARGET: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"https?://[^\s"'<>]*bilibili\.com[^\s"'<>]*"#).unwrap());
/// 内置那七条预筛式，编译一次用到底
static PREFILTER: Lazy<Vec<Regex>> = Lazy::new(|| {
    DEFAULT_LINK_PATTERNS
        .iter()
        .filter_map(|raw| Regex::new(raw).ok())
        .collect()
});

/// 认出的是哪一类地址
enum Found {
    /// 动态号
    Dynamic(i64),
    /// 直播间号
    Live(i64),
    /// UP 主 uid
    User(i64),
    /// 番剧号：原样前缀（ss/ep/md）+ 数字
    Season(String, i64),
    /// 视频标识（BVxx 或 avxx）
    Video(String),
    /// 专栏号
    Article(i64),
    /// 短链码
    Short(String),
}

pub(crate) fn register(ctx: &PluginContext) {
    let holder = ctx.clone();
    ctx.listen_feature(
        crate::FEATURE,
        &[BodyFilter::GroupMessage, BodyFilter::PrivateMessage],
        ListenerPriority::Normal,
        event_handler(move |event: Arc<EventContext>| {
            let ctx = holder.clone();
            Box::pin(async move { resolve(&ctx, &event).await })
        }),
    );

    // 原插件的 BotLeaveEvent 分支：bot 已经不在那个群里了，订阅关系留着只会
    // 让检测每轮都撞一次"目标不存在"，所以直接清掉。
    ctx.listen_feature(
        crate::FEATURE,
        &[BodyFilter::Notice(NoticeKind::GroupDecrease)],
        ListenerPriority::Normal,
        event_handler(|event: Arc<EventContext>| Box::pin(async move { left_group(&event).await })),
    );
}

/// 一条消息走完整条解析链。始终 [`HookFlow::Pass`]：解析是附加服务，
/// 不能因为识别到了链接就把消息吞掉，后面的命令分发与别的插件都要还能看见它。
async fn resolve(ctx: &PluginContext, event: &EventContext) -> HookFlow {
    let conf = ctx.config::<BiliConfig>(SECTION).get();
    let want = conf.link_resolve.clone();
    if !want.enable || want.trigger.eq_ignore_ascii_case("never") {
        return HookFlow::Pass;
    }
    if want.trigger.eq_ignore_ascii_case("at") && !mentioned(event) {
        return HookFlow::Pass;
    }
    let text = plain(event);
    if text.trim().is_empty() || !looks_like_bili(&text) || !prefiltered(&text, &want) {
        return HookFlow::Pass;
    }
    let Some(found) = classify(&text) else {
        return HookFlow::Pass;
    };
    let bili = client(conf.account.clone(), conf.check.timeout.max(5));
    let key = target_key(event);
    let color = color_of(ctx, &key);
    let _ = dispatch(ctx, event, &bili, &want, &key, &color, found).await;
    HookFlow::Pass
}

/// 认出来的地址先过一道短链还原，再交给 [`render`] 画。
async fn dispatch(
    ctx: &PluginContext,
    event: &EventContext,
    bili: &Arc<Bili>,
    want: &LinkResolveConfig,
    key: &str,
    color: &str,
    found: Found,
) -> bool {
    let found = match found {
        Found::Short(code) => match expand(event, bili, want, &code).await {
            Ok(found) => found,
            Err(done) => return done,
        },
        other => other,
    };
    render(ctx, event, bili, want, key, color, found).await
}

/// 按类型出图。
async fn render(
    ctx: &PluginContext,
    event: &EventContext,
    bili: &Arc<Bili>,
    want: &LinkResolveConfig,
    key: &str,
    color: &str,
    found: Found,
) -> bool {
    match found {
        Found::Dynamic(id) => {
            let dynamic = match bili.detail(id).await {
                Ok(Some(dynamic)) => dynamic,
                Ok(None) => return say(event, "解析失败：这条动态不存在或已被删除").await,
                Err(reason) => return say(event, &format!("解析失败 {reason}")).await,
            };
            let post = tasks::preview(ctx, bili, &dynamic, color).await;
            if push::send_preview(ctx, key, &post).await == 0 {
                return say(event, "发送失败，检查这个目标还在不在").await;
            }
            if want.return_link {
                let _ = event.reply(dynamic.link()).await;
            }
            true
        }
        Found::Live(room) => {
            let info = match bili.room(room).await {
                Ok(info) => info,
                Err(reason) => return say(event, &format!("解析失败 {reason}")).await,
            };
            let post = tasks::preview_live(ctx, bili, &info, color).await;
            if push::send_preview(ctx, key, &post).await == 0 {
                return say(event, "发送失败，检查这个目标还在不在").await;
            }
            if want.return_link {
                let _ = event.reply(info.link()).await;
            }
            true
        }
        Found::User(uid) => {
            let brief = match bili.user(uid).await {
                Ok(brief) => brief,
                Err(reason) => return say(event, &format!("解析失败 {reason}")).await,
            };
            let mut lines = vec![format!("UP主：{}（UID {}）", brief.name, brief.uid)];
            if !brief.sign.trim().is_empty() {
                lines.push(format!("签名：{}", brief.sign.trim()));
            }
            lines.push(format!("等级：Lv{}", brief.level));
            if brief.room_id > 0 {
                lines.push(format!(
                    "直播间：https://live.bilibili.com/{}",
                    brief.room_id
                ));
            }
            lines.push(format!("空间：https://space.bilibili.com/{}", brief.uid));
            text_card(
                event,
                bili,
                lines,
                &brief.face,
                "https://space.bilibili.com/",
            )
            .await
        }
        Found::Season(kind, id) => {
            let bili_kind = kind.to_ascii_lowercase();
            let season = match bili_kind.as_str() {
                "ss" => bili.season(id).await,
                "ep" => bili.season_by_ep(id).await,
                _ => bili.media(id).await,
            };
            let season = match season {
                Ok(season) => season,
                Err(reason) => return say(event, &format!("解析失败 {reason}")).await,
            };
            let mut lines = vec![format!("番剧：{}", season.title)];
            if !season.newest_title.trim().is_empty() {
                lines.push(format!("最新：{}", season.newest_title.trim()));
            }
            lines.push(format!(
                "播放：https://www.bilibili.com/bangumi/play/ss{}",
                season.season_id
            ));
            text_card(
                event,
                bili,
                lines,
                &season.cover,
                "https://www.bilibili.com/bangumi/",
            )
            .await
        }
        Found::Video(token) => {
            let kind = if token.starts_with("BV") {
                "视频"
            } else {
                "视频（av 号）"
            };
            text_card(
                event,
                bili,
                vec![
                    format!("{kind}：{token}"),
                    format!("链接：https://www.bilibili.com/video/{token}"),
                    "（这个插件没铺视频详情接口，画不出标题与封面那种小卡）".to_string(),
                ],
                "",
                "https://www.bilibili.com/",
            )
            .await
        }
        Found::Article(id) => {
            text_card(
                event,
                bili,
                vec![
                    format!("专栏：cv{id}"),
                    format!("链接：https://www.bilibili.com/read/cv{id}"),
                    "（这个插件没铺专栏详情接口，画不出标题与封面那种小卡）".to_string(),
                ],
                "",
                "https://www.bilibili.com/",
            )
            .await
        }
        // 短链在 dispatch 那一步就该被 expand 消化掉了。走到这儿只剩一种可能：
        // 还原出来的还是短链。不再 hop 第二层，直接认输。
        Found::Short(_) => say(event, "解析失败：这个短链还原出来还是短链").await,
    }
}

/// 把短链码展开成一个能画的地址。
///
/// 框架的 HTTP 出口只给响应正文，没有状态码也没有 302 的 `Location`，
/// 所以这里只能抓回短链页面、从正文里找出第一条 B 站正式链接再认一次类型。
/// 认不出、或认出来还是短链，就把那条正式链接直接回给用户——
/// 至少他知道自己点的是哪儿，比一句"解析失败"有用。
/// `Err(bool)` 表示"已经回过话了"，那个 bool 就是发送是否成功。
async fn expand(
    event: &EventContext,
    bili: &Arc<Bili>,
    want: &LinkResolveConfig,
    code: &str,
) -> Result<Found, bool> {
    if !want.allows("b23.tv") {
        return Err(say(event, "这个短链的域名不在允许解析的名单里").await);
    }
    let url = format!("https://b23.tv/{code}");
    let raw = match bili.bytes(&url, "https://www.bilibili.com/").await {
        Ok(raw) => raw,
        Err(reason) => return Err(say(event, &format!("解析失败 {reason}")).await),
    };
    let page = String::from_utf8_lossy(&raw);
    let Some(target) = TARGET
        .find(&page)
        .map(|found| found.as_str().to_string())
        .filter(|target| !target.contains("b23.tv"))
    else {
        return Err(say(event, "解析失败：短链页面里没找到 B 站正式链接").await);
    };
    match classify(&target) {
        Some(found) if !matches!(found, Found::Short(_)) => Ok(found),
        _ => Err(say(event, &format!("这个短链指向：{target}")).await),
    }
}

/// bot 自己退群或被踢：这个群的订阅整个作废
async fn left_group(event: &EventContext) -> HookFlow {
    let Some(group_id) = event.group_id() else {
        return HookFlow::Pass;
    };
    if event.user_id() != event.bot().self_id() {
        return HookFlow::Pass;
    }
    let key = store::group_key(group_id);
    let removed = crate::store().write(|data| data.clear_subs(&[key]));
    arona::runtime::log::info(format!(
        "bilibili：bot 已不在群 {group_id}，该群的订阅记录已清除（{removed} 项目标）"
    ));
    HookFlow::Pass
}

/// 一行文本 + 可选的一张封面/头像。图抓不到就只发文本，不因为图挂了而整条不回。
async fn text_card(
    event: &EventContext,
    bili: &Arc<Bili>,
    lines: Vec<String>,
    picture: &str,
    referer: &str,
) -> bool {
    let mut segments: Vec<MessageSegment> = Vec::new();
    if !picture.trim().is_empty() {
        // 先自己抓下来再交出去：B 站图床对不带 Referer 的裸请求会给 403，
        // 直接把 url 丢给实现端多半只能收到一个裂开的图。
        let image = match bili.bytes(picture, referer).await {
            Ok(raw) => MessageSegment::Image {
                url: None,
                file: None,
                data: Some(attachment(raw, None)),
                file_id: None,
            },
            Err(_) => MessageSegment::Image {
                url: Some(picture.to_string()),
                file: None,
                data: None,
                file_id: None,
            },
        };
        segments.push(image);
    }
    segments.push(MessageSegment::Text(lines.join("\n")));
    event
        .reply_message(OutgoingMessage::new(segments))
        .await
        .is_ok()
}

/// 回一句话，把"发出去了"当成处理成功
async fn say(event: &EventContext, text: &str) -> bool {
    event.reply(text).await.is_ok()
}

/// 群消息算群、私聊算那个人——与命令侧 `current_key` 同一套换算
fn target_key(event: &EventContext) -> String {
    match event.group_id() {
        Some(group_id) => store::group_key(group_id),
        None => store::friend_key(event.user_id()),
    }
}

/// 这个目标的配色（没设过就用全局默认色），跟 `/bili search` 那条路一致
fn color_of(ctx: &PluginContext, key: &str) -> String {
    crate::store()
        .read(|data| data.targets.get(key).and_then(|state| state.color.clone()))
        .unwrap_or_else(|| ctx.config::<ImageConfig>(IMAGE_SECTION).get().default_color)
}

/// `at` 档要求消息里 @ 了机器人自己
fn mentioned(event: &EventContext) -> bool {
    let self_id = event.bot().self_id();
    event
        .segments
        .iter()
        .any(|segment| matches!(segment, MessageSegment::At(id) if *id == self_id))
}

/// 只拼 Text 段。原插件同样把 At 与 Image 段滤掉——@ 渲染出来的那串 qq 号
/// 不该进识别式（`@12345678 看下这个视频` 会裸号误命中）。
/// 实现端只给纯文本、一个段都没解析出来时退回 `text`。
fn plain(event: &EventContext) -> String {
    let joined = event
        .segments
        .iter()
        .filter_map(|segment| match segment {
            MessageSegment::Text(value) => Some(value.as_str()),
            _ => None,
        })
        .collect::<Vec<&str>>()
        .join(" ");
    if joined.trim().is_empty() {
        event.text.clone()
    } else {
        joined
    }
}

/// 便宜的粗筛：一句里既没有 B 站域名也没有裸号特征时，连预筛的正则都不必跑
fn looks_like_bili(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("bilibili")
        || lowered.contains("b23.tv")
        || lowered.contains("bili2233")
        || VIDEO.is_match(text)
        || ARTICLE.is_match(text)
        || PGC.is_match(text)
}

/// 配置里那批识别式的预筛。留空用内置那七条；写坏了的正则按字面子串处理，
/// 不让一条坏配置把整功能关掉。
fn prefiltered(text: &str, want: &LinkResolveConfig) -> bool {
    if want.patterns.is_empty() {
        return PREFILTER.iter().any(|pattern| pattern.is_match(text));
    }
    want.patterns.iter().any(|raw| {
        Regex::new(raw).is_ok_and(|pattern| pattern.is_match(text)) || text.contains(raw.as_str())
    })
}

/// 按内置式认类型。顺序就是优先级。
fn classify(text: &str) -> Option<Found> {
    if let Some(found) = SHORT.captures(text).and_then(|cap| cap.get(1)) {
        return Some(Found::Short(found.as_str().to_string()));
    }
    if let Some(found) = DYNAMIC.captures(text).and_then(|cap| cap.get(1)) {
        return found.as_str().parse::<i64>().ok().map(Found::Dynamic);
    }
    if let Some(found) = LIVE.captures(text).and_then(|cap| cap.get(1)) {
        return found.as_str().parse::<i64>().ok().map(Found::Live);
    }
    if let Some(found) = USER.captures(text).and_then(|cap| cap.get(1)) {
        return found.as_str().parse::<i64>().ok().map(Found::User);
    }
    if let Some(cap) = PGC.captures(text) {
        let kind = cap[1].to_string();
        if let Ok(id) = cap[2].parse::<i64>() {
            return Some(Found::Season(kind, id));
        }
    }
    if let Some(found) = ARTICLE.captures(text).and_then(|cap| cap.get(1)) {
        return found.as_str().parse::<i64>().ok().map(Found::Article);
    }
    if let Some(found) = VIDEO.captures(text).and_then(|cap| cap.get(1)) {
        return Some(Found::Video(found.as_str().to_string()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_every_link_shape() {
        assert!(matches!(
            classify("看看 https://t.bilibili.com/123456789"),
            Some(Found::Dynamic(123456789))
        ));
        assert!(matches!(
            classify("https://live.bilibili.com/21452505"),
            Some(Found::Live(21452505))
        ));
        assert!(matches!(
            classify("https://space.bilibili.com/227838340/fans/dynamics"),
            Some(Found::User(227838340))
        ));
        assert!(matches!(
            classify("https://www.bilibili.com/bangumi/play/ss37457"),
            Some(Found::Season(_, 37457))
        ));
        assert!(matches!(
            classify("https://www.bilibili.com/video/BV1GJ411x7h7"),
            Some(Found::Video(_))
        ));
        assert!(matches!(
            classify("https://b23.tv/Kn5WGfB"),
            Some(Found::Short(_))
        ));
    }

    #[test]
    fn hosts_do_not_leak_into_the_dynamic_bucket() {
        // 直播间/空间那两类各自限定了域名，纯数字不该被动态那条抢走
        assert!(matches!(
            classify("https://live.bilibili.com/123456"),
            Some(Found::Live(123456))
        ));
        assert!(classify("今天没有链接").is_none());
    }
}
