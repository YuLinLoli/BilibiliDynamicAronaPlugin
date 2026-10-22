//! `/bili config` 的多步向导：mirai 那句「回复编号往下选」在这里的等价物。
//!
//! 原插件挂在 `whileSelectMessages` 上（`service/ConfigService.kt`）——那是协程加「等这个会话
//! 的下一条消息」的语法糖。框架没给这一句，但给了做成同一件事的两块料：消息钩子
//! （[`HookFlow::Handled`] 能在命令分发之前把这条消息吃掉）与插件自己存的会话表。
//! 所以这里的循环不是一条挂着不动的协程，而是**每条入站消息推进一步**的状态机。
//!
//! 关键取舍：每一步都不自己写库，而是把用户回的文本拼成一条 `/bili` 子命令的参数，
//! 交给 [`super::run`] 去跑。权限门控、文案、操作通报因此与手打命令完全同源——
//! 向导一套、手打一套的话早晚漂移。为此起了一份 [`CommandContext`]，它的 `sender`
//! 由本模块实现（[`Replay`]，转交框架的 `Bot::send`）。
//!
//! 四处与原插件不同，都写进 README：
//! * 只有**发起人**回的消息算输入。原插件在群里听任何人的发言，别人随手打一个「2」
//!   就能把管理员正在配的那一格改掉。
//! * 每一步做完**重打整张菜单**，中括号里的当前值跟着刷新。原插件只补一句
//!   「输入编号以继续」并引用那张旧菜单——上面的值已经是错的了。
//! * 编号补齐。原插件的 At全体 那一档把「添加」和「删除」都印成 `2`
//!   （`ConfigService.kt:46-47`），却按 `3` 处理删除，照着屏幕敲的人必然撞一次「没有这个选项哦」。
//! * 多认一个 `全局`：打出先前那份一次性配置清单。顶掉它是因为原插件的交互更贴需求，
//!   但那份只读清单有人用，就让它留在这一个关键字后面。
//!
//! 2 分钟没人回就作废（`WINDOW`），连着两次认错自动收（`MAX_STRICKS`）——都是原插件那两个数。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use arona::bot::Bot;
use arona::framework::Framework;
use arona::onebot::HookFlow;
use arona::onebot::hooks::{BodyFilter, EventContext, event_handler};
use arona::plugin::PluginContext;
use arona::runtime::dispatcher::CommandContext;
use arona::runtime::message::{
    BoxFuture, MessageReceipt, MessageSegment, MessageSender, MessageTarget, OutgoingMessage,
};
use arona::runtime::priority::ListenerPriority;
use once_cell::sync::Lazy;

use crate::config::{
    BiliConfig, IMAGE_SECTION, ImageConfig, SECTION, TEMPLATE_SECTION, TemplateConfig,
};
use crate::store;

use super::{Call, Reply, browse, reply};

/// 原插件 `whileSelectMessages` 那句 `timeout(120_000)`
const WINDOW: Duration = Duration::from_secs(120);
/// 原插件那句「超出重试次数, 退出」
const MAX_STRICKS: u8 = 2;

/// 停在等哪一样输入上
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// 等编号
    Menu,
    /// 等 16 进制颜色
    Color,
    /// 等 @全体 的种类；`true` 是添加，`false` 是删除
    AtAll(bool),
    /// 等一个动态类型名
    FilterKind,
    /// 等一段正文正则
    FilterReg,
    /// 等 `t` / `r`——切换哪一类过滤器的黑白名单
    FilterMode,
    /// 等一个过滤器下标（`t0` / `r1`）
    FilterDel,
    /// 等一个模板名；带的是 `d` / `l` / `le`
    Template(&'static str),
    /// 会话结束了（占位用，不往表里存）
    Done,
}

/// 一个会话正在进行中的向导
#[derive(Clone)]
struct Session {
    /// 只有这个人的回复算输入
    owner: i64,
    /// 会话所在群；私聊为 `None`
    group_id: Option<i64>,
    /// 订阅者 uid，`0` 表示该目标的全部订阅
    uid: i64,
    /// 用户当时写的那个目标参数原样（空串 = 当前会话）
    target: String,
    /// 展开后的目标键，读中括号里的当前值用
    keys: Vec<String>,
    stage: Stage,
    strikes: u8,
    expires: Instant,
}

