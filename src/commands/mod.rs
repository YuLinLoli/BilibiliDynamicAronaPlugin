//! `/bili` 那一整棵子命令树。
//!
//! 原 mirai 插件这层挂在 chat-command 上（`@SubCommand` 声明别名、`GroupOrContactParser`
//! 解析目标参数）。本插件不引任何前置插件：主命令名从配置读（`bili.command.names`），
//! 子命令、别名、参数位次都在这份文件里自己判——框架把命令名之后的参数按空白切成
//! `Vec<String>` 交进来，剩下的就是逐个位子取值。
//!
//! 参数位次沿用原插件的签名，缺省值也一样：
//! * `[目标]` 一律是最后一个参数，留空表示"当前会话这个群/这位好友"；
//!   写数字按群号或好友号找，写分组名则展开成那批成员（见 `store::Data::resolve_target`）。
//! * `[uid]` 留空即 `0`，意思是"对我在这个目标上的全部订阅生效"。
//!
//! 权限也照原插件的两条：`checkPerm` 管"能不能动这个目标"（自己的会话 / 自己管的分组 /
//! 框架管理员或 `bili.admin` 名单），`listAll`、`listUser`、`login`、`clear` 这几条只给管理员。
//!
//! 加子命令时只在 [`SUBS`] 加一行 + 在 [`dispatch`] 加一个分支：那个 `match` 打全枚举，
//! 漏了分支编译就报错，帮助文本和实际能力因此不会两处漂移。

mod browse;
mod filter;
mod group;
mod subscribe;

use std::sync::Arc;

use arona::plugin::PluginContext;
use arona::runtime::dispatcher::{CommandContext, CommandRegistration, handler};
use arona::runtime::message::OutgoingMessage;

use crate::bili::api::{self, Bili};
use crate::config::{BiliConfig, CommandConfig, SECTION};
use crate::store;

/// 子命令的回复。`None` 表示这条命令已经自己把东西发出去了（卡片图、合并转发预览），
/// 框架不用再回一句文本。
pub(crate) type Reply = Option<OutgoingMessage>;

/// 回一句文本
pub(crate) fn reply(text: impl Into<String>) -> Reply {
    Some(OutgoingMessage::text(text.into()))
}

/// 回一句并且拼上换行（多处用 `Vec::join` 组段，别在字符串里手写 `\n`）
pub(crate) fn lines(items: &[String]) -> Reply {
    reply(items.join("\n"))
}

/// [`SUBS`] 表里那条子命令的用法串，参数不全时回显它
pub(crate) fn usage_str(call: &Call, cmd: Cmd) -> String {
    SUBS.iter()
        .find(|(_, found, _, _)| *found == cmd)
        .map(|(_, _, text, _)| format!("{} {text}", call.prefix()))
        .unwrap_or_else(|| call.prefix())
}

/// 子命令的枚举：分发按它走，帮助表按它挂
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cmd {
    Help,
    Reload,
    Color,
    Add,
    Del,
    DelAll,
    List,
    ListAll,
    ListUser,
    FilterMode,
    FilterType,
    FilterReg,
    FilterList,
    FilterDel,
    TemplateList,
    Template,
    Login,
    AtAll,
    DelAtAll,
    ListAtAll,
    Config,
    Search,
    Live,
    New,
    Video,
    Create,
    ListGroup,
    DelGroup,
    AddGroupAdmin,
    BanGroupAdmin,
    Push,
    Ban,
    Clear,
}

/// 一行子命令：别名、用法、说明。第一个别名就是 `dispatch` 里认的那个规范名。
type Row = (&'static [&'static str], Cmd, &'static str, &'static str);

