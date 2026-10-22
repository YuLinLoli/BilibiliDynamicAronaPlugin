//! 浏览与运维那一档子命令：重载配置、模板预览、扫码登录、看配置、看单条动态/直播、清失效目标。
//!
//! 这一层的共同点是"只看不改订阅关系"（`reload` 与 `clear` 除外）：
//! 发出去的东西一律走 [`push::send_preview`] 与 [`crate::tasks::preview`]，
//! 和真实推送共用同一条取素材、画图、解析模板的路径——
//! 用户在命令里看到的效果图，就是稍后订阅者收到的那张。
//!
//! 与原插件的几处刻意为之的差别，都记在 README：
//! * 「加载中…」那一档（`bili.showLoadingMessage`）只挂在 `search`/`new`/`video` 这三条
//!   要联网的命令上。原插件还挂在 `templateList` 上（`DynamicCommand.kt:224`），那边画图要时间，
//!   这边模板预览用的是假数据、不联网也不绘图，占位一条只会闪一下，所以没挂。
//! * `login` 成功之后多回一句"登录成功!"，原插件只默默写 Cookie。
//! * `clear` 那句「请回复 确定 或 取消」现在是真等一句答复（见 [`confirm_step`]），与原插件
//!   `nextMessage(300000)` 同形。先前这里写成两步命令，理由是「框架没有等下一条消息的出口」，
//!   那个判断是错的——消息钩子加插件自己那份会话表就能做同一件事（`/bili config` 的向导是同一套料）。
//! * `templateList` 原来是"每条模板一个嵌套转发"。框架的转发只有一层，所以摊平成一个转发，
//!   条目之间靠"模板名"那一行分隔。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use arona::bot::Bot;
use arona::onebot::HookFlow;
use arona::onebot::hooks::{BodyFilter, EventContext, event_handler};
use arona::plugin::PluginContext;
use arona::runtime::message::{ForwardMessage, MessageSegment, OutgoingMessage};
use arona::runtime::priority::ListenerPriority;
use once_cell::sync::Lazy;

use super::{Call, Cmd};
use super::{Match, Reply, filter::template_kind, lines, match_uid, reply, usage_str};
use crate::bili::api::Bili;
use crate::bili::model::{Dynamic, LiveInfo};
use crate::config::{
    BiliConfig, IMAGE_SECTION, ImageConfig, SECTION, TEMPLATE_SECTION, TemplateConfig,
};
use crate::push::{
    self, Kind,
    template::{Supply, Vals, render, render_close},
};
use crate::store::{self, TargetState};
use crate::tasks;

/// `/bili reload`：让框架重读这个插件那一份配置文件。
/// 框架会在读完之后回调插件的 `on_config_reload`，检测间隔与外观跟着重排。
pub(crate) fn reload(call: &Call) -> Reply {
    if !call.is_bot_admin() {
        return reply("仅 bot 管理员可以重载配置");
    }
    call.ctx().framework().configs().reload(crate::PLUGIN_ID);
    // 菜单上那些中括号里的值是旧配置下的，重载之后不能再让人照着点
    super::wizard::clear();
    reply("配置重载成功")
}

/// `/bili tl [d|l|le]`：把这一类模板各渲染一遍，发成一个合并转发。
/// 不联网取样本内容，字段用 [`Vals::test()`] 那一份假数据——
/// 要看的是"这套模板长什么样"，不是"某条真动态长什么样"。
pub(crate) async fn template_list(call: &Call) -> Reply {
    let raw = call.arg(0).unwrap_or("d");
    let Some((_, table_name)) = template_kind(raw) else {
        return reply("类型错误 d:动态 l:直播 le:直播结束");
    };
    let config = call.ctx().config::<TemplateConfig>(TEMPLATE_SECTION).get();
    let table: &BTreeMap<String, String> = match table_name {
        "dynamic" => &config.dynamic_push,
        "live" => &config.live_push,
        _ => &config.live_close,
    };
    if table.is_empty() {
        return reply("这一类模板一个都没配，检查 bili-template 那几区");
    }
    let kind = match table_name {
        "dynamic" => Kind::Dynamic,
        "live" => Kind::Live,
        _ => Kind::LiveClose,
    };
    let vals = Vals::test();
    // 预览不绘图：绘图要 cookie、要下素材，看模板不该被这些拖住
    let supply = Supply {
        draw_missing: "（模板预览，不含卡片）".to_string(),
        ..Default::default()
    };
    let author = vals.get("name").unwrap_or("bilibili").to_string();
    let uin = vals.uid();
    let mut nodes: Vec<ForwardMessage> = Vec::new();
    nodes.push(node(&author, uin, format!("{}推送模板", kind.label())));
    nodes.push(node(
        &author,
        uin,
        "下面每一段以模板名开头，其后是该模板的渲染效果".to_string(),
    ));
    for (name, source) in table {
        nodes.push(node(name, uin, format!("模板 {name}")));
        let messages = if kind == Kind::LiveClose {
            vec![render_close(source, &vals)]
        } else {
            render(source, &vals, &supply, &config.forward_card)
        };
        for message in messages {
            for segment in message.segments {
                nodes.push(ForwardMessage {
                    name: name.clone(),
                    uin,
                    content: vec![segment],
                });
            }
        }
    }
    let bot = Bot::of(call.ctx().framework());
    bot.send(
        call.from().target(),
        OutgoingMessage::forward(format!("{}推送模板预览", kind.label()), nodes),
    )
    .await;
    None
}

