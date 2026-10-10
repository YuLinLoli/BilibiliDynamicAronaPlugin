//! B 站返回的结构化模型。
//!
//! 一律从 [`serde_json::Value`] 手工取字段而不是 derive：动态流的 shape 随 B 站前端版本变过
//! （同一批图片待过 `major.draw.items[].src` 与 `major.opus.pics[].url` 两种写法，
//! `mid` 一会儿是数字一会儿是字符串），derive 一旦对不上就整条丢。手工取可以多路兜底，
//! 实在取不到就返回 None 让上层跳过并记日志——别拿半截数据去画图。

use serde_json::Value;

/// 沿路径取节点。**JSON 的 `null` 当没这个键看待**：
///
/// 2026-09-30 抓的 feed/space 响应里，`major` 的每一族兄弟键都占着位置、值是 `null`
/// （`{"archive":null,"article":null,"live":null,"live_rcmd":{…},"draw":null,…}`），
/// 而 `detail` 那一份只带真正有内容的那一族。按「键在不在」分支的代码在动态流上会全线走错支：
/// 直播推荐动态因此被 `major.live = null` 顶掉、房间信息整块丢掉，卡片就成了空名空题空房间号。
pub(crate) fn at<'a>(root: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let found = keys.iter().try_fold(root, |node, key| node.get(*key))?;
    (!found.is_null()).then_some(found)
}

/// 取字符串；数字也接受
pub(crate) fn text(root: &Value, keys: &[&str]) -> Option<String> {
    match at(root, keys)? {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// 取整数：数字与"数字串"都认
pub(crate) fn number(root: &Value, keys: &[&str]) -> Option<i64> {
    match at(root, keys)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|v| v as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// 数组里每项取一个字段；`field` 为空表示数组本身就是字符串列表
fn list(root: &Value, keys: &[&str], field: &str) -> Vec<String> {
    let Some(items) = at(root, keys).and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            if field.is_empty() {
                item.as_str().map(str::to_string)
            } else {
                text(item, &[field])
            }
        })
        .collect()
}

/// 正文里的一小段。B 站把 `desc.rich_text_nodes` 拆好了类型，绘图按类型上色、表情按 url 贴图，
/// 就不必再拿正则去猜原文里的 `#话题#` 和 `[表情]`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Text(String),
    /// `#话题#`
    Topic(String),
    /// `@某人`
    At(String),
    /// 站外链接，`url` 是 B 站给的 jump_url
    Link {
        text: String,
        url: String,
    },
    /// `BV…` 视频号
    Video(String),
    Vote(String),
    Lottery(String),
    Shop(String),
    /// 争议提示条
    Dispute(String),
    /// B 站自定义表情：`text` 是 `[笑]` 这类原文，`url` 指向它的 png
    Emoji {
        text: String,
        url: String,
    },
}

impl Node {
    pub fn text(&self) -> &str {
        match self {
            Node::Text(value)
            | Node::Topic(value)
            | Node::At(value)
            | Node::Video(value)
            | Node::Vote(value)
            | Node::Lottery(value)
            | Node::Shop(value)
            | Node::Dispute(value) => value,
            Node::Link { text, .. } => text,
            Node::Emoji { text, .. } => text,
        }
    }

    /// 该用链接色（蓝）而不是正文色的那些
    pub fn is_link_like(&self) -> bool {
        matches!(
            self,
            Node::Topic(_)
                | Node::Link { .. }
                | Node::Video(_)
                | Node::Vote(_)
                | Node::Lottery(_)
                | Node::Shop(_)
                | Node::Dispute(_)
        )
    }
}

/// 一条链接（模板里的 {links} 逐条列出来）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub label: String,
    pub url: String,
}

/// 动态附带的视频/专栏/番剧/分享卡片
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attached {
    /// 视频 / 专栏 / 番剧 / 活动 …
    pub kind: String,
    pub title: String,
    pub cover: String,
    pub url: String,
    /// 时长文案，视频封面右下角那一格
    pub duration: String,
    /// 播放量 / 阅读量
    pub play: String,
    /// 副标题：附加卡与活动卡上的第二行
    pub desc: String,
}

/// 直播间信息
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveInfo {
    pub uid: i64,
    pub room_id: i64,
    pub anchor: String,
    /// 主播头像；列表接口给 `face`，单房间接口只能从名片补，仍为空时直播卡退回画个灰圆
    pub face: String,
    pub area: String,
    pub title: String,
    pub cover: String,
    pub online: i64,
    /// 开播时间（秒），0 表示接口没给
    pub live_start: i64,
    /// B 站的 live_status：0 未开播 1 直播中 2 已结束
    pub status: i64,
}

impl LiveInfo {
    pub fn link(&self) -> String {
        format!("https://live.bilibili.com/{}", self.room_id)
    }

    pub fn living(&self) -> bool {
        self.status == 1
    }

    /// 开播时间的展示串；接口没给时间就留空，卡片上不画假时间
    pub fn started_text(&self) -> String {
        if self.live_start <= 0 {
            String::new()
        } else {
            format_time(self.live_start)
        }
    }
}

/// 一条动态。只留模板与绘图真要用的字段。
/// `Default` 是给"接口没给全"和测试里的构造用的：字段全是可默认的类型。
#[derive(Clone, Debug, Default)]
pub struct Dynamic {
    pub id: i64,
    pub uid: i64,
    pub name: String,
    pub face: String,
    /// 判类用的规范词，只有六个：动态 / 转发动态 / 视频 / 音乐 / 专栏 / 直播。
    /// 类型过滤器与 @全体 认它，**不要**拿它去显示。
    pub kind: String,
    /// 显示用的类型名（投稿视频 / 合集 / 番剧…），进卡片角标与模板 `{type}`
    pub type_text: String,
    pub time: i64,
    pub text: String,
    /// 正文的分段（话题、表情、链接…），绘图按它上色贴图
    pub nodes: Vec<Node>,
    pub images: Vec<String>,
    pub links: Vec<Link>,
    pub attached: Option<Attached>,
    /// 正文下面那些附加小卡：预约、投票、商品、充电抽奖
    pub extra: Vec<Attached>,
    pub live: Option<LiveInfo>,
    /// 转发动态里被转的那条
    pub origin: Option<Box<Dynamic>>,
    /// B 站把内容挡掉了（屏蔽 / 仅粉丝 / 已删除），这句是它给的提示语
    pub blocked: Option<String>,
    /// 作者身上的那一格标签（"直播中"、"专属动态"…）
    pub label: String,
}

impl Dynamic {
    pub fn link(&self) -> String {
        format!("https://t.bilibili.com/{}", self.id)
    }

    pub fn time_text(&self) -> String {
        format_time(self.time)
    }

    /// 卡片正文：转发动态把被转发的那条拼在后面
    pub fn display_text(&self) -> String {
        match &self.origin {
            Some(source) => format!(
                "{}\n\n 转发 {} 的动态:\n{}",
                self.text, source.name, source.text
            ),
            None => self.text.clone(),
        }
    }

    /// 封面图：附带内容的封面优先，其次动态里的第一张图
    pub fn cover(&self) -> Option<String> {
        self.attached
            .as_ref()
            .map(|item| item.cover.clone())
            .filter(|cover| !cover.is_empty())
            .or_else(|| self.images.first().cloned())
    }
}

/// 秒级时间戳 → 北京时间文本。B 站接口不给时区，统一按 UTC+8 展示；
/// 格式沿用原插件的 `yyyy年MM月dd日 HH:mm:ss`，卡片与模板 `{time}` 共用这一份。
pub fn format_time(seconds: i64) -> String {
    use chrono::{FixedOffset, LocalResult, TimeZone};
    let Some(zone) = FixedOffset::east_opt(8 * 3600) else {
        return "-".to_string();
    };
    match zone.timestamp_opt(seconds, 0) {
        LocalResult::Single(value) => value.format("%Y年%m月%d日 %H:%M:%S").to_string(),
        _ => "-".to_string(),
    }
}

/// 北京时间文本 → 秒级时间戳，[`format_time`] 的反向。
///
/// 直播间的 `live_time` 不是时间戳而是 `yyyy-MM-dd HH:mm:ss` 字符串，未开播时 B 站给的是
/// 全零日期 `0000-00-00 00:00:00`——那种月份越界，解析自然失败，统一按「接口没给时间」返回 None。
pub(crate) fn parse_time(value: &str) -> Option<i64> {
    use chrono::{FixedOffset, LocalResult, NaiveDateTime, TimeZone};
    let naive = NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    let zone = FixedOffset::east_opt(8 * 3600)?;
    match zone.from_local_datetime(&naive) {
        LocalResult::Single(moment) => (moment.timestamp() > 0).then_some(moment.timestamp()),
        _ => None,
    }
}

