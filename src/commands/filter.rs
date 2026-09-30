//! 过滤器（类型/正则、黑/白名单）、模板覆盖与 @全体 这三组命令。
//!
//! 文案与判错逐条抄原插件 `service/FilterService.kt` 与 `service/AtAllService.kt`：
//! * `还未订阅此人哦`——过滤器与 @全体 都挂在"某个目标 × 某个订阅者"这一格上，
//!   没订过这个人就没有这一格。`uid` 留空（写 `0`）表示"对该目标的全部订阅生效"，
//!   这一档原插件的 `isFollow` 直接返回真，这里一致。
//! * `仅在群聊中有用哦` / `Bot不为管理员, 无法使用At全体`——@全体 只在群里成立，
//!   而且 bot 得是群主或管理员。原插件读的是 mirai 的 `botPermission.level`，
//!   这里换成框架的 `Bot::group(id).member_info(bot.self_id())`。
//! * 两套种类词是分开的：类型过滤认 `动态/转发动态/视频/音乐/专栏/直播`，
//!   @全体 认 `全部/全部动态/视频/音乐/专栏/直播`。见 `store::normalize_filter_kind`。
//!
//! 目标是分组名时，这几项都按**当前成员**批量写：原插件把过滤器存在分组名那个键上，
//! 而推送时按真实群号查，于是分组级过滤器与 @全体 永不生效——这里不复制那个 bug。

use arona::bot::Bot;

use crate::config::{TEMPLATE_SECTION, TemplateConfig};
use crate::store;

use super::{Call, Reply, Targets, reply, usage_str};

/// 原插件的 `isFollow`：`0` 这一档恒真，其余要在这个目标上订过
fn is_follow(targets: &Targets, uid: i64) -> bool {
    if uid == 0 {
        return true;
    }
    crate::store().read(|data| data.is_subscribed(&targets.keys, targets.cluster.as_deref(), uid))
}

/// 参数顺序统一：`[UID] [目标]`，uid 留空即 0
fn uid_and_targets(
    call: &Call,
    uid_at: usize,
    target_at: usize,
) -> Result<(i64, Vec<String>), String> {
    let uid = call.sub_uid(uid_at)?;
    let targets = call.targets(target_at)?;
    if !is_follow(&targets, uid) {
        return Err("还未订阅此人哦".to_string());
    }
    let keys = targets.writable()?.to_vec();
    Ok((uid, keys))
}

/// `/bili filtermode <t|r> <w|b> [UID] [目标]`：只改黑白名单模式
pub(crate) fn mode(call: &Call) -> Reply {
    let which = match call.arg(0) {
        Some(raw) if raw.eq_ignore_ascii_case("t") => "type",
        Some(_) => "reg",
        None => return reply(usage_str(call, super::Cmd::FilterMode)),
    };
    let Some(raw_mode) = call.arg(1) else {
        return reply(usage_str(call, super::Cmd::FilterMode));
    };
    let Some(mode) = store::Mode::parse(raw_mode) else {
        return reply("模式只能写 w(白名单) 或 b(黑名单)".to_string());
    };
    let (uid, keys) = match uid_and_targets(call, 2, 3) {
        Ok(pair) => pair,
        Err(reason) => return reply(reason),
    };
    crate::store().write(|data| {
        for key in &keys {
            data.add_filter(key, uid, which, Some(mode), None);
        }
    });
    reply("设置成功")
}

/// `/bili filtertype <种类> [UID] [目标]`：加一条类型过滤
pub(crate) fn kind(call: &Call) -> Reply {
    let Some(raw) = call.arg(0) else {
        return reply(usage_str(call, super::Cmd::FilterType));
    };
    let Some(kind) = store::normalize_filter_kind(raw) else {
        return reply(format!(
            "没有这个类型 {raw}，可选：{}",
            store::FILTER_KINDS.join("、")
        ));
    };
    let (uid, keys) = match uid_and_targets(call, 1, 2) {
        Ok(pair) => pair,
        Err(reason) => return reply(reason),
    };
    crate::store().write(|data| {
        for key in &keys {
            data.add_filter(key, uid, "type", None, Some(kind));
        }
    });
    reply("设置成功")
}