/// 进行中的向导：键是「会话号#发起人」。
/// 一次配置就一两分钟，过期条目在每次写入时顺手扫掉，不必专门起一个清理任务。
static SESSIONS: Lazy<RwLock<HashMap<String, Session>>> = Lazy::new(|| RwLock::new(HashMap::new()));

/// 菜单上的一格
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    AtAll,
    Color,
    Push,
    Filter,
}

/// 这一步跑完之后的走向
enum Outcome {
    /// 还在配置中，重打菜单
    Again,
    /// 还停在等同一样的东西上（命令把它那句拒绝说完了，菜单就别再刷一遍）
    Stay,
    /// 结束了，把会话删掉
    Quit,
}

/// 「会话#发起人」那个复合键。向导与 `/bili clear` 的待确认清单共用它：原插件那两处等的都是
/// **发命令这个人**的下一句话（`fromEvent.nextMessage` / `whileSelectMessages`），
/// 只按会话存的话，同群里另一个人说句「确定」就会把别人的清单顶掉。
pub(crate) fn key(contact: &str, owner: i64) -> String {
    format!("{contact}#{owner}")
}

/// 会话号：群里认群号，私聊认那个人——与 `commands::Call::current_key` 同一套换算。
/// `/bili clear` 那句「等下一句话」也用同一个键（见 `browse::register_confirm`）。
pub(crate) fn contact_of(group_id: Option<i64>, user_id: i64) -> String {
    match group_id {
        Some(group_id) => store::group_key(group_id),
        None => store::friend_key(user_id),
    }
}

/// 挂上「等下一条消息」那一跳。优先级排在链接解析之前：正在配置的人打的「2」
/// 不该被别的钩子当成一句话拿去解析。
pub(crate) fn register(ctx: &PluginContext) {
    let holder = ctx.clone();
    ctx.listen_feature(
        crate::FEATURE,
        &[BodyFilter::GroupMessage, BodyFilter::PrivateMessage],
        ListenerPriority::High,
        event_handler(move |event: Arc<EventContext>| {
            let ctx = holder.clone();
            Box::pin(async move { step(&ctx, &event).await })
        }),
    );
}

/// 清空所有进行中的向导：`/bili reload` 之后配置换了，菜单上的旧值不该还能点
pub(crate) fn clear() {
    if let Ok(mut table) = SESSIONS.write() {
        table.clear();
    }
}

/// `/bili config [UID] [目标]`：开一个会话并把菜单打出去。
pub(crate) async fn start(call: &Call) -> Reply {
    let uid = match call.sub_uid(0) {
        Ok(value) => value,
        Err(reason) => return reply(reason),
    };
    let targets = match call.targets(1) {
        Ok(value) => value,
        Err(reason) => return reply(reason),
    };
    let keys = match targets.writable() {
        Ok(list) => list.to_vec(),
        Err(reason) => return reply(reason),
    };
    let session = Session {
        owner: call.from().user_id,
        group_id: call.from().group_id,
        uid,
        target: targets.raw.clone(),
        keys,
        stage: Stage::Menu,
        strikes: 0,
        expires: Instant::now() + WINDOW,
    };
    let text = menu(call.ctx(), &session);
    remember(session);
    reply(text)
}

/// 把会话存进去，顺带扫掉过期的那些
fn remember(session: Session) {
    let id = key(&contact_of(session.group_id, session.owner), session.owner);
    let Ok(mut table) = SESSIONS.write() else {
        return;
    };
    let now = Instant::now();
    table.retain(|_, live| live.expires > now);
    table.insert(id, session);
}

/// 一条入站消息推进一步。没有进行中的会话就 [`HookFlow::Pass`]，
/// 正常聊天与别的插件都不受影响。
async fn step(ctx: &PluginContext, event: &EventContext) -> HookFlow {
    let id = key(
        &contact_of(event.group_id(), event.user_id()),
        event.user_id(),
    );
    let Some(mut session) = take(&id) else {
        return HookFlow::Pass;
    };
    let input = typed(event);
    if input.is_empty() {
        remember(session);
        return HookFlow::Pass;
    }
    // 打真命令一律放行：向导不该挡在命令前面，会话还留着
    if is_command(ctx, &input) {
        remember(session);
        return HookFlow::Pass;
    }
    let (lines, outcome) = feed(ctx, &mut session, event, &input).await;
    for line in lines {
        let _ = event.reply(line).await;
    }
    match outcome {
        Outcome::Quit => {
            if let Ok(mut table) = SESSIONS.write() {
                table.remove(&id);
            }
        }
        outcome => {
            session.expires = Instant::now() + WINDOW;
            if matches!(outcome, Outcome::Again) {
                let _ = event.reply(menu(ctx, &session)).await;
            }
            remember(session);
        }
    }
    HookFlow::Handled
}