/// 转发里的一个文本节点
fn node(name: &str, uin: i64, text: String) -> ForwardMessage {
    ForwardMessage {
        name: name.to_string(),
        uin,
        content: vec![MessageSegment::Text(text)],
    }
}

/// `/bili login`：出二维码 → 每 3 秒问一次 → 拿到 cookie 写回配置 → 撤掉码和提示。
/// 只有管理员能发（二维码等于登录凭证，留在群里谁扫到都是麻烦）。
pub(crate) async fn login(call: &Call) -> Reply {
    if !call.is_bot_admin() {
        return reply("仅bot管理员可进行登录");
    }
    let bili = call.bili();
    let (url, key) = match bili.qrcode().await {
        Ok(pair) => pair,
        Err(reason) => return reply(format!("获取二维码失败 {reason}")),
    };
    let bot = Bot::of(call.ctx().framework());
    let target = call.from().target();
    let mut sent: Vec<i64> = Vec::new();
    let message = match crate::draw::card::qr_png(&url, 360) {
        Some(png) => OutgoingMessage::image_data(png),
        // 二维码点阵画不出来就把链接发出去，手机上打开一样能扫
        None => OutgoingMessage::text(format!("二维码画不出来，请在浏览器打开：{url}")),
    };
    if let Some(id) = bot.send(target, message).await.message_id {
        sent.push(id);
    }
    let prompt = bot
        .send(
            target,
            OutgoingMessage::text("请使用BiliBili手机APP扫码登录 3分钟有效"),
        )
        .await;
    if let Some(id) = prompt.message_id {
        sent.push(id);
    }
    let outcome = wait_for_scan(&bili, &key).await;
    // 收尾把码撤了：一张没人扫的二维码留在群里就是别人登你号的入口
    let api = call.from().api();
    for id in sent {
        let _ = api.delete_msg(id).await;
    }
    match outcome {
        Err(reason) => reply(format!("登录失败 {reason}")),
        Ok((cookie, uid)) => {
            let entry = call.ctx().config::<BiliConfig>(SECTION);
            if let Err(reason) = entry.update(|conf| {
                conf.account.cookie = cookie;
                conf.account.uid = uid;
            }) {
                return reply(format!(
                    "登录成功，但 Cookie 没写进配置（{reason}），请手工填 bili.account.cookie"
                ));
            }
            reply("登录成功!")
        }
    }
}

/// 三分钟窗口，每 3 秒问一次扫码结果。成功返回 (cookie 串, DedeUserID)
async fn wait_for_scan(bili: &Arc<Bili>, key: &str) -> Result<(String, i64), String> {
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let (code, message, cookie) = bili.poll(key).await?;
        match code {
            0 => {
                let cookie = cookie.ok_or("B 站没给出 Cookie")?;
                let uid = cookie
                    .split(';')
                    .filter_map(|part| part.trim().split_once('='))
                    .find(|(name, _)| *name == "DedeUserID")
                    .and_then(|(_, value)| value.parse::<i64>().ok())
                    .unwrap_or_default();
                return Ok((cookie, uid));
            }
            86038 => return Err("二维码已过期，重新发一次 login".to_string()),
            // 86101 还没人扫，86090 扫了还没在手机上点确认——两种都继续等
            86101 | 86090 => {}
            _ => return Err(format!("(code {code}) {message}")),
        }
    }
    Err("三分钟没人扫码".to_string())
}