/// `/bili filterreg <正则> [UID] [目标]`：加一条正文正则过滤
pub(crate) fn regex(call: &Call) -> Reply {
    let Some(raw) = call.arg(0) else {
        return reply(usage_str(call, super::Cmd::FilterReg));
    };
    // 正则写坏了推送不会停（store::regex_ok 按不命中处理），但这里先告诉用户
    if regex::Regex::new(raw).is_err() {
        return reply(format!("正则有问题：{raw}"));
    }
    let (uid, keys) = match uid_and_targets(call, 1, 2) {
        Ok(pair) => pair,
        Err(reason) => return reply(reason),
    };
    crate::store().write(|data| {
        for key in &keys {
            data.add_filter(key, uid, "reg", None, Some(raw));
        }
    });
    reply("设置成功")
}

/// `/bili filterlist [UID] [目标]`
pub(crate) fn list(call: &Call) -> Reply {
    let (uid, keys) = match uid_and_targets(call, 0, 1) {
        Ok(pair) => pair,
        Err(reason) => return reply(reason),
    };
    let found = crate::store().read(|data| {
        keys.iter()
            .filter_map(|key| {
                data.targets
                    .get(key)
                    .and_then(|state| state.filters.get(&uid.to_string()))
                    .cloned()
                    .map(|filter| (key.clone(), filter))
            })
            .collect::<Vec<_>>()
    });
    if found.is_empty() {
        return reply("目标没有过滤器");
    }
    let mut out: Vec<String> = Vec::new();
    for (key, filter) in &found {
        if found.len() > 1 {
            out.push(format!("目标: {key}"));
        }
        if !filter.types.is_empty() {
            out.push(format!("动态类型过滤器: {}", filter.type_mode.label()));
            for (index, kind) in filter.types.iter().enumerate() {
                out.push(format!("  t{index}: {kind}"));
            }
            out.push(String::new());
        }
        if !filter.regs.is_empty() {
            out.push(format!("正则过滤器: {}", filter.reg_mode.label()));
            for (index, pattern) in filter.regs.iter().enumerate() {
                out.push(format!("  r{index}: {pattern}"));
            }
            out.push(String::new());
        }
    }
    reply(out.join("\n").trim_end().to_string())
}

/// `/bili filterdel <t0|r1> [UID] [目标]`
pub(crate) fn remove(call: &Call) -> Reply {
    let Some(index) = call.arg(0) else {
        return reply(usage_str(call, super::Cmd::FilterDel));
    };
    let (uid, keys) = match uid_and_targets(call, 1, 2) {
        Ok(pair) => pair,
        Err(reason) => return reply(reason),
    };
    // 原插件在多个目标上只有一个下标口径，这里同样按写进去的顺序取第一个命中的
    for key in &keys {
        let outcome = crate::store().write(|data| data.remove_filter(key, uid, index.trim()));
        match outcome {
            Ok(removed) => {
                return reply(if index.trim().starts_with('t') {
                    format!("已删除 {removed} 类型过滤")
                } else {
                    format!("已删除 {removed} 正则过滤")
                });
            }
            Err(reason) => {
                // 这一格没过滤器就接着看下一个成员，全是这样才回最后一句
                if keys.len() == 1 {
                    return reply(reason);
                }
            }
        }
    }
    reply("当前目标没有过滤器")
}

/// `/bili template <d|l|le> <模板名> [目标]`
pub(crate) fn template(call: &Call) -> Reply {
    let Some(raw_type) = call.arg(0) else {
        return reply(usage_str(call, super::Cmd::Template));
    };
    let Some((kind_key, table_name)) = template_kind(raw_type) else {
        return reply("类型错误 d:动态 l:直播 le:直播结束");
    };
    let name = match call.need(1, &usage_str(call, super::Cmd::Template)) {
        Ok(value) => value,
        Err(reason) => return reply(reason),
    };
    let targets = match call.targets(2) {
        Ok(targets) => targets,
        Err(reason) => return reply(reason),
    };
    let keys = match targets.writable() {
        Ok(keys) => keys.to_vec(),
        Err(reason) => return reply(reason),
    };
    let config = call.ctx().config::<TemplateConfig>(TEMPLATE_SECTION).get();
    let table = match table_name {
        "dynamic" => &config.dynamic_push,
        "live" => &config.live_push,
        _ => &config.live_close,
    };
    if !table.contains_key(&name) {
        return reply(format!("没有这个模板哦 {name}"));
    }
    crate::store().write(|data| data.set_template(&keys, kind_key, &name));
    reply(if targets.is_self() {
        "配置完成".to_string()
    } else {
        format!("{}配置完成", targets.grant())
    })
}

