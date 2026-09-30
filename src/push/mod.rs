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

use std::time::Duration;

use arona::bot::Bot;
use arona::plugin::PluginContext;
use arona::runtime::message::{MessageSegment, MessageTarget, OutgoingMessage};

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

/// 推给订阅了某个 uid 的全部目标（分组已展开），返回收到推送的目标数
pub async fn push_uid(ctx: &PluginContext, uid: i64, post: &Post) -> usize {
    let keys = crate::store().read(|data| data.targets_of(uid));
    let push = ctx.config::<BiliConfig>(SECTION).get().push;
    let mut hit = 0usize;
    for key in keys {
        // 分组新加进来的成员可能还没写过记录，按默认状态推（过滤器全空、没静音）
        let state = crate::store()
            .read(|data| data.targets.get(&key).cloned())
            .unwrap_or_default();
        if push_state(ctx, &key, &state, post).await > 0 {
            hit += 1;
            pause(push.push_interval).await;
        }
    }
    hit
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