/// 一次性配置清单。`/bili config` 现在顶给交互向导了（见 `wizard`），这张表挂在
/// 向导里的 `全局` 那一档下面——原插件把两件事分在同一个子命令上做不了，
/// 因为它的向导会吃掉随后每一条消息。
pub(crate) fn overview(call: &Call) -> Vec<String> {
    let uid = match call.sub_uid(0) {
        Ok(value) => value,
        Err(reason) => return vec![reason],
    };
    let targets = match call.targets(1) {
        Ok(value) => value,
        Err(reason) => return vec![reason],
    };
    let keys = match targets.writable() {
        Ok(list) => list.to_vec(),
        Err(reason) => return vec![reason],
    };
    let bili = call.conf();
    let appearance = call.ctx().config::<ImageConfig>(IMAGE_SECTION).get();
    let templates = call.ctx().config::<TemplateConfig>(TEMPLATE_SECTION).get();
    let mut out: Vec<String> = vec![
        "bilibili 全局配置".to_string(),
        format!(
            "总开关 {}｜管理员 {:?}｜操作通报 {}｜加载中占位 {}",
            bili.enable,
            bili.admin,
            on_off(bili.notify_enable),
            on_off(bili.show_loading_message)
        ),
        format!(
            "命令名 {}｜检测间隔 动态 {} 秒 / 直播 {} 秒｜低频 {}｜关注流翻页 {} 页",
            call.prefix(),
            bili.check.interval,
            bili.check.live_interval,
            bili.check.low_speed,
            bili.check.timeline_pages
        ),
        format!(
            "节流 消息间 {} 毫秒 / 目标间 {} 毫秒｜短链 {}｜@全体 挂法 {}｜结束通知 {}",
            bili.push.message_interval,
            bili.push.push_interval,
            if bili.push.to_short_link {
                "开"
            } else {
                "关"
            },
            bili.push.at_all_plus,
            if bili.push.live_close_notify {
                "开"
            } else {
                "关"
            }
        ),
        format!(
            "B 站账号 uid={} 登录态 {}",
            bili.account.uid,
            if bili.account.signed() { "有" } else { "无" }
        ),
        format!(
            "绘图 {}（标签 左{}）画质={} 主题={} 默认配色={} 页脚={}",
            if appearance.draw_enable { "开" } else { "关" },
            if appearance.badge_enable.left {
                "开"
            } else {
                "关"
            },
            appearance.quality,
            appearance.theme,
            appearance.default_color,
            appearance.card_ornament
        ),
        format!(
            "默认模板 动态={} 直播={} 直播结束={}",
            templates.default_dynamic_push,
            templates.default_live_push,
            templates.default_live_close
        ),
        String::new(),
        format!("目标 {} 自己那一档：", targets.label()),
    ];
    for key in &keys {
        let state = crate::store()
            .read(|data| data.targets.get(key).cloned())
            .unwrap_or_default();
        out.extend(target_lines(key, uid, &state, &appearance));
    }
    out
}

/// 一个目标的配置明细。`uid` 为 0 时把所有订阅者的分档都列出来，否则只列那一档。
fn target_lines(key: &str, uid: i64, state: &TargetState, appearance: &ImageConfig) -> Vec<String> {
    let mut out = vec![format!(
        "[{key}] {}订阅 {} 个 UP主、{} 部番剧｜配色 {}｜静音 {}",
        if state.name.is_empty() {
            String::new()
        } else {
            format!("{}｜", state.name)
        },
        state.subs.len(),
        state.seasons.len(),
        match state.color.as_deref().map(str::trim) {
            Some(own) if !own.is_empty() => own.to_string(),
            // 没设过就是跟随全局。把实际生效的色一并写出来——只写"默认"的话，用户改了
            // `image.defaultColor` 也无从判断这个群到底会出哪种底色
            _ => format!("默认({})", appearance.color_for(None)),
        },
        if state.muted { "是" } else { "否" }
    )];
    out.push(format!(
        "  模板覆盖 动态={} 直播={} 直播结束={}",
        dash(state.templates.get("dynamic")),
        dash(state.templates.get("live")),
        dash(state.templates.get("liveClose"))
    ));
    for (slot, set) in &state.filters {
        if uid != 0 && *slot != uid.to_string() {
            continue;
        }
        let types = if set.types.is_empty() {
            "无".to_string()
        } else {
            set.types.join("、")
        };
        let regs = if set.regs.is_empty() {
            "无".to_string()
        } else {
            set.regs.join("、")
        };
        out.push(format!(
            "  过滤器（{}）类型[{}] {types}｜正则[{}] {regs}",
            if slot == "0" { "全部订阅" } else { slot },
            set.type_mode.label(),
            set.reg_mode.label(),
        ));
    }
    for (slot, kinds) in &state.at_all {
        if uid != 0 && *slot != uid.to_string() {
            continue;
        }
        out.push(format!(
            "  @全体（{}）{}",
            if slot == "0" { "全部订阅" } else { slot },
            kinds
                .iter()
                .map(String::as_str)
                .collect::<Vec<&str>>()
                .join("、")
        ));
    }
    if state.subs.contains(&0) {
        out.push("  已订阅「全部动态」（走关注时间线，要有登录态）".to_string());
    }
    out
}

