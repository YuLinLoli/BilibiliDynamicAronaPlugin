//! 订阅关系这一组命令：`color` `add` `del` `delall` `list` `listall` `listuser`。
//!
//! 回复文案逐条抄原插件 `service/DynamicService.kt`，包括"之前订阅过这个人哦"、
//! "未关注此用户"、"此账号已被拉黑"这三句要照着做的提示。
//!
//! 有两处和本插件的存储形态有关：
//! * 订阅可以写在**分组**上（原插件把分组名直接当一个目标存），所以这里每个写操作
//!   都同时动"成员键"和"分组自己那份"，`store::Data` 的两个入口成对调用。
//! * 番剧号统一存成 `ss<season_id>`：`md`/`ep` 在命令里就先向 B 站换算一次，
//!   检测任务因此只认一种写法。

use crate::bili::api::Bili;
use crate::store;

use super::{Call, Cmd, Match, Reply, Targets, lines, match_uid, reply, season_id, usage_str};

/// 主题色校验：分号分隔的 `#rrggbb`，与原插件同一句报错
fn check_color(raw: &str) -> Result<(), String> {
    for part in raw.split([';', '；']) {
        let part = part.trim();
        if !part.starts_with('#') || part.len() != 7 {
            return Err("格式错误，请输入16进制颜色，如: #d3edfa".to_string());
        }
        if !part[1..].chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err("格式错误，请输入16进制颜色，如: #d3edfa".to_string());
        }
    }
    Ok(())
}

/// `/bili color <#rrggbb[;#rrggbb…]> [目标]`
///
/// 原插件把主题色记在**订阅者**上（`dynamic[uid].color`），本插件记在**目标**上
/// （一个群可以把它订的 UP 都换成同一套配色），所以参数是"颜色 + 目标"而不是"用户 + 颜色"。
pub(crate) async fn color(call: &Call) -> Reply {
    let raw = match call.need(0, "<#rrggbb[;#rrggbb…]> [目标]") {
        Ok(value) => value,
        Err(reason) => return reply(reason),
    };
    if let Err(reason) = check_color(&raw) {
        return reply(reason);
    }
    let targets = match call.targets(1) {
        Ok(targets) => targets,
        Err(reason) => return reply(reason),
    };
    let keys = match targets.writable() {
        Ok(keys) => keys.to_vec(),
        Err(reason) => return reply(reason),
    };
    crate::store().write(|data| data.set_color(&keys, &raw));
    reply(if targets.is_self() {
        "设置完成".to_string()
    } else {
        format!("{}主题色设置完成", targets.grant())
    })
}

/// `/bili mute [目标]` 与 `/bili unmute [目标]`：整体停推 / 复推某个目标。
/// 订阅关系、配色、模板、过滤器一概不动，只翻 `store.yml` 里那一格 `muted`——
/// 检测任务与推送路径本来就有三处按它跳过（`tasks/mod.rs`、`push/mod.rs`）。
pub(crate) fn mute(call: &Call, muted: bool) -> Reply {
    let targets = match call.targets(0) {
        Ok(targets) => targets,
        Err(reason) => return reply(reason),
    };
    let keys = match targets.writable() {
        Ok(keys) => keys.to_vec(),
        Err(reason) => return reply(reason),
    };
    crate::store().write(|data| data.set_muted(&keys, muted));
    let word = if muted {
        "静音设置完成，这个目标不再收任何推送"
    } else {
        "已恢复推送"
    };
    reply(if targets.is_self() {
        word.to_string()
    } else {
        format!("{}{word}", targets.grant())
    })
}

