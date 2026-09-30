//! 浏览与运维那一档子命令：重载配置、模板预览、扫码登录、看配置、看单条动态/直播、清失效目标。
//!
//! 这一层的共同点是"只看不改订阅关系"（`reload` 与 `clear` 除外）：
//! 发出去的东西一律走 [`push::send_preview`] 与 [`crate::tasks::preview`]，
//! 和真实推送共用同一条取素材、画图、解析模板的路径——
//! 用户在命令里看到的效果图，就是稍后订阅者收到的那张。
//!
//! 与原插件的几处刻意为之的差别，都记在 README：
//! * 原插件有一档 `enableConfig.showLoadingMessage`（先发"加载中…"再撤回）。本插件没这一档配置，
//!   慢的时候就是慢，不占一次额外发送。
//! * `login` 成功之后多回一句"登录成功!"，原插件只默默写 Cookie。
//! * `clear` 要等用户回一句"确定"，框架没有 `nextMessage` 这种"等下一条消息"的出口，
//!   所以改成两步命令（见 [`clear`]）。
//! * `templateList` 原来是"每条模板一个嵌套转发"。框架的转发只有一层，所以摊平成一个转发，
//!   条目之间靠"模板名"那一行分隔。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use arona::bot::Bot;
use arona::runtime::message::{ForwardMessage, MessageSegment, OutgoingMessage};

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

