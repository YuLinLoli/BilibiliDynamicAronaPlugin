//! 分组这一组命令：`create` `listgroup` `delgroup` `aga` `bga` `push` `ban`。
//!
//! 文案逐条抄原插件 `service/GroupService.kt`（"分组名不能全为数字"、"无权添加"、
//! "[123, ] 添加失败"那几句）。
//!
//! 归属口径也照原插件：`createGroup(name, subject?.id)` 记的是**发命令那个会话的号**——
//! 在群里建的就归那个群，私聊建的就归那个人；`checkGroupPerm` 比的也是这个号。
//! 所以"同一个人在别的群里改这个分组"在原插件里不行，这里也不放开；
//! 只有 `bili.admin`（或框架管理员）在建组时不受"号"限制（见 `Call::targets`）。
//!
//! 原插件加目标时靠 `findContactAll` 判"bot 在不在这个群/是不是好友"，
//! 这里换成框架的 `Bot::groups()` / `Bot::friends()` 快照，语义一致。

use std::collections::BTreeSet;

use arona::bot::Bot;

use crate::store;

use super::{Call, Reply, reply, usage_str};

/// 逗号分隔的一串号：能解析的留下，解析不了的按原插件那半句报错攒起来
fn ids(raw: &str) -> (Vec<i64>, String) {
    let mut good = Vec::new();
    let mut fail = String::new();
    for part in raw
        .split([',', '，'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        match part.parse::<i64>() {
            Ok(id) => good.push(id),
            Err(_) => fail.push_str(&format!("{part}, ")),
        }
    }
    (good, fail)
}

/// 失败清单那句：`[123, 456, ] 添加失败`
fn failed(fail: &str, verb: &str) -> String {
    format!("[{fail}] {verb}失败")
}

/// `/bili create <分组名>`
pub(crate) fn create(call: &Call) -> Reply {
    let Some(name) = call.arg(0) else {
        return reply(usage_str(call, super::Cmd::Create));
    };
    if name.chars().all(|ch| ch.is_ascii_digit()) {
        return reply("分组名不能全为数字");
    }
    let creator = call.current_id();
    let created = crate::store().write(|data| data.create_cluster(name, creator));
    reply(if created {
        "创建成功"
    } else {
        "分组名称重复"
    })
}

/// `/bili listgroup [分组名]`：不给名字就列自己能管的分组，给了就出明细
pub(crate) fn list(call: &Call) -> Reply {
    let operator = call.current_id();
    let admin = call.is_bot_admin();
    let Some(name) = call.arg(0) else {
        let lines = crate::store().read(|data| {
            data.clusters
                .iter()
                .filter(|(_, cluster)| admin || cluster.governs(operator))
                .map(|(cluster_name, cluster)| format!("{cluster_name}@{}", cluster.creator))
                .collect::<Vec<String>>()
        });
        if lines.is_empty() {
            return reply("没有创建或管理任何分组哦");
        }
        return reply(lines.join("\n"));
    };
    let found = crate::store().read(|data| {
        let cluster = data.clusters.get(name)?;
        Some(ClusterText {
            name: name.to_string(),
            creator: cluster.creator,
            admins: cluster
                .admins
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<String>>(),
            members: cluster
                .members
                .iter()
                .map(|key| match store::parse_key(key) {
                    Some((_, id)) => {
                        let label = data
                            .targets
                            .get(key)
                            .map(|state| state.name.clone())
                            .unwrap_or_default();
                        if label.is_empty() {
                            key.clone()
                        } else {
                            format!("{label}({id})")
                        }
                    }
                    None => key.clone(),
                })
                .collect::<Vec<String>>(),
        })
    });
    match found {
        Some(found) => reply(found.render()),
        None => reply("没有此分组哦"),
    }
}

/// `listGroup(name)` 那份明细，版式抄原插件 `Group.toString()`
struct ClusterText {
    name: String,
    creator: i64,
    admins: Vec<String>,
    members: Vec<String>,
}

impl ClusterText {
    fn render(&self) -> String {
        format!(
            "分组名: {}\n创建者: {}\n\n管理员: \n{}\n\n用户: \n{}",
            self.name,
            self.creator,
            if self.admins.is_empty() {
                "暂无管理员".to_string()
            } else {
                self.admins.join("\n")
            },
            if self.members.is_empty() {
                "暂无用户".to_string()
            } else {
                self.members.join("\n")
            }
        )
    }
}

/// 建组那个会话才有权删组、改管理员（原插件这两处都只比 `creator`）
fn owner_only(call: &Call, name: &str, denied: &str) -> Result<(), String> {
    let operator = call.current_id();
    match crate::store().read(|data| {
        data.clusters
            .get(name)
            .map(|cluster| cluster.creator == operator)
    }) {
        None => Err(format!("没有此分组 [{name}]")),
        Some(true) => Ok(()),
        Some(false) => Err(denied.to_string()),
    }
}

/// `/bili delgroup <分组名>`
pub(crate) fn remove(call: &Call) -> Reply {
    let Some(name) = call.arg(0) else {
        return reply(usage_str(call, super::Cmd::DelGroup));
    };
    if let Err(reason) = owner_only(call, name, "无权删除") {
        return reply(reason);
    }
    let gone = crate::store().write(|data| data.remove_cluster(name));
    reply(if gone {
        "删除成功"
    } else {
        "没有此分组哦"
    })
}

/// `/bili aga|bga <分组名> <QQ号,…>`：分组管理员决定谁能替这个分组改订阅
pub(crate) async fn admins(call: &Call, add: bool) -> Reply {
    let Some(name) = call.arg(0) else {
        return reply(usage_str(
            call,
            if add {
                super::Cmd::AddGroupAdmin
            } else {
                super::Cmd::BanGroupAdmin
            },
        ));
    };
    if let Err(reason) = owner_only(call, name, if add { "无权添加" } else { "无权删除" }) {
        return reply(reason);
    }
    let raw = match call.need(1, "需要给出至少一个 QQ 号") {
        Ok(value) => value,
        Err(reason) => return reply(reason),
    };
    let (mut wanted, mut fail) = ids(&raw);
    if add {
        // 原插件只让 bot 的好友当管理员：不是好友就收不到私聊推送，授权也没意义
        let bot = Bot::of(call.ctx().framework());
        match bot.friends().await {
            Ok(list) => {
                let friends: BTreeSet<i64> = list.iter().map(|friend| friend.user_id()).collect();
                let mut kept = Vec::new();
                for id in wanted {
                    if friends.contains(&id) {
                        kept.push(id);
                    } else {
                        fail.push_str(&format!("{id}, "));
                    }
                }
                wanted = kept;
            }
            Err(reason) => {
                // 好友列表拿不到就不拦（实现端可能没给这个接口），只记一条日志
                arona::runtime::log::warning(format!(
                    "bilibili：好友列表读不出来（{reason}），管理员名单不做身份校验"
                ));
            }
        }
    }
    let touched = crate::store().write(|data| data.set_cluster_admins(name, &wanted, add));
    if !fail.is_empty() {
        return reply(failed(&fail, if add { "添加" } else { "删除" }));
    }
    reply(match (add, touched) {
        (true, 0) => "已经是管理员了".to_string(),
        (true, _) => "添加成功".to_string(),
        (false, 0) => "删除失败".to_string(),
        (false, _) => "删除成功".to_string(),
    })
}

/// `/bili push|ban <分组名> <群号|好友号,…>`：往分组里加/删推送目标
pub(crate) async fn members(call: &Call, add: bool) -> Reply {
    let Some(name) = call.arg(0) else {
        return reply(usage_str(
            call,
            if add {
                super::Cmd::Push
            } else {
                super::Cmd::Ban
            },
        ));
    };
    let operator = call.current_id();
    let governed = crate::store().read(|data| {
        data.clusters
            .get(name)
            .map(|cluster| cluster.governs(operator))
    });
    match governed {
        None => return reply(format!("没有此分组 [{name}]")),
        Some(false) => return reply(if add { "无权添加" } else { "无权删除" }),
        Some(true) => {}
    }
    let raw = match call.need(1, "需要给出至少一个群号或好友号") {
        Ok(value) => value,
        Err(reason) => return reply(reason),
    };
    let (wanted, mut fail) = ids(&raw);
    let mut keys: Vec<String> = Vec::new();
    if add {
        // 加进去得是 bot 真在的群 / 真加好友的人，否则订阅写了也发不出去
        let bot = Bot::of(call.ctx().framework());
        let groups: BTreeSet<i64> = bot
            .groups()
            .await
            .unwrap_or_default()
            .iter()
            .map(|group| group.id())
            .collect();
        let friends: BTreeSet<i64> = bot
            .friends()
            .await
            .unwrap_or_default()
            .iter()
            .map(|friend| friend.user_id())
            .collect();
        for id in wanted {
            if groups.contains(&id) {
                keys.push(store::group_key(id));
            } else if friends.contains(&id) {
                keys.push(store::friend_key(id));
            } else {
                fail.push_str(&format!("{id}, "));
            }
        }
    } else {
        // 移出分组不再验身份：本来就在名单里才有得移，两种键都试一次
        for id in wanted {
            keys.push(store::group_key(id));
            keys.push(store::friend_key(id));
        }
    }
    let touched = crate::store().write(|data| {
        if add {
            data.push_members(name, &keys)
        } else {
            data.ban_members(name, &keys)
        }
    });
    if !fail.is_empty() {
        return reply(failed(&fail, if add { "添加" } else { "删除" }));
    }
    reply(match (add, touched) {
        (true, 0) => "已经在分组里了".to_string(),
        (true, _) => "添加成功".to_string(),
        (false, 0) => "删除失败".to_string(),
        (false, _) => "删除成功".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comma_lists_split_on_both_commas() {
        let (good, fail) = ids("12,34，56,x");
        assert_eq!(good, vec![12, 34, 56]);
        assert_eq!(fail, "x, ");
        let (none, fail) = ids("");
        assert!(none.is_empty());
        assert_eq!(fail, "");
    }

    #[test]
    fn failure_wording_matches_the_original() {
        assert_eq!(failed("123, ", "添加"), "[123, ] 添加失败");
        assert_eq!(failed("456, ", "删除"), "[456, ] 删除失败");
    }
}