fn take(id: &str) -> Option<Session> {
    let found = SESSIONS.read().ok()?.get(id).cloned()?;
    if found.expires <= Instant::now() {
        if let Ok(mut table) = SESSIONS.write() {
            table.remove(id);
        }
        return None;
    }
    Some(found)
}

/// 处理一步输入。返回要发出去的话（菜单由调用方统一补打）。
async fn feed(
    ctx: &PluginContext,
    session: &mut Session,
    event: &EventContext,
    input: &str,
) -> (Vec<String>, Outcome) {
    if input == "退出" || input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit")
    {
        session.stage = Stage::Done;
        return (vec!["已退出配置".to_string()], Outcome::Quit);
    }
    match session.stage {
        Stage::Menu => menu_step(ctx, session, event, input).await,
        Stage::Color => {
            let answer = invoke_at(ctx, session, event, &["color", input]).await;
            if bad_color(input) {
                return retry(session, answer);
            }
            accept(session, answer)
        }
        Stage::AtAll(add) => {
            let head = if add { "atall" } else { "delatall" };
            let answer = invoke(ctx, session, event, &[head, input]).await;
            if store::normalize_at_all_kind(input).is_none() {
                return retry(session, answer);
            }
            accept(session, answer)
        }
        Stage::FilterKind => {
            let answer = invoke(ctx, session, event, &["filtertype", input]).await;
            if store::normalize_filter_kind(input).is_none() {
                return retry(session, answer);
            }
            accept(session, answer)
        }
        Stage::FilterReg => {
            let answer = invoke(ctx, session, event, &["filterreg", input]).await;
            if regex::Regex::new(input).is_err() {
                return retry(session, answer);
            }
            accept(session, answer)
        }
        Stage::FilterMode => {
            let Some(which) = mode_choice(input) else {
                // 这一格不经过子命令，没有现成的拒绝文案可复用，只能自己写一句
                return retry(
                    session,
                    vec!["只认 t（类型过滤器）或 r（正则过滤器）".to_string()],
                );
            };
            let mode = if current_mode(session, which) == store::Mode::Black {
                "w"
            } else {
                "b"
            };
            let answer = invoke(ctx, session, event, &["filtermode", which, mode]).await;
            accept(session, answer)
        }
        Stage::FilterDel => {
            let answer = invoke(ctx, session, event, &["filterdel", input]).await;
            accept(session, answer)
        }
        Stage::Template(kind) => {
            let answer = invoke_at(ctx, session, event, &["template", kind, input]).await;
            if !known_template(ctx, kind, input) {
                return retry(session, answer);
            }
            accept(session, answer)
        }
        Stage::Done => (Vec::new(), Outcome::Quit),
    }
}

/// 这一格配成了：回菜单那一档，重试计数清零
fn accept(session: &mut Session, lines: Vec<String>) -> (Vec<String>, Outcome) {
    session.stage = Stage::Menu;
    session.strikes = 0;
    (lines, Outcome::Again)
}

/// 这一格打错了：命令已经把它那句拒绝说完了，向导只数次数、停在原地再问一次
fn retry(session: &mut Session, mut lines: Vec<String>) -> (Vec<String>, Outcome) {
    session.strikes += 1;
    if session.strikes >= MAX_STRICKS {
        session.stage = Stage::Done;
        lines.push("超出重试次数，已退出配置".to_string());
        return (lines, Outcome::Quit);
    }
    (lines, Outcome::Stay)
}

/// `t` / `r` 的认法：中文写法也收，照着屏幕打的人不必切输入法
fn mode_choice(raw: &str) -> Option<&'static str> {
    let lowered = raw.trim().to_lowercase();
    match lowered.as_str() {
        "t" | "类型" | "类型过滤器" => Some("t"),
        "r" | "正则" | "正则过滤器" => Some("r"),
        _ => None,
    }
}