/// 时长秒 → "1:02:03" / "12:34"
pub fn format_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (hour, minute, second) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if hour > 0 {
        format!("{hour}:{minute:02}:{second:02}")
    } else {
        format!("{minute}:{second:02}")
    }
}

/// 动态 id 的高 32 位是发布时间的秒数（相对 2017-06-30 的偏移）。
/// feed 里的 `pub_time` 常是"1小时前"这种人话，换算不出可比的水位线，所以时间靠 id 反推。
pub fn time_of_id(id: i64) -> i64 {
    (id >> 32) + 1_498_838_400
}

/// 类型英文 → 中文名。过滤器、卡片标签与 {type} 占位符共用这一份。
/// 判类用的规范词：只有这六个进得了类型过滤器与 @全体 那两张表
/// （照原插件 `SendTasker.toFilterType()`，`SendTasker.kt:212-230`）。
///
/// 必须与显示词分开：那边「投稿视频」「合集」「动态被删除」这些写法，
/// `FILTER_KINDS` 一个都不认——先前把显示词直接当判类词用，活动位与番剧类动态会被类型过滤器整类拒掉。
pub fn filter_kind(type_str: &str) -> &'static str {
    match type_str {
        "DYNAMIC_TYPE_FORWARD" => "转发动态",
        "DYNAMIC_TYPE_AV"
        | "DYNAMIC_TYPE_UGC_SEASON"
        | "DYNAMIC_TYPE_PGC"
        | "DYNAMIC_TYPE_PGC_UNION" => "视频",
        "DYNAMIC_TYPE_MUSIC" => "音乐",
        "DYNAMIC_TYPE_ARTICLE" | "DYNAMIC_TYPE_READ" => "专栏",
        "DYNAMIC_TYPE_LIVE" | "DYNAMIC_TYPE_LIVE_RCMD" => "直播",
        // 文字、图文、活动位（COMMON_*）、被删除与没见过的类型，原插件一律算进「动态」这一类
        _ => "动态",
    }
}

/// 显示词：卡片左上角那一格角标与模板 `{type}` 用
/// （照原插件 `DynamicType.text`，`Dynamic.kt:39-55`；模板侧见 `SendTasker.kt:423`）。
///
/// 与原插件唯一对不上的一处：那边 `DYNAMIC_TYPE_UNKNOWN` 显式写着「未知的动态」，
/// 而我们拿到的是 B 站原始字符串，没见过的值分不出是「站方的 UNKNOWN」还是「新出的类型」，
/// 所以两种都落成「动态」。
pub fn type_label(type_str: &str) -> &'static str {
    match type_str {
        "DYNAMIC_TYPE_ARTICLE" | "DYNAMIC_TYPE_READ" => "专栏",
        "DYNAMIC_TYPE_FORWARD" => "转发动态",
        "DYNAMIC_TYPE_AV" => "投稿视频",
        "DYNAMIC_TYPE_MUSIC" => "音乐",
        "DYNAMIC_TYPE_LIVE" | "DYNAMIC_TYPE_LIVE_RCMD" => "直播",
        "DYNAMIC_TYPE_PGC" | "DYNAMIC_TYPE_PGC_UNION" => "番剧",
        "DYNAMIC_TYPE_UGC_SEASON" => "合集",
        "DYNAMIC_TYPE_NONE" => "动态被删除",
        _ => "动态",
    }
}

pub fn kind_name(type_str: &str) -> String {
    let label = match type_str {
        "DYNAMIC_TYPE_FORWARD" => "转发动态",
        "DYNAMIC_TYPE_AV" | "DYNAMIC_TYPE_UGC_SEASON" | "DYNAMIC_TYPE_VIDEO" => "视频",
        "DYNAMIC_TYPE_MUSIC" => "音乐",
        "DYNAMIC_TYPE_ARTICLE" | "DYNAMIC_TYPE_READ" => "专栏",
        "DYNAMIC_TYPE_LIVE" | "DYNAMIC_TYPE_LIVE_RCMD" => "直播",
        "DYNAMIC_TYPE_PGC" | "DYNAMIC_TYPE_PGC_UNION" => "番剧",
        "DYNAMIC_TYPE_COMMON_SQUARE" | "DYNAMIC_TYPE_COMMON_VERTICAL" => "活动",
        "DYNAMIC_TYPE_NONE" => "已删除动态",
        _ => "动态",
    };
    label.to_string()
}

/// major 里带标题的那几格，按 B 站给出的顺序找
const MAJOR_SLOTS: [&str; 11] = [
    "archive",
    "article",
    "music",
    "pgc",
    "ugc_season",
    "common",
    "live",
    "opus",
    "video",
    "none",
    "blocked",
];

/// 从 major 里捞第一个能当正文的文本
fn major_body(major: &Value) -> String {
    for slot in MAJOR_SLOTS {
        for field in ["title", "summary", "hint_text", "tips"] {
            if let Some(value) = text(major, &[slot, field]) {
                return value;
            }
            if let Some(value) = text(major, &[slot, "summary", "text"]) {
                return value;
            }
        }
    }
    String::new()
}

/// 解析 major 里的直播卡片。LIVE_RCMD 的 content 是一段被字符串化的 JSON，要二次解析。
///
/// 2026-09-30 抓的真实包：`live_play_info` 的键是 `room_id/uid/title/area_name/parent_area_name/
/// cover/online/live_status/live_start_time/live_id/link/watched_show`，
/// **开播时间叫 `live_start_time`，不是 `live_start`；这一路压根不给主播名和头像**，
/// 那两格由动态作者顶上（见 `tasks::live_of`）。`user_cover` 是横屏封面，不是头像，别拿它填 `face`。
fn parse_live(major: &Value, own_uid: i64) -> Option<LiveInfo> {
    let raw = if let Some(info) = at(major, &["live"]) {
        info.clone()
    } else {
        let content = text(major, &["live_rcmd", "content"])?;
        let parsed = serde_json::from_str::<Value>(&content).ok()?;
        at(&parsed, &["live_play_info"])
            .or_else(|| at(&parsed, &["live_info", "live_play_info"]))?
            .clone()
    };
    let room_id = number(&raw, &["room_id"]).or_else(|| number(&raw, &["roomid"]))?;
    Some(LiveInfo {
        uid: number(&raw, &["uid"]).unwrap_or(own_uid),
        room_id,
        anchor: text(&raw, &["uname"]).unwrap_or_default(),
        // 实测这一路没有主播名与头像那两格；认 `face` 只是给旧结构留着，
        // 千万别学 batch 接口用 `user_cover`——那是横屏封面，画进头像格就是一张横图
        face: text(&raw, &["face"]).unwrap_or_default(),
        area: text(&raw, &["area_name"])
            .or_else(|| text(&raw, &["parent_area_name"]))
            .unwrap_or_default(),
        title: text(&raw, &["title"]).unwrap_or_default(),
        cover: text(&raw, &["cover"])
            .or_else(|| text(&raw, &["user_cover"]))
            .or_else(|| text(&raw, &["keyframe"]))
            .unwrap_or_default(),
        online: number(&raw, &["online"]).unwrap_or_default(),
        live_start: number(&raw, &["live_start_time"])
            .or_else(|| number(&raw, &["live_start"]))
            .unwrap_or_default(),
        status: number(&raw, &["live_status"]).unwrap_or(1),
    })
}