/// `/bili add <UP主UID|番剧号> [目标]`
pub(crate) async fn add(call: &Call) -> Reply {
    let raw = match call.need(0, &usage_str(call, Cmd::Add)) {
        Ok(value) => value,
        Err(reason) => return reply(reason),
    };
    let targets = match call.targets(1) {
        Ok(targets) => targets,
        Err(reason) => return reply(reason),
    };
    if let Some((kind, id)) = season_id(&raw) {
        return follow_season(call, &targets, kind, id).await;
    }
    // 原插件这里走的是 `id.toLong()`，名字要留给 del/atall 那几条才模糊匹配
    let Ok(uid) = raw.trim().parse::<i64>() else {
        return reply(format!("ID错误 [{raw}]"));
    };
    if already(&targets, uid) {
        return reply("之前订阅过这个人哦");
    }
    let bili = call.bili();
    let relation = if uid == 0 {
        // 通配位没有"这个人"可查关系，下面那半句提示也不看它
        Relation::Unconfirmed
    } else {
        match relation_guard(call, &bili, uid).await {
            Ok(state) => state,
            Err(reason) => return reply(reason),
        }
    };
    let name = if uid == 0 {
        // 通配位：原插件的 listenAll，推的是这个账号关注时间线里的全部动态
        "全部动态".to_string()
    } else {
        match bili.user(uid).await {
            Ok(brief) => brief.name,
            Err(reason) => {
                // 名字拿不到不拦订阅：水位线与检测都只认 uid，列表里先显示 uid
                arona::runtime::log::warning(format!(
                    "bilibili：{uid} 的名片读不出来（{reason}），订阅照旧，列表里先按 uid 显示"
                ));
                uid.to_string()
            }
        }
    };
    crate::store().write(|data| {
        data.subscribe(&targets.keys, uid, &name);
        if let Some(cluster) = &targets.cluster {
            data.follow_cluster(cluster, uid);
        }
    });
    let hint = if uid == 0 {
        wildcard_hint(bili.signed())
    } else {
        follow_hint(bili.signed(), relation)
    };
    reply(format!("{}订阅 {name} 成功!{hint}", targets.grant()))
}

/// 通配档在没有登录态时确实订上了，可它每一轮都拿不到内容（匿名 `feed/all` 不出包）——
/// 不提醒就是「回了一句成功，却永远不推」，那是最坏的一种成功。
///
/// 只提这一条，是因为只有这一档在匿名态下**整档失效**：普通 uid 走匿名 `feed/space`，照常推。
///
/// `signed()` 判的是 `api::client` 里 `with_cookie_file` 之后的那份账号态，所以 `cookies.json` 导入的
/// 登录态也算数——这里不能换成直接读配置串的那个 `signed()`，否则文件导入的人会被误告一句要扫码。
fn wildcard_hint(signed: bool) -> &'static str {
    if signed {
        ""
    } else {
        "（这一档走关注时间线，匿名拿不到内容，要 /bili login 扫码之后才生效）"
    }
}

/// 关注关系那一关放行时到底确认到了什么
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Relation {
    /// 这一轮真把这个人加进了关注列表
    Confirmed,
    /// 只是没被拦住：`attribute` 不是 0/128 所以放行，或者关系根本查不出来
    Unconfirmed,
}

/// 有登录态时检测走关注时间线，那份只覆盖这个账号**已关注**的人，所以「放行了却没确认到」这一档
/// 同样可能是「订上了、每一轮都 0 条」——形状与上面那句通配档一样，只是它不必然是坏的。
///
/// 文案只说「没确认到」，不说「你没关注」：`attribute` 的取值表这边没实测过（原插件 `Follow.kt` 那份
/// 注释给的是 0 未关注 / 2 已关注 / 6 已互粉 / 128 拉黑），代码里除了 0 与 128 都不判方向。
fn follow_hint(signed: bool, relation: Relation) -> &'static str {
    if !signed || relation == Relation::Confirmed {
        return "";
    }
    "（没确认到这个账号已经关注他，而登录态下的检测只看关注时间线；\
     真没关注就一条都不会推——先关注，或把 account.autoFollow 打开）"
}

/// 这个 uid 在这批目标上订过没有
fn already(targets: &Targets, uid: i64) -> bool {
    crate::store().read(|data| data.is_subscribed(&targets.keys, targets.cluster.as_deref(), uid))
}

/// 关注关系这一关（原插件 `followUser`）：没登录态就整个跳过。
///
/// 返回 `Err` 表示这条订阅不该继续。查关系本身失败只记日志——
/// B 站的 `-412` 之类风控不该让用户以为"订阅功能坏了"。
/// 放行不等于确认，所以成功的一面带出 `Relation`，由调用方决定要不要多说那半句。
async fn relation_guard(call: &Call, bili: &Bili, uid: i64) -> Result<Relation, String> {
    if bili.uid() == uid {
        // 订的是本账号自己，关注关系这一问没有对象
        return Ok(Relation::Confirmed);
    }
    if !bili.signed() {
        return Ok(Relation::Unconfirmed);
    }
    let account = &call.conf().account;
    match bili.relation(uid).await {
        Ok(0) => {
            if !account.auto_follow {
                return Err("未关注此用户".to_string());
            }
            bili.follow(uid)
                .await
                .map_err(|reason| format!("关注失败: {reason}"))?;
            if !account.follow_group.is_empty()
                && let Err(reason) = move_to_tag(bili, uid, &account.follow_group).await
            {
                arona::runtime::log::warning(format!("bilibili：移动分组失败：{reason}"));
            }
            arona::runtime::log::info(format!("bilibili：账号已关注 {uid}"));
            Ok(Relation::Confirmed)
        }
        Ok(128) => Err("此账号已被拉黑".to_string()),
        Ok(_) => Ok(Relation::Unconfirmed),
        Err(reason) => {
            arona::runtime::log::warning(format!(
                "bilibili：{uid} 的关注关系查不出来（{reason}），订阅照旧"
            ));
            Ok(Relation::Unconfirmed)
        }
    }
}