/// `Option<&String>` 打成"—"或原值，清单里读着整齐
fn dash(value: Option<&String>) -> String {
    value.cloned().unwrap_or_else(|| "—".to_string())
}

/// `/bili search <动态ID>`：取一条动态画出来给当前会话看。
///
/// 要联网的慢命令统一包一层：按 `bili.showLoadingMessage` 先发「加载中…」，结果出来后撤掉
/// （原插件 `DynamicCommand.kt:314/343/363` 那一档）。包在外面而不是写在正文里，是因为
/// 这几条每条都有十来条提前 `return` 的失败支路，靠人记着撤占位迟早漏一条。
pub(crate) async fn search(call: &Call) -> Reply {
    let loading = call.loading().await;
    let answer = search_body(call).await;
    call.dismiss_loading(loading).await;
    answer
}

async fn search_body(call: &Call) -> Reply {
    let raw = match call.need(0, &usage_str(call, Cmd::Search)) {
        Ok(value) => value,
        Err(reason) => return reply(reason),
    };
    let Ok(did) = raw.trim().parse::<i64>() else {
        return reply("动态 ID 要写数字");
    };
    let bili = call.bili();
    let found = match bili.detail(did).await {
        Ok(found) => found,
        Err(reason) => return reply(format!("获取动态失败 {reason}")),
    };
    let Some(dynamic) = found else {
        return reply("未找到动态");
    };
    show(call, &bili, &dynamic).await
}

/// `/bili live`：看一间正在播的。有登录态就报关注列表里的第一间，
/// 没有就退一步只在自己订阅的那批人里挑在线人数最高的。
pub(crate) async fn live(call: &Call) -> Reply {
    let bili = call.bili();
    let rooms = if bili.signed() {
        bili.live_list(1).await
    } else {
        let uids: Vec<i64> = crate::store()
            .read(|data| data.subscribed_uids())
            .into_iter()
            .filter(|uid| *uid > 0)
            .collect();
        bili.live_status(&uids).await
    };
    let rooms = match rooms {
        Ok(rooms) => rooms,
        Err(reason) => return reply(format!("获取直播失败 {reason}")),
    };
    let Some(info) = rooms
        .into_iter()
        .filter(LiveInfo::living)
        .max_by_key(|room| room.online)
    else {
        return reply("当前没有人在直播");
    };
    let (key, color) = local_target(call);
    let post = tasks::preview_live(call.ctx(), &bili, &info, &color).await;
    if push::send_preview(call.ctx(), &key, &post).await == 0 {
        return reply("发送失败，检查这个目标还在不在");
    }
    None
}

/// 配置回显里那两个字的开关写法
fn on_off(flag: bool) -> &'static str {
    if flag { "开" } else { "关" }
}

/// `/bili new <UID|名字> [条数]`：这个 UP 最新的几条
pub(crate) async fn new(call: &Call) -> Reply {
    let loading = call.loading().await;
    let answer = new_body(call).await;
    call.dismiss_loading(loading).await;
    answer
}