/// 子命令总表。别名逐个抄自原插件 `DynamicCommand.kt` 的 `@SubCommand`，
/// 再补上中文写法，用户抄原 README 的命令表就能直接用。
const SUBS: &[Row] = &[
    (&["h", "help", "帮助", "menu"], Cmd::Help, "", "看这份帮助"),
    (&["reload", "重载"], Cmd::Reload, "", "重新读一遍配置文件"),
    (
        &["color", "颜色"],
        Cmd::Color,
        "<#rrggbb[;#rrggbb…]> [目标]",
        "给某个目标单独指定卡片配色",
    ),
    (
        &["add", "follow", "添加", "订阅"],
        Cmd::Add,
        "<UP主UID|番剧号> [目标]",
        "订阅一个 UP 主，或追一部番",
    ),
    (
        &["del", "unfollow", "删除", "取消订阅"],
        Cmd::Del,
        "<UP主UID|名字|番剧号> [目标]",
        "取消订阅",
    ),
    (
        &["delall", "删除全部订阅"],
        Cmd::DelAll,
        "[目标]",
        "清空这个目标的全部订阅",
    ),
    (
        &["list", "列表"],
        Cmd::List,
        "[目标]",
        "看这个目标订了些什么",
    ),
    (
        &["listall", "la", "全部订阅列表"],
        Cmd::ListAll,
        "",
        "全局订阅总表（仅管理员）",
    ),
    (
        &["listuser", "lu", "用户列表"],
        Cmd::ListUser,
        "[UP主UID|名字]",
        "看某个 UP 被哪些目标订着（仅管理员）",
    ),
    (
        &["filtermode", "fm", "过滤模式"],
        Cmd::FilterMode,
        "<t|r> <w|b> [UID] [目标]",
        "设类型/正则过滤器的黑白名单模式",
    ),
    (
        &["filtertype", "ft", "类型过滤"],
        Cmd::FilterType,
        "<动态|转发动态|视频|音乐|专栏|直播> [UID] [目标]",
        "按动态类型过滤",
    ),
    (
        &["filterreg", "fr", "正则过滤"],
        Cmd::FilterReg,
        "<正则> [UID] [目标]",
        "按正文正则过滤",
    ),
    (
        &["filterlist", "fl", "过滤列表"],
        Cmd::FilterList,
        "[UID] [目标]",
        "看过滤器",
    ),
    (
        &["filterdel", "fd", "过滤删除"],
        Cmd::FilterDel,
        "<t0|r1…> [UID] [目标]",
        "按下标删一条过滤器",
    ),
    (
        &["templatelist", "tl", "模板列表"],
        Cmd::TemplateList,
        "[d|l|le]",
        "把每套模板各渲染一遍发成合并转发看效果",
    ),
    (
        &["template", "t", "模板"],
        Cmd::Template,
        "<d|l|le> <模板名> [目标]",
        "给某个目标换模板",
    ),
    (
        &["login", "登录"],
        Cmd::Login,
        "",
        "扫码登录 B 站账号，Cookie 写回配置（仅管理员）",
    ),
    (
        &["atall", "aa", "at全体"],
        Cmd::AtAll,
        "[全部|全部动态|直播|视频|音乐|专栏] [UID] [目标]",
        "这类内容推送时 @全体",
    ),
    (
        &["delatall", "daa", "取消at全体"],
        Cmd::DelAtAll,
        "[种类] [UID] [目标]",
        "取消 @全体",
    ),
    (
        &["listatall", "laa", "at全体列表"],
        Cmd::ListAtAll,
        "[UID] [目标]",
        "看 @全体 设置",
    ),
    (
        &["config", "配置"],
        Cmd::Config,
        "[UID] [目标]",
        "看这个目标当前生效的配置",
    ),
    (
        &["search", "s", "搜索"],
        Cmd::Search,
        "<动态ID>",
        "按动态 ID 取一条画出来看",
    ),
    (
        &["live", "直播"],
        Cmd::Live,
        "",
        "看当前在线人数最高的一间直播",
    ),
    (
        &["new", "最新动态"],
        Cmd::New,
        "<UID|名字> [条数]",
        "拉这个 UP 最新的几条动态",
    ),
    (
        &["video", "最新视频"],
        Cmd::Video,
        "<UID|名字>",
        "这个 UP 最新一条视频动态",
    ),
    (
        &["create", "创建分组"],
        Cmd::Create,
        "<分组名>",
        "建一个分组（推送目标可以整批写）",
    ),
    (
        &["listgroup", "lg", "分组列表"],
        Cmd::ListGroup,
        "[分组名]",
        "看分组，或看某个分组的明细",
    ),
    (
        &["delgroup", "dg", "删除分组"],
        Cmd::DelGroup,
        "<分组名>",
        "删分组（只有创建者能删）",
    ),
    (
        &["addgroupadmin", "aga", "添加分组管理员"],
        Cmd::AddGroupAdmin,
        "<分组名> <QQ号,…>",
        "授权别人也能替这个分组改订阅",
    ),
    (
        &["bangroupadmin", "bga", "删除分组管理员"],
        Cmd::BanGroupAdmin,
        "<分组名> <QQ号,…>",
        "收回授权",
    ),
    (
        &["push", "添加分组"],
        Cmd::Push,
        "<分组名> <群号|好友号,…>",
        "往分组里加目标",
    ),
    (
        &["ban", "移出分组"],
        Cmd::Ban,
        "<分组名> <群号|好友号,…>",
        "从分组里删目标",
    ),
    (
        &["clear", "清理失效目标"],
        Cmd::Clear,
        "[确定]",
        "清掉已经不在的群/好友的订阅（仅管理员）",
    ),
];

