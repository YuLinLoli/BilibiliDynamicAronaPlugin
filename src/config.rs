//! 本插件的三块强类型配置区，落到框架运行目录的 `config/bilibili/arona.yml`。
//!
//! 对应原 mirai 插件的 `BiliConfig.yml` + `ImageQuality.yml` + `ImageTheme.yml` + 模板配置，
//! 区别是这里**不需要前置插件、也不需要改 YAML 之外的任何文件**：字段注释、默认值补齐、
//! 未知子键过滤、热重载全部由框架完成（PLUGIN_DEVELOPMENT.md §12）。
//!
//! 键名保留原插件的 camelCase（`#[serde(rename_all = "camelCase")]`），这样老用户拿原 README
//! 的表格就能逐行对照着迁移。
//!
//! 三块区各管一摊：
//! - `bili`：账号、检测节奏、推送节流、代理、翻译、链接解析、缓存过期
//! - `bili-image`：绘图分辨率档位、主题、字体、主题色、卡片装饰、页脚
//! - `bili-template`：动态/直播/直播结束的推送模板与转发卡片外观
//!
//! 区块键带 `bili-` 前缀：框架的区块表是全局唯一的（重名时先登记的一方胜出），
//! `image`、`template` 这种通名容易被别家插件抢走。

use arona::config::arona::PluginConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 主配置区键名
pub const SECTION: &str = "bili";
/// 绘图配置区键名
pub const IMAGE_SECTION: &str = "bili-image";
/// 模板配置区键名
pub const TEMPLATE_SECTION: &str = "bili-template";

// ==================== bili ====================

/// `bili` 区：功能开关、命令名、账号、检测、推送、缓存、代理、翻译、链接解析。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BiliConfig {
    /// 总开关：关掉后检测任务不再推送，命令仍然可用
    pub enable: bool,
    /// 框架管理员之外的额外管理员（QQ 号或群号）
    pub admin: Vec<i64>,
    /// 非管理员的操作是否给管理员发一条通报
    pub notify_enable: bool,
    /// 慢命令是否先发一条「加载中…」再撤回
    pub show_loading_message: bool,
    /// 主命令名与别名
    pub command: CommandConfig,
    /// B 站账号（Cookie 可手填，也可用 /bili login 扫码登录自动写入）
    pub account: AccountConfig,
    /// 动态/直播检测节奏
    pub check: CheckConfig,
    /// 推送节流与拼接方式
    pub push: PushConfig,
    /// 图片缓存
    pub cache: CacheConfig,
    /// 代理列表，留空表示跟随系统的 `HTTP(S)_PROXY`
    pub proxy: Vec<String>,
    /// 转发时给别人的动态补中文（机翻接口走框架 HTTP）
    pub translate: TranslateConfig,
    /// 短链/分享链还原
    pub link_resolve: LinkResolveConfig,
}

impl Default for BiliConfig {
    fn default() -> Self {
        BiliConfig {
            enable: true,
            admin: Vec::new(),
            notify_enable: true,
            show_loading_message: true,
            command: CommandConfig::default(),
            account: AccountConfig::default(),
            check: CheckConfig::default(),
            push: PushConfig::default(),
            cache: CacheConfig::default(),
            proxy: Vec::new(),
            translate: TranslateConfig::default(),
            link_resolve: LinkResolveConfig::default(),
        }
    }
}

impl PluginConfig for BiliConfig {
    const TITLE: &'static str = "B 站动态订阅";
    const DOC: &'static str = "订阅 UP 主动态/直播/番剧并推送到群或好友。\n\
         改完保存即热生效；只有 command 那一栏改名需要重启（命令表在装配阶段定型）。\n\
         命令与配置都不依赖任何前置插件。";

