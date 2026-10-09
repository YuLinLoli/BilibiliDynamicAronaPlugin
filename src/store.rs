//! 插件自己的持久化状态：订阅关系、分组、过滤器、@全体、模板覆盖、主题色、检测水位线。
//!
//! 落在 `data/bilibili/store.yml`（框架按插件 id 规定的目录，见 PLUGIN_DEVELOPMENT.md §16）。
//! 这块**不是配置**——配置是用户手写的、框架负责生成模板与热重载的那份；这里是命令改出来的
//! 运行数据，量随群数增长，所以自己管读写，只做一件事：临时文件 + rename 原子替换，
//! 免得写一半崩了把用户所有订阅清空。
//!
//! 相比原 mirai 插件多带了两样：**水位线**（`watermark`）与**已推送的动态 id**，
//! 机器人重启后不会把老动态重推一遍。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Mutex, RwLock};

/// 目标种类过滤器与正则过滤器各自的模式
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Mode {
    /// 命中就不推
    #[default]
    Black,
    /// 只有命中才推
    White,
}

impl Mode {
    pub fn parse(raw: &str) -> Option<Mode> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "b" | "black" | "黑名单" => Some(Mode::Black),
            "w" | "white" | "白名单" => Some(Mode::White),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Black => "黑名单",
            Mode::White => "白名单",
        }
    }
}

/// 一个目标（群 / 好友 / 分组成员）对某个订阅者设的过滤器。
/// 类型过滤与正则过滤各自带模式，两条独立生效（与原插件一致）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FilterSet {
    pub type_mode: Mode,
    pub types: Vec<String>,
    pub reg_mode: Mode,
    pub regs: Vec<String>,
}

impl FilterSet {
    pub fn is_empty(&self) -> bool {
        self.types.is_empty() && self.regs.is_empty()
    }

    /// 这条动态能不能推。类型过滤与正则过滤各自带模式、各自独立判定（与原插件一致）：
    /// 黑名单命中就不推，白名单没命中就不推；某一条没配就等于不设限。
    pub fn passes(&self, kind: &str, content: &str) -> bool {
        if !self.types.is_empty() {
            let hit = self.types.iter().any(|t| t == kind);
            let blocked = match self.type_mode {
                Mode::Black => hit,
                Mode::White => !hit,
            };
            if blocked {
                return false;
            }
        }
        if !self.regs.is_empty() {
            let hit = self.regs.iter().any(|pattern| regex_ok(pattern, content));
            let blocked = match self.reg_mode {
                Mode::Black => hit,
                Mode::White => !hit,
            };
            if blocked {
                return false;
            }
        }
        true
    }
}

/// 正则写坏了不能把整条推送判死：当成不命中，命令那边会提示语法错误
fn regex_ok(pattern: &str, text: &str) -> bool {
    match regex::Regex::new(pattern) {
        Ok(re) => re.is_match(text),
        Err(_) => false,
    }
}

/// 可 @全体成员 的动态种类（与原插件 `AtAllType` 同一套词，注意是"全部动态"不是"动态"）
pub const AT_ALL_KINDS: [&str; 6] = ["全部", "全部动态", "视频", "音乐", "专栏", "直播"];

/// 类型过滤器认的种类（原插件 `DynamicFilterType`：这一套里有"动态"和"转发动态"，没有"全部"）
pub const FILTER_KINDS: [&str; 6] = ["动态", "转发动态", "视频", "音乐", "专栏", "直播"];

/// 规范化类型过滤里写的种类名（支持英文与原插件的简写）
pub fn normalize_filter_kind(raw: &str) -> Option<&'static str> {
    let key = raw.trim().to_lowercase();
    Some(match key.as_str() {
        "动态" | "普通动态" | "dynamic" | "d" => "动态",
        "转发动态" | "转发" | "forward" | "f" => "转发动态",
        "直播" | "live" | "l" => "直播",
        "视频" | "video" | "v" => "视频",
        "音乐" | "music" | "m" => "音乐",
        "专栏" | "article" | "a" => "专栏",
        _ => return None,
    })
}

/// 规范化 @全体 命令里写的种类名。这里的"全部动态"是"除直播外都 @"，
/// 与类型过滤那套词不同，别混用。
pub fn normalize_at_all_kind(raw: &str) -> Option<&'static str> {
    let key = raw.trim().to_lowercase();
    Some(match key.as_str() {
        "全部" | "all" | "a" => "全部",
        "全部动态" | "dynamic" | "d" => "全部动态",
        "直播" | "live" | "l" => "直播",
        "视频" | "video" | "v" => "视频",
        "音乐" | "music" | "m" => "音乐",
        "专栏" | "article" => "专栏",
        _ => return None,
    })
}

/// 这一类内容命中 @全体 没有。包含关系照原插件 `checkAtAll`：
/// "全部" 通吃；"全部动态" 盖住动态/转发动态/视频/音乐/专栏，但**不**盖直播；
/// 单品种类只盖自己那一类。
pub fn at_all_hit(kinds: &BTreeSet<String>, kind: &str) -> bool {
    if kinds.contains("全部") {
        return true;
    }
    if kind == "直播" {
        return kinds.contains("直播");
    }
    kinds.contains("全部动态") || kinds.contains(kind)
}

/// 加一个 @全体 种类时的互斥规则（原插件 `addAtAll`）：
/// "全部"清空其余；"全部动态"顶掉视频/音乐/专栏；单品种类顶掉"全部"与"全部动态"；
/// "直播"只顶掉"全部"，可以和种类并存。
pub fn merge_at_all(kinds: &mut BTreeSet<String>, kind: &str) {
    if kinds.is_empty() {
        kinds.insert(kind.to_string());
        return;
    }
    match kind {
        "全部" => {
            kinds.clear();
            kinds.insert("全部".to_string());
        }
        "全部动态" => {
            for drop in ["全部", "视频", "音乐", "专栏"] {
                kinds.remove(drop);
            }
            kinds.insert("全部动态".to_string());
        }
        "直播" => {
            kinds.remove("全部");
            kinds.insert("直播".to_string());
        }
        other => {
            kinds.remove("全部");
            kinds.remove("全部动态");
            kinds.insert(other.to_string());
        }
    }
}