async fn new_body(call: &Call) -> Reply {
    let raw = match call.need(0, &usage_str(call, Cmd::New)) {
        Ok(value) => value,
        Err(reason) => return reply(reason),
    };
    let count = match call.arg(1) {
        None => 1usize,
        Some(value) => match value.trim().parse::<usize>() {
            Ok(0) | Err(_) => return reply("条数要写 1 到 10 的数字"),
            Ok(found) => found.min(10),
        },
    };
    let uid = match one_uid(&raw) {
        Ok(uid) => uid,
        Err(reason) => return reply(reason),
    };
    let bili = call.bili();
    let items = match bili.space(uid, "").await {
        Ok(items) => items,
        Err(reason) => return reply(format!("获取动态失败 {reason}")),
    };
    let fresh = newest_first(items);
    if fresh.is_empty() {
        return reply("未找到动态");
    }
    for dynamic in fresh.iter().take(count) {
        show(call, &bili, dynamic).await;
    }
    None
}

/// `/bili video <UID|名字>`：这个 UP 最新一条视频动态。
/// 原插件走的是 `searchUserVideo`（拿 bvid 画一张视频卡），那个接口现在必须带登录态，
/// 所以这里改成在动态流里挑**时间最新**那条"视频"类型的——效果一样，少一次签名依赖。
pub(crate) async fn video(call: &Call) -> Reply {
    let loading = call.loading().await;
    let answer = video_body(call).await;
    call.dismiss_loading(loading).await;
    answer
}

async fn video_body(call: &Call) -> Reply {
    let raw = match call.need(0, &usage_str(call, Cmd::Video)) {
        Ok(value) => value,
        Err(reason) => return reply(reason),
    };
    let uid = match one_uid(&raw) {
        Ok(uid) => uid,
        Err(reason) => return reply(reason),
    };
    let bili = call.bili();
    let items = match bili.space(uid, "").await {
        Ok(items) => items,
        Err(reason) => return reply(format!("获取动态失败 {reason}")),
    };
    match newest_first(items).iter().find(|item| item.kind == "视频") {
        Some(dynamic) => show(call, &bili, dynamic).await,
        None => reply("未找到视频"),
    }
}

/// 把一条动态按当前会话那套模板发出去看一眼
async fn show(call: &Call, bili: &Arc<Bili>, dynamic: &Dynamic) -> Reply {
    let (key, color) = local_target(call);
    let post = tasks::preview(call.ctx(), bili, dynamic, &color).await;
    if push::send_preview(call.ctx(), &key, &post).await == 0 {
        return reply("发送失败，检查这个目标还在不在");
    }
    None
}

/// 「最新 N 条」的取法：**按时间倒序，不信页序**。
///
/// 两个坑叠在一起：`feed/space` 会把**置顶**那条塞在第一格，而这个接口不返回任何置顶标记
/// （2026-10-16 实测蔚蓝档案官方：第一格 `item_control` 是 null，`id_str` 反而比后面两条小）；
/// 而 [`parse_feed`](crate::bili::model::parse_feed) 为了检测任务是**按时间升序**排的。
/// 两头一凑，"取前 N 条"取到的正是整页**最旧**那条——`/bili new 蔚蓝档案` 于是把 9 月的置顶
/// 抽奖动态当"最新"发了出来，而原插件（`getUserNewDynamic(hasTop=false)`，打的是不含置顶的
/// 关注流）给的是当天那条。原插件那边不需要这条排序：它拿的列表本身就是最新在前。
fn newest_first(mut items: Vec<Dynamic>) -> Vec<Dynamic> {
    items.sort_by(|a, b| b.time.cmp(&a.time).then(b.id.cmp(&a.id)));
    items
}

/// `[UP主UID|名字]` 取成一个 uid，取不干净就把原话回出去
fn one_uid(raw: &str) -> Result<i64, String> {
    match match_uid(raw) {
        Match::One(uid) => Ok(uid),
        found => Err(found
            .explain()
            .unwrap_or_else(|| "未匹配到用户哦".to_string())),
    }
}

/// 当前会话的目标键，以及它自己那一份配色（没设过就用全局默认色）
fn local_target(call: &Call) -> (String, String) {
    let key = call.current_key();
    let own =
        crate::store().read(|data| data.targets.get(&key).and_then(|state| state.color.clone()));
    let color = call
        .ctx()
        .config::<ImageConfig>(IMAGE_SECTION)
        .get()
        .color_for(own.as_deref());
    (key, color)
}

/// `/bili clear` 那份待确认清单。键是「会话号#发起人」——原插件那句
/// `fromEvent.nextMessage(300000)` 等的就是「发命令这个人在这个会话里的下一句话」，
/// 用表而不是单个槽位，是为了让两个人各清各的清单时不互相顶掉。
#[derive(Clone)]
struct Pending {
    keys: Vec<String>,
    expires: Instant,
}