/// `/bili config [UID] [目标]`：把这个目标当前生效的配置打出来。
/// 原插件是一个"回复编号往下选"的交互向导，框架没有"等下一条消息"的出口，
/// 所以这里改成一次性的静态清单（要改哪一项，照着清单上的名字去发对应子命令）。
pub(crate) async fn config(call: &Call) -> Reply {
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
    let bili = call.conf();
    let appearance = call.ctx().config::<ImageConfig>(IMAGE_SECTION).get();
    let templates = call.ctx().config::<TemplateConfig>(TEMPLATE_SECTION).get();
    let mut out: Vec<String> = vec![
        "bilibili 全局配置".to_string(),
        format!("总开关 {}｜管理员 {:?}", bili.enable, bili.admin),
        format!(
            "命令名 {}｜检测间隔 动态 {} 秒 / 直播 {} 秒｜低频 {}",
            call.prefix(),
            bili.check.interval,
            bili.check.live_interval,
            bili.check.low_speed
        ),
        format!(
            "节流 消息间 {} 毫秒 / 目标间 {} 毫秒｜短链 {}｜@全体 挂法 {}",
            bili.push.message_interval,
            bili.push.push_interval,
            if bili.push.to_short_link {
                "开"
            } else {
                "关"
            },
            bili.push.at_all_plus
        ),
        format!(
            "B 站账号 uid={} 登录态 {}",
            bili.account.uid,
            if bili.account.signed() { "有" } else { "无" }
        ),
        format!(
            "绘图 画质={} 主题={} 默认配色={} 页脚={}",
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
        out.extend(target_lines(key, uid, &state));
    }
    lines(&out)
}

/// 一个目标的配置明细。`uid` 为 0 时把所有订阅者的分档都列出来，否则只列那一档。
fn target_lines(key: &str, uid: i64, state: &TargetState) -> Vec<String> {
    let mut out = vec![format!(
        "[{key}] {}订阅 {} 个 UP主、{} 部番剧｜配色 {}｜静音 {}",
        if state.name.is_empty() {
            String::new()
        } else {
            format!("{}｜", state.name)
        },
        state.subs.len(),
        state.seasons.len(),
        state.color.clone().unwrap_or_else(|| "默认".to_string()),
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

/// `/bili search <动态ID>`：取一条动态画出来给当前会话看
pub(crate) async fn search(call: &Call) -> Reply {
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

/// `/bili new <UID|名字> [条数]`：这个 UP 最新的几条
pub(crate) async fn new(call: &Call) -> Reply {
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
    if items.is_empty() {
        return reply("未找到动态");
    }
    for dynamic in items.iter().take(count) {
        show(call, &bili, dynamic).await;
    }
    None
}

/// `/bili video <UID|名字>`：这个 UP 最新一条视频动态。
/// 原插件走的是 `searchUserVideo`（拿 bvid 画一张视频卡），那个接口现在必须带登录态，
/// 所以这里改成在动态流里挑第一条"视频"类型的——效果一样，少一次签名依赖。
pub(crate) async fn video(call: &Call) -> Reply {
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
    match items.iter().find(|item| item.kind == "视频") {
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
    let color = crate::store()
        .read(|data| data.targets.get(&key).and_then(|state| state.color.clone()))
        .unwrap_or_else(|| {
            call.ctx()
                .config::<ImageConfig>(IMAGE_SECTION)
                .get()
                .default_color
        });
    (key, color)
}

/// 待确认的清理结果：谁发的、什么时候过期、要清哪几个目标
struct Pending {
    who: i64,
    until: i64,
    keys: Vec<String>,
}

static PENDING: OnceLock<Mutex<Option<Pending>>> = OnceLock::new();

fn pending() -> &'static Mutex<Option<Pending>> {
    PENDING.get_or_init(|| Mutex::new(None))
}

/// `/bili clear`：列出订阅记录里 bot 已经不在的群、已经不是好友的人。
/// 原插件在这里 `nextMessage(300000)` 等一句"确定"，框架没有这个出口，
/// 所以分两步：先 `{前缀} clear` 看清单，再 `{前缀} clear 确定` 真删。
pub(crate) async fn clear(call: &Call) -> Reply {
    if !call.is_bot_admin() {
        return reply("权限不足，仅bot管理员可操作");
    }
    match call.arg(0).unwrap_or("").trim() {
        "确定" => return confirm_clear(call).await,
        "取消" => {
            if let Ok(mut slot) = pending().lock() {
                *slot = None;
            }
            return reply("已取消");
        }
        _ => {}
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
    if let Ok(mut slot) = pending().lock() {
        *slot = Some(Pending {
            who: call.from().user_id,
            until: chrono::Utc::now().timestamp() + 300,
            keys: dead.clone(),
        });
    }
    let mut out: Vec<String> = vec![String::new(), "发现以下失效的群/好友：".to_string()];
    out.extend(dead.iter().cloned());
    out.extend(vec![
        String::new(),
        "g 开头是群，f 开头是好友。确认删除这些目标的订阅吗".to_string(),
        format!(
            "确认请再发一次 {} clear 确定，放弃请发 {} clear 取消（五分钟内有效）",
            call.prefix(),
            call.prefix()
        ),
    ]);
    lines(&out)
}

/// 第二步：按清单删订阅。清单过期或发命令的人换了，都算没这一步。
async fn confirm_clear(call: &Call) -> Reply {
    let Some(slot) = take_pending(call) else {
        return reply("没有待确认的清理，或已经过了五分钟。先发一次 clear 看清单");
    };
    let removed = crate::store().write(|data| data.clear_subs(&slot));
    reply(format!(
        "删除成功（清掉 {} 个目标的订阅，共 {removed} 条）",
        slot.len()
    ))
}

/// 取出待确认的清单，同时把它作废：确认这一步只许做一次
fn take_pending(call: &Call) -> Option<Vec<String>> {
    let mut slot = pending().lock().ok()?;
    let taken = slot.take()?;
    if taken.who != call.from().user_id {
        return None;
    }
    if chrono::Utc::now().timestamp() > taken.until {
        return None;
    }
    Some(taken.keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_target_lines_read_well() {
        let mut state = TargetState::default();
        state.subs.insert(123);
        state.muted = true;
        let out = target_lines("g1", 0, &state);
        assert!(out[0].contains("[g1]"));
        assert!(out[0].contains("静音 是"));
        assert!(out.iter().any(|line| line.contains("模板覆盖")));
    }

    #[test]
    fn a_wildcard_slot_is_spelled_out() {
        let mut state = TargetState::default();
        state.subs.insert(0);
        let out = target_lines("f1", 0, &state);
        assert!(
            out.iter().any(|line| line.contains("全部动态")),
            "订了通配档却没在清单里说出来：{out:?}"
        );
    }
}