/// 模板名要在配置那张表里查得到，否则 `template` 那条只会回一句「没有这个模板哦」
/// ——那句是对的，但向导该停在原地让人再打一次，而不是把整张菜单刷新一遍。
fn known_template(ctx: &PluginContext, kind: &str, name: &str) -> bool {
    let templates = ctx.config::<TemplateConfig>(TEMPLATE_SECTION).get();
    let table = match kind {
        "d" => &templates.dynamic_push,
        "l" => &templates.live_push,
        _ => &templates.live_close,
    };
    table.contains_key(name)
}

/// 编号那一格怎么走。认错两次收摊，与原插件一致。
async fn menu_step(
    ctx: &PluginContext,
    session: &mut Session,
    event: &EventContext,
    input: &str,
) -> (Vec<String>, Outcome) {
    if input == "全局" || input.eq_ignore_ascii_case("all") {
        let args = session.config_args();
        let lines = browse::overview(&draft(ctx, event, args));
        return (vec![lines.join("\n")], Outcome::Again);
    }
    let Some(choice) = parse_choice(input) else {
        return strike(
            session,
            Vec::new(),
            "请输入上面那张表里的编号，例如 2 或 3.1；回 退出 结束",
        );
    };
    let Some(item) = locate(session.group_id.is_some(), choice.head) else {
        return strike(session, Vec::new(), "没有这个编号");
    };
    let sub = choice.sub.unwrap_or(0);
    match item {
        Item::AtAll => match sub {
            1 => (
                invoke(ctx, session, event, &["listatall"]).await,
                Outcome::Again,
            ),
            2 => {
                session.stage = Stage::AtAll(true);
                session.strikes = 0;
                (
                    vec![
                        "请选择要At全体的内容:".to_string(),
                        "  全部".to_string(),
                        "  ├─ 全部动态".to_string(),
                        "  │   ├─ 视频".to_string(),
                        "  │   ├─ 音乐".to_string(),
                        "  │   └─ 专栏".to_string(),
                        "  └─ 直播".to_string(),
                        format!("请回复其一：{}", store::AT_ALL_KINDS.join(" / ")),
                    ],
                    Outcome::Again,
                )
            }
            3 => {
                let mut lines = invoke(ctx, session, event, &["listatall"]).await;
                lines.push("请回复要删除的项".to_string());
                session.stage = Stage::AtAll(false);
                session.strikes = 0;
                (lines, Outcome::Again)
            }
            _ => (submenu(item, choice.head), Outcome::Again),
        },
        Item::Color => {
            session.stage = Stage::Color;
            session.strikes = 0;
            (
                vec![
                    "请输入16进制颜色，例如: #d3edfa".to_string(),
                    "单值自动渐变，多值用分号分隔: #fde8ed;#000000".to_string(),
                ],
                Outcome::Again,
            )
        }
        Item::Push => {
            let kind = match sub {
                1 => "d",
                2 => "l",
                3 => "le",
                _ => return (submenu(item, choice.head), Outcome::Again),
            };
            let mut lines = invoke(ctx, session, event, &["templatelist", kind]).await;
            lines.push("请回复模板名（上面那份列表里的名字，大小写都行）".to_string());
            session.stage = Stage::Template(kind);
            session.strikes = 0;
            (lines, Outcome::Again)
        }
        Item::Filter => match sub {
            1 => (
                invoke(ctx, session, event, &["filterlist"]).await,
                Outcome::Again,
            ),
            2 => {
                session.stage = Stage::FilterKind;
                session.strikes = 0;
                (
                    vec![
                        format!(
                            "当前过滤器类型: {}\n支持的类型:",
                            current_mode(session, "t").label()
                        ),
                        store::FILTER_KINDS
                            .iter()
                            .map(|kind| format!("    {kind}"))
                            .collect::<Vec<String>>()
                            .join("\n"),
                        "请回复要过滤的类型".to_string(),
                    ],
                    Outcome::Again,
                )
            }
            3 => {
                session.stage = Stage::FilterReg;
                session.strikes = 0;
                (
                    vec![
                        format!("当前过滤器类型: {}", current_mode(session, "r").label()),
                        "请回复过滤文本或正则".to_string(),
                    ],
                    Outcome::Again,
                )
            }
            4 => {
                session.stage = Stage::FilterMode;
                session.strikes = 0;
                (
                    vec![
                        format!("类型过滤器: {}", current_mode(session, "t").label()),
                        format!("正则过滤器: {}", current_mode(session, "r").label()),
                        "请选择要切换的过滤的类型\nt: 类型过滤器\nr: 正则过滤器".to_string(),
                    ],
                    Outcome::Again,
                )
            }
            5 => {
                let mut lines = invoke(ctx, session, event, &["filterlist"]).await;
                lines.push("请回复要删除的下标（形如 t0 / r1）".to_string());
                session.stage = Stage::FilterDel;
                session.strikes = 0;
                (lines, Outcome::Again)
            }
            _ => (submenu(item, choice.head), Outcome::Again),
        },
    }
}