/// 按用户写的别名找子命令。别名比对不区分大小写（`/bili FT` 也能命中）。
fn find(raw: &str) -> Option<(Cmd, &'static str, &'static str)> {
    let key = raw.trim().to_lowercase();
    SUBS.iter()
        .find(|(aliases, _, _, _)| aliases.iter().any(|alias| alias.to_lowercase() == key))
        .map(|(_, cmd, usage, note)| (*cmd, *usage, *note))
}

/// 主命令名：`prefixFree=true` 时剥掉开头的前缀，交给框架按它自己的 `commandPrefixes` 判。
fn command_names(config: &CommandConfig) -> Vec<String> {
    config
        .names
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .map(|name| {
            if config.prefix_free {
                name.trim_start_matches(['/', '#']).to_string()
            } else {
                name.to_string()
            }
        })
        .collect()
}

/// 登记 `/bili` 这一棵树（装配阶段调用一次；改名要重启才生效）。
pub(crate) fn register(ctx: &PluginContext) {
    let config = ctx.config::<BiliConfig>(SECTION).get();
    let names = command_names(&config.command);
    if names.is_empty() {
        arona::runtime::log::warning("bilibili：bili.command.names 被清空了，一条命令都没登记");
        return;
    }
    let context = ctx.clone();
    ctx.command(
        CommandRegistration::new(
            names,
            "B 站动态订阅：订阅、分组、过滤器、模板、卡片绘图，全部在本插件里",
            handler(move |from, arguments| {
                let context = context.clone();
                run(context, from, arguments)
            }),
        )
        .with_feature(crate::FEATURE)
        .with_usage("/bili <子命令> [参数…]　/bili help 看全部子命令"),
    );
}

/// 一条命令的完整执行：读配置 → 认子命令 → 交出去。
async fn run(context: PluginContext, from: Arc<CommandContext>, arguments: Vec<String>) -> Reply {
    let conf = context.config::<BiliConfig>(SECTION).get();
    let mut call = Call {
        ctx: context,
        from,
        conf,
        args: arguments,
    };
    let Some(head) = call.args.first().cloned() else {
        return help(&call);
    };
    let Some((cmd, _, _)) = find(&head) else {
        return reply(format!(
            "没有这个子命令 [{head}]，发 {} help 看可用的命令",
            call.prefix()
        ));
    };
    call.args.remove(0);
    dispatch(cmd, &call).await
}