/// 按名字找到（没有就建一个）B 站分组，把人放进去
async fn move_to_tag(bili: &Bili, uid: i64, tag: &str) -> Result<(), String> {
    let tags = bili.tags().await?;
    if let Some((id, _)) = tags.iter().find(|(_, name)| name == tag) {
        return bili.add_to_tag(uid, *id).await;
    }
    bili.create_tag(tag).await?;
    let id = bili
        .tags()
        .await?
        .into_iter()
        .find(|(_, name)| name == tag)
        .map(|(id, _)| id)
        .ok_or_else(|| format!("建了分组「{tag}」又没查到它"))?;
    bili.add_to_tag(uid, id).await
}

/// 追番：`md`/`ep` 先换算成季度号，再按 `ss<号>` 记进状态
async fn follow_season(call: &Call, targets: &Targets, kind: &str, id: i64) -> Reply {
    let bili = call.bili();
    let season = match kind {
        "ss" => bili.season(id).await,
        "ep" => bili.season_by_ep(id).await,
        _ => bili.media(id).await,
    };
    let season = match season {
        Ok(season) => season,
        Err(reason) => {
            return reply(if kind == "md" {
                format!("{reason}，如果是港澳台番剧请确认 md 号对不对")
            } else {
                format!("获取番剧信息失败: {reason}")
            });
        }
    };
    if season.season_id == 0 {
        return reply("获取番剧信息失败, 如果是港澳台番剧请用 media id (md11111) 订阅");
    }
    // B 站那边的追番只是"顺便"：没登录态就只记本地订阅，检测照样跑得起来
    if let Err(reason) = bili.follow_pgc(season.season_id).await {
        arona::runtime::log::warning(format!(
            "bilibili：没能在 B 站那边追上《{}》（{reason}），只记本地订阅",
            season.title
        ));
    }
    let text = format!("ss{}", season.season_id);
    crate::store().write(|data| {
        data.follow_season(&targets.keys, &text, &season.title);
        if let Some(cluster) = &targets.cluster {
            data.follow_cluster_season(cluster, &text);
        }
    });
    reply(format!(
        "{}追番成功( •̀ ω •́ )✧ [{}]",
        targets.grant(),
        season.title
    ))
}

/// `/bili del <UP主UID|名字|番剧号> [目标]`
pub(crate) async fn del(call: &Call) -> Reply {
    let raw = match call.need(0, &usage_str(call, Cmd::Del)) {
        Ok(value) => value,
        Err(reason) => return reply(reason),
    };
    let targets = match call.targets(1) {
        Ok(targets) => targets,
        Err(reason) => return reply(reason),
    };
    if let Some((kind, id)) = season_id(&raw) {
        return drop_season(call, &targets, kind, id).await;
    }
    let uid = match match_uid(&raw) {
        Match::One(uid) => uid,
        found => return reply(found.explain().unwrap_or_default()),
    };
    if !already(&targets, uid) {
        return reply("还未订阅此人哦");
    }
    let name = names(uid);
    crate::store().write(|data| {
        data.unsubscribe(&targets.keys, uid);
        if let Some(cluster) = &targets.cluster {
            data.unfollow_cluster(cluster, uid);
        }
    });
    reply(format!("{}取消订阅 {name} 成功", targets.grant()))
}