/// 认错一次：还能再错一次，第二次就收
fn strike(session: &mut Session, mut lines: Vec<String>, hint: &str) -> (Vec<String>, Outcome) {
    session.strikes += 1;
    if session.strikes >= MAX_STRICKS {
        lines.push("没有这个选项哦，超出重试次数，已退出配置".to_string());
        session.stage = Stage::Done;
        return (lines, Outcome::Quit);
    }
    lines.push(format!("没有这个选项哦，请重新输入。{hint}"));
    (lines, Outcome::Again)
}

/// 编号指向一格主项，但这一项要的是 `x.1` 那种带子编号的写法
fn submenu(item: Item, head: u8) -> Vec<String> {
    let rows: &[&str] = match item {
        Item::AtAll => &[
            "  1.1: 当前At全体项",
            "  1.2: 添加At全体",
            "  1.3: 删除At全体",
        ],
        Item::Color => &[],
        Item::Push => &[
            "  x.1: 动态推送模板",
            "  x.2: 直播推送模板",
            "  x.3: 直播结束模板",
        ],
        Item::Filter => &[
            "  x.1: 过滤器列表",
            "  x.2: 添加类型过滤器",
            "  x.3: 添加正则过滤器",
            "  x.4: 切换过滤模式",
            "  x.5: 删除过滤器",
        ],
    };
    if rows.is_empty() {
        return vec![format!("{head} 这一格直接回值就行（不用带子编号）")];
    }
    vec![
        format!("{head} 还要带上子编号，可选："),
        rows.iter()
            .map(|row| row.replacen('x', &head.to_string(), 1))
            .collect::<Vec<String>>()
            .join("\n"),
    ]
}