/// 子命令分发。`match` 打全 [`Cmd`]，加了变体忘了分支就编译不过。
async fn dispatch(cmd: Cmd, call: &Call) -> Reply {
    match cmd {
        Cmd::Help => help(call),
        Cmd::Reload => browse::reload(call),
        Cmd::Color => subscribe::color(call).await,
        Cmd::Add => subscribe::add(call).await,
        Cmd::Del => subscribe::del(call).await,
        Cmd::DelAll => subscribe::del_all(call).await,
        Cmd::List => subscribe::list(call).await,
        Cmd::ListAll => subscribe::list_all(call).await,
        Cmd::ListUser => subscribe::list_user(call).await,
        Cmd::FilterMode => filter::mode(call),
        Cmd::FilterType => filter::kind(call),
        Cmd::FilterReg => filter::regex(call),
        Cmd::FilterList => filter::list(call),
        Cmd::FilterDel => filter::remove(call),
        Cmd::TemplateList => browse::template_list(call).await,
        Cmd::Template => filter::template(call),
        Cmd::Login => browse::login(call).await,
        Cmd::AtAll => filter::at_all(call, true).await,
        Cmd::DelAtAll => filter::at_all(call, false).await,
        Cmd::ListAtAll => filter::at_all_list(call),
        Cmd::Config => browse::config(call).await,
        Cmd::Search => browse::search(call).await,
        Cmd::Live => browse::live(call).await,
        Cmd::New => browse::new(call).await,
        Cmd::Video => browse::video(call).await,
        Cmd::Create => group::create(call),
        Cmd::ListGroup => group::list(call),
        Cmd::DelGroup => group::remove(call),
        Cmd::AddGroupAdmin => group::admins(call, true).await,
        Cmd::BanGroupAdmin => group::admins(call, false).await,
        Cmd::Push => group::members(call, true).await,
        Cmd::Ban => group::members(call, false).await,
        Cmd::Clear => browse::clear(call).await,
    }
}

/// 帮助文本。原插件发的是一张 `HELP.png`，本插件没随包图，直接列文本——
/// 顺带把命令名换成用户自己配的那个前缀，比图片更不容易过期。
fn help(call: &Call) -> Reply {
    let prefix = call.prefix();
    let mut out = vec![
        "B 站动态订阅插件".to_string(),
        format!("用法：{prefix} <子命令> [参数…]"),
        String::new(),
    ];
    for (aliases, _, usage, note) in SUBS {
        let name = aliases.join("/");
        out.push(if usage.is_empty() {
            format!("  {prefix} {name} — {note}")
        } else {
            format!("  {prefix} {name} {usage} — {note}")
        });
    }
    out.push(String::new());
    out.push(format!(
        "[目标] 留空就是当前会话；可以写群号/好友号，也可以写分组名。配置改动即时生效，只有改名要重启。"
    ));
    lines(&out)
}

/// 一次命令调用
pub(crate) struct Call {
    ctx: PluginContext,
    from: Arc<CommandContext>,
    conf: BiliConfig,
    /// 子命令之后的参数
    args: Vec<String>,
}

impl Call {
    pub(crate) fn ctx(&self) -> &PluginContext {
        &self.ctx
    }

    pub(crate) fn conf(&self) -> &BiliConfig {
        &self.conf
    }

    pub(crate) fn from(&self) -> &CommandContext {
        &self.from
    }

    /// 第 `index` 个参数（去掉子命令名之后重新数）
    pub(crate) fn arg(&self, index: usize) -> Option<&str> {
        self.args
            .get(index)
            .map(String::as_str)
            .filter(|raw| !raw.trim().is_empty())
    }