static CONFIRMS: Lazy<RwLock<HashMap<String, Pending>>> = Lazy::new(|| RwLock::new(HashMap::new()));

/// 原插件 `nextMessage(300000)` 那五分钟
const CONFIRM_WINDOW: Duration = Duration::from_secs(300);

/// 挂上「等那一句答复」那一跳。与向导同一档优先级（排在链接解析之前）：
/// 正在等确认的人打的那句「确定」不该被别的钩子拿去解析。
/// 同一个人如果同时开着 `/bili config` 的向导，那句话会先被向导吃掉当编号用——
/// 向导注册在前，而这份清单又不该被一个编号悄悄删掉，所以留着直到五分钟过。
pub(crate) fn register_confirm(ctx: &PluginContext) {
    let holder = ctx.clone();
    ctx.listen_feature(
        crate::FEATURE,
        &[BodyFilter::GroupMessage, BodyFilter::PrivateMessage],
        ListenerPriority::High,
        event_handler(move |event: Arc<EventContext>| {
            let ctx = holder.clone();
            Box::pin(async move { confirm_step(&ctx, &event).await })
        }),
    );
}

/// 一份清单只许答复一次，答复完（或认了过期）就从表里摘掉
fn forget(id: &str) {
    if let Ok(mut table) = CONFIRMS.write() {
        table.remove(id);
    }
}

/// 那一句答复到了。`确定` 就真删，其它任何一句都算放弃——原插件那句
/// `else sendMessage("已取消")` 就是这个意思；超时则整条静默作废，不追一句「过期了」。
/// 唯一例外是又打了一条 `/bili` 命令：命令照常走分发，清单留到过期为止。
async fn confirm_step(ctx: &PluginContext, event: &EventContext) -> HookFlow {
    let id = super::wizard::key(
        &super::wizard::contact_of(event.group_id(), event.user_id()),
        event.user_id(),
    );
    let Some(found) = CONFIRMS
        .read()
        .ok()
        .and_then(|table| table.get(&id).cloned())
    else {
        return HookFlow::Pass;
    };
    if found.expires <= Instant::now() {
        forget(&id);
        return HookFlow::Pass;
    }
    let input = super::wizard::typed(event);
    if input.is_empty() || super::wizard::is_command(ctx, &input) {
        return HookFlow::Pass;
    }
    forget(&id);
    if input != "确定" {
        let _ = event.reply("已取消").await;
        return HookFlow::Handled;
    }
    let removed = crate::store().write(|data| data.remove_targets(&found.keys));
    let _ = event
        .reply(format!(
            "删除成功（清掉 {} 个失效目标，共 {removed} 条）",
            found.keys.len()
        ))
        .await;
    HookFlow::Handled
}

