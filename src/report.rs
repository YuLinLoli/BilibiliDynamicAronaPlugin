//! 把订阅表交给框架的会话展示板，好让 GUI 的「群管理」页列出"这个群订了谁"。
//!
//! 框架那边只有 `(标题, 条目)` 这一种形状，它不认识"UP 主""番剧"这些词，也不读本插件的
//! `store.yml`（框架本体里不许出现任何插件信息）。所以是**我们报上去**，不是它来查。
//!
//! 两件事决定了这个模块的写法：
//!
//! * **不许靠人记**。哪个命令改了订阅都记得自己调一次上报是不可能的纪律，所以挂在
//!   [`Store::attach_reporter`] 上：状态库每次落盘之后顺手重报一次，忘了改的地方不存在。
//! * **不许每轮敲一次门**。检测任务每轮都要写水位线，那也会走落盘——订阅面没变时
//!   拿 [`LAST`] 比一下就退回不动作，GUI 看的又不是心跳。
//!
//! 只报**群**：框架 GUI 只有群这一张列表，没有好友/私聊页。订阅给个人的那一路用
//! `/bili list <QQ>` 查，这里不硬造一个显示位置。

use arona::plugin::PluginContext;
use arona::views::ViewRow;
use std::sync::Mutex;

use crate::store::Data;

/// 上一次报出去的内容，用来认出"这次落盘其实没改订阅"
static LAST: Mutex<Vec<(i64, Vec<ViewRow>)>> = Mutex::new(Vec::new());

/// 订阅那两档的标题。GUI 里看到的就是这两个词，由我们自己定，框架不参与。
const UP_ROW: &str = "UP 主";
const SEASON_ROW: &str = "番剧";

/// 挂上报回调并立刻补报一次。`start` 与 `on_config_reload` 都调它：
/// 盘上的订阅表早就在，而板子是内存里的现状，装载后不主动报一次就会一直空着。
pub(crate) fn attach(ctx: &PluginContext) {
    let holder = ctx.clone();
    crate::store().attach_reporter(move |data| publish(&holder, data));
    crate::store().read(|data| publish(ctx, data));
}

/// 订阅表 → 每个群的两行。群号为键；没有任何订阅的群直接跳过（报空等于摘掉那一格）。
fn collect(data: &Data) -> Vec<(i64, Vec<ViewRow>)> {
    let mut out = Vec::new();
    for (key, target) in &data.targets {
        // 目标键是 `g123` / `f456`：只收群，好友那一路 GUI 没有地方放
        let Some(number) = key.strip_prefix('g') else {
            continue;
        };
        let Ok(peer) = number.parse::<i64>() else {
            continue;
        };
        let ups: Vec<String> = target
            .subs
            .iter()
            .map(|uid| {
                data.names
                    .get(&uid.to_string())
                    .cloned()
                    .unwrap_or(uid.to_string())
            })
            .collect();
        let seasons: Vec<String> = target.seasons.iter().cloned().collect();
        if ups.is_empty() && seasons.is_empty() {
            continue;
        }
        let mut rows = Vec::new();
        if !ups.is_empty() {
            rows.push(ViewRow::new(UP_ROW, ups));
        }
        if !seasons.is_empty() {
            rows.push(ViewRow::new(SEASON_ROW, seasons));
        }
        out.push((peer, rows));
    }
    out.sort_by_key(|(peer, _)| *peer);
    out
}

fn publish(ctx: &PluginContext, data: &Data) {
    let fresh = collect(data);
    let mut last = LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if *last == fresh {
        return;
    }
    // 整个群被删掉/清空时得主动摘，否则 GUI 留一行停在旧名单上的僵尸
    for (peer, _) in last
        .iter()
        .filter(|(peer, _)| !fresh.iter().any(|(shown, _)| shown == peer))
    {
        ctx.clear_rows(*peer);
    }
    for (peer, rows) in &fresh {
        ctx.publish_rows(*peer, rows);
    }
    *last = fresh;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Data, TargetState};
    use std::collections::BTreeMap;

    fn data_with(targets: BTreeMap<String, TargetState>, names: BTreeMap<String, String>) -> Data {
        Data {
            targets,
            names,
            ..Default::default()
        }
    }

    /// 报上去的只有群，标题与名字都用插件自己那套；昵称快照没了也要能用 uid 顶上——
    /// 刚订阅还没拉过资料的人，GUI 不该整行空白。
    #[test]
    fn only_group_targets_are_collected_and_names_fall_back_to_uids() {
        let mut targets = BTreeMap::new();
        targets.insert(
            "g20001".to_string(),
            TargetState {
                subs: [487550002i64, 546195].into_iter().collect(),
                seasons: ["ss38942".to_string()].into_iter().collect(),
                ..Default::default()
            },
        );
        targets.insert(
            "f12345".to_string(),
            TargetState {
                subs: [1i64].into_iter().collect(),
                ..Default::default()
            },
        );
        targets.insert("g20002".to_string(), TargetState::default());
        let names = [("487550002".to_string(), "某位 UP".to_string())]
            .into_iter()
            .collect();

        let rows = collect(&data_with(targets, names));
        assert_eq!(rows.len(), 1, "好友目标与空群都不该占一格: {rows:?}");
        assert_eq!(rows[0].0, 20001);
        assert_eq!(rows[0].1.len(), 2, "UP 主与番剧各一行");
        assert_eq!(rows[0].1[0].label, UP_ROW);
        // BTreeSet 的序是 uid 的数值序，546195 排在 487550002 之前
        assert_eq!(rows[0].1[0].items, ["546195", "某位 UP"]);
        assert_eq!(rows[0].1[1].items, ["ss38942"]);
    }

    /// 只有昵称里带控制字符时不该把一行撑成两行——框架那边是按 `\n`/`\t` 拆的。
    #[test]
    fn a_nickname_with_a_tab_cannot_split_a_row() {
        let mut targets = BTreeMap::new();
        targets.insert(
            "g1".to_string(),
            TargetState {
                subs: [7i64].into_iter().collect(),
                ..Default::default()
            },
        );
        let names = [("7".to_string(), "甲\t乙\n丙".to_string())]
            .into_iter()
            .collect();
        let rows = collect(&data_with(targets, names));
        assert_eq!(rows[0].1[0].items.len(), 1);
        assert_eq!(rows[0].1[0].items[0], "甲 乙 丙");
    }
}