/// 解析 major / module_dynamic.common 里的附带内容
fn parse_attached(major: &Value, dynamic: &Value, type_str: &str) -> Option<Attached> {
    // 视频那一支单独处理：bvid 与 aid 两种写法都要认，时长和播放量也只在它身上有
    for slot in ["archive", "ugc_season"] {
        if at(major, &[slot]).is_some() {
            let url = text(major, &[slot, "bvid"])
                .map(|bvid| format!("https://www.bilibili.com/video/{bvid}"))
                .or_else(|| {
                    number(major, &[slot, "aid"])
                        .map(|aid| format!("https://www.bilibili.com/video/av{aid}"))
                })
                .unwrap_or_default();
            return Some(Attached {
                kind: "视频".into(),
                title: text(major, &[slot, "title"]).unwrap_or_default(),
                cover: text(major, &[slot, "cover"]).unwrap_or_default(),
                duration: text(major, &[slot, "duration_text"])
                    .or_else(|| number(major, &[slot, "duration"]).map(format_duration))
                    .unwrap_or_default(),
                play: text(major, &[slot, "stat", "view"]).unwrap_or_default(),
                desc: text(major, &[slot, "desc"]).unwrap_or_default(),
                url,
            });
        }
    }
    if let Some(article) = at(major, &["article"]) {
        let id = number(article, &["id"]).unwrap_or_default();
        return Some(Attached {
            kind: "专栏".into(),
            title: text(article, &["title"]).unwrap_or_default(),
            cover: list(article, &["covers"], "")
                .first()
                .cloned()
                .unwrap_or_default(),
            url: format!("https://www.bilibili.com/read/cv{id}"),
            ..Default::default()
        });
    }
    if let Some(pgc) = at(major, &["pgc"]) {
        let url = text(pgc, &["ep_link"]).unwrap_or_else(|| {
            format!(
                "https://www.bilibili.com/bangumi/play/ep{}",
                number(pgc, &["ep_id"]).unwrap_or_default()
            )
        });
        return Some(Attached {
            kind: "番剧".into(),
            title: text(pgc, &["title"]).unwrap_or_default(),
            cover: text(pgc, &["cover"]).unwrap_or_default(),
            url,
            ..Default::default()
        });
    }
    if let Some(music) = at(major, &["music"]) {
        return Some(Attached {
            kind: "音乐".into(),
            title: text(music, &["title"]).unwrap_or_default(),
            cover: text(music, &["cover"]).unwrap_or_default(),
            url: text(music, &["jump_url"])
                .or_else(|| text(music, &["url"]))
                .unwrap_or_default(),
            duration: text(music, &["duration_text"]).unwrap_or_default(),
            ..Default::default()
        });
    }
    // 普通分享卡片：major.common 与 module_dynamic.common 两个位置都出现过
    for candidate in [at(major, &["common"]), at(dynamic, &["common"])] {
        let Some(common) = candidate else { continue };
        let title = text(common, &["title"]);
        if title.is_none() {
            continue;
        }
        return Some(Attached {
            kind: kind_name(type_str),
            title: title.unwrap_or_default(),
            cover: text(common, &["cover"]).unwrap_or_default(),
            url: text(common, &["jump_url"])
                .or_else(|| text(common, &["url"]))
                .unwrap_or_default(),
            play: text(common, &["sub_title"]).unwrap_or_default(),
            ..Default::default()
        });
    }
    None
}

/// 正文分段。B 站把 `desc.rich_text_nodes` 拆好类型给出来，照它走就不必拿正则猜 `#话题#`。
/// 没给节点的老 shape 退回整段纯文本，正文不能因此变空。
pub fn parse_nodes(dynamic: &Value) -> Vec<Node> {
    let Some(items) = at(dynamic, &["desc", "rich_text_nodes"]).and_then(Value::as_array) else {
        return match text(dynamic, &["desc", "text"]) {
            Some(value) => vec![Node::Text(value)],
            None => Vec::new(),
        };
    };
    items
        .iter()
        .filter_map(|item| {
            let raw = text(item, &["orig_text"]).or_else(|| text(item, &["text"]))?;
            let kind = text(item, &["type"]).unwrap_or_default();
            Some(match kind.as_str() {
                "RICH_TEXT_NODE_TYPE_EMOJI" => Node::Emoji {
                    text: raw,
                    url: text(item, &["emoji", "icon_url"]).unwrap_or_default(),
                },
                "RICH_TEXT_NODE_TYPE_TOPIC" => Node::Topic(raw),
                "RICH_TEXT_NODE_TYPE_AT" => Node::At(raw),
                "RICH_TEXT_NODE_TYPE_WEB" => Node::Link {
                    text: raw,
                    url: text(item, &["jump_url"]).unwrap_or_default(),
                },
                "RICH_TEXT_NODE_TYPE_BV" => Node::Video(raw),
                "RICH_TEXT_NODE_TYPE_VOTE" => Node::Vote(raw),
                "RICH_TEXT_NODE_TYPE_LOTTERY" => Node::Lottery(raw),
                "RICH_TEXT_NODE_TYPE_GOODS" | "RICH_TEXT_NODE_TYPE_SHOP" => Node::Shop(raw),
                "RICH_TEXT_NODE_TYPE_DISPUTE" => Node::Dispute(raw),
                _ => Node::Text(raw),
            })
        })
        .collect()
}

/// 附加小卡。B 站按 `additional[].type` 给了七八种，字段名各不一样，
/// 这里只取每种都存在的四格（标题/封面/跳转/一行说明），画成同一种小卡。
pub fn parse_additional(dynamic: &Value) -> Vec<Attached> {
    let Some(items) = at(dynamic, &["additional"]).and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|entry| {
            let type_str = text(entry, &["type"]).unwrap_or_default();
            let head = match type_str.strip_prefix("ADDITIONAL_TYPE_")? {
                "RESERVE" => "预约",
                "VOTE" => "投票",
                "GOODS" => "商品",
                "UPOWER_LOTTERY" => "充电抽奖",
                "UGC_SEASON" => "合集",
                _ => "活动",
            };
            let item = entry.get("item")?;
            let pick = |keys: &[&str]| text(item, keys);
            // 投票和抽奖把内容挂在各自的子对象下，通用四格取不到再往那一层找一次
            let nested = at(item, &["vote"])
                .or_else(|| at(item, &["lottery"]))
                .or_else(|| at(item, &["premiere"]))
                .or_else(|| {
                    item.get("items")
                        .and_then(Value::as_array)
                        .and_then(|list| list.first())
                });
            let title = pick(&["title"])
                .or_else(|| pick(&["desc1"]))
                .or_else(|| nested.and_then(|inner| text(inner, &["desc"])))
                .or_else(|| nested.and_then(|inner| text(inner, &["title"])))
                .unwrap_or_default();
            // desc1/desc2 在 COMMON 下是裸字符串，在 RESERVE 下是 {text:…}，两种写法都认
            let desc = ["desc2", "desc3"]
                .iter()
                .find_map(|key| pick(&[*key]).or_else(|| pick(&[*key, "text"])))
                .or_else(|| pick(&["desc1", "text"]))
                .or_else(|| nested.and_then(|inner| text(inner, &["desc", "text"])))
                .or_else(|| {
                    nested
                        .and_then(|inner| text(inner, &["end_time"]))
                        .map(|end| format!("结束时间 {end}"))
                })
                .unwrap_or_default();
            let cover = pick(&["cover"])
                .or_else(|| nested.and_then(|inner| text(inner, &["cover"])))
                .unwrap_or_default();
            if title.is_empty() && cover.is_empty() {
                return None;
            }
            Some(Attached {
                kind: head.to_string(),
                desc,
                title,
                cover,
                url: pick(&["jump_url"]).unwrap_or_default(),
                ..Default::default()
            })
        })
        .collect()
}

/// 图片段：DRAW 走 items[].src，OPUS 走 pics[].url（新版），ARTICLE 走 covers[]
fn parse_images(major: &Value) -> Vec<String> {
    let mut images = list(major, &["draw", "items"], "src");
    if images.is_empty() {
        images = list(major, &["opus", "pics"], "url");
    }
    if images.is_empty() {
        images = list(major, &["opus", "pics"], "src");
    }
    if images.is_empty() {
        images = list(major, &["article", "covers"], "");
    }
    images
}