/// `/bili clear`：列出订阅记录里 bot 已经不在的群、已经不是好友的人，然后等那一句「确定」。
/// 与原插件同形（`DynamicCommand.kt:452-464`）：清单发出来就完事，答复走 [`confirm_step`]
/// 那条钩子，不必再打一次命令。确认之后是 `remove_targets`——条目整条删掉，
/// 因为这份清单本来就是从 `targets` 的键扫出来的，只清内容的话同一个失效群会永远重复上榜。
pub(crate) async fn clear(call: &Call) -> Reply {
    if !call.is_bot_admin() {
        return reply("权限不足，仅bot管理员可操作");
    }
    let bot = Bot::of(call.ctx().framework());
    // 两边各查一次；哪一边查不到就不判那一边的目标，免得把还好好的订阅清了
    let groups = bot.groups().await.map(|list| {
        list.into_iter()
            .map(|group| group.id())
            .collect::<std::collections::BTreeSet<i64>>()
    });
    let friends = bot.friends().await.map(|list| {
        list.into_iter()
            .map(|friend| friend.user_id())
            .collect::<std::collections::BTreeSet<i64>>()
    });
    if groups.is_err() && friends.is_err() {
        return reply(
            "实现端读不到群列表与好友列表（get_group_list / get_friend_list），没法判断哪些目标已失效",
        );
    }
    let keys: Vec<String> = crate::store().read(|data| data.targets.keys().cloned().collect());
    let mut dead: Vec<String> = Vec::new();
    for key in keys {
        let Some((is_group, id)) = store::parse_key(&key) else {
            continue;
        };
        let gone = if is_group {
            groups
                .as_ref()
                .map(|list| !list.contains(&id))
                .unwrap_or(false)
        } else {
            friends
                .as_ref()
                .map(|list| !list.contains(&id))
                .unwrap_or(false)
        };
        if gone {
            dead.push(key);
        }
    }
    if dead.is_empty() {
        return reply("未找到失效的群/好友");
    }
    if let Ok(mut table) = CONFIRMS.write() {
        let now = Instant::now();
        table.retain(|_, live| live.expires > now);
        table.insert(
            super::wizard::key(
                &super::wizard::contact_of(call.from().group_id, call.from().user_id),
                call.from().user_id,
            ),
            Pending {
                keys: dead.clone(),
                expires: now + CONFIRM_WINDOW,
            },
        );
    }
    let mut out: Vec<String> = vec![String::new(), "发现以下失效的群/好友：".to_string()];
    out.extend(dead.iter().cloned());
    out.extend(vec![
        String::new(),
        "g 开头是群，f 开头是好友。确认删除这些目标的订阅吗（配色与模板覆盖会一起删掉）"
            .to_string(),
        "请回复 '确定' 或 '取消'（五分钟内有效）".to_string(),
    ]);
    lines(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_target_lines_read_well() {
        let mut state = TargetState::default();
        state.subs.insert(123);
        state.muted = true;
        let out = target_lines("g1", 0, &state, &ImageConfig::default());
        assert!(out[0].contains("[g1]"));
        assert!(out[0].contains("静音 是"));
        assert!(out.iter().any(|line| line.contains("模板覆盖")));
    }

    /// 没设过配色的目标要显示"默认(实际生效色)"：只写"默认"的话，用户改完
    /// `image.defaultColor` 也判断不出这个群的推送到底会不会变
    #[test]
    fn unset_color_shows_the_effective_default() {
        let appearance = ImageConfig {
            default_color: "#fde8ed".to_string(),
            ..Default::default()
        };
        let state = TargetState::default();
        let out = target_lines("g1", 0, &state, &appearance);
        assert!(
            out[0].contains("默认(#fde8ed)"),
            "没设配色时该把跟随的全局色写出来：{out:?}"
        );

        let own = TargetState {
            color: Some("#ff8800".into()),
            ..Default::default()
        };
        let out = target_lines("g1", 0, &own, &appearance);
        assert!(out[0].contains("配色 #ff8800"), "{out:?}");
    }

    #[test]
    fn a_wildcard_slot_is_spelled_out() {
        let mut state = TargetState::default();
        state.subs.insert(0);
        let out = target_lines("f1", 0, &state, &ImageConfig::default());
        assert!(
            out.iter().any(|line| line.contains("全部动态")),
            "订了通配档却没在清单里说出来：{out:?}"
        );
    }

    /// 「最新动态」拿到的必须是**时间最新**那条。真实现场（2026-10-16，蔚蓝档案官方）：
    /// `feed/space` 把 9 月那条置顶塞在第一格、且不带任何置顶标记（`item_control` 是 null），
    /// 而 `parse_feed` 为了检测任务是按时间**升序**排的——两头一凑，"取前 N 条"取到的正是最旧那条，
    /// 用户拿原插件的截图对出来才发现
    #[test]
    fn latest_means_newest_not_first_on_the_page() {
        let item = |id: i64, time: i64| Dynamic {
            id,
            uid: 3_493_265_644_980_448,
            time,
            ..Default::default()
        };
        // 这就是修之前 new_body 拿到的顺序：置顶那条在最前
        let page = vec![
            item(1_251_109_903_019_278_338, 1_758_600_000),
            item(1_259_633_164_731_547_654, 1_760_000_000),
            item(1_259_648_628_710_965_297, 1_760_003_600),
        ];
        let fresh = newest_first(page);
        assert_eq!(
            fresh[0].id, 1_259_648_628_710_965_297,
            "第一条该是当天那条投稿，不是几个月前的置顶"
        );
        assert_eq!(
            fresh.last().unwrap().id,
            1_251_109_903_019_278_338,
            "置顶那条该落到末尾"
        );
        // 时间相同就按 id 倒序，别把顺序交给随机
        assert_eq!(newest_first(vec![item(11, 100), item(12, 100)])[0].id, 12);
    }
}