/// `d|l|le` → (存进 store 的键, 配置里的那张表)
pub(crate) fn template_kind(raw: &str) -> Option<(&'static str, &'static str)> {
    match raw.trim().to_lowercase().as_str() {
        "d" | "dynamic" | "动态" => Some(("dynamic", "dynamic")),
        "l" | "live" | "直播" => Some(("live", "live")),
        "le" | "liveclose" | "直播结束" => Some(("liveClose", "liveClose")),
        _ => None,
    }
}

/// @全体 只在群里成立，而且 bot 得有身份（原插件那两条报错）
async fn at_all_gate(call: &Call, key: &str) -> Result<(), String> {
    let Some((is_group, id)) = store::parse_key(key) else {
        return Err("仅在群聊中有用哦".to_string());
    };
    if !is_group {
        return Err("仅在群聊中有用哦".to_string());
    }
    let bot = Bot::of(call.ctx().framework());
    match bot.group(id).member_info(bot.self_id()).await {
        Ok(member) if member.is_owner() || member.is_admin() => Ok(()),
        Ok(_) => Err("Bot不为管理员, 无法使用At全体".to_string()),
        // 实现端没给身份字段时不拦：能不能 @ 出去归发送端
        Err(_) => Ok(()),
    }
}

/// `/bili atall [种类] [UID] [目标]` 与 `/bili delatall ...`
pub(crate) async fn at_all(call: &Call, add: bool) -> Reply {
    let raw = call.arg(0).unwrap_or("a");
    let Some(kind) = store::normalize_at_all_kind(raw) else {
        return reply(format!(
            "没有这个类型哦 [{}]，可选：{}",
            raw,
            store::AT_ALL_KINDS.join("、")
        ));
    };
    let uid = match call.sub_uid(1) {
        Ok(uid) => uid,
        Err(reason) => return reply(reason),
    };
    let targets = match call.targets(2) {
        Ok(targets) => targets,
        Err(reason) => return reply(reason),
    };
    let keys = match targets.writable() {
        Ok(keys) => keys.to_vec(),
        Err(reason) => return reply(reason),
    };
    let mut touched = 0usize;
    let mut blocked: Option<String> = None;
    for key in &keys {
        if let Err(reason) = at_all_gate(call, key).await {
            blocked.get_or_insert(reason);
            continue;
        }
        let hit = crate::store().write(|data| {
            if add {
                data.add_at_all(key, uid, kind);
                true
            } else {
                data.remove_at_all(key, uid, kind)
            }
        });
        if hit {
            touched += 1;
        }
    }
    if touched == 0 {
        return reply(match blocked {
            Some(reason) => reason,
            None if add => "添加成功".to_string(),
            None => "删除失败".to_string(),
        });
    }
    reply(if add { "添加成功" } else { "删除成功" })
}

/// `/bili listatall [UID] [目标]`
pub(crate) fn at_all_list(call: &Call) -> Reply {
    let uid = match call.sub_uid(0) {
        Ok(uid) => uid,
        Err(reason) => return reply(reason),
    };
    let targets = match call.targets(1) {
        Ok(targets) => targets,
        Err(reason) => return reply(reason),
    };
    let keys = match targets.writable() {
        Ok(keys) => keys.to_vec(),
        Err(reason) => return reply(reason),
    };
    let found = crate::store().read(|data| {
        let mut kinds = std::collections::BTreeSet::<String>::new();
        for key in &keys {
            if let Some(state) = data.targets.get(key) {
                kinds.extend(
                    state
                        .at_all
                        .get(&uid.to_string())
                        .cloned()
                        .unwrap_or_default(),
                );
            }
        }
        kinds
    });
    if found.is_empty() {
        return reply("没有At全体项哦");
    }
    reply(found.into_iter().collect::<Vec<String>>().join("\n"))
}