/// 把一条 feed item（或 detail 的 item）还原成 [`Dynamic`]
pub fn parse_item(item: &Value) -> Option<Dynamic> {
    let id = number(item, &["id_str"]).or_else(|| number(item, &["id"]))?;
    let author = at(item, &["modules", "module_author"])?;
    let uid = number(author, &["mid"]).unwrap_or_default();
    let type_str = text(item, &["type"]).unwrap_or_default();
    let dynamic = at(item, &["modules", "module_dynamic"])?;
    let major = dynamic.get("major").cloned().unwrap_or(Value::Null);

    let mut images = parse_images(&major);
    let attached = parse_attached(&major, dynamic, &type_str);
    let live = parse_live(&major, uid);

    // 封面没有独立字段时，把附带内容的封面并进图片列表（原插件的 {images} 就是这么来的）
    if let Some(cover) = attached
        .as_ref()
        .map(|item| item.cover.clone())
        .filter(|c| !c.is_empty())
        && !images.contains(&cover)
    {
        images.push(cover);
    }
    if let Some(room) = &live
        && !room.cover.is_empty()
        && !images.contains(&room.cover)
    {
        images.push(room.cover.clone());
    }

    let origin = item.get("orig").and_then(parse_item).map(Box::new);
    let nodes = parse_nodes(dynamic);
    let extra = parse_additional(dynamic);
    let body = text(dynamic, &["desc", "text"])
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let joined = nodes.iter().map(Node::text).collect::<Vec<_>>().join("");
            if joined.is_empty() {
                major_body(&major)
            } else {
                joined
            }
        });
    let label = text(author, &["label"]).unwrap_or_default();
    // 内容看不到时 B 站只给一句话：屏蔽挂在 blocked.hint_text，已删除挂在 none.tips
    let blocked =
        text(&major, &["blocked", "hint_text"]).or_else(|| text(&major, &["none", "tips"]));

    let mut links = vec![Link {
        label: "动态".into(),
        url: format!("https://t.bilibili.com/{id}"),
    }];
    if let Some(source) = &origin {
        links.push(Link {
            label: "原动态".into(),
            url: source.link(),
        });
    }
    if let Some(extra) = &attached
        && !extra.url.is_empty()
    {
        links.insert(
            0,
            Link {
                label: extra.kind.clone(),
                url: extra.url.clone(),
            },
        );
    }
    if let Some(room) = &live {
        links.insert(
            0,
            Link {
                label: "直播间".into(),
                url: room.link(),
            },
        );
    }

    Some(Dynamic {
        id,
        uid,
        name: text(author, &["name"]).unwrap_or_default(),
        face: text(author, &["face"]).unwrap_or_default(),
        kind: filter_kind(&type_str).to_string(),
        type_text: type_label(&type_str).to_string(),
        time: number(author, &["pub_ts"]).unwrap_or_else(|| time_of_id(id)),
        text: body,
        nodes,
        images,
        links,
        attached,
        extra,
        live,
        blocked,
        label,
        origin,
    })
}

/// feed 响应的 items，按时间升序——检测任务据此判断"哪几条是新的"
pub fn parse_feed(data: &Value) -> Vec<Dynamic> {
    let mut out: Vec<Dynamic> = at(data, &["items"])
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_item).collect())
        .unwrap_or_default();
    out.sort_by_key(|item| item.time);
    out
}