/// 取消追番：`ep` 那一路原插件也做不到（集数号换算不出唯一的季号），文案照抄
async fn drop_season(call: &Call, targets: &Targets, kind: &str, id: i64) -> Reply {
    if kind == "ep" {
        return reply("无法通过ep进行删除，请使用 ss 或 md");
    }
    let text = if kind == "ss" {
        format!("ss{id}")
    } else {
        // md 号本地没存，只能再问一次 B 站换算
        match call.bili().media(id).await {
            Ok(season) => format!("ss{}", season.season_id),
            Err(_) => return reply("没有这个番剧哦"),
        }
    };
    let known = crate::store().read(|data| {
        data.listed(&targets.keys, targets.cluster.as_deref())
            .1
            .contains(&text)
    });
    if !known {
        return reply("没有订阅这个番剧哦");
    }
    crate::store().write(|data| {
        data.unfollow_season(&targets.keys, &text);
        if let Some(cluster) = &targets.cluster {
            data.unfollow_cluster_season(cluster, &text);
        }
    });
    // 一个目标都不再订这部了，才顺手在 B 站那边取关：那边的追番列表是账号级的，
    // 别的目标还订着就取关，等于把人家在 B 站的订阅悄悄抹掉了。
    let orphaned = crate::store().read(|data| {
        !data
            .season_targets()
            .iter()
            .any(|(_, ids)| ids.contains(&text))
    });
    if orphaned
        && let Ok(season_id) = text.trim_start_matches("ss").parse::<i64>()
        && let Err(reason) = call.bili().unfollow_pgc(season_id).await
    {
        arona::runtime::log::warning(format!(
            "bilibili：没能在 B 站那边取关 {text}（{reason}），本地订阅已经删掉了"
        ));
    }
    reply("删除成功")
}

/// 本地存着的订阅者名字（没有就退回 uid）
fn names(uid: i64) -> String {
    crate::store().read(|data| {
        data.names
            .get(&uid.to_string())
            .cloned()
            .unwrap_or_else(|| uid.to_string())
    })
}

/// `/bili delall [目标]`
pub(crate) async fn del_all(call: &Call) -> Reply {
    let targets = match call.targets(0) {
        Ok(targets) => targets,
        Err(reason) => return reply(reason),
    };
    let removed = crate::store().write(|data| {
        let mut count = data.clear_subs(&targets.keys);
        if let Some(cluster) = &targets.cluster {
            count += data.clear_cluster(cluster);
        }
        count
    });
    reply(format!("删除订阅成功! 共删除 {removed} 个订阅"))
}

/// `/bili list [目标]`——原插件那份"目标 / UP主 / 番剧 / 共 N 个订阅"
pub(crate) async fn list(call: &Call) -> Reply {
    let targets = match call.targets(0) {
        Ok(targets) => targets,
        Err(reason) => return reply(reason),
    };
    let (uids, seasons) =
        crate::store().read(|data| data.listed(&targets.keys, targets.cluster.as_deref()));
    let mut out = vec![
        format!("目标: {}", targets.label()),
        String::new(),
        "UP主:".to_string(),
    ];
    if uids.is_empty() {
        out.push("无".to_string());
    } else {
        out.extend(uids.iter().map(|uid| {
            let name = names(*uid);
            if *uid == 0 {
                format!("{name}@全部动态")
            } else {
                format!("{name}@{uid}")
            }
        }));
    }
    out.push(String::new());
    out.push("番剧:".to_string());
    if seasons.is_empty() {
        out.push("无".to_string());
    } else {
        out.extend(seasons.iter().map(|id| {
            let title = crate::store()
                .read(|data| data.names.get(id).cloned().unwrap_or_else(|| id.clone()));
            format!("{title}@{id}")
        }));
    }
    out.push(String::new());
    out.push(format!("共 {} 个订阅", uids.len() + seasons.len()));
    lines(&out)
}

/// `/bili listall`——全局总表，原插件只给 bot 管理员看
pub(crate) async fn list_all(call: &Call) -> Reply {
    if !call.is_bot_admin() {
        return reply("仅bot管理员可获取");
    }
    let rows = crate::store().read(|data| {
        data.subscribed_uids()
            .into_iter()
            .map(|uid| {
                let name = data
                    .names
                    .get(&uid.to_string())
                    .cloned()
                    .unwrap_or_else(|| uid.to_string());
                format!("{name}@{uid}#{}", data.targets_of(uid).len())
            })
            .collect::<Vec<String>>()
    });
    let count = rows.len();
    let mut out = vec!["名称@UID#订阅人数".to_string(), String::new()];
    if rows.is_empty() {
        out.push("还没有任何订阅".to_string());
    } else {
        out.extend(rows);
    }
    out.push(String::new());
    out.push(format!("共 {count} 个订阅"));
    lines(&out)
}