/// 某个目标的订阅与个性化设置
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TargetState {
    /// 展示名（命令里可以改，仅用于列表好看）
    pub name: String,
    /// 订阅的 B 站 uid
    pub subs: BTreeSet<i64>,
    /// 订阅的番剧（ss / ep / md 开头的号）
    pub seasons: BTreeSet<String>,
    /// 过滤器：键是订阅者 uid 的字符串，"0" 表示对该目标的全部订阅生效
    pub filters: BTreeMap<String, FilterSet>,
    /// @全体成员 的种类：键同样是 uid（"0" 为全部订阅）
    pub at_all: BTreeMap<String, BTreeSet<String>>,
    /// 模板覆盖："dynamic" / "live" / "liveClose" -> 模板名
    pub templates: BTreeMap<String, String>,
    /// 该目标单独的绘图主题色（分号分隔的 HEX）
    pub color: Option<String>,
    /// 关掉就不再给这个目标推任何东西（订阅关系保留）
    pub muted: bool,
}

impl TargetState {
    /// 取某个订阅者生效的过滤器：先找专属，再退回 "0"（全部订阅）那一条
    pub fn filter_for(&self, uid: i64) -> Option<&FilterSet> {
        self.filters
            .get(&uid.to_string())
            .or_else(|| self.filters.get("0"))
    }

    /// 某个订阅者的这一类内容要不要 @全体（同样是先专属再 "0"）
    pub fn at_all_for(&self, uid: i64, kind: &str) -> bool {
        self.at_all
            .get(&uid.to_string())
            .or_else(|| self.at_all.get("0"))
            .is_some_and(|kinds| at_all_hit(kinds, kind))
    }

    /// 该目标用的模板名（没覆盖时返回 None，由调用方回落到配置里的默认模板）
    pub fn template(&self, kind: &str) -> Option<&str> {
        self.templates.get(kind).map(String::as_str)
    }
}

/// 分组：把一批目标打包，命令里的 [目标] 可以直接写分组名
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Cluster {
    /// 建组的那个人（只有他能删组、改管理员）
    pub creator: i64,
    pub members: BTreeSet<String>,
    pub admins: BTreeSet<i64>,
}

impl Cluster {
    /// 谁能替这个分组动订阅命令（原插件 `checkGroupPerm`）
    pub fn governs(&self, user_id: i64) -> bool {
        self.creator == user_id || self.admins.contains(&user_id)
    }
}

/// 一次直播的状态，用来判断"开播"与"结束"这两条边沿
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LiveState {
    /// B 站的 live_status：0 未开播 1 直播中 2 直播已结束
    pub status: i64,
    pub room_id: i64,
    pub title: String,
    /// 开播时间戳（秒）
    pub started_at: i64,
    /// 已经发过结束通知，避免重复播报
    pub closed: bool,
}

/// 分组自身的订阅。原插件把分组名直接当一个"目标"存进 `contacts`，
/// 所以先建组、后加群的写法也能收到推送——这里用一张单独的表记这件事，
/// 不假装它们是 `g<号>` 那种目标键。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Follow {
    pub uids: BTreeSet<i64>,
    pub seasons: BTreeSet<String>,
}

/// store.yml 的全部内容
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Data {
    pub version: u32,
    /// 目标键（`g123` / `f456`）-> 状态
    pub targets: BTreeMap<String, TargetState>,
    /// 分组名 -> 分组
    pub clusters: BTreeMap<String, Cluster>,
    /// 分组名 -> 这个分组自己订了什么（成员后加进来也照样收）
    pub follows: BTreeMap<String, Follow>,
    /// uid -> 名称快照，用户名模糊搜索就靠它（只有订阅过的才匹配得到，与原插件一致）
    pub names: BTreeMap<String, String>,
    /// uid -> 已推送到的最大动态 id。动态 id 高位含时间，比时间戳更可靠
    pub watermark: BTreeMap<String, i64>,
    /// uid -> 直播状态
    pub live: BTreeMap<String, LiveState>,
    /// 番剧号（`ss12345`）-> 上次看到的更新标题。集数文本一变就算有新内容
    pub seasons: BTreeMap<String, String>,
}

impl Default for Data {
    fn default() -> Self {
        Data {
            version: 1,
            targets: BTreeMap::new(),
            clusters: BTreeMap::new(),
            follows: BTreeMap::new(),
            names: BTreeMap::new(),
            watermark: BTreeMap::new(),
            live: BTreeMap::new(),
            seasons: BTreeMap::new(),
        }
    }
}

/// 目标键：群带 g、好友带 f，两种 id 会撞号，靠前缀区分
pub fn group_key(id: i64) -> String {
    format!("g{id}")
}

pub fn friend_key(id: i64) -> String {
    format!("f{id}")
}

/// 从目标键还原成 (是否群, id)
pub fn parse_key(key: &str) -> Option<(bool, i64)> {
    let (is_group, raw) = match key.split_once(['g', 'f']) {
        Some(("", rest)) => (key.starts_with('g'), rest),
        _ => return None,
    };
    raw.parse::<i64>().ok().map(|id| (is_group, id))
}

impl Data {
    /// 读到内存；文件不存在返回默认值，文件坏了返回错误由上层记日志后按空库继续
    pub fn load(path: &Path) -> Result<Data, String> {
        if !path.exists() {
            return Ok(Data::default());
        }
        let text = std::fs::read_to_string(path).map_err(|err| format!("读取失败: {err}"))?;
        serde_yaml::from_str(&text).map_err(|err| format!("格式错误: {err}"))
    }