/// 关注的主播直播列表（`GetWebList` 的 rooms）。这一条要登录，匿名实测取不到包，
/// 所以字段映射没有像 `parse_live_status` 那样对过真实响应——登录后第一次用到如果发现不对，
/// 拿真实响应照 `user_and_wbi_shapes` 那个路子补一条 fixture 就能钉死。
pub fn parse_live_list(data: &Value) -> Vec<LiveInfo> {
    at(data, &["rooms"])
        .and_then(Value::as_array)
        .map(|rooms| {
            rooms
                .iter()
                .filter_map(|room| {
                    let room_id = number(room, &["room_id"]).unwrap_or_default();
                    let uid = number(room, &["uid"]).unwrap_or_default();
                    if room_id == 0 && uid == 0 {
                        return None;
                    }
                    Some(LiveInfo {
                        uid,
                        room_id,
                        anchor: text(room, &["anchor_name"]).unwrap_or_default(),
                        face: text(room, &["user_cover"]).unwrap_or_default(),
                        area: text(room, &["area_name"]).unwrap_or_default(),
                        title: text(room, &["title"]).unwrap_or_default(),
                        cover: text(room, &["cover"]).unwrap_or_default(),
                        online: number(room, &["online"]).unwrap_or_default(),
                        live_start: number(room, &["live_time"]).unwrap_or_default(),
                        status: number(room, &["live_status"]).unwrap_or(1),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `get_status_info_by_uids`：既见过数组，也见过 uid 索引的对象
pub fn parse_live_status(data: &Value) -> Vec<LiveInfo> {
    let rooms: Vec<Value> = match data {
        Value::Array(list) => list.clone(),
        Value::Object(map) => map
            .values()
            .map(|value| value.get("room").cloned().unwrap_or_else(|| value.clone()))
            .collect(),
        _ => Vec::new(),
    };
    rooms
        .iter()
        .filter_map(|room| {
            let room_id = number(room, &["roomid"]).or_else(|| number(room, &["room_id"]))?;
            // 批量状态接口不给 `cover`：主播没设自定义封面时只有 `keyframe`（直播截图）
            let cover = ["cover", "cover_from_user", "keyframe"]
                .into_iter()
                .find_map(|key| text(room, &[key]))
                .unwrap_or_default();
            Some(LiveInfo {
                uid: number(room, &["uid"]).unwrap_or_default(),
                room_id,
                anchor: text(room, &["uname"]).unwrap_or_default(),
                // 这一路给 `face`；不给就留空由主播资料接口补，不拿 `user_cover` 顶——那是横屏封面
                face: text(room, &["face"]).unwrap_or_default(),
                area: text(room, &["area_name"]).unwrap_or_default(),
                title: text(room, &["title"]).unwrap_or_default(),
                cover,
                online: number(room, &["online"]).unwrap_or_default(),
                live_start: number(room, &["live_time"])
                    .or_else(|| number(room, &["live_begin_timestamp"]))
                    .unwrap_or_default(),
                status: number(room, &["live_status"]).unwrap_or_default(),
            })
        })
        .collect()
}

/// 单房间详情（`room/v1/Room/get_info`）。
///
/// 真实响应把字段平铺在 `data` 下，而且**没有** `anchor_info`（2026-09-30 curl 核过：
/// 键是 `uid`/`room_id`/`title`/`user_cover`/`keyframe`/`area_name`/`live_status`/`online`/`live_time`）。
/// 所以主播名和头像这里取不到，由 [`crate::bili::api::Bili::room`] 拿 `uid` 补一次名片；
/// 留空就是留空，模板与卡片按"没有"处理，不编一个假名字上去。
/// `live_time` 是日期字符串不是时间戳，未开播时给全零日期，那条走 [`parse_time`] 的 None 分支。
/// 封面 `user_cover` 是主播上传的横版封面，没设过时为空串，退回 `keyframe`（实时截图）。
pub fn parse_room(data: &Value, requested: i64) -> LiveInfo {
    LiveInfo {
        uid: number(data, &["uid"]).unwrap_or_default(),
        room_id: number(data, &["room_id"])
            .or_else(|| number(data, &["roomid"]))
            .unwrap_or(requested),
        anchor: String::new(),
        face: String::new(),
        area: text(data, &["area_name"]).unwrap_or_default(),
        title: text(data, &["title"]).unwrap_or_default(),
        cover: text(data, &["user_cover"])
            .or_else(|| text(data, &["keyframe"]))
            .unwrap_or_default(),
        online: number(data, &["online"]).unwrap_or_default(),
        live_start: text(data, &["live_time"])
            .and_then(|value| parse_time(&value))
            .unwrap_or_default(),
        status: number(data, &["live_status"]).unwrap_or_default(),
    }
}

/// 用户名片：昵称、头像、签名、等级、直播间号
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserBrief {
    pub uid: i64,
    pub name: String,
    pub face: String,
    pub sign: String,
    pub level: i64,
    /// 直播间房间号，0 表示没开通过直播
    pub room_id: i64,
}

/// 直播用户资料（`live_user/v1/Master/info`）→ 名片形状，填 uid/name/face/room_id。
///
/// 字段名照 2026-09-30 curl 下来的真实包：名字在 `data.info.uname`、头像在 `data.info.face`、
/// 房间号在 `data.room_id`（与 `info` 平级），外层还有 `exp`/`follower_num`/`medal_name` 这些用不着的键。
/// 签名与等级这一路不给，那两格留默认值——要完整名片请走 [`parse_user`]。
pub fn parse_master(data: &Value) -> Option<UserBrief> {
    let info = data.get("info")?;
    let uid = number(info, &["uid"]).or_else(|| number(data, &["uid"]))?;
    Some(UserBrief {
        uid,
        name: text(info, &["uname"]).unwrap_or_default(),
        face: text(info, &["face"]).unwrap_or_default(),
        room_id: number(data, &["room_id"]).unwrap_or_default(),
        ..Default::default()
    })
}

/// 用户名片（`x/web-interface/card`）：资料都裹在 `data.card` 下面。
///
/// 换掉 `x/space/wbi/acc/info` 是因为那条要 WBI 签名，同一秒连打就被判风控
/// （2026-09-30 两次实测 `-352 风控校验失败`），而这条带上匿名设备指纹就能取，
/// 等级也是真值：同一天端到端里签名那条成功时回的是 Lv0，这条给 546195 的是 Lv6。
/// 房间号这一版的 `card` 里没给（实测键列表见单测），由 `Bili::user` 从主播资料补；
/// `room.room_id` 这条路径留着，B 站哪天给了就直接命中。
pub fn parse_user(data: &Value) -> Option<UserBrief> {
    let card = data.get("card")?;
    let uid = number(card, &["mid"])?;
    Some(UserBrief {
        uid,
        name: text(card, &["name"]).unwrap_or_default(),
        face: text(card, &["face"]).unwrap_or_default(),
        sign: text(card, &["sign"]).unwrap_or_default(),
        level: number(card, &["level_info", "current_level"]).unwrap_or_default(),
        room_id: number(card, &["room", "room_id"]).unwrap_or_default(),
    })
}

/// nav 里的两张 WBI 水印图名
pub fn parse_wbi_img(data: &Value) -> Option<(String, String)> {
    let img = text(data, &["wbi_img", "img_url"])?;
    let sub = text(data, &["wbi_img", "sub_url"])?;
    Some((
        crate::bili::wbi::key_of(&img),
        crate::bili::wbi::key_of(&sub),
    ))
}

/// 番剧更新（`pgc/view/web/season` 的 result）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Season {
    pub season_id: i64,
    pub title: String,
    pub cover: String,
    /// 最新一集的剧集号（`new_ep.id`）。基线比对认它——集号文本会撞车（同一话有中文配音版）
    pub newest_ep: i64,
    /// 最新一集给用户看的文本，如「第9话 命悬一线」
    pub newest_title: String,
}

/// 最新一集在 `new_ep` 那一格，集号是光秃秃的一个「9」，集名却在 `episodes` 对应项的
/// `long_title` 里。`newest` 那个键是早年的接口形状：2026-09-30 实测三部（含未完结的
/// ss298872）一个都不给，照它取等于最新集恒空——番剧更新因此从来没真推过一条。
pub fn parse_season(data: &Value) -> Option<Season> {
    let season_id = number(data, &["season_id"]).or_else(|| number(data, &["id"]))?;
    let newest_ep = number(data, &["new_ep", "id"]).unwrap_or_default();
    let episode = at(data, &["episodes"])
        .and_then(Value::as_array)
        .and_then(|eps| eps.iter().find(|ep| number(ep, &["id"]) == Some(newest_ep)))
        .cloned();
    let index = episode
        .as_ref()
        .and_then(|ep| text(ep, &["title"]))
        .or_else(|| text(data, &["new_ep", "title"]))
        .unwrap_or_default();
    let long = episode
        .as_ref()
        .and_then(|ep| text(ep, &["long_title"]))
        .unwrap_or_default();
    let newest_title = match (index.is_empty(), long.is_empty()) {
        (true, _) => long,
        (false, true) => format!("第{index}话"),
        (false, false) => format!("第{index}话 {long}"),
    };
    Some(Season {
        season_id,
        title: text(data, &["title"]).unwrap_or_default(),
        cover: text(data, &["cover"]).unwrap_or_default(),
        newest_ep,
        newest_title,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 判类词永远只能是类型过滤器那六词之一（照原插件 `toFilterType()`，`SendTasker.kt:212-230`）。
    /// 先前这里把显示词「活动」「番剧」当判类词用，而那两类动态因此在类型过滤器里谁都匹配不上——
    /// 用户设了白名单就整类静默漏推，设了黑名单则永远删不掉它们。
    #[test]
    fn filter_kind_only_ever_emits_words_the_filter_recognizes() {
        for raw in [
            "DYNAMIC_TYPE_WORD",
            "DYNAMIC_TYPE_DRAW",
            "DYNAMIC_TYPE_ARTICLE",
            "DYNAMIC_TYPE_READ",
            "DYNAMIC_TYPE_FORWARD",
            "DYNAMIC_TYPE_AV",
            "DYNAMIC_TYPE_MUSIC",
            "DYNAMIC_TYPE_LIVE",
            "DYNAMIC_TYPE_LIVE_RCMD",
            "DYNAMIC_TYPE_PGC",
            "DYNAMIC_TYPE_PGC_UNION",
            "DYNAMIC_TYPE_COMMON_SQUARE",
            "DYNAMIC_TYPE_COMMON_VERTICAL",
            "DYNAMIC_TYPE_UGC_SEASON",
            "DYNAMIC_TYPE_NONE",
            "DYNAMIC_TYPE_UNKNOWN",
            "DYNAMIC_TYPE_某天真新增的类型",
        ] {
            let kind = filter_kind(raw);
            assert!(
                crate::store::FILTER_KINDS.contains(&kind),
                "{raw} 判类成「{kind}」，而过滤器认的词表里没有它"
            );
        }
        // 用户截图那条「个性装扮」走 COMMON_SQUARE：判类与显示都该落回「动态」
        assert_eq!(filter_kind("DYNAMIC_TYPE_COMMON_SQUARE"), "动态");
        assert_eq!(type_label("DYNAMIC_TYPE_COMMON_SQUARE"), "动态");
        // 番剧按原插件归进「视频」这一类判，显示仍写「番剧」
        assert_eq!(filter_kind("DYNAMIC_TYPE_PGC"), "视频");
        assert_eq!(type_label("DYNAMIC_TYPE_PGC"), "番剧");
    }

    /// 显示词逐条对原插件 `DynamicType.text`（`Dynamic.kt:39-55`，模板侧取用见 `SendTasker.kt:423`）
    #[test]
    fn type_labels_match_the_original_enum() {
        for (raw, want) in [
            ("DYNAMIC_TYPE_WORD", "动态"),
            ("DYNAMIC_TYPE_DRAW", "动态"),
            ("DYNAMIC_TYPE_ARTICLE", "专栏"),
            ("DYNAMIC_TYPE_FORWARD", "转发动态"),
            ("DYNAMIC_TYPE_AV", "投稿视频"),
            ("DYNAMIC_TYPE_MUSIC", "音乐"),
            ("DYNAMIC_TYPE_LIVE", "直播"),
            ("DYNAMIC_TYPE_LIVE_RCMD", "直播"),
            ("DYNAMIC_TYPE_PGC", "番剧"),
            ("DYNAMIC_TYPE_PGC_UNION", "番剧"),
            ("DYNAMIC_TYPE_COMMON_SQUARE", "动态"),
            ("DYNAMIC_TYPE_COMMON_VERTICAL", "动态"),
            ("DYNAMIC_TYPE_UGC_SEASON", "合集"),
            ("DYNAMIC_TYPE_NONE", "动态被删除"),
        ] {
            assert_eq!(type_label(raw), want, "{raw} 的显示词与原插件不一致");
        }
    }

    /// 结构照真实样例（原插件 test/resources/json/dynamic_item 那批）缩编
    fn draw_item() -> Value {
        json!({
            "id_str": "1029210402684141574",
            "type": "DYNAMIC_TYPE_DRAW",
            "basic": {"comment_id_str": "341586801", "jump_url": "//www.bilibili.com/opus/1029210402684141574"},
            "modules": {
                "module_author": {"mid": 3546838751775383i64, "name": "优妮", "face": "https://i2.hdslb.com/a.jpg", "pub_time": "2025年02月02日 12:22", "label": ""},
                "module_dynamic": {"desc": {"text": "分享图片"}, "major": {"type": "MAJOR_TYPE_DRAW", "draw": {"items": [{"src": "https://i0.hdslb.com/1.png", "width": 1080, "height": 1920}]}}},
            }
        })
    }

    #[test]
    fn draw_item_yields_text_images_and_link() {
        let item = parse_item(&draw_item()).expect("该解析出来");
        assert_eq!(item.name, "优妮");
        assert_eq!(item.uid, 3546838751775383);
        assert_eq!(item.kind, "动态");
        assert_eq!(item.text, "分享图片");
        assert_eq!(item.images, vec!["https://i0.hdslb.com/1.png".to_string()]);
        assert_eq!(item.link(), "https://t.bilibili.com/1029210402684141574");
        assert!(item.origin.is_none());
    }

    #[test]
    fn id_carries_the_publish_time() {
        let time = time_of_id(1029210402684141574);
        assert_eq!(time, 1_738_470_120, "{time}");
        // 这条动态是 2025-02-02 04:22 UTC，按 B 站口径展示成北京时间的中文格式
        assert_eq!(format_time(time), "2025年02月02日 12:22:00");
    }

    #[test]
    fn opus_pics_are_read_as_images() {
        let mut item = draw_item();
        item["modules"]["module_dynamic"]["major"] = json!({
            "type": "MAJOR_TYPE_OPUS",
            "opus": {"title": "♡立绘公开♡", "summary": {"text": "看看！我很可爱"}, "pics": [{"url": "https://i0.hdslb.com/opus.png", "width": 1366, "height": 2048}]}
        });
        item["modules"]["module_dynamic"]["desc"] = Value::Null;
        let parsed = parse_item(&item).unwrap();
        assert_eq!(
            parsed.images,
            vec!["https://i0.hdslb.com/opus.png".to_string()]
        );
        // desc 没了就从 opus 的标题/摘要兜底，正文不能是空的
        assert!(!parsed.text.is_empty());
    }

    /// B 站把正文挡掉了（仅粉丝 / 已删除）时，卡片上就照抄它给的那句提示
    #[test]
    fn blocked_hint_is_carried_out() {
        let mut item = draw_item();
        item["modules"]["module_dynamic"]["major"] = json!({
            "type": "MAJOR_TYPE_BLOCKED",
            "blocked": {"hint_text": "你当前不是他的粉丝"}
        });
        let parsed = parse_item(&item).unwrap();
        assert_eq!(parsed.blocked.as_deref(), Some("你当前不是他的粉丝"));
    }

    #[test]
    fn forward_item_keeps_the_origin_and_two_links() {
        let mut item = draw_item();
        item["type"] = "DYNAMIC_TYPE_FORWARD".into();
        item["orig"] = json!({
            "id_str": "555111",
            "type": "DYNAMIC_TYPE_DRAW",
            "modules": {"module_author": {"mid": 42i64, "name": "被转发者", "face": ""}, "module_dynamic": {"desc": {"text": "原文"}}}
        });
        let parsed = parse_item(&item).unwrap();
        assert_eq!(parsed.kind, "转发动态");
        let source = parsed.origin.as_ref().expect("转发的原动态要在");
        assert_eq!(source.name, "被转发者");
        assert!(parsed.display_text().contains("转发 被转发者 的动态"));
        assert!(parsed.display_text().contains("原文"));
        assert!(parsed.links.iter().any(|link| link.label == "原动态"));
        assert_eq!(source.link(), "https://t.bilibili.com/555111");
    }

    #[test]
    fn archive_item_yields_cover_and_video_link() {
        let mut item = draw_item();
        item["type"] = "DYNAMIC_TYPE_AV".into();
        item["modules"]["module_dynamic"]["major"] = json!({
            "type": "MAJOR_TYPE_ARCHIVE",
            "archive": {"title": "新视频", "cover": "https://i0.hdslb.com/cover.jpg", "bvid": "BV1xx411c7mD", "duration_text": "03:12", "stat": {"view": "1.2万"}}
        });
        let parsed = parse_item(&item).unwrap();
        assert_eq!(parsed.kind, "视频");
        let attached = parsed.attached.clone().expect("视频卡信息");
        assert_eq!(attached.title, "新视频");
        assert_eq!(attached.url, "https://www.bilibili.com/video/BV1xx411c7mD");
        assert_eq!(attached.duration, "03:12");
        assert_eq!(attached.play, "1.2万");
        assert_eq!(
            parsed.images,
            vec!["https://i0.hdslb.com/cover.jpg".to_string()]
        );
        assert_eq!(
            parsed.cover(),
            Some("https://i0.hdslb.com/cover.jpg".to_string())
        );
        assert_eq!(parsed.links[0].label, "视频");
    }

    /// aid 写法也要认（老视频常用）
    #[test]
    fn archive_with_aid_only() {
        let mut item = draw_item();
        item["modules"]["module_dynamic"]["major"] = json!({
            "type": "MAJOR_TYPE_ARCHIVE",
            "archive": {"title": "老视频", "aid": 12345}
        });
        let parsed = parse_item(&item).unwrap();
        assert_eq!(
            parsed.attached.unwrap().url,
            "https://www.bilibili.com/video/av12345"
        );
    }

    #[test]
    fn live_rcmd_parses_its_double_encoded_content() {
        let mut item = draw_item();
        item["type"] = "DYNAMIC_TYPE_LIVE_RCMD".into();
        item["modules"]["module_dynamic"]["major"] = json!({
            "type": "MAJOR_TYPE_LIVE_RCMD",
            "live_rcmd": {"content": serde_json::to_string(&json!({"live_play_info": {"room_id": 23058, "uid": 123i64, "uname": "主播", "title": "开播", "area_name": "手游", "cover": "https://i0.hdslb.com/l.jpg", "online": 100, "live_status": 1}})).unwrap()}
        });
        let parsed = parse_item(&item).unwrap();
        assert_eq!(parsed.kind, "直播");
        let live = parsed.live.clone().expect("直播信息");
        assert_eq!(live.room_id, 23058);
        assert_eq!(live.anchor, "主播");
        assert!(live.living());
        assert_eq!(live.link(), "https://live.bilibili.com/23058");
        assert_eq!(parsed.links[0].label, "直播间");
    }

    #[test]
    fn plain_live_major_is_parsed_too() {
        let mut item = draw_item();
        item["type"] = "DYNAMIC_TYPE_LIVE".into();
        item["modules"]["module_dynamic"]["major"] = json!({
            "type": "MAJOR_TYPE_LIVE",
            "live": {"roomid": 999, "title": "直播中", "cover": "https://c/1.jpg", "area_name": "分区", "uname": "某人", "status": 1}
        });
        let parsed = parse_item(&item).unwrap();
        assert_eq!(parsed.live.unwrap().room_id, 999);
    }

    #[test]
    fn pgc_item_becomes_a_bangumi_link() {
        let mut item = draw_item();
        item["type"] = "DYNAMIC_TYPE_PGC".into();
        item["modules"]["module_dynamic"]["major"] = json!({
            "type": "MAJOR_TYPE_PGC",
            "pgc": {"title": "第 3 话", "cover": "https://i0.hdslb.com/pgc.jpg", "ep_id": 12345}
        });
        let parsed = parse_item(&item).unwrap();
        assert_eq!(parsed.kind, "视频", "番剧按原插件 toFilterType 归进「视频」这一类判");
        assert_eq!(parsed.type_text, "番剧", "显示词仍是「番剧」");
        assert_eq!(
            parsed.attached.unwrap().url,
            "https://www.bilibili.com/bangumi/play/ep12345"
        );
    }

    /// module_dynamic.common 那一支（音乐与普通分享卡片常挂在这）
    #[test]
    fn common_card_under_module_dynamic() {
        let mut item = draw_item();
        item["type"] = "DYNAMIC_TYPE_COMMON_SQUARE".into();
        item["modules"]["module_dynamic"]["common"] = json!({"title": "一首歌", "cover": "https://c/m.jpg", "jump_url": "https://www.bilibili.com/audio/am1"});
        item["modules"]["module_dynamic"]["major"] = Value::Null;
        let parsed = parse_item(&item).unwrap();
        assert_eq!(
            parsed.kind,
            "动态",
            "活动位（COMMON_SQUARE）判类必须落回「动态」——六词表里没有「活动」，落错就整类筛不到"
        );
        assert_eq!(parsed.type_text, "动态", "与原插件 DynamicType.text 同词");
        assert_eq!(parsed.attached.unwrap().title, "一首歌");
        assert_eq!(parsed.images, vec!["https://c/m.jpg".to_string()]);
    }

    /// B 站以后加的新类型不能整条丢
    #[test]
    fn unknown_major_still_produces_a_dynamic() {
        let mut item = draw_item();
        item["type"] = "DYNAMIC_TYPE_FUTURE".into();
        item["modules"]["module_dynamic"]["major"] = json!({"type": "MAJOR_TYPE_FUTURE"});
        let parsed = parse_item(&item).unwrap();
        assert_eq!(parsed.kind, "动态");
        // major 看不懂就只剩 desc 正文，这条仍然要能推出去
        assert_eq!(parsed.text, "分享图片");
        assert!(
            parsed.images.is_empty(),
            "图挂在看不懂的 major 里，取不到就是取不到"
        );
        assert!(parsed.blocked.is_none(), "未知类型不等于被屏蔽");
    }

    #[test]
    fn missing_blocks_are_dropped_not_panicked() {
        assert!(parse_item(&json!({"id_str": "1"})).is_none());
        assert!(parse_item(&json!({"modules": {"module_author": {}}})).is_none());
        assert!(parse_item(&json!({})).is_none());
    }

    #[test]
    fn feed_is_sorted_by_time() {
        let mut older = draw_item();
        older["id_str"] = "767166448722247682".into();
        let feed = parse_feed(&json!({"items": [draw_item(), older]}));
        assert_eq!(feed.len(), 2);
        assert!(
            feed[0].time <= feed[1].time,
            "检测任务按时间升序推才不会把旧的当新的"
        );
        assert!(parse_feed(&json!({"no_items": true})).is_empty());
    }

    #[test]
    fn live_list_and_status_shapes_both_parse() {
        let rooms = parse_live_list(
            &json!({"rooms": [{"room_id": 1, "uid": 2, "anchor_name": "a", "title": "t", "cover": "c", "area_name": "ar", "online": 5, "live_time": 100, "live_status": 1}]}),
        );
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].anchor, "a");
        assert_eq!(rooms[0].live_start, 100);
        assert!(parse_live_list(&json!({"rooms": [{"no_id": 1}]})).is_empty());

        let by_array = parse_live_status(
            &json!([{"uid": 7, "roomid": 8, "live_status": 0, "title": "x", "uname": "u"}]),
        );
        assert_eq!(by_array.len(), 1);
        assert_eq!(by_array[0].room_id, 8);
        assert_eq!(by_array[0].status, 0);

        let by_object = parse_live_status(
            &json!({"123": {"uid": 123, "room": {"roomid": 55, "live_status": 2, "title": "结束"}}}),
        );
        assert_eq!(by_object.len(), 1);
        assert_eq!(by_object[0].room_id, 55);
    }

    /// 一条「直播推荐」动态的真实包（2026-09-30 从 web-dynamic/v1/detail 抓的）：
    /// 房间信息裹在被字符串化的 `live_rcmd.content` 里。这一路解析不出来的话，
    /// 直播版式就拿一堆默认值画出一张主播名、标题、房间号全空的卡。
    #[test]
    fn a_live_recommend_dynamic_carries_its_room() {
        let content = json!({
            "type": "live_play",
            "live_record_info": null,
            "live_play_info": {
                "live_start_time": 1790761298, "room_id": 21852, "uid": 546195,
                "title": "最手忙脚乱的一集", "area_name": "其他单机", "parent_area_name": "单机游戏",
                "cover": "https://i0.hdslb.com/bfs/live/new_room_cover/0dcada.jpg",
                "online": 1092851, "live_status": 1, "live_id": "734393653329417564",
                "link": "//live.bilibili.com/21852?live_from=85002"
            }
        });
        let item = json!({
            "id_str": "1253801885478944772", "type": "DYNAMIC_TYPE_LIVE_RCMD",
            "modules": {
                "module_author": {
                    "mid": 546195, "name": "老番茄", "face": "https://i0.hdslb.com/bfs/face/bc5ca1.jpg"
                },
                "module_dynamic": {
                    "major": {"live_rcmd": {"content": content.to_string(), "reserve_type": 0}, "type": "live_rcmd"},
                    "desc": {"text": ""}
                }
            }
        });
        let dy = parse_item(&item).expect("这条直播推荐动态该解析出来");
        assert_eq!(dy.kind, "直播");
        assert_eq!(dy.name, "老番茄");
        let live = dy.live.clone().expect("直播间信息不能丢");
        assert_eq!(live.room_id, 21852);
        assert_eq!(live.uid, 546195);
        assert_eq!(live.title, "最手忙脚乱的一集");
        assert_eq!(live.area, "其他单机");
        assert_eq!(live.online, 1092851);
        assert_eq!(live.status, 1);
        assert!(live.living());
        // 开播时间在 content 里叫 `live_start_time`，写成 `live_start` 取到的一直是 0
        assert_eq!(live.live_start, 1790761298);
        assert_eq!(live.started_text(), "2026年09月30日 17:41:38");
        // 这个包不给主播名和头像，那两格由动态作者顶上（tasks::live_of）
        assert!(live.anchor.is_empty());
        assert!(live.face.is_empty());
        // 房间封面并进图片列表，直播卡才有底图可画
        assert!(dy.images.contains(&live.cover));
    }

    /// 动态流（feed/space）那一份包（2026-09-30 抓的）：`major` 里每一族兄弟键都占位、值是 `null`。
    /// 按「键在不在」分支的解析在这份包上会全线走错支——这一条就是钉住「null 当没这个键」。
    #[test]
    fn the_feed_shape_pads_every_major_family_with_null() {
        let content = json!({
            "type": 1,
            "live_record_info": Value::Null,
            "live_play_info": {
                "room_id": 21852, "uid": 546195, "live_status": 1,
                "title": "最手忙脚乱的一集", "cover": "https://i0.hdslb.com/bfs/live/new_room_cover/0dcada.jpg",
                "online": 1089108, "area_name": "其他单机", "parent_area_name": "单机游戏",
                "live_start_time": 1790761298, "live_id": 734393653329417564i64
            }
        });
        let item = json!({
            "id_str": "1253801885478944772", "type": "DYNAMIC_TYPE_LIVE_RCMD",
            "modules": {
                "module_author": {"mid": 546195, "name": "老番茄", "face": "https://i0.hdslb.com/bfs/face/bc5ca1.jpg"},
                "module_dynamic": {
                    "additional": null, "desc": null, "topic": null,
                    "major": {
                        "archive": null, "article": null, "blocked": null, "common": null,
                        "courses": null, "draw": null, "live": null, "medialist": null,
                        "music": null, "none": null, "opus": null, "pgc": null,
                        "subscription": null, "subscription_new": null, "ugc_season": null,
                        "upower_common": null, "type": "MAJOR_TYPE_LIVE_RCMD",
                        "live_rcmd": {"content": content.to_string(), "reserve_type": 0}
                    }
                }
            }
        });
        let dy = parse_item(&item).expect("动态流这份包也该解析出来");
        let live = dy
            .live
            .clone()
            .expect("`major.live` 是 null 不能把房间信息顶掉");
        assert_eq!(live.room_id, 21852);
        assert_eq!(live.title, "最手忙脚乱的一集");
        assert_eq!(live.live_start, 1790761298);
        // 附带内容也一样：满屏 null 的 `archive` 不该被当成视频附件
        assert!(dy.attached.is_none(), "拿 null 的 archive 当视频附件了");
        // 与 detail 那份包解析出同一条动态，两份形状就该给出同样的结果
        assert_eq!(dy.kind, "直播");
        assert_eq!(dy.id, 1253801885478944772);
    }

    /// 名片接口的真实包形状（2026-09-30 curl）：资料裹在 `data.card` 下面，等级在
    /// `level_info.current_level`，`mid` 是字符串。房间号这一版不给（card 的键里没有 room），
    /// 那一格由主播资料接口补——接口名一旦从 card 换回签名那条，这两行断言就会红。
    #[test]
    fn user_and_wbi_shapes() {
        let user = parse_user(&json!({
            "card": {
                "mid": "546195", "name": "老番茄", "sign": "新浪微博：_老番茄_",
                "face": "http://i0.hdslb.com/bfs/face/bc5ca101.jpg",
                "level_info": {"current_level": 6, "current_min": 0},
                "official_verify": {"type": 1, "desc": "2024百UP年度UP主"}, "vip": {"type": 2}
            },
            "following": false, "archive_count": 120, "article_count": 30, "follower": 12809000
        }))
        .unwrap();
        assert_eq!(user.uid, 546195);
        assert_eq!(user.name, "老番茄");
        assert_eq!(user.sign, "新浪微博：_老番茄_");
        assert_eq!(user.level, 6);
        assert_eq!(user.room_id, 0);
        assert!(parse_user(&json!({"mid": 1})).is_none());

        let (img, sub) = parse_wbi_img(&json!({"wbi_img": {"img_url": "https://i0.hdslb.com/bfs/wbi/7cd787b007c248e0a7c3d0f0c0b0c0de.png", "sub_url": "https://i0.hdslb.com/bfs/wbi/493cfcaff4a64f6d8aa33c1a5f0cba15.png"}})).unwrap();
        assert_eq!(img, "7cd787b007c248e0a7c3d0f0c0b0c0de");
        assert_eq!(sub.chars().count(), 32);
        assert!(parse_wbi_img(&json!({})).is_none());

        // 结构照 2026-09-30 抓的 ss298872（未完结、在更）：最新一集只有 `new_ep` 给 id，集名在 `episodes` 里
        let season = parse_season(&json!({
            "season_id": 298872,
            "title": "小车队大救援：自然护卫队",
            "cover": "https://i0.hdslb.com/bfs/bangumi/image/4e0a.jpg",
            "new_ep": {"desc": "连载中", "id": 6291966, "is_new": 1, "title": "9"},
            "episodes": [
                {"id": 6291908, "title": "1", "long_title": "命悬一线"},
                {"id": 6291966, "title": "9", "long_title": "发烧不出门"}
            ]
        }))
        .unwrap();
        assert_eq!(season.season_id, 298872);
        assert_eq!(season.newest_ep, 6291966);
        assert_eq!(season.newest_title, "第9话 发烧不出门");
        // `new_ep` 指的那一集不在 `episodes` 里（预售、番外那类）：退回它自己给的集号
        let orphan = parse_season(&json!({
            "season_id": 1, "new_ep": {"id": 777, "title": "3"}, "episodes": [{"id": 1, "title": "1"}]
        }))
        .unwrap();
        assert_eq!(orphan.newest_title, "第3话");
        // 最新集一格都没给：季度信息照收，文案留空由比对那侧决定不推
        let blank = parse_season(&json!({
            "season_id": 38942, "title": "爆笑罗米熊与丹米兔", "new_ep": null, "episodes": null
        }))
        .unwrap();
        assert_eq!(blank.newest_ep, 0);
        assert!(blank.newest_title.is_empty());
        assert_eq!(blank.title, "爆笑罗米熊与丹米兔");
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(75), "1:15");
        assert_eq!(format_duration(3671), "1:01:11");
        assert_eq!(format_duration(-5), "0:00");
        assert_eq!(kind_name("DYNAMIC_TYPE_MUSIC"), "音乐");
        assert_eq!(kind_name("DYNAMIC_TYPE_UNKNOWN"), "动态");
    }

    /// 开播批量状态接口的响应形状，字段名照 2026-09-30 抓的真实包：
    /// `data` 以 uid 字符串为键，封面叫 `cover_from_user`（没设自定义封面时只有 `keyframe`），
    /// 开播时间叫 `live_time`。这几个名字一旦写错，开播检测就静默拿到空值。
    #[test]
    fn live_status_map_flattens_the_real_shape() {
        let data = json!({
            "546195": {
                "title": "最手忙脚乱的一集", "room_id": 21852, "uid": 546195,
                "online": 1068182, "live_time": 1790761298, "live_status": 1,
                "area_name": "单机联机", "uname": "老番茄", "face": "https://i0.hdslb.com/f.jpg",
                "cover_from_user": "https://i0.hdslb.com/c.jpg", "keyframe": "https://i0.hdslb.com/k.jpg"
            },
            "23058": {
                "title": "无人直播的房", "room_id": 23058, "uid": 23058,
                "live_status": 0, "keyframe": "https://i0.hdslb.com/k2.jpg"
            }
        });
        let list = parse_live_status(&data);
        assert_eq!(list.len(), 2);
        let one = list.iter().find(|live| live.uid == 546195).unwrap();
        assert_eq!(one.room_id, 21852);
        assert_eq!(one.anchor, "老番茄");
        // 头像字段就叫 `face`；从前写成 ["user_cover","face"] 那条路径，取的一直是空
        assert_eq!(one.face, "https://i0.hdslb.com/f.jpg");
        assert_eq!(one.area, "单机联机");
        assert_eq!(one.cover, "https://i0.hdslb.com/c.jpg");
        assert_eq!(one.live_start, 1790761298);
        assert_eq!(one.status, 1);
        // 没设自定义封面的那间退回直播截图
        assert_eq!(
            list.iter().find(|live| live.uid == 23058).unwrap().cover,
            "https://i0.hdslb.com/k2.jpg"
        );
    }

    /// `live_time` 在单房间接口里是日期字符串，全零日期表示没开播过
    #[test]
    fn bj_date_string_parses_to_the_right_instant() {
        // 2026-09-30 20:00 北京时间 = 1790769600；一旦按 UTC 解析会差 8 小时
        assert_eq!(parse_time("2026-09-30 20:00:00"), Some(1790769600));
        assert_eq!(parse_time(" 2026-09-30 20:00:00 "), Some(1790769600));
        assert_eq!(parse_time("0000-00-00 00:00:00"), None);
        assert_eq!(parse_time("1793361600"), None);
        // 反向对得上，说明两边用的是同一个偏移
        assert_eq!(
            format_time(parse_time("2026-09-30 20:00:00").unwrap()),
            "2026年09月30日 20:00:00"
        );
    }

    /// 单房间详情（`Room/get_info`）的真实包形状，字段名照 2026-09-30 curl 下来的响应抄：
    /// 平铺在 `data` 下、**没有** `anchor_info`。上一版按 `anchor_info.base_info.uname` 取，
    /// 于是链接解析里主播名恒空——那三行取值一直是死的。
    #[test]
    fn room_info_is_flat_and_has_no_anchor_envelope() {
        let data = json!({
            "uid": 11153765, "room_id": 23058, "short_id": 3, "online": 0,
            "live_status": 0, "area_name": "唱见电台", "title": "哔哩哔哩音悦台",
            "user_cover": "", "keyframe": "https://i0.hdslb.com/bfs/live-key-frame/k23058.jpg",
            "live_time": "0000-00-00 00:00:00", "area_id": 190
        });
        let room = parse_room(&data, 23058);
        assert_eq!(room.uid, 11153765);
        assert_eq!(room.room_id, 23058);
        assert_eq!(room.title, "哔哩哔哩音悦台");
        assert_eq!(room.area, "唱见电台");
        // `user_cover` 是空串：text() 不认空串，于是退回 keyframe
        assert_eq!(
            room.cover,
            "https://i0.hdslb.com/bfs/live-key-frame/k23058.jpg"
        );
        assert_eq!(room.live_start, 0);
        assert_eq!(room.status, 0);
        // 这接口确实不给主播名，由 Bili::room 拿 uid 补名片
        assert!(room.anchor.is_empty());
        assert!(room.face.is_empty());

        // 开播中的那间：时间是日期字符串，要落成时间戳，卡片才画得出「几点开播」
        let living = parse_room(
            &json!({"uid": 1, "room_id": 2, "live_status": 1, "live_time": "2026-09-30 20:00:00"}),
            2,
        );
        assert_eq!(living.live_start, 1790769600);
        assert!(living.living());

        // room_id 缺失时认调用方给的那个，不退回 0 再把链接拼成 live.bilibili.com/0
        assert_eq!(parse_room(&json!({"uid": 7}), 23058).room_id, 23058);
    }

    /// 直播用户资料接口的真实包形状（2026-09-30 curl）：名字在 `info.uname`、头像在 `info.face`。
    /// 房间详情不给主播名，直播卡那两格就靠它补——这一路匿名可取且不需要 WBI 签名。
    #[test]
    fn master_gives_anchor_name_and_face() {
        let data = json!({
            "info": {
                "uid": 11153765, "uname": "3号直播间",
                "face": "https://i2.hdslb.com/bfs/face/5d35da6e.jpg",
                "official_verify": {"type": 1, "desc": "bilibili企业认证"}, "gender": -1
            },
            "follower_num": 1234, "room_id": 23058, "medal_name": ""
        });
        let anchor = parse_master(&data).unwrap();
        assert_eq!(anchor.uid, 11153765);
        assert_eq!(anchor.name, "3号直播间");
        assert_eq!(anchor.face, "https://i2.hdslb.com/bfs/face/5d35da6e.jpg");
        // 房间号在与 info 平级的位置上
        assert_eq!(anchor.room_id, 23058);
        // 只有名字头像房间号，签名等级那两格留默认值——要完整名片走 user()
        assert!(anchor.sign.is_empty());
        assert!(parse_master(&json!({"uid": 1})).is_none());
    }
}