    fn comment(path: &str) -> Option<&'static str> {
        Some(match path {
            "enable" => "总开关：关掉后检测任务不再推送，命令仍然可用",
            "admin" => "框架管理员之外的额外管理员，写 QQ 号或群号",
            "notifyEnable" => {
                "别人（不在 admin 里的人）动了订阅/分组/模板/过滤/@全体 时，\
                 是否给每个管理员发一条私聊通报\n关掉就只在日志里记"
            }
            "showLoadingMessage" => {
                "慢命令（搜索、最新动态、最新视频）是否先发一条「加载中…」，\n出结果后撤回\n\
                 模板列表不挂这一档：它用假数据渲染，占位一条只会闪一下"
            }
            "command" | "command.names" => {
                "主命令名与别名，必须带前缀\n多个名字都指向同一棵子命令树\n改名要重启才生效"
            }
            "command.prefixFree" => "群聊里是否允许不带前缀直接发（默认要 @机器人 或带 /）",
            "account" => {
                "B 站账号 Cookie\n手填写法: SESSDATA=xxx; bili_jct=yyy\n留空也能起：插件会自己去取匿名设备指纹，\
                 订阅检测/番剧/开播/链接解析都不必登录；只有关注时间线、自动关注与分组才要扫码"
            }
            "account.cookie" => "完整 Cookie 串；/bili login 扫码成功后框架会把它写回这里",
            "account.uid" => "本 Cookie 所属的 B 站 uid，扫码登录会自动填；手填也行",
            "account.csrf" => "bili_jct，改关系（关注/加组）时要用；留空时从 cookie 里自动解析",
            "account.autoFollow" => "添加订阅时顺手关注这个 UP",
            "account.followGroup" => "自动关注时把对方归到哪个 B 站分组（最长 16 字符）",
            "check" => "检测节奏",
            "check.interval" => "动态检测间隔(秒)，推荐 15-60",
            "check.liveInterval" => "直播检测间隔(秒)",
            "check.lowSpeed" => {
                "低频时段，例 3-8x2 表示 3 点到 8 点间隔为正常间隔的 2 倍；留空表示全天同速"
            }
            "check.reportInterval" => "接口失败时每隔多少分钟记一条日志，0 表示每次失败都记",
            "check.timeout" => "单次 B 站请求的超时(秒)",
            "push" => "推送节流",
            "push.messageInterval" => "同一个群连续发多条之间的间隔(毫秒)，防风控",
            "push.pushInterval" => "连续两个目标之间的间隔(毫秒)",
            "push.atAllPlus" => {
                "@全体 拼在哪：SINGLE_MESSAGE=单独一条，PLUS_END=追加到最后一条后面"
            }
            "push.toShortLink" => "是否把推送里的长链换成短链（依赖第三方，不推荐）",
            "push.liveCloseNotify" => {
                "是否在直播结束时发一条结束通知；关掉只记状态，不再向订阅目标播报"
            }
            "cache" => "图片缓存",
            "cache.downloadOriginal" => "是否下载动态里的原图（关了就只画卡片，不缓存原图）",
            "cache.expires" => {
                "各类图片多少天没被再用到就清理；0 表示不清。键: DRAW/IMAGES/EMOJI/USER/OTHER"
            }
            "proxy" => "代理列表，如 http://127.0.0.1:7890；留空跟随系统环境变量",
            "translate" => "翻译（给非中文动态补一段译文）",
            "translate.enable" => "翻译开关",
            "translate.appId" => "百度翻译开放平台的 APP ID",
            "translate.securityKey" => "百度翻译的密钥，与 APP ID 成对；留空则不调翻译接口",
            "translate.cutLine" => "正文与译文之间的分割线",
            "linkResolve" => {
                "链接解析：把 B 站视频/专栏/番剧/动态/直播间/用户主页/短链这些地址画成一张卡回回去"
            }
            "linkResolve.enable" => "解析开关",
            "linkResolve.trigger" => {
                "什么时候才去解析：at=只有 @机器人 的消息才看，always=见消息就试，never=不解析"
            }
            "linkResolve.returnLink" => "出图之后是否再补一条原始链接",
            "linkResolve.patterns" => {
                "自定义识别式，捕获组一是拿去查详情的 id\n留空表示用内置那七条"
            }
            "linkResolve.allow" => "允许解析的域名白名单，留空表示内置那一份",
            "linkResolve.reject" => "额外禁止的域名",
            _ => return None,
        })
    }
}