    /// 原子落盘：先写同目录的 .tmp，再 rename 覆盖
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| format!("建目录失败: {err}"))?;
        }
        let text = serde_yaml::to_string(self).map_err(|err| format!("序列化失败: {err}"))?;
        let tmp = path.with_extension("yml.tmp");
        std::fs::write(&tmp, text.as_bytes()).map_err(|err| format!("写临时文件失败: {err}"))?;
        std::fs::rename(&tmp, path).map_err(|err| format!("替换失败: {err}"))
    }

    /// 拿到（或新建）某个目标的槽位
    pub fn target_mut(&mut self, key: &str) -> &mut TargetState {
        self.targets.entry(key.to_string()).or_default()
    }

    /// 某个订阅者的所有推送目标。`0` 是原插件 `dynamic[0]` 那个通配位
    /// （"订我关注时间线里的全部动态"），订了它的目标什么条都收；
    /// 分组按它自己那份订阅展开成成员，所以后加进来的群也收得到。
    pub fn targets_of(&self, uid: i64) -> Vec<String> {
        let mut keys: BTreeSet<String> = self
            .targets
            .iter()
            .filter(|(_, state)| state.subs.contains(&uid) || state.subs.contains(&0))
            .map(|(key, _)| key.clone())
            .collect();
        for (name, follow) in &self.follows {
            if (follow.uids.contains(&uid) || follow.uids.contains(&0))
                && let Some(cluster) = self.clusters.get(name)
            {
                keys.extend(cluster.members.iter().cloned());
            }
        }
        keys.into_iter().collect()
    }

    /// 全部被订阅的 uid（检测任务的遍历范围）。通配位 `0` 不在这里，它走时间线那条路。
    pub fn subscribed_uids(&self) -> Vec<i64> {
        let mut uids: BTreeSet<i64> = BTreeSet::new();
        for state in self.targets.values() {
            uids.extend(state.subs.iter().copied());
        }
        for follow in self.follows.values() {
            uids.extend(follow.uids.iter().copied());
        }
        uids.remove(&0);
        uids.into_iter().collect()
    }

    /// 有没有目标订了"全部动态"（对应原插件的 listenAll，要有登录态才有内容可推）
    pub fn wants_all(&self) -> bool {
        self.targets.values().any(|state| state.subs.contains(&0))
            || self.follows.values().any(|follow| follow.uids.contains(&0))
    }

    /// 记下"这个 uid 已经推到这条动态"，返回是否真的前进了（用于跳过重复推送）
    pub fn advance_watermark(&mut self, uid: i64, id: i64) -> bool {
        let entry = self.watermark.entry(uid.to_string()).or_insert(0);
        if id > *entry {
            *entry = id;
            true
        } else {
            false
        }
    }

    /// 按名称模糊找订阅过的 uid（原插件的用户名本地模糊搜索）
    pub fn find_uid_by_name(&self, keyword: &str) -> Vec<i64> {
        let keyword = keyword.trim().to_lowercase();
        if keyword.is_empty() {
            return Vec::new();
        }
        self.names
            .iter()
            .filter(|(_, name)| name.to_lowercase().contains(&keyword))
            .filter_map(|(uid, _)| uid.parse::<i64>().ok())
            .collect()
    }

    /// 解析命令里的 [目标]：留空取当前会话，纯数字先按已有键找、找不到按当前会话形态猜，
    /// 其余当分组名展开
    pub fn resolve_target(&self, raw: &str, current: Option<String>) -> Vec<String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return current.into_iter().collect();
        }
        if let Ok(id) = raw.parse::<i64>() {
            let group = group_key(id);
            let friend = friend_key(id);
            if self.targets.contains_key(&group) {
                return vec![group];
            }
            if self.targets.contains_key(&friend) {
                return vec![friend];
            }
            // 两边都没记录过：从群里发的命令就按群建，私聊发的按好友建
            let from_group = current
                .as_deref()
                .and_then(parse_key)
                .is_some_and(|(g, _)| g);
            return vec![if from_group { group } else { friend }];
        }
        if let Some(cluster) = self.clusters.get(raw) {
            return cluster.members.iter().cloned().collect();
        }
        // 也可能是没建过记录的目标键直接写出来（g123 / f456）
        if parse_key(raw).is_some() {
            return vec![raw.to_string()];
        }
        Vec::new()
    }

    /// 订阅：把这批目标都订上这个 uid。`name` 是接口拿到的 UP 主名。返回新增的目标数。
    pub fn subscribe(&mut self, keys: &[String], uid: i64, name: &str) -> usize {
        let mut added = 0usize;
        for key in keys {
            let state = self.target_mut(key);
            if state.subs.insert(uid) {
                added += 1;
            }
        }
        if !name.is_empty() {
            self.names.insert(uid.to_string(), name.to_string());
        }
        added
    }

    /// 取消订阅：从这批目标摘掉这个 uid，顺带清掉它专属的过滤器与 @全体。返回摘掉的目标数。
    pub fn unsubscribe(&mut self, keys: &[String], uid: i64) -> usize {
        let mut removed = 0usize;
        for key in keys {
            let Some(state) = self.targets.get_mut(key) else {
                continue;
            };
            if state.subs.remove(&uid) {
                removed += 1;
            }
            state.filters.remove(&uid.to_string());
            state.at_all.remove(&uid.to_string());
        }
        removed
    }

    /// 这批目标（分组也算）订过这个 uid 没有
    pub fn is_subscribed(&self, keys: &[String], cluster: Option<&str>, uid: i64) -> bool {
        keys.iter().any(|key| {
            self.targets
                .get(key)
                .is_some_and(|state| state.subs.contains(&uid))
        }) || cluster
            .and_then(|name| self.follows.get(name))
            .is_some_and(|follow| follow.uids.contains(&uid))
    }

    /// 这批目标（连同分组自己那份）实际订了哪些 uid 与番剧——`/bili list` 的清单
    pub fn listed(
        &self,
        keys: &[String],
        cluster: Option<&str>,
    ) -> (BTreeSet<i64>, BTreeSet<String>) {
        let mut uids: BTreeSet<i64> = BTreeSet::new();
        let mut seasons: BTreeSet<String> = BTreeSet::new();
        for key in keys {
            if let Some(state) = self.targets.get(key) {
                uids.extend(state.subs.iter().copied());
                seasons.extend(state.seasons.iter().cloned());
            }
        }
        if let Some(follow) = cluster.and_then(|name| self.follows.get(name)) {
            uids.extend(follow.uids.iter().copied());
            seasons.extend(follow.seasons.iter().cloned());
        }
        (uids, seasons)
    }

    /// 清空这批目标的全部订阅与番剧（订阅关系没了，个性化配置也一起清）。返回删掉的条数。
    pub fn clear_subs(&mut self, keys: &[String]) -> usize {
        let mut count = 0usize;
        for key in keys {
            let Some(state) = self.targets.get_mut(key) else {
                continue;
            };
            count += state.subs.len() + state.seasons.len();
            state.subs.clear();
            state.seasons.clear();
            state.filters.clear();
            state.at_all.clear();
            state.templates.clear();
            for cluster in self.clusters.values_mut() {
                cluster.members.remove(key);
            }
        }
        count
    }

    /// 整条删掉这批目标：订阅、个性化配置和分组里的挂名一起消失。返回原本挂着的订阅条数。
    ///
    /// `/bili clear` 确认之后走这里而不是 [`clear_subs`](Self::clear_subs)：那条命令是拿
    /// `targets` 的键当候选的，只清内容不删条目的话，同一个失效群每次都会被重新列出来。
    /// 反正这个群/好友 bot 已经不在里面了，那份配色和模板覆盖也没有下一个主人。
    pub fn remove_targets(&mut self, keys: &[String]) -> usize {
        let mut count = 0usize;
        for key in keys {
            if let Some(state) = self.targets.remove(key) {
                count += state.subs.len() + state.seasons.len();
            }
            for cluster in self.clusters.values_mut() {
                cluster.members.remove(key);
            }
        }
        count
    }

    /// 设主题色。空串表示取消单独配色、回到全局默认。
    pub fn set_color(&mut self, keys: &[String], color: &str) {
        for key in keys {
            self.target_mut(key).color = if color.is_empty() {
                None
            } else {
                Some(color.to_string())
            };
        }
    }

    /// 设模板覆盖。空模板名同样表示回到默认。
    pub fn set_template(&mut self, keys: &[String], kind: &str, name: &str) {
        for key in keys {
            let state = self.target_mut(key);
            if name.is_empty() {
                state.templates.remove(kind);
            } else {
                state.templates.insert(kind.to_string(), name.to_string());
            }
        }
    }

    /// 静音某个目标：订阅、配色、模板、过滤器全留着，只是不再给它推任何东西。
    pub fn set_muted(&mut self, keys: &[String], muted: bool) {
        for key in keys {
            self.target_mut(key).muted = muted;
        }
    }

    /// 追番：记下番剧号与标题。返回新增的目标数。
    pub fn follow_season(&mut self, keys: &[String], id: &str, title: &str) -> usize {
        let mut added = 0usize;
        for key in keys {
            if self.target_mut(key).seasons.insert(id.to_string()) {
                added += 1;
            }
        }
        if !title.is_empty() {
            self.names.insert(id.to_string(), title.to_string());
        }
        added
    }

    /// 取消追番。返回摘掉的目标数。
    pub fn unfollow_season(&mut self, keys: &[String], id: &str) -> usize {
        let mut removed = 0usize;
        for key in keys {
            if let Some(state) = self.targets.get_mut(key)
                && state.seasons.remove(id)
            {
                removed += 1;
            }
        }
        removed
    }

    /// 分组自己订一个 uid：成员后来才加进来的也收得到（原插件把分组名存进 contacts）
    pub fn follow_cluster(&mut self, name: &str, uid: i64) -> bool {
        self.follows
            .entry(name.to_string())
            .or_default()
            .uids
            .insert(uid)
    }

    pub fn unfollow_cluster(&mut self, name: &str, uid: i64) -> bool {
        let Some(follow) = self.follows.get_mut(name) else {
            return false;
        };
        let removed = follow.uids.remove(&uid);
        if follow.uids.is_empty() && follow.seasons.is_empty() {
            self.follows.remove(name);
        }
        removed
    }

    /// 分组自己追一部番
    pub fn follow_cluster_season(&mut self, name: &str, id: &str) -> bool {
        self.follows
            .entry(name.to_string())
            .or_default()
            .seasons
            .insert(id.to_string())
    }

    pub fn unfollow_cluster_season(&mut self, name: &str, id: &str) -> bool {
        let mut removed = false;
        if let Some(follow) = self.follows.get_mut(name) {
            removed = follow.seasons.remove(id);
            if follow.uids.is_empty() && follow.seasons.is_empty() {
                self.follows.remove(name);
            }
        }
        removed
    }

    /// 清掉某个分组自己的全部订阅（成员各自的订阅不动——原插件就是这个口径）
    pub fn clear_cluster(&mut self, name: &str) -> usize {
        self.follows
            .remove(name)
            .map(|follow| follow.uids.len() + follow.seasons.len())
            .unwrap_or_default()
    }

    /// 要检测的番剧清单：目标键 -> 该目标的番剧号集合。分组的按成员展开。
    pub fn season_targets(&self) -> Vec<(String, BTreeSet<String>)> {
        let mut merged: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (key, state) in &self.targets {
            for id in &state.seasons {
                merged.entry(key.clone()).or_default().insert(id.clone());
            }
        }
        for (name, follow) in &self.follows {
            if let Some(cluster) = self.clusters.get(name) {
                for member in &cluster.members {
                    for id in &follow.seasons {
                        merged.entry(member.clone()).or_default().insert(id.clone());
                    }
                }
            }
        }
        merged.into_iter().collect()
    }

    /// 加过滤器：`which` 是 type/reg 两路之一。返回是否真的记了一笔。
    pub fn add_filter(
        &mut self,
        key: &str,
        uid: i64,
        which: &str,
        mode: Option<Mode>,
        value: Option<&str>,
    ) -> bool {
        let state = self.target_mut(key);
        let filter = state.filters.entry(uid.to_string()).or_default();
        let mut touched = false;
        if let Some(mode) = mode {
            match which {
                "type" => filter.type_mode = mode,
                "reg" => filter.reg_mode = mode,
                _ => return false,
            }
        }
        if let Some(value) = value {
            match which {
                "type" => {
                    if !filter.types.iter().any(|have| have == value) {
                        filter.types.push(value.to_string());
                        touched = true;
                    }
                }
                "reg" => {
                    if !filter.regs.iter().any(|have| have == value) {
                        filter.regs.push(value.to_string());
                        touched = true;
                    }
                }
                _ => return false,
            }
        }
        // 只写了模式、一条规则都没有：别留个空壳在文件里
        if filter.is_empty() {
            state.filters.remove(&uid.to_string());
        }
        touched || mode.is_some()
    }

    /// 按下标删一条过滤规则（`t0` / `r1` 那套写法，下标从 0 起，与原插件的列表一致）。
    /// 返回 `Err` 区分"索引写法不对/越界"，让命令能回一句人话。
    pub fn remove_filter(&mut self, key: &str, uid: i64, index: &str) -> Result<String, String> {
        let (which, raw) = index.split_at(1);
        let position: usize = raw
            .parse()
            .map_err(|_| format!("索引错误 [{index}]，应当写成 t0 或 r1"))?;
        let state = self
            .targets
            .get_mut(key)
            .ok_or_else(|| "当前目标没有过滤器".to_string())?;
        let filter = state
            .filters
            .get_mut(&uid.to_string())
            .ok_or_else(|| "当前目标没有过滤器".to_string())?;
        let list = match which {
            "t" => &mut filter.types,
            "r" => &mut filter.regs,
            _ => return Err("索引类型错误，只有 t(类型) 与 r(正则) 两种".to_string()),
        };
        if position >= list.len() {
            return Err(format!("索引超出范围 [{index}]"));
        }
        let removed = list.remove(position);
        if filter.is_empty() {
            state.filters.remove(&uid.to_string());
        }
        Ok(removed)
    }

    /// @全体：按原插件的互斥规则并进去
    pub fn add_at_all(&mut self, key: &str, uid: i64, kind: &str) {
        let kinds = self
            .target_mut(key)
            .at_all
            .entry(uid.to_string())
            .or_default();
        merge_at_all(kinds, kind);
    }

    /// @全体：摘掉一个种类，摘空了就把这一档整个删掉
    pub fn remove_at_all(&mut self, key: &str, uid: i64, kind: &str) -> bool {
        let Some(state) = self.targets.get_mut(key) else {
            return false;
        };
        let hit = state
            .at_all
            .get_mut(&uid.to_string())
            .is_some_and(|kinds| kinds.remove(kind));
        if state
            .at_all
            .get(&uid.to_string())
            .is_some_and(BTreeSet::is_empty)
        {
            state.at_all.remove(&uid.to_string());
        }
        hit
    }

    /// 建分组：建组人记在 creator 上，删组与改管理员只认他
    pub fn create_cluster(&mut self, name: &str, creator: i64) -> bool {
        if self.clusters.contains_key(name) {
            return false;
        }
        self.clusters.insert(
            name.to_string(),
            Cluster {
                creator,
                members: BTreeSet::new(),
                admins: BTreeSet::new(),
            },
        );
        true
    }

    /// 删分组：删分组本身和它自己那份订阅。目标键（g***/f***）上的订阅不受影响，
    /// 因为这里的关系是"分组 → 一批目标"，删组不该把成员的订阅一起清掉。
    pub fn remove_cluster(&mut self, name: &str) -> bool {
        self.follows.remove(name);
        self.clusters.remove(name).is_some()
    }

    /// 往分组里加目标。返回新增的个数（分组不存在时什么都不加）
    pub fn push_members(&mut self, name: &str, keys: &[String]) -> usize {
        let Some(cluster) = self.clusters.get_mut(name) else {
            return 0;
        };
        let mut added = 0usize;
        for key in keys {
            if cluster.members.insert(key.clone()) {
                added += 1;
            }
        }
        added
    }

    /// 从分组里摘目标
    pub fn ban_members(&mut self, name: &str, keys: &[String]) -> usize {
        let Some(cluster) = self.clusters.get_mut(name) else {
            return 0;
        };
        let mut removed = 0usize;
        for key in keys {
            if cluster.members.remove(key) {
                removed += 1;
            }
        }
        removed
    }

    /// 分组的管理员增删（只有创建者能改，命令层已判过）
    pub fn set_cluster_admins(&mut self, name: &str, users: &[i64], add: bool) -> usize {
        let Some(cluster) = self.clusters.get_mut(name) else {
            return 0;
        };
        let mut touched = 0usize;
        for user in users {
            let changed = if add {
                cluster.admins.insert(*user)
            } else {
                cluster.admins.remove(user)
            };
            if changed {
                touched += 1;
            }
        }
        touched
    }
}