/// `[2.3]` → 主项 2 的第 3 格；`[2]` → 主项 2、子编号 None。
/// 分隔符认四种：半角点、全角点、中文句号与间隔号——输入法切漏一次不该让人白打。
fn parse_choice(raw: &str) -> Option<Choice> {
    let parts: Vec<&str> = raw
        .trim()
        .split(['.', '。', '．', '·'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    match parts.as_slice() {
        [head] => Some(Choice {
            head: head.parse().ok()?,
            sub: None,
        }),
        [head, sub] => Some(Choice {
            head: head.parse().ok()?,
            sub: Some(sub.parse().ok()?),
        }),
        _ => None,
    }
}

struct Choice {
    head: u8,
    sub: Option<u8>,
}

/// 菜单上有哪几格、各占哪个编号。编号按出现顺序现算，与原插件那个 `i++` 同一写法。
fn items(is_group: bool) -> Vec<(u8, Item)> {
    let mut out: Vec<(u8, Item)> = Vec::new();
    let mut next = 1u8;
    if is_group {
        out.push((next, Item::AtAll));
        next += 1;
    }
    out.push((next, Item::Color));
    next += 1;
    out.push((next, Item::Push));
    next += 1;
    out.push((next, Item::Filter));
    out
}

fn locate(is_group: bool, head: u8) -> Option<Item> {
    items(is_group)
        .into_iter()
        .find(|(n, _)| *n == head)
        .map(|(_, item)| item)
}

/// 这张表就是用户看到的菜单：中括号里是当前值
fn menu(ctx: &PluginContext, session: &Session) -> String {
    let state = session.state();
    let appearance = ctx.config::<ImageConfig>(IMAGE_SECTION).get();
    let templates = ctx.config::<TemplateConfig>(TEMPLATE_SECTION).get();
    let uid_slot = session.uid.to_string();
    let at_all = state
        .at_all
        .get(&uid_slot)
        .is_some_and(|kinds| !kinds.is_empty());
    let filter = state.filter_for(session.uid);
    let mode = match filter {
        Some(set) => format!(
            "类型: {} | 正则: {}",
            set.type_mode.label(),
            set.reg_mode.label()
        ),
        None => "无过滤器".to_string(),
    };
    let mut out: Vec<String> = vec![
        "配置:".to_string(),
        format!(
            "用户: {}",
            if session.uid == 0 {
                "全局（对该目标的全部订阅生效）".to_string()
            } else {
                format!("UID {}", session.uid)
            }
        ),
        format!(
            "目标: {}",
            if session.target.is_empty() {
                "当前会话".to_string()
            } else {
                session.target.clone()
            }
        ),
        String::new(),
        "当前可配置项:".to_string(),
    ];
    for (head, item) in items(session.group_id.is_some()) {
        match item {
            Item::AtAll => out.extend([
                format!("  {head}: At全体 [{}]", if at_all { "是" } else { "否" }),
                format!("      {head}.1: 当前At全体项"),
                format!("      {head}.2: 添加At全体"),
                format!("      {head}.3: 删除At全体"),
            ]),
            Item::Color => out.push(format!(
                "  {head}: 主题色 [{}]",
                match state.color.as_deref().map(str::trim) {
                    Some(own) if !own.is_empty() => format!("{own}（本目标）"),
                    // 跟随全局时把全局那一档写出来，免得"改了 image.defaultColor 却不知道有没有生效"
                    _ => format!("{}（跟随全局）", appearance.color_for(None)),
                }
            )),
            Item::Push => out.extend([
                format!("  {head}: 推送模板"),
                format!(
                    "      {head}.1: 动态推送模板 [{}]",
                    pick(&state, "dynamic", &templates.default_dynamic_push)
                ),
                format!(
                    "      {head}.2: 直播推送模板 [{}]",
                    pick(&state, "live", &templates.default_live_push)
                ),
                format!(
                    "      {head}.3: 直播结束模板 [{}]",
                    pick(&state, "liveClose", &templates.default_live_close)
                ),
            ]),
            Item::Filter => out.extend([
                format!("  {head}: 过滤器"),
                format!("      {head}.1: 过滤器列表"),
                format!("      {head}.2: 添加类型过滤器"),
                format!("      {head}.3: 添加正则过滤器"),
                format!("      {head}.4: 切换过滤模式 [{mode}]"),
                format!("      {head}.5: 删除过滤器"),
            ]),
        }
    }
    out.extend([
        String::new(),
        "[中括号]内为当前值".to_string(),
        format!(
            "请输入编号, {}分钟未回复自动退出\n或回复 退出 来主动退出；回 全局 看完整配置清单",
            WINDOW.as_secs() / 60
        ),
    ]);
    out.join("\n")
}

fn pick(state: &store::TargetState, kind: &str, fallback: &str) -> String {
    state
        .templates
        .get(kind)
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

/// 这一档当前的黑白名单模式（没配过按原插件的默认：黑名单）
fn current_mode(session: &Session, which: &str) -> store::Mode {
    session
        .state()
        .filter_for(session.uid)
        .map(|set| {
            if which == "t" {
                set.type_mode
            } else {
                set.reg_mode
            }
        })
        .unwrap_or(store::Mode::Black)
}

impl Session {
    /// 菜单上的值都从这一格读：多目标（分组）时取第一个成员，写回则每个成员都写
    fn state(&self) -> store::TargetState {
        self.keys
            .first()
            .and_then(|key| crate::store().read(|data| data.targets.get(key).cloned()))
            .unwrap_or_default()
    }

    /// 拼 `[UID] [目标]` 那两个尾巴，`0` 那档也照写——留空与写 0 在 `sub_uid` 里等价
    fn tail(&self) -> Vec<String> {
        let mut out = vec![self.uid.to_string()];
        if !self.target.is_empty() {
            out.push(self.target.clone());
        }
        out
    }

    /// 只带 `[目标]`：`color` 与 `template` 那两条的位次里没有 UID，多塞一个会当成颜色/模板名去解析
    fn target_only(&self) -> Vec<String> {
        if self.target.is_empty() {
            Vec::new()
        } else {
            vec![self.target.clone()]
        }
    }

    /// `/bili config` 那条清单要的参数
    fn config_args(&self) -> Vec<String> {
        self.tail()
    }
}

/// 替用户跑一条子命令（尾巴是 `[UID] [目标]` 那一族）：与手打完全同一条路，
/// 权限门控、文案、操作通报都由命令层出一份，向导不另写一套。
async fn invoke(
    ctx: &PluginContext,
    session: &Session,
    event: &EventContext,
    head: &[&str],
) -> Vec<String> {
    let mut args: Vec<String> = head.iter().map(|arg| arg.to_string()).collect();
    args.extend(session.tail());
    send(ctx, event, args).await
}

/// 同上，但尾巴只带 `[目标]`——`color`、`template` 那两条的位次里没有 UID
async fn invoke_at(
    ctx: &PluginContext,
    session: &Session,
    event: &EventContext,
    head: &[&str],
) -> Vec<String> {
    let mut args: Vec<String> = head.iter().map(|arg| arg.to_string()).collect();
    args.extend(session.target_only());
    send(ctx, event, args).await
}

/// 命令自己已经把东西发出去了（图片、模板预览）时返回空，由它自己发。
async fn send(ctx: &PluginContext, event: &EventContext, args: Vec<String>) -> Vec<String> {
    let from = command_context(ctx, event);
    match super::run(ctx.clone(), from, args).await {
        Some(message) => message
            .segments
            .iter()
            .filter_map(|segment| match segment {
                MessageSegment::Text(value) => Some(value.as_str()),
                _ => None,
            })
            .collect::<Vec<&str>>()
            .join(" ")
            .lines()
            .map(str::to_string)
            .collect(),
        None => Vec::new(),
    }
}

/// 向导里的每一步都当作「这个人打了一条命令」：身份、会话、消息段都照原样带过去
fn command_context(ctx: &PluginContext, event: &EventContext) -> Arc<CommandContext> {
    let framework = ctx.framework().clone();
    Arc::new(CommandContext {
        user_id: event.user_id(),
        group_id: event.group_id(),
        text: typed(event),
        sender_name: event.sender_name().map(str::to_string),
        is_admin: framework.gating().is_manager(event.user_id()),
        sender_role: event.sender_role(),
        message_id: event.message_id(),
        time: event.event.time,
        quoted: event.quoted(),
        segments: event.segments.clone(),
        sender: Arc::new(Replay(framework.clone())),
        framework,
    })
}

/// 直接造一条 `Call`，给「全局」那一档看清单用（它要的是 `browse::overview`，不是又开一个向导）
fn draft(ctx: &PluginContext, event: &EventContext, args: Vec<String>) -> Call {
    Call {
        ctx: ctx.clone(),
        from: command_context(ctx, event),
        conf: ctx.config::<BiliConfig>(SECTION).get(),
        args,
    }
}

/// 颜色先自己判一次，好让向导停在原地重问；真正的格式校验在 `subscribe::color` 里，
/// 那句「格式错误」的文案也因此与手打命令一模一样。
fn bad_color(raw: &str) -> bool {
    !raw.contains('#')
        || raw.split(';').any(|part| {
            let body = part.trim().trim_start_matches('#');
            body.len() != 6 || !body.chars().all(|ch| ch.is_ascii_hexdigit())
        })
}

/// 用户在这一格里打的那句话：只拼文本段，@ 与图片段丢掉
pub(crate) fn typed(event: &EventContext) -> String {
    let joined = event
        .segments
        .iter()
        .filter_map(|segment| match segment {
            MessageSegment::Text(value) => Some(value.as_str()),
            _ => None,
        })
        .collect::<Vec<&str>>()
        .join(" ")
        .trim()
        .to_string();
    if joined.is_empty() {
        event.text.trim().to_string()
    } else {
        joined
    }
}

/// 是不是又打了一条 `/bili`（或配置里的其它命令名）。是就放行给命令分发。
pub(crate) fn is_command(ctx: &PluginContext, text: &str) -> bool {
    let conf = ctx.config::<BiliConfig>(SECTION).get();
    let lowered = text.to_lowercase();
    super::command_names(&conf.command)
        .iter()
        .any(|name| lowered.starts_with(&name.to_lowercase()))
}

/// 向导发东西的出口：转交框架的对象模型，与 `Call::notify` 那条同一扇门
struct Replay(Arc<Framework>);

impl MessageSender for Replay {
    fn send<'a>(
        &'a self,
        target: MessageTarget,
        message: OutgoingMessage,
    ) -> BoxFuture<'a, MessageReceipt> {
        let bot = Bot::of(&self.0);
        Box::pin(async move { bot.send(target, message).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 向导与 `/bili clear` 的待确认清单共用这一个键：只按会话存不够——同群里换个人说话
    /// 就是另一份会话，否则旁人一句「确定」会把别人的清单顶掉。
    #[test]
    fn a_dialog_key_carries_who_asked() {
        assert_eq!(key(&contact_of(Some(1), 7), 7), "g1#7");
        assert_eq!(key(&contact_of(None, 7), 7), "f7#7");
        assert_ne!(
            key(&contact_of(Some(1), 7), 7),
            key(&contact_of(Some(1), 8), 8),
            "同一个群里两个人各等各的下一句话"
        );
    }

    #[test]
    fn menu_numbering_follows_which_items_are_present() {
        // 群里四格，私聊没有 At全体 那一格，后面的编号要跟着前移
        assert_eq!(
            items(true)
                .iter()
                .map(|(n, item)| (*n, *item))
                .collect::<Vec<(u8, Item)>>(),
            vec![
                (1, Item::AtAll),
                (2, Item::Color),
                (3, Item::Push),
                (4, Item::Filter),
            ]
        );
        assert_eq!(
            items(false),
            vec![(1, Item::Color), (2, Item::Push), (3, Item::Filter)]
        );
        assert_eq!(locate(true, 1), Some(Item::AtAll));
        assert_eq!(locate(false, 1), Some(Item::Color));
        assert_eq!(locate(false, 4), None);
    }

    #[test]
    fn choices_carry_their_sub_number() {
        assert_eq!(parse_choice("3").unwrap().head, 3);
        assert_eq!(parse_choice("3").unwrap().sub, None);
        assert_eq!(parse_choice(" 3.2 ").unwrap().sub, Some(2));
        // 中文输入法下的句号、全角点与间隔号也算分隔，别让人白打一次
        assert_eq!(parse_choice("3。2").unwrap().sub, Some(2));
        assert_eq!(parse_choice("3．2").unwrap().sub, Some(2));
        assert_eq!(parse_choice("3.·2").unwrap().sub, Some(2));
        assert!(parse_choice("订阅").is_none());
        assert!(parse_choice("3.2.1").is_none());
        // 尾巴那个点当没有：只是手滑，不是另一个编号
        assert_eq!(parse_choice("3.").unwrap().sub, None);
        assert!(parse_choice("0").is_none() || parse_choice("0").unwrap().sub.is_none());
    }

    #[test]
    fn a_bare_number_on_a_sub_itemized_row_shows_the_rows() {
        let lines = submenu(Item::Push, 3);
        let text = lines.join("\n");
        assert!(text.contains("3.1"));
        assert!(text.contains("3.3"));
        // 主题色那格要的是颜色本身
        assert!(submenu(Item::Color, 2).join("\n").contains("直接回值"));
    }

    #[test]
    fn tails_match_each_subcommands_argument_slots() {
        let session = Session {
            owner: 100,
            group_id: Some(20001),
            uid: 546195,
            target: "我的分组".to_string(),
            keys: vec!["g20001".to_string()],
            stage: Stage::Menu,
            strikes: 0,
            expires: Instant::now() + WINDOW,
        };
        // `[UID] [目标]` 那一族
        assert_eq!(
            session.tail(),
            vec!["546195".to_string(), "我的分组".to_string()]
        );
        // `color` / `template` 的位次里没有 UID：多塞一个会被当成颜色或模板名
        assert_eq!(session.target_only(), vec!["我的分组".to_string()]);
        // 目标留空时两个尾巴都不该多出那一项
        let current = Session {
            target: String::new(),
            uid: 0,
            ..session.clone()
        };
        assert_eq!(current.tail(), vec!["0".to_string()]);
        assert!(current.target_only().is_empty());
    }

    #[test]
    fn mode_switch_accepts_the_letters_and_the_chinese_words() {
        assert_eq!(mode_choice("t"), Some("t"));
        assert_eq!(mode_choice("R"), Some("r"));
        assert_eq!(mode_choice("正则"), Some("r"));
        assert_eq!(mode_choice("类型"), Some("t"));
        assert_eq!(mode_choice("黑白"), None);
    }

    #[test]
    fn only_hex_values_get_accepted_as_colors() {
        assert!(!bad_color("#d3edfa"));
        assert!(!bad_color("#fde8ed;#0a1b2c"));
        assert!(bad_color("d3edfa"));
        assert!(bad_color("#d3ed"));
        assert!(bad_color("#zzzzzz"));
        assert!(bad_color("#d3edfa;#12"));
    }
}