    /// 从 `index` 起的剩余参数拼回一句（分组名、页脚这种带空格的写法要用它）
    pub(crate) fn rest(&self, index: usize) -> String {
        self.args
            .get(index..)
            .map(|tail| tail.join(" "))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    /// 用户配的第一个命令名，帮助与提示里都用它
    pub(crate) fn prefix(&self) -> String {
        command_names(&self.conf.command)
            .first()
            .cloned()
            .unwrap_or_else(|| "/bili".to_string())
    }

    /// 当前会话的目标键：群里就是 `g<群号>`，私聊就是 `f<发命令的人>`
    pub(crate) fn current_key(&self) -> String {
        match self.from.group_id {
            Some(group_id) => store::group_key(group_id),
            None => store::friend_key(self.from.user_id),
        }
    }

    /// 发命令那个会话的号：群里是群号，私聊是那个人。
    /// 原插件的分组归属（`createGroup(name, subject?.id)` 与 `checkGroupPerm`）比的就是它，
    /// 也就是"在群里建的组归这个群"，不是归某个 QQ 用户。
    pub(crate) fn current_id(&self) -> i64 {
        self.from.group_id.unwrap_or(self.from.user_id)
    }

    /// 框架管理员，或写在 `bili.admin` 里的 QQ 号/群号
    pub(crate) fn is_bot_admin(&self) -> bool {
        self.from.is_admin
            || self.conf.admin.contains(&self.from.user_id)
            || self
                .from
                .group_id
                .is_some_and(|group_id| self.conf.admin.contains(&group_id))
    }

    /// 按当前配置起一个 B 站接口客户端
    pub(crate) fn bili(&self) -> Arc<Bili> {
        api::client(self.conf.account.clone(), self.conf.check.timeout.max(5))
    }

    /// 取必填参数，缺了就回用法
    pub(crate) fn need(&self, index: usize, usage: &str) -> Result<String, String> {
        self.arg(index)
            .map(str::to_string)
            .ok_or_else(|| format!("参数不完整，用法：{}{}", self.prefix(), usage))
    }

    /// 解析 `[目标]` 并判权限。
    pub(crate) fn targets(&self, index: usize) -> Result<Targets, String> {
        let raw = self.arg(index).unwrap_or("").trim().to_string();
        let current = self.current_key();
        if raw.is_empty() {
            return Ok(Targets {
                keys: vec![current],
                cluster: None,
                raw,
                current: self.current_key(),
            });
        }
        let owned = crate::store().read(|data| {
            data.clusters
                .get(&raw)
                .map(|cluster| cluster.governs(self.current_id()))
        });
        let keys = crate::store().read(|data| data.resolve_target(&raw, Some(current.clone())));
        if keys.is_empty() && owned.is_none() {
            return Err(format!("没有这个目标或分组 [{raw}]"));
        }
        // 就是当前会话：谁都能动自己
        if owned.is_none() && !keys.is_empty() && keys.iter().all(|key| key == &current) {
            return Ok(Targets {
                keys,
                cluster: None,
                raw,
                current,
            });
        }
        if self.is_bot_admin() {
            return Ok(Targets {
                cluster: owned.map(|_| raw.clone()),
                keys,
                raw,
                current,
            });
        }
        match owned {
            Some(true) => Ok(Targets {
                keys,
                cluster: Some(raw.clone()),
                raw,
                current,
            }),
            Some(false) => Err("权限不足, 无法操作其他分组".to_string()),
            None => Err("权限不足, 无法操作其他人".to_string()),
        }
    }

    /// `[UID]` 这一档参数：留空即 `0`（对该目标的全部订阅生效）。
    /// 过滤器与 @全体 都按 uid 分档，写错了要能说清。
    pub(crate) fn sub_uid(&self, index: usize) -> Result<i64, String> {
        match self.arg(index) {
            None => Ok(0),
            Some(raw) => raw
                .parse::<i64>()
                .map_err(|_| "UID 要写数字，留空表示对该目标的全部订阅都生效".to_string()),
        }
    }

    /// `[UP主UID|名字]` 这一档：数字直接用，名字在本地名单里模糊找。留空即 `0`。
    pub(crate) fn matched_uid(&self, index: usize) -> Match {
        match self.arg(index) {
            None => Match::One(0),
            Some(raw) => match_uid(raw),
        }
    }
}

/// 一条命令要操作的那批目标
pub(crate) struct Targets {
    /// 展开后的目标键（分组已经摊平成成员）
    pub(crate) keys: Vec<String>,
    /// 用户写的是分组名：订阅关系另外记在分组自己那份上，成员后来才加进来的也收得到
    pub(crate) cluster: Option<String>,
    /// 用户写的那个参数原样，列表类输出拿它当标题
    pub(crate) raw: String,
    /// 当前会话的目标键
    current: String,
}

impl Targets {
    /// 动的是不是就是当前会话（原插件据此决定回"订阅 X 成功!"还是"为群 123 订阅 X 成功!"）
    pub(crate) fn is_self(&self) -> bool {
        self.cluster.is_none() && self.keys.len() == 1 && self.keys[0] == self.current
    }