/// 订阅表变了要通知谁：拿当前库快照上报展示板的那一个闭包
type Reporter = Box<dyn Fn(&Data) + Send + Sync>;

/// 内存里那份状态：命令改它、检测任务读它，读写各走一把 RwLock
pub struct Store {
    path: std::path::PathBuf,
    data: RwLock<Data>,
    /// 订阅表变了就重报一次框架的展示板（GUI 的群管理页读那张板子）。
    /// 挂在这里而不是让每个改订阅的命令自己调一遍，是因为"谁会忘了调"正是这种面板最先烂掉的方式：
    /// 落盘的那一手本来就在这里，报出去顺手就跟着做了。为空（还没 start 起来）时什么都不做。
    reporter: Mutex<Option<Reporter>>,
}

impl Store {
    /// 从盘上加载（文件不存在或损坏都按空库继续，损坏要记一条日志）
    pub fn open(path: impl AsRef<Path>) -> Store {
        let path = path.as_ref().to_path_buf();
        let data = match Data::load(&path) {
            Ok(data) => data,
            Err(reason) => {
                arona::runtime::log::warning(format!(
                    "bilibili：状态文件 {} 读不出来（{reason}），本轮按空库继续",
                    path.display()
                ));
                Data::default()
            }
        };
        Store {
            path,
            data: RwLock::new(data),
            reporter: Mutex::new(None),
        }
    }