/// 命令名：改这里等于改 `/bili` 这一棵树的名字。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CommandConfig {
    pub names: Vec<String>,
    pub prefix_free: bool,
}

impl Default for CommandConfig {
    fn default() -> Self {
        CommandConfig {
            names: vec!["/bili".into(), "/b".into(), "/动态".into()],
            prefix_free: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
#[derive(Default)]
pub struct AccountConfig {
    pub cookie: String,
    pub uid: i64,
    pub csrf: String,
    pub auto_follow: bool,
    pub follow_group: String,
}

impl AccountConfig {
    /// 从 Cookie 串里取某个字段，省得用户漏填 csrf
    pub fn field(&self, name: &str) -> Option<String> {
        self.cookie
            .split(';')
            .filter_map(|part| part.trim().split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    /// 生效的 bili_jct：显式配置优先，其次从 cookie 里解析
    pub fn csrf_value(&self) -> Option<String> {
        if !self.csrf.is_empty() {
            return Some(self.csrf.clone());
        }
        self.field("bili_jct")
    }

    /// 有没有可用的登录态
    pub fn signed(&self) -> bool {
        self.field("SESSDATA").is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CheckConfig {
    pub interval: u64,
    pub live_interval: u64,
    pub low_speed: String,
    pub report_interval: u64,
    pub timeout: u64,
}

impl Default for CheckConfig {
    fn default() -> Self {
        CheckConfig {
            interval: 30,
            live_interval: 15,
            low_speed: "3-8x2".into(),
            report_interval: 0,
            timeout: 20,
        }
    }
}

/// 低频时段：`3-8x2` → [3,8) 这几小时把间隔乘以 2。解析失败就当没配。
pub fn low_speed_window(raw: &str) -> Option<(u64, u64, u64)> {
    let (span, times) = raw.split_once('x')?;
    let factor: u64 = times.trim().parse().ok()?;
    let factor = factor.max(1);
    let (from, to) = span.split_once('-')?;
    let from: u64 = from.trim().parse().ok()?;
    let to: u64 = to.trim().parse().ok()?;
    if from > 23 || to > 24 || from == to {
        return None;
    }
    Some((from, to, factor))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PushConfig {
    pub message_interval: u64,
    pub push_interval: u64,
    pub at_all_plus: String,
    pub to_short_link: bool,
    /// 直播结束通知开关：关掉之后下播只记状态，不再向订阅目标播报那一条
    pub live_close_notify: bool,
}

impl Default for PushConfig {
    fn default() -> Self {
        PushConfig {
            message_interval: 500,
            push_interval: 1000,
            at_all_plus: "SINGLE_MESSAGE".into(),
            to_short_link: false,
            live_close_notify: true,
        }
    }
}

impl PushConfig {
    /// @全体 是否单独发一条（否则追加到最后一条消息后面）
    pub fn at_all_is_single(&self) -> bool {
        !self.at_all_plus.eq_ignore_ascii_case("PLUS_END")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CacheConfig {
    pub download_original: bool,
    pub expires: BTreeMap<String, u64>,
}

impl Default for CacheConfig {
    fn default() -> Self {
        let mut expires = BTreeMap::new();
        for (kind, days) in [
            ("DRAW", 7u64),
            ("IMAGES", 30),
            ("EMOJI", 90),
            ("USER", 90),
            ("OTHER", 30),
        ] {
            expires.insert(kind.into(), days);
        }
        CacheConfig {
            download_original: false,
            expires,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TranslateConfig {
    pub enable: bool,
    /// 百度翻译开放平台的 APP ID
    pub app_id: String,
    /// 百度翻译的密钥，与 APP ID 成对
    pub security_key: String,
    pub cut_line: String,
}

impl Default for TranslateConfig {
    fn default() -> Self {
        TranslateConfig {
            enable: false,
            app_id: String::new(),
            security_key: String::new(),
            cut_line: "\n\n〓〓〓 翻译 〓〓〓\n".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LinkResolveConfig {
    pub enable: bool,
    /// 什么情况下才去解析：`at`=只处理 @机器人 的消息，`always`=见消息就试，`never`=关着
    pub trigger: String,
    /// 解析出来之后，除了图再补一条原始链接
    pub return_link: bool,
    /// 自定义识别式（捕获组一是 id）。留空用内置那七条
    pub patterns: Vec<String>,
    pub allow: Vec<String>,
    pub reject: Vec<String>,
}

impl Default for LinkResolveConfig {
    fn default() -> Self {
        LinkResolveConfig {
            enable: true,
            trigger: "at".to_string(),
            return_link: false,
            patterns: Vec::new(),
            allow: Vec::new(),
            reject: Vec::new(),
        }
    }
}

/// 内置识别式，逐条抄自原插件 `LinkResolveConfig.regex`：
/// 视频、专栏、番剧、动态、直播间、用户主页、短链。捕获组一是拿去查详情的 id。
pub const DEFAULT_LINK_PATTERNS: [&str; 7] = [
    r"(www\.bilibili\.com/video/((BV[0-9A-Za-z]{10})|(av\d{1,20})))|^(BV[0-9A-Za-z]{10})$|^(av\d{1,20})$",
    r"(www\.bilibili\.com/read/cv\d{1,10})|(www\.bilibili\.com/read/mobile/\d{1,10})|^(cv\d{1,10})$",
    r"((www|m)\.bilibili\.com/bangumi/(play|media)/(ss|ep|md)\d+)|^((ss|ep|md)\d+)$",
    r"[tm]\.bilibili\.com/(?:dynamic/)?(\d+)|(?:www|m)\.bilibili\.com/opus/(\d+)",
    r"live\.bilibili\.com/(?:h5/)?(\d+)",
    r"space\.bilibili\.com/(\d+)",
    r"(?:b23\.tv|bili2233\.cn)/([0-9A-Za-z]+)",
];

/// 内置白名单：只还原这几个域名的短链，别拿它当通用跳转探针
pub const DEFAULT_LINK_ALLOW: [&str; 5] = [
    "b23.tv",
    "bilibili.com",
    "live.bilibili.com",
    "t.bilibili.com",
    "space.bilibili.com",
];

impl LinkResolveConfig {
    /// 这个 host 允不允许解析。allow 留空走内置白名单，写了就完全按用户那份来。
    pub fn allows(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.');
        if self
            .reject
            .iter()
            .any(|deny| covers(host, deny.trim_end_matches('.')))
        {
            return false;
        }
        if self.allow.is_empty() {
            return DEFAULT_LINK_ALLOW
                .iter()
                .any(|allow| covers(host, allow) || host == *allow);
        }
        self.allow
            .iter()
            .any(|allow| covers(host, allow.trim_end_matches('.')))
    }
}

/// host 是否落在 pattern 这个域名（或其子域）里
fn covers(host: &str, pattern: &str) -> bool {
    host.eq_ignore_ascii_case(pattern)
        || host
            .to_ascii_lowercase()
            .ends_with(&format!(".{}", pattern.to_ascii_lowercase()))
}

// ==================== bili-image ====================

/// `bili-image` 区：绘图外观。分辨率档位与主题的像素布局内置在 `draw::layout`，
/// 这里只暴露原插件里用户真会改的那几项。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImageConfig {
    /// 绘图总开关：关掉之后动态与直播推送只走模板文本，一张卡都不画
    pub draw_enable: bool,
    /// 图片宽度档位：800w / 1000w / 1200w / 1500w
    pub quality: String,
    /// 主题：v3 / v3RainbowOutline / v2
    pub theme: String,
    /// 字体名或字体文件名（放 config/bilibili/font/ 下）；留空自动找系统中文字体
    pub font: String,
    /// 默认主题色，多个用分号分隔构成渐变；单个值按 colorGenerator 自动生成渐变
    pub default_color: String,
    /// 卡片装饰：FanCard / QrCode / None
    pub card_ornament: String,
    /// 卡片顶部的标签，左右两档各一个开关
    pub badge_enable: BadgeEnable,
    /// 渐变生成
    pub color_generator: ColorGenerator,
    /// 页脚
    pub footer: FooterConfig,
}

impl Default for ImageConfig {
    fn default() -> Self {
        ImageConfig {
            draw_enable: true,
            quality: "1000w".into(),
            theme: "v3".into(),
            font: String::new(),
            default_color: "#d3edfa".into(),
            card_ornament: "None".into(),
            badge_enable: BadgeEnable::default(),
            color_generator: ColorGenerator::default(),
            footer: FooterConfig::default(),
        }
    }
}

impl PluginConfig for ImageConfig {
    const TITLE: &'static str = "B 站动态绘图";
    const DOC: &'static str = "推送卡片的分辨率、主题、字体与配色。\n\
         像素布局内置在插件里，这里改的是外观参数；\
         某个订阅想要别的配色，用 /bili color 单独设置，不必动这里。";

    fn comment(path: &str) -> Option<&'static str> {
        Some(match path {
            "quality" => "图片宽度档位：800w / 1000w / 1200w / 1500w",
            "theme" => "绘图主题：v3 / v3RainbowOutline / v2",
            "font" => {
                "字体名或字体文件名(不用加后缀)，放到 config/bilibili/font/ 下；留空自动找系统中文字体"
            }
            "defaultColor" => {
                "默认主题色，支持多个值自定义渐变，中间用分号分隔，例: #fde8ed;#fde8ed；单个值会自动生成渐变色"
            }
            "cardOrnament" => {
                "卡片装饰：QrCode(右下角画动态链接二维码) / None(不装饰)。FanCard(粉丝勋章) 这一档当前\
                 不出效果——插件没铺 B 站的勋章数据，填了等同 None"
            }
            "badgeEnable" => "卡片顶部的标签，左右两档各一个开关",
            "badgeEnable.left" => "左边那一档：动态类型（直播卡是分区）那枚标签",
            "badgeEnable.right" => "右边那一档：动态 id（直播卡是房间号）",
            "drawEnable" => {
                "绘图总开关：关掉之后动态与直播推送只发模板文本，一张卡都不画\
                 （原插件同样只管这两条推送路径，命令的「先看一眼」和链接解析不受它管）"
            }
            "colorGenerator" | "colorGenerator.hueStep" => {
                "渐变色生成器（仅当主题色只给了一个值时生效）"
            }
            "colorGenerator.lockSb" => "锁定饱和度与亮度",
            "colorGenerator.saturation" => "锁定的饱和度 0.0-1.0",
            "colorGenerator.brightness" => "锁定的亮度 0.0-1.0",
            "footer" => "图片页脚",
            "footer.dynamicFooter" => {
                "动态页脚模板，支持 {name} {uid} {id} {type} {time}；留空不画页脚"
            }
            "footer.liveFooter" => "直播页脚模板，同上",
            "footer.footerAlign" => "页脚对齐：LEFT / CENTER / RIGHT",
            _ => return None,
        })
    }
}

/// 卡片顶部那两枚标签的开关。左右是两档独立的键（原插件 `BadgeEnable` 就是这个形状，
/// 默认左边开、右边关），两个都关时整条标签带的高度一起收掉。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BadgeEnable {
    pub left: bool,
    pub right: bool,
}

/// 默认值照原插件：左边开、右边关（`BiliConfig.kt:187-189`）
impl Default for BadgeEnable {
    fn default() -> Self {
        BadgeEnable {
            left: true,
            right: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ColorGenerator {
    pub hue_step: i64,
    pub lock_sb: bool,
    pub saturation: f64,
    pub brightness: f64,
}

impl Default for ColorGenerator {
    fn default() -> Self {
        ColorGenerator {
            hue_step: 30,
            lock_sb: true,
            saturation: 0.25,
            brightness: 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FooterConfig {
    pub dynamic_footer: String,
    pub live_footer: String,
    pub footer_align: String,
}

impl Default for FooterConfig {
    fn default() -> Self {
        FooterConfig {
            dynamic_footer: String::new(),
            live_footer: String::new(),
            footer_align: "CENTER".into(),
        }
    }
}

// ==================== bili-template ====================

/// `bili-template` 区：推送模板。
///
/// 模板串里的占位符（与原插件一致）：
/// `{draw}` 绘图卡片、`{name}` 名称、`{uid}` 用户 ID、`{did}` 动态 ID、`{type}` 动态类型、
/// `{time}` 时间、`{content}` 正文、`{images}` 动态里的图、`{link}` 链接、`{links}` 多链接、
/// `\\n` 换行、`\\r` 拆成多条消息、`{>>}` `{<<}` 之间的内容包成合并转发。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TemplateConfig {
    pub default_dynamic_push: String,
    pub default_live_push: String,
    pub default_live_close: String,
    pub dynamic_push: BTreeMap<String, String>,
    pub live_push: BTreeMap<String, String>,
    pub live_close: BTreeMap<String, String>,
    pub forward_card: ForwardCard,
}

impl Default for TemplateConfig {
    fn default() -> Self {
        let mut dynamic_push = BTreeMap::new();
        dynamic_push.insert("OnlyDraw".into(), "{draw}".into());
        dynamic_push.insert("SimpleMsg".into(), "{draw}\r{content}\r{images}".into());
        dynamic_push.insert(
            "ForwardMsg".into(),
            "{draw}{>>}作者：{name}\nUID：{uid}\n时间：{time}\n类型：{type}\n链接：{link}\r{content}\r{images}{<<}".into(),
        );
        dynamic_push.insert(
            "NoDraw".into(),
            "{name} 更新了{type}\n{content}\n{images}\n{link}".into(),
        );
        // 下面四条是原插件 `TemplateConfig.dynamicPush` 里那几档，名字与内容都照搬
        // （`BiliConfig.kt:231-237`）：那边配置里写着 `defaultDynamicPush: OneMsg` 的，
        // 换到这边不改名就该查不到模板，所以这几个名字必须一直在。
        dynamic_push.insert("DrawOnly".into(), "{draw}".into());
        dynamic_push.insert(
            "TextOnly".into(),
            "{name}@{type}\n{link}\n{content}\n{images}".into(),
        );
        dynamic_push.insert("OneMsg".into(), "{draw}\n{name}@{type}\n{link}".into());
        dynamic_push.insert(
            "TwoMsg".into(),
            "{draw}\r{name}@{uid}@{type}\n{time}\n{link}".into(),
        );

        let mut live_push = BTreeMap::new();
        live_push.insert("OnlyDraw".into(), "{draw}".into());
        live_push.insert(
            "SimpleMsg".into(),
            "{draw}\r{name} 正在直播：{title}\n分区：{area}\n房间号：{rid}\n{link}".into(),
        );
        live_push.insert("TextOnly".into(), "{name} 开播了！\n{title}\n{link}".into());
        // 同样照搬原插件 `livePush` 那三档（`BiliConfig.kt:238-242`）
        live_push.insert("DrawOnly".into(), "{draw}".into());
        live_push.insert("OneMsg".into(), "{draw}\n{name}@直播\n{link}".into());
        live_push.insert(
            "TwoMsg".into(),
            "{draw}\r{name}@{uid}@直播\n{title}\n{time}\n{link}".into(),
        );

        let mut live_close = BTreeMap::new();
        live_close.insert(
            "SimpleMsg".into(),
            "{name} 的直播已结束\n标题：{title}\n分区：{area}\n房间号：{rid}\n开播：{startTime}\n结束：{endTime}\n时长：{duration}".into(),
        );
        // 原插件这一档只有两句（`BiliConfig.kt:244-247`），照原名留着
        live_close.insert(
            "ComplexMsg".into(),
            "{name} 直播结束啦!\n直播时长: {duration}".into(),
        );

        TemplateConfig {
            default_dynamic_push: "SimpleMsg".into(),
            default_live_push: "SimpleMsg".into(),
            default_live_close: "SimpleMsg".into(),
            dynamic_push,
            live_push,
            live_close,
            forward_card: ForwardCard::default(),
        }
    }
}

impl PluginConfig for TemplateConfig {
    const TITLE: &'static str = "B 站推送模板";
    const DOC: &'static str = "每个目标默认用哪套模板由 default* 决定，单个群/好友/分组可以用\n\
         /bili template 覆盖。模板串自己加新项即可，键名就是命令里写的模板名。\n\
         占位符：{draw} {name} {uid} {did} {type} {time} {content} {images} {link} {links}\n\
         直播另有 {rid} {title} {area} {cover} {startTime} {endTime} {duration}\n\
         换行 \\n，拆多条消息 \\r，{>>}…{<<} 之间的内容发成合并转发。";

    fn comment(path: &str) -> Option<&'static str> {
        Some(match path {
            "defaultDynamicPush" => "动态推送的默认模板名",
            "defaultLivePush" => "开播推送的默认模板名",
            "defaultLiveClose" => "直播结束通知的默认模板名",
            "dynamicPush" => "动态模板表：键是模板名，值是模板串",
            "livePush" => "直播模板表（不支持 {>>}{<<} 转发）",
            "liveClose" => "直播结束模板表（不支持 {>>}{<<} 与 \\r）",
            "forwardCard"
            | "forwardCard.title"
            | "forwardCard.preview"
            | "forwardCard.summary"
            | "forwardCard.brief" => {
                "QQ 转发卡片的外观，可用 {name} {uid} {did} {type} {time} {content} {link}"
            }
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ForwardCard {
    pub title: String,
    pub preview: String,
    pub summary: String,
    pub brief: String,
}

impl Default for ForwardCard {
    fn default() -> Self {
        ForwardCard {
            title: "{name} 的动态".into(),
            preview: "{content}".into(),
            summary: "点击查看完整动态".into(),
            brief: "{name}: {content}".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认值不能主动骚扰人：Cookie 空、频率保守
    #[test]
    fn defaults_are_conservative() {
        let config = BiliConfig::default();
        assert!(config.account.cookie.is_empty());
        assert!(config.proxy.is_empty());
        assert!(!config.translate.enable);
        assert!(config.check.interval >= 15, "间隔太短会被 B 站限流");
        assert_eq!(
            config.command.names.first().map(String::as_str),
            Some("/bili")
        );
        assert!(
            config
                .command
                .names
                .iter()
                .all(|name| name.starts_with('/')),
            "命令名必须带前缀，框架才认"
        );
    }

    #[test]
    fn cookie_fields_parse() {
        let account = AccountConfig {
            cookie: "SESSDATA=abc; bili_jct=def; dedeuserid=42".into(),
            ..Default::default()
        };
        assert_eq!(account.field("SESSDATA").as_deref(), Some("abc"));
        assert_eq!(account.csrf_value().as_deref(), Some("def"));
        assert_eq!(account.field("nope"), None);
        assert!(account.signed());
        assert!(!AccountConfig::default().signed());
    }

    #[test]
    fn low_speed_window_parses() {
        assert_eq!(low_speed_window("3-8x2"), Some((3, 8, 2)));
        assert_eq!(low_speed_window("22-6x3"), Some((22, 6, 3)));
        assert_eq!(low_speed_window(""), None);
        assert_eq!(low_speed_window("3-8"), None);
        assert_eq!(low_speed_window("30-8x2"), None);
    }

    #[test]
    fn at_all_placement_switch() {
        assert!(PushConfig::default().at_all_is_single());
        let plus_end = PushConfig {
            at_all_plus: "PLUS_END".into(),
            ..Default::default()
        };
        assert!(!plus_end.at_all_is_single());
    }

    /// 结束通知默认开着，而且这条键得在配置模板里露出来（原插件 `liveCloseNotifyEnable`）
    #[test]
    fn live_close_notify_defaults_on_and_is_documented() {
        assert!(PushConfig::default().live_close_notify);
        assert!(BiliConfig::comment("push.liveCloseNotify").is_some());
    }

    #[test]
    fn link_allowlist_blocks_foreign_hosts() {
        let config = LinkResolveConfig::default();
        assert!(config.allows("b23.tv"));
        assert!(config.allows("live.bilibili.com"));
        assert!(!config.allows("evil.example.com"));
        let custom = LinkResolveConfig {
            allow: vec!["example.org".into()],
            reject: vec!["bad.example.org".into()],
            ..Default::default()
        };
        assert!(custom.allows("example.org"));
        assert!(!custom.allows("bad.example.org"));
    }

    /// 原插件 `TemplateConfig` 里那几档模板名必须一直在（`BiliConfig.kt:227-247`）：
    /// 那边配置里写 `defaultDynamicPush: OneMsg` 的人换到这边不该需要改名
    #[test]
    fn the_original_template_names_all_exist() {
        let config = TemplateConfig::default();
        for name in ["DrawOnly", "TextOnly", "OneMsg", "TwoMsg", "ForwardMsg"] {
            assert!(
                config.dynamic_push.contains_key(name),
                "动态模板少了 {name}"
            );
        }
        for name in ["DrawOnly", "TextOnly", "OneMsg", "TwoMsg"] {
            assert!(config.live_push.contains_key(name), "直播模板少了 {name}");
        }
        assert!(config.live_close.contains_key("ComplexMsg"));
    }

    /// 模板表要能被配置区渲染出来：空表会被渲染成 `{}`，用户看不出该填什么
    #[test]
    fn builtin_templates_are_present() {
        let config = TemplateConfig::default();
        assert!(config.dynamic_push.contains_key("ForwardMsg"));
        assert!(config.live_push.contains_key("SimpleMsg"));
        assert!(config.live_close.contains_key("SimpleMsg"));
        assert!(
            config
                .dynamic_push
                .contains_key(&config.default_dynamic_push)
        );
    }

    /// 绘图这两档开关的默认值要跟原插件一致：总开关开、徽章左开右关（`BiliConfig.kt:145,187-189`）
    #[test]
    fn drawing_defaults_match_the_original() {
        let appearance = ImageConfig::default();
        assert!(appearance.draw_enable);
        assert!(appearance.badge_enable.left);
        assert!(!appearance.badge_enable.right);
        // 这三条键都得在配置模板里有注释，否则用户看不见有这个东西
        assert!(ImageConfig::comment("drawEnable").is_some());
        assert!(ImageConfig::comment("badgeEnable.left").is_some());
        assert!(ImageConfig::comment("badgeEnable.right").is_some());
    }

    /// 序列化回来的键名必须是 camelCase，否则老用户的配置抄不过来
    #[test]
    fn yaml_keys_stay_camel_case() {
        let text = serde_yaml::to_string(&BiliConfig::default()).unwrap();
        assert!(text.contains("liveInterval"), "{text}");
        assert!(text.contains("downloadOriginal"), "{text}");
        let back: BiliConfig = serde_yaml::from_str(&text).unwrap();
        assert_eq!(back, BiliConfig::default());
    }
}