    /// 替别人操作时的那半句文案
    pub(crate) fn grant(&self) -> String {
        if self.is_self() {
            return String::new();
        }
        if let Some(name) = &self.cluster {
            return format!("为分组 {name} ");
        }
        match self.keys.first().and_then(|key| store::parse_key(key)) {
            Some((true, id)) => format!("为群 {id} "),
            Some((false, id)) => format!("为好友 {id} "),
            None => String::new(),
        }
    }

    /// 列表标题用的目标名
    pub(crate) fn label(&self) -> String {
        if let Some(name) = &self.cluster {
            return name.clone();
        }
        if self.keys.is_empty() {
            return self.raw.clone();
        }
        if self.keys.len() == 1 {
            return self.keys[0].clone();
        }
        format!("{}（共 {} 个目标）", self.keys.join("、"), self.keys.len())
    }

    /// 只在单个目标上生效的命令用它取那一个键
    pub(crate) fn single(&self) -> Option<String> {
        match self.keys.as_slice() {
            [only] => Some(only.clone()),
            [] => Some(self.current.clone()),
            _ => None,
        }
    }

    /// 写操作落地的目标键。分组还没加成员时 `keys` 是空的，这里给一句人话，
    /// 别让命令表现为"回了一句成功但其实谁都没收到"。
    pub(crate) fn writable(&self) -> Result<&[String], String> {
        if self.keys.is_empty() {
            return Err(match &self.cluster {
                Some(name) => format!("分组「{name}」里还没有目标，先用 push 往分组里加群或好友"),
                None => format!("没有这个目标或分组 [{}]", self.raw),
            });
        }
        Ok(&self.keys)
    }
}

/// 原插件那套 `ss/ep/md + 4~10 位数字` 的番剧号写法
pub(crate) fn season_id(raw: &str) -> Option<(&'static str, i64)> {
    let raw = raw.trim().to_lowercase();
    for (prefix, kind) in [("ss", "ss"), ("md", "md"), ("ep", "ep")] {
        if let Some(digits) = raw.strip_prefix(prefix) {
            if digits.len() < 4
                || digits.len() > 10
                || !digits.chars().all(|ch| ch.is_ascii_digit())
            {
                return None;
            }
            return digits.parse::<i64>().ok().map(|id| (kind, id));
        }
    }
    None
}

/// 一个 uid，或一个能在本地名单里模糊匹配上的名字。
/// 原插件 `matchUser` 只查本地已订阅过的名字（不联网搜人），这里一致。
pub(crate) fn match_uid(raw: &str) -> Match {
    let raw = raw.trim();
    if raw.is_empty() {
        return Match::None;
    }
    if let Ok(uid) = raw.parse::<i64>() {
        return Match::One(uid);
    }
    let found = crate::store().read(|data| data.find_uid_by_name(raw));
    match found.len() {
        0 => Match::None,
        1 => Match::One(found[0]),
        _ => Match::Many(
            found
                .iter()
                .map(|uid| {
                    let name = crate::store().read(|data| {
                        data.names
                            .get(&uid.to_string())
                            .cloned()
                            .unwrap_or_else(|| uid.to_string())
                    });
                    format!("{name}（{uid}）")
                })
                .collect(),
        ),
    }
}

pub(crate) enum Match {
    One(i64),
    None,
    Many(Vec<String>),
}

impl Match {
    /// 命中一个就交给 `then`，否则给一句回话
    pub(crate) fn then_with(self, then: impl FnOnce(i64) -> Reply) -> Reply {
        match self {
            Match::One(uid) => then(uid),
            Match::None => reply("未匹配到用户哦"),
            Match::Many(names) => reply(format!("有多个匹配项：\n{}", names.join("\n"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_unique_across_the_whole_tree() {
        let mut seen: Vec<String> = Vec::new();
        for (aliases, _, _, _) in SUBS {
            for alias in *aliases {
                let key = alias.to_lowercase();
                assert!(!seen.contains(&key), "别名「{key}」被两个子命令抢了");
                seen.push(key);
            }
        }
    }

    #[test]
    fn every_command_is_reachable_by_its_first_alias() {
        for (aliases, cmd, _, _) in SUBS {
            let found = find(aliases[0]);
            assert!(found.is_some(), "别名「{}」查不到", aliases[0]);
            assert_eq!(found.unwrap().0, *cmd);
            // 大小写混写也要命中
            assert_eq!(
                find(&aliases[0].to_uppercase()).map(|found| found.0),
                Some(*cmd)
            );
        }
        assert!(find("不存在").is_none());
    }

    #[test]
    fn the_tree_covers_the_original_subcommand_list() {
        // 原插件 DynamicCommand.kt 里的 @SubCommand 一个都不能少
        for name in [
            "h",
            "help",
            "reload",
            "color",
            "add",
            "del",
            "delall",
            "list",
            "listall",
            "listuser",
            "filtermode",
            "filtertype",
            "filterreg",
            "filterlist",
            "filterdel",
            "templatelist",
            "template",
            "login",
            "atall",
            "delatall",
            "listatall",
            "config",
            "search",
            "live",
            "new",
            "video",
            "create",
            "listgroup",
            "delgroup",
            "addgroupadmin",
            "bangroupadmin",
            "push",
            "ban",
            "clear",
        ] {
            assert!(find(name).is_some(), "子命令「{name}」没登记");
        }
    }

    #[test]
    fn command_names_honor_the_prefix_free_switch() {
        let configured = CommandConfig {
            names: vec!["/bili".into(), "#b".into(), "无斜杠".into()],
            prefix_free: false,
        };
        assert_eq!(
            command_names(&configured),
            vec!["/bili".to_string(), "#b".to_string(), "无斜杠".to_string()]
        );
        let free = CommandConfig {
            prefix_free: true,
            ..configured
        };
        assert_eq!(
            command_names(&free),
            vec!["bili".to_string(), "b".to_string(), "无斜杠".to_string()]
        );
        // 空名与空白名不参与登记（框架会按第一个名字补前缀，留空只会添乱）
        let empty = CommandConfig {
            names: vec!["  ".into()],
            prefix_free: false,
        };
        assert!(command_names(&empty).is_empty());
    }

    #[test]
    fn season_ids_follow_the_original_regex() {
        assert_eq!(season_id("ss11111"), Some(("ss", 11111)));
        assert_eq!(season_id("MD22222"), Some(("md", 22222)));
        assert_eq!(season_id("ep33333"), Some(("ep", 33333)));
        assert_eq!(season_id("ss111"), None, "位数不够");
        assert_eq!(season_id("ss11111111111"), None, "位数太多");
        assert_eq!(season_id("487550002"), None, "uid 不是番剧号");
    }
}