    /// 挂上一个"订阅面变了"的回调。回调拿的是已经放开发写锁之后的快照，
    /// 所以它**可以**去读别的东西，但绝不该再回到 `Store` 里读写（那会把锁套锁）。
    pub fn attach_reporter(&self, reporter: impl Fn(&Data) + Send + Sync + 'static) {
        let mut slot = self
            .reporter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = Some(Box::new(reporter));
    }

    /// 只读取一次快照
    pub fn read<T>(&self, f: impl FnOnce(&Data) -> T) -> T {
        f(&self.data.read().unwrap())
    }

    /// 改完之后自动落盘；落盘失败只记日志，不回滚内存（否则命令会表现为"没反应"）
    pub fn write<T>(&self, f: impl FnOnce(&mut Data) -> T) -> T {
        let (result, snapshot) = {
            let mut guard = self.data.write().unwrap();
            let result = f(&mut guard);
            if let Err(reason) = guard.save(&self.path) {
                arona::runtime::log::warning(format!(
                    "bilibili：状态没存下去（{reason}），内存里的改动仍然生效"
                ));
            }
            // 快照是克隆出来的：报板子时不再持着写锁，回调里出任何岔子都不会把整张表锁死
            (result, guard.clone())
        };
        self.tell(&snapshot);
        result
    }