/// `/bili listuser [UP主UID|名字]`——某个 UP 被哪些目标订着
pub(crate) async fn list_user(call: &Call) -> Reply {
    if !call.is_bot_admin() {
        return reply("仅bot管理员可获取");
    }
    let raw = call.arg(0).unwrap_or("").trim().to_string();
    let (header, keys) = if raw.is_empty() {
        let uids = crate::store().read(|data| data.subscribed_uids());
        let keys = crate::store().read(|data| {
            let mut set = std::collections::BTreeSet::<String>::new();
            for uid in data.subscribed_uids() {
                set.extend(data.targets_of(uid));
            }
            set
        });
        (
            if uids.is_empty() {
                String::new()
            } else {
                "全部订阅者".to_string()
            },
            keys.into_iter().collect::<Vec<String>>(),
        )
    } else {
        let uid = match match_uid(&raw) {
            Match::One(uid) => uid,
            found => return reply(found.explain().unwrap_or_default()),
        };
        let known = crate::store().read(|data| {
            data.names.contains_key(&uid.to_string())
                || data.targets.values().any(|state| state.subs.contains(&uid))
        });
        if !known {
            return reply(format!("没有这个用户哦 [{uid}]"));
        }
        let name = names(uid);
        let keys = crate::store().read(|data| data.targets_of(uid));
        (format!("{name}[{uid}]"), keys)
    };
    let clusters = crate::store().read(|data| {
        data.follows
            .iter()
            .filter(|(_, follow)| !follow.uids.is_empty())
            .map(|(name, _)| name.clone())
            .collect::<Vec<String>>()
    });
    let mut group = Vec::new();
    let mut friend = Vec::new();
    for key in &keys {
        let Some((is_group, id)) = store::parse_key(key) else {
            continue;
        };
        let name = crate::store().read(|data| {
            data.targets
                .get(key)
                .map(|state| state.name.clone())
                .unwrap_or_default()
        });
        let line = if name.is_empty() {
            id.to_string()
        } else {
            format!("{name}@{id}")
        };
        if is_group {
            group.push(line);
        } else {
            friend.push(line);
        }
    }
    let mut out = Vec::new();
    if !header.is_empty() {
        out.push(header);
        out.push(String::new());
    }
    out.push("====群====".to_string());
    out.extend(if group.is_empty() {
        vec!["无".to_string()]
    } else {
        group
    });
    out.push("====好友====".to_string());
    out.extend(if friend.is_empty() {
        vec!["无".to_string()]
    } else {
        friend
    });
    out.push("====分组====".to_string());
    out.extend(if clusters.is_empty() {
        vec!["无".to_string()]
    } else {
        clusters
    });
    out.push(String::new());
    out.push(format!("共 {} 名用户", keys.len()));
    lines(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 通配档在匿名态下整档拿不到内容，`/bili add 0` 那句「成功」必须自己交代这一点——
    /// 用户看到的是一句成功、之后一条都不会推，而清单那一头（`browse.rs` 的 `a_wildcard_slot_is_spelled_out`）
    /// 只有他主动去列才看得到。
    #[test]
    fn adding_the_wildcard_while_anonymous_says_it_needs_login() {
        let hint = wildcard_hint(false);
        assert!(
            hint.contains("关注时间线"),
            "得说清这一档走的是哪条接口：{hint}"
        );
        assert!(hint.contains("/bili login"), "匿名那句要给出口：{hint}");
        assert_eq!(wildcard_hint(true), "", "有登录态时不该多这一句");
    }

    /// 有登录态、而关注关系那一关只是"没被拦住"时，那句成功也得把「可能一条都不推」说出来；
    /// 真确认到了、或根本没有登录态（那时走逐 UP，照常推），都不许多说这一句。
    #[test]
    fn an_unconfirmed_follow_says_so_only_when_signed() {
        let hint = follow_hint(true, Relation::Unconfirmed);
        assert!(
            hint.contains("没确认到"),
            "别写成「你没关注」那种带方向的断言：{hint}"
        );
        assert!(hint.contains("autoFollow"), "要给出口：{hint}");
        assert_eq!(
            follow_hint(true, Relation::Confirmed),
            "",
            "确认到了就不许多话"
        );
        assert_eq!(
            follow_hint(false, Relation::Unconfirmed),
            "",
            "匿名走逐 UP 那条路，照常推，不该吓唬人"
        );
    }
}