    /// 改内存但不落盘：批量导入时用，之后自己调一次 [`Store::flush`]（那一手会报板子）
    pub fn edit_only<T>(&self, f: impl FnOnce(&mut Data) -> T) -> T {
        f(&mut self.data.write().unwrap())
    }

    pub fn flush(&self) -> Result<(), String> {
        let (result, snapshot) = {
            let guard = self.data.read().unwrap();
            (guard.save(&self.path), guard.clone())
        };
        if result.is_ok() {
            self.tell(&snapshot);
        }
        result
    }

    fn tell(&self, data: &Data) {
        let reporter = self
            .reporter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(reporter) = reporter.as_ref() {
            (reporter)(data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> TargetState {
        TargetState {
            subs: [487550002i64].into_iter().collect(),
            ..Default::default()
        }
    }

    #[test]
    fn black_and_white_type_filters() {
        let black = FilterSet {
            type_mode: Mode::Black,
            types: vec!["转发动态".into()],
            ..Default::default()
        };
        assert!(!black.passes("转发动态", "anything"));
        assert!(black.passes("视频", "anything"));

        let white = FilterSet {
            type_mode: Mode::White,
            types: vec!["视频".into()],
            ..Default::default()
        };
        assert!(white.passes("视频", "anything"));
        assert!(!white.passes("转发动态", "anything"));
    }

    #[test]
    fn regex_filters_both_modes() {
        let black = FilterSet {
            reg_mode: Mode::Black,
            regs: vec!["^新年好".into(), "转发抽奖".into()],
            ..Default::default()
        };
        assert!(!black.passes("动态", "新年好呀"));
        assert!(!black.passes("动态", "内含转发抽奖"));
        assert!(black.passes("动态", "普通内容"));

        let white = FilterSet {
            reg_mode: Mode::White,
            regs: vec!["直播开播".into()],
            ..Default::default()
        };
        assert!(white.passes("直播", "直播开播了"));
        assert!(!white.passes("直播", "没关键词"));
    }

    #[test]
    fn a_broken_regex_does_not_block_pushing() {
        let filter = FilterSet {
            regs: vec!["(未闭合".into()],
            ..Default::default()
        };
        assert!(filter.passes("动态", "正文"));
    }

    #[test]
    fn both_filters_apply_independently() {
        let both = FilterSet {
            type_mode: Mode::Black,
            types: vec!["音乐".into()],
            reg_mode: Mode::Black,
            regs: vec!["抽奖".into()],
        };
        assert!(!both.passes("音乐", "普通"));
        assert!(!both.passes("动态", "有抽奖"));
        assert!(both.passes("动态", "普通"));
    }

    #[test]
    fn filter_lookup_falls_back_to_all_subs() {
        let mut state = target();
        state.filters.insert(
            "0".into(),
            FilterSet {
                types: vec!["视频".into()],
                ..Default::default()
            },
        );
        state.filters.insert(
            "487550002".into(),
            FilterSet {
                types: vec!["音乐".into()],
                ..Default::default()
            },
        );
        assert_eq!(
            state.filter_for(487550002).unwrap().types,
            vec!["音乐".to_string()]
        );
        assert_eq!(
            state.filter_for(999).unwrap().types,
            vec!["视频".to_string()]
        );
    }

    #[test]
    fn filter_and_at_all_vocabularies_are_separate() {
        // 类型过滤那套词里有"动态/转发动态"、没有"全部"
        assert_eq!(normalize_filter_kind("转发动态"), Some("转发动态"));
        assert_eq!(normalize_filter_kind("article"), Some("专栏"));
        assert_eq!(normalize_filter_kind("L"), Some("直播"));
        assert_eq!(normalize_filter_kind("全部"), None);
        // @全体 那套词反过来：有"全部/全部动态"、没有"动态"
        assert_eq!(normalize_at_all_kind("全部动态"), Some("全部动态"));
        assert_eq!(normalize_at_all_kind("d"), Some("全部动态"));
        assert_eq!(normalize_at_all_kind("动态"), None);
    }

    #[test]
    fn at_all_inclusion_matches_the_original() {
        let mut kinds = BTreeSet::new();
        kinds.insert("直播".to_string());
        assert!(at_all_hit(&kinds, "直播"));
        assert!(!at_all_hit(&kinds, "视频"));
        // "全部动态"盖住除直播外的一切
        let mut dynamic = BTreeSet::new();
        dynamic.insert("全部动态".to_string());
        assert!(at_all_hit(&dynamic, "视频"));
        assert!(!at_all_hit(&dynamic, "直播"));
        // "全部"通吃
        let mut all = BTreeSet::new();
        all.insert("全部".to_string());
        assert!(at_all_hit(&all, "直播"));
        assert!(at_all_hit(&all, "动态"));
    }

    /// 并集规则照抄原插件 addAtAll：加了"全部"就把别的清掉，加单种把两个通名挤掉
    #[test]
    fn at_all_merge_rules() {
        let mut kinds = BTreeSet::new();
        merge_at_all(&mut kinds, "视频");
        merge_at_all(&mut kinds, "全部");
        assert_eq!(kinds, ["全部".to_string()].into_iter().collect());

        let mut kinds = BTreeSet::new();
        merge_at_all(&mut kinds, "视频");
        merge_at_all(&mut kinds, "全部动态");
        assert_eq!(kinds, ["全部动态".to_string()].into_iter().collect());

        let mut kinds = BTreeSet::new();
        merge_at_all(&mut kinds, "全部动态");
        merge_at_all(&mut kinds, "音乐");
        assert_eq!(kinds, ["音乐".to_string()].into_iter().collect());

        // 直播能与单品种类并存，只被"全部"挤掉
        let mut kinds = BTreeSet::new();
        merge_at_all(&mut kinds, "视频");
        merge_at_all(&mut kinds, "直播");
        assert_eq!(
            kinds,
            ["视频".to_string(), "直播".to_string()]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn at_all_lookup_falls_back_to_all_subs() {
        let mut state = target();
        state
            .at_all
            .insert("0".into(), ["全部动态".to_string()].into_iter().collect());
        state.at_all.insert(
            "487550002".into(),
            ["直播".to_string()].into_iter().collect(),
        );
        assert!(state.at_all_for(487550002, "直播"));
        assert!(
            !state.at_all_for(487550002, "视频"),
            "专属档位不再吃 0 那一条"
        );
        assert!(state.at_all_for(999, "视频"));
    }

    #[test]
    fn target_keys_roundtrip() {
        assert_eq!(group_key(123), "g123");
        assert_eq!(parse_key("g123"), Some((true, 123)));
        assert_eq!(parse_key("f456"), Some((false, 456)));
        assert_eq!(parse_key("x123"), None);
        assert_eq!(parse_key("g"), None);
    }

    #[test]
    fn watermarks_only_move_forward() {
        let mut data = Data::default();
        assert!(data.advance_watermark(1, 100));
        assert!(!data.advance_watermark(1, 100), "同一条不该再推一次");
        assert!(!data.advance_watermark(1, 90), "旧动态不该倒灌水位线");
        assert!(data.advance_watermark(1, 101));
    }

    /// 分组的展开按"分组自己那份订阅"走，而不是"成员里有一户订了就连坐全组"
    #[test]
    fn targets_of_expands_cluster_follows() {
        let mut data = Data::default();
        data.target_mut("g1").subs.insert(487550002);
        data.target_mut("g2");
        data.clusters.insert(
            "主播群".into(),
            Cluster {
                creator: 1,
                members: ["g2".to_string()].into_iter().collect(),
                admins: BTreeSet::new(),
            },
        );
        // g2 只是 g1 的分组同伴，它自己没订过，就不该收到推送
        assert_eq!(data.targets_of(487550002), vec!["g1".to_string()]);
        // 分组名上订了才连坐成员，g1 那份自己的订阅还在
        data.follow_cluster("主播群", 487550002);
        assert_eq!(
            data.targets_of(487550002),
            vec!["g1".to_string(), "g2".to_string()]
        );
        data.target_mut("g3");
        data.push_members("主播群", &["g3".to_string()]);
        assert_eq!(
            data.targets_of(487550002),
            vec!["g1".to_string(), "g2".to_string(), "g3".to_string()]
        );
    }

    /// `/bili clear` 确认之后走 remove_targets：条目整条没了，下一轮才不会再把它列出来
    #[test]
    fn removed_targets_stop_coming_back() {
        let mut data = Data::default();
        data.target_mut("g1").subs.insert(7);
        data.target_mut("g1").seasons.insert("ss1".into());
        data.target_mut("g1").color = Some("#ff8800".into());
        data.clusters.insert(
            "主播群".into(),
            Cluster {
                creator: 1,
                members: ["g1".to_string()].into_iter().collect(),
                admins: BTreeSet::new(),
            },
        );
        // 没记录过的键不计数，但也不报错——清单里那份可能已经被别的会话清掉了
        assert_eq!(data.remove_targets(&["g1".into(), "g9".into()]), 2);
        assert!(data.targets.is_empty(), "配色那些也该跟着走");
        assert!(
            data.clusters["主播群"].members.is_empty(),
            "分组里的挂名要一并摘掉，不然推分组时又把这个死群算进去"
        );
        // 对照：clear_subs 只清内容，条目留着，所以下一轮 clear 还会列它
        data.target_mut("g2").subs.insert(7);
        assert_eq!(data.clear_subs(&["g2".into()]), 1);
        assert!(data.targets.contains_key("g2"));
    }

    /// `/bili mute` 只翻那一格：订阅、配色、模板全留着，`unmute` 得能原样恢复
    #[test]
    fn muting_a_target_keeps_every_subscription() {
        let mut data = Data::default();
        data.target_mut("g1").subs.insert(7);
        data.target_mut("g1").seasons.insert("ss1".into());
        data.target_mut("g1").color = Some("#ff8800".into());
        data.target_mut("g1")
            .templates
            .insert("dynamic".into(), "OneMsg".into());
        data.target_mut("g2").subs.insert(9);

        data.set_muted(&["g1".into(), "g2".into()], true);
        assert!(data.targets["g1"].muted);
        assert!(data.targets["g2"].muted);
        // 静音不该动到订阅关系：这三样是 /bili mute 唯一不该碰的
        assert_eq!(
            data.targets["g1"].subs,
            [7].into_iter().collect::<BTreeSet<i64>>()
        );
        assert!(data.targets["g1"].seasons.contains("ss1"));
        assert_eq!(
            data.targets["g1"].templates["dynamic"], "OneMsg",
            "模板覆盖不能被静音抹掉，否则恢复推送之后换了一套版式"
        );

        data.set_muted(&["g1".into()], false);
        assert!(!data.targets["g1"].muted);
        assert!(
            data.targets["g2"].muted,
            "只点名 g1 的时候不该顺手把 g2 也解开"
        );
    }

    /// `0` 是原插件 `dynamic[0]` 的通配位：订了它的目标什么动态都收，
    /// 但它不算"被订阅的 uid"（没人能去轮询 uid 0 的主页）
    #[test]
    fn the_zero_slot_takes_everything_but_is_not_a_uid() {
        let mut data = Data::default();
        data.target_mut("g1").subs.insert(0);
        data.target_mut("g2").subs.insert(7);
        assert_eq!(data.targets_of(7), vec!["g1".to_string(), "g2".to_string()]);
        assert_eq!(data.subscribed_uids(), vec![7]);
        assert!(data.wants_all());
    }

    #[test]
    fn subscribed_uids_is_deduplicated() {
        let mut data = Data::default();
        data.target_mut("g1").subs.insert(1);
        data.target_mut("g2").subs.extend([1, 2]);
        assert_eq!(data.subscribed_uids(), vec![1, 2]);
    }

    #[test]
    fn name_search_is_local_and_fuzzy() {
        let mut data = Data::default();
        data.names.insert("487550002".into(), "某幻君".into());
        data.names.insert("123".into(), "某某某".into());
        assert_eq!(data.find_uid_by_name("某"), vec![123, 487550002]);
        assert_eq!(data.find_uid_by_name("某幻"), vec![487550002]);
        assert!(data.find_uid_by_name("没见过的名字").is_empty());
        assert!(data.find_uid_by_name("").is_empty());
    }

    #[test]
    fn target_resolution_prefers_existing_records() {
        let mut data = Data::default();
        data.target_mut("f999").subs.insert(1);
        assert_eq!(
            data.resolve_target("999", Some("g1".into())),
            vec!["f999".to_string()]
        );
        // 全新目标：在群里发的就建群槽位
        assert_eq!(
            data.resolve_target("888", Some("g1".into())),
            vec!["g888".to_string()]
        );
        assert_eq!(
            data.resolve_target("888", Some("f2".into())),
            vec!["f888".to_string()]
        );
        assert_eq!(
            data.resolve_target("", Some("g1".into())),
            vec!["g1".to_string()]
        );
        assert!(
            data.resolve_target("不存在的分组", Some("g1".into()))
                .is_empty()
        );
    }

    /// 状态文件要能原样读回来：命令改完落盘、重启再读，丢一项都会表现为"订阅莫名消失"
    #[test]
    fn data_roundtrips_through_yaml() {
        let mut data = Data::default();
        let state = data.target_mut("g1");
        state.subs.insert(487550002);
        state.seasons.insert("ss11111".into());
        state.name = "测试群".into();
        state.color = Some("#fde8ed".into());
        state
            .at_all
            .insert("0".into(), ["直播".to_string()].into_iter().collect());
        state
            .templates
            .insert("dynamic".into(), "ForwardMsg".into());
        state.filters.insert(
            "0".into(),
            FilterSet {
                type_mode: Mode::White,
                types: vec!["视频".into()],
                reg_mode: Mode::Black,
                regs: vec!["抽奖".into()],
            },
        );
        data.clusters.insert(
            "分组".into(),
            Cluster {
                creator: 10001,
                members: ["g1".to_string()].into_iter().collect(),
                admins: [20002i64].into_iter().collect(),
            },
        );
        data.live.insert(
            "487550002".into(),
            LiveState {
                status: 1,
                room_id: 123,
                title: "开播了".into(),
                started_at: 1700000000,
                closed: false,
            },
        );
        let text = serde_yaml::to_string(&data).unwrap();
        let back: Data = serde_yaml::from_str(&text).unwrap();
        assert_eq!(back, data);
    }

    /// 落盘必须是原子的：写一半崩了不能把整份订阅清空
    #[test]
    fn save_writes_and_reloads_through_a_temp_file() {
        let dir = std::env::temp_dir().join(format!("bili-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.yml");
        let store = Store::open(&path);
        store.write(|data| {
            data.target_mut("g7").subs.insert(42);
        });
        assert!(path.exists());
        assert!(
            !path.with_extension("yml.tmp").exists(),
            "临时文件应当已被 rename 掉"
        );
        let reloaded = Store::open(&path);
        assert!(reloaded.read(|data| data.targets.get("g7").unwrap().subs.contains(&42)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 坏文件不该让插件起不来：记一条日志、按空库继续
    #[test]
    fn a_corrupt_file_falls_back_to_empty() {
        let dir = std::env::temp_dir().join(format!("bili-store-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.yml");
        std::fs::write(&path, "\t这根本不是 yaml: [").unwrap();
        assert!(Data::load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
