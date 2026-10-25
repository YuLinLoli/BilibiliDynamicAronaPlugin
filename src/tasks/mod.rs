//! 定时任务：动态检测、直播检测、番剧检测与低频时段重排。
//!
//! 排期口径照原插件 `tasker/`：`bili.check.interval` 是动态轮询秒数，`liveInterval` 是直播轮询，
//! `lowSpeed` 形如 `3-8x2` 的那几个小时里把间隔乘以 2（由 `BiliReschedule` 每 5 分钟复核一次进出窗口）。
//! 三个任务都不重入：上一轮没跑完就直接跳过这一轮，避免慢网络下任务越积越多。
//!
//! 与原插件的几处刻意为之的差别，都记在 README：
//! * 首次订阅某个 UP 主时只把水位推到最新一条、不推历史，否则一订阅就刷二十条。
//! * 直播第一次观测就在播也只记状态，不补一条"开播"。
//! * 番剧的 `ep`/`md` 在命令层就换成 `ss` 号了（`season_by_ep`/`season_by_md`），检测层只见 `ss`。
//! * 有登录态时检测主路径就是关注时间线（一轮一到几页，请求数与订阅人数无关）；没登录态换成逐个 UP 主
//!   的匿名 `feed/space`。订"全部动态"（`/bili add 0`，原插件的 listenAll）落在通配档 `0`，只有前者能喂它。
//! * `{draw}` 卡片按目标的配色缓存，同色只画一张。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use arona::plugin::PluginContext;
use arona::quartz::JobFn;

use crate::bili::api::{self, Bili};
use crate::bili::model::{Dynamic, LiveInfo, Node, format_time};
use crate::cache;
use crate::config::{
    BiliConfig, CheckConfig, IMAGE_SECTION, ImageConfig, SECTION, low_speed_window,
};
use crate::draw::canvas::Color;
use crate::draw::card::{Art, Style, dynamic_card};
use crate::draw::live::live_card;
use crate::draw::quality::Quality;
use crate::draw::text::{EmojiSet, Typeface};
use crate::draw::theme::{Theme, background};
use crate::push::template::{Supply, Vals};
use crate::push::{self, Kind, Post, Ready};
use crate::store::{LiveState, TargetState};
use crate::translate::{self, Rendered};
use crate::{CACHE_JOB, DYNAMIC_JOB, LIVE_JOB, RESCHEDULE_JOB};

/// 低频窗口的复核周期（秒）：进出窗口的最大延迟就是它
const RESCHEDULE_EVERY: u64 = 300;
/// 图片缓存清理的周期。原插件 `CacheClearTasker` 也是 24 小时一轮
const CACHE_EVERY: u64 = 60 * 60 * 24;
/// 关注流连着几轮读不出来就临时退回逐 UP（一份坏 Cookie 不该把整个检测弄停）
const FALLBACK_AFTER: u64 = 3;

static DYNAMIC_BUSY: AtomicBool = AtomicBool::new(false);
static LIVE_BUSY: AtomicBool = AtomicBool::new(false);
static SEASON_BUSY: AtomicBool = AtomicBool::new(false);
/// 上一轮排出去的间隔，低频重排靠它判断"该不该重排"
static LAST_DYNAMIC_INTERVAL: AtomicU64 = AtomicU64::new(0);
static LAST_LIVE_INTERVAL: AtomicU64 = AtomicU64::new(0);
/// 关注流连着几轮读不出来，计数到 [`FALLBACK_AFTER`] 就临时退回逐 UP
static FIREHOSE_FAILS: AtomicU64 = AtomicU64::new(0);
/// 已经问过关系接口的订阅者，见 [`probe_unfollowed`]
static UNFOLLOW_PROBED: OnceLock<Mutex<BTreeSet<i64>>> = OnceLock::new();
/// `--test-notify` 只测一次，配置热重载不能重复推
static SELFTEST_DONE: AtomicBool = AtomicBool::new(false);
/// 接口失败的汇报节流点（unix 秒）
static LAST_REPORT: AtomicU64 = AtomicU64::new(0);

/// 一轮里字面的持有者。
///
/// 第一次真要画卡时才去装载，之后整轮复用同一份；本轮跑出作用域就把这一份还掉——
/// 框架那层缓存只存 Weak（见 [`Typeface::discover`] 的说明），所以最后一个持有者离开后
/// 那二十来 MB 字体字节就整个还给分配器。两头的白付都不付：没有新动态的一轮不装载，
/// 一轮里多次出卡也只装载一次（惰性后端下这一档是 0.01 s，见 `draw/text.rs`）。
struct Faces {
    configured: String,
    held: Option<Arc<Typeface>>,
    /// 解析失败本轮只报一次：字体文件不在是配置问题，每张卡各报一遍只会刷日志
    complained: bool,
}

impl Faces {
    fn for_round(appearance: &ImageConfig) -> Faces {
        Faces {
            configured: appearance.font.clone(),
            held: None,
            complained: false,
        }
    }

    /// 画这张卡要用的字面；解析不出来（配置写错、系统里没有中文字体）回 None，按无卡推
    fn need(&mut self) -> Option<&Arc<Typeface>> {
        if self.held.is_none() && !self.complained {
            self.complained = true;
            match Typeface::discover(&self.configured) {
                Ok(face) => self.held = Some(face),
                Err(reason) => arona::runtime::log::warning(format!(
                    "bilibili：卡画不出来（{reason}），这一轮只发文本"
                )),
            }
        }
        self.held.as_ref()
    }
}

/// 任务不重入的守卫：拿到就占住，离开（含被取消）自动放行
struct Busy(&'static AtomicBool);

impl Busy {
    fn try_lock(flag: &'static AtomicBool) -> Option<Busy> {
        (!flag.swap(true, Ordering::SeqCst)).then_some(Busy(flag))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// 按当前配置排（或重排）三个任务。`start` 与 `on_config_reload` 都走这里，
/// 同名任务登记时会直接替换旧的那份，所以重复调用是安全的。
pub(crate) fn schedule(ctx: &PluginContext) {
    let conf = ctx.config::<BiliConfig>(SECTION).get();
    if !conf.enable {
        ctx.remove_job(DYNAMIC_JOB);
        ctx.remove_job(LIVE_JOB);
        ctx.remove_job(RESCHEDULE_JOB);
        ctx.remove_job(CACHE_JOB);
        LAST_DYNAMIC_INTERVAL.store(0, Ordering::SeqCst);
        LAST_LIVE_INTERVAL.store(0, Ordering::SeqCst);
        arona::runtime::log::info(
            "bilibili：配置里已关闭（bili.enable=false），检测与缓存清理都撤了",
        );
        return;
    }
    let (dynamic_secs, live_secs) = current_intervals(&conf.check);
    ctx.repeat_job(dynamic_secs, DYNAMIC_JOB, dynamic_job(ctx));
    ctx.repeat_job(live_secs, LIVE_JOB, live_job(ctx));
    ctx.repeat_job(RESCHEDULE_EVERY, RESCHEDULE_JOB, reschedule_job(ctx));
    ctx.repeat_job(CACHE_EVERY, CACHE_JOB, cache_job(ctx));
    LAST_DYNAMIC_INTERVAL.store(dynamic_secs, Ordering::SeqCst);
    LAST_LIVE_INTERVAL.store(live_secs, Ordering::SeqCst);
    arona::runtime::log::info(format!(
        "bilibili：动态每 {dynamic_secs} 秒、直播每 {live_secs} 秒检测一次（低频时段「{}」）",
        conf.check.low_speed
    ));

    if ctx.test_notify && !SELFTEST_DONE.swap(true, Ordering::SeqCst) {
        let context = ctx.clone();
        ctx.spawn_as("测试推送", async move {
            push::selftest(&context).await;
        });
    }
}

/// 当前该用的两个间隔（低频时段把秒数乘以倍数）
fn current_intervals(check: &CheckConfig) -> (u64, u64) {
    let factor = low_speed_factor(check);
    (
        check.interval.max(5) * factor,
        check.live_interval.max(5) * factor,
    )
}

/// 现在这个小时在不在低频窗口里
fn low_speed_factor(check: &CheckConfig) -> u64 {
    let Some((from, to, factor)) = low_speed_window(&check.low_speed) else {
        return 1;
    };
    let hour = chrono::Timelike::hour(&chrono::Local::now()) as u64;
    let inside = if from < to {
        (from..to).contains(&hour)
    } else {
        // 跨零点写的窗口（`22-3x2`）
        hour >= from || hour < to
    };
    if inside { factor } else { 1 }
}

fn dynamic_job(ctx: &PluginContext) -> JobFn {
    let ctx = ctx.clone();
    Arc::new(move || {
        // spawn_as 借 ctx 起任务，任务体自己要再克隆一份（否则 E0505）
        let spawned = ctx.clone();
        ctx.spawn_as("动态检测", async move {
            run_dynamic(spawned).await;
        });
        // 番剧跟着动态这一轮跑，不单独占一个任务名。它自带 SEASON_BUSY 互斥，
        // 上一轮没跑完就直接跳过，所以搭车排程不会叠出一堆并发请求。
        let seasons = ctx.clone();
        ctx.spawn_as("番剧检测", async move {
            run_seasons(seasons).await;
        });
    })
}

fn live_job(ctx: &PluginContext) -> JobFn {
    let ctx = ctx.clone();
    Arc::new(move || {
        let spawned = ctx.clone();
        ctx.spawn_as("直播检测", async move {
            run_live(spawned).await;
        });
    })
}

/// 图片缓存清理：按 `bili.cache.expires` 那五个类别各按自己的天数删过期文件
fn cache_job(ctx: &PluginContext) -> JobFn {
    let ctx = ctx.clone();
    Arc::new(move || {
        let spawned = ctx.clone();
        ctx.spawn_as("图片缓存清理", async move {
            let conf = spawned.config::<BiliConfig>(SECTION).get();
            let removed = cache::clear(&conf.cache.expires);
            if removed > 0 {
                arona::runtime::log::info(format!("bilibili：图片缓存清掉 {removed} 个过期文件"));
            }
        });
    })
}

/// 进出低频时段就按新间隔重排那两个任务
fn reschedule_job(ctx: &PluginContext) -> JobFn {
    let ctx = ctx.clone();
    Arc::new(move || {
        let spawned = ctx.clone();
        ctx.spawn_as("低频重排", async move {
            let conf = spawned.config::<BiliConfig>(SECTION).get();
            if !conf.enable {
                return;
            }
            let (dynamic_secs, live_secs) = current_intervals(&conf.check);
            if LAST_DYNAMIC_INTERVAL.swap(dynamic_secs, Ordering::SeqCst) != dynamic_secs {
                spawned.repeat_job(dynamic_secs, DYNAMIC_JOB, dynamic_job(&spawned));
                arona::runtime::log::info(format!("bilibili：动态检测间隔改成 {dynamic_secs} 秒"));
            }
            if LAST_LIVE_INTERVAL.swap(live_secs, Ordering::SeqCst) != live_secs {
                spawned.repeat_job(live_secs, LIVE_JOB, live_job(&spawned));
                arona::runtime::log::info(format!("bilibili：直播检测间隔改成 {live_secs} 秒"));
            }
        });
    })
}

/// 一轮动态检测。有登录态就走关注时间线：一轮只打一到几次 `feed/all`，请求数与订阅人数无关
/// （原插件 `DynamicCheckTasker.kt:38-39` 同一模型）。没登录态留在逐 UP——匿名 `feed/all` 恒 `-101`，
/// 而匿名 `feed/space` 还出内容，这条路删不得，否则没扫码的人整个检测功能就没了。
async fn run_dynamic(ctx: PluginContext) {
    let Some(_guard) = Busy::try_lock(&DYNAMIC_BUSY) else {
        arona::runtime::log::debug("bilibili：上一轮动态检测还没跑完，这一轮跳过");
        return;
    };
    let conf = ctx.config::<BiliConfig>(SECTION).get();
    if !conf.enable {
        return;
    }
    let appearance = image_of(&ctx);
    // 字面在这轮里第一次真要画卡时才解析，轮次结束就还掉（口径见 [`Faces`]）
    let mut faces = Faces::for_round(&appearance);
    let bili = api::client(conf.account.clone(), conf.check.timeout.max(5));
    if bili.signed() {
        // 关注流读不出来的两种退法见 [`Firehose`]：登录失效是定论，立刻退回逐 UP；
        // 单次故障先计数，连着三轮都读不出来才退——一份坏 Cookie 不该把整个检测弄停，
        // 可也正因如此，被风控那一轮不该再补 N 次签名接口把情况弄得更糟。
        let fails = match firehose_round(&ctx, &conf, &bili, &appearance, &mut faces).await {
            Firehose::Done => {
                FIREHOSE_FAILS.store(0, Ordering::SeqCst);
                0
            }
            Firehose::Dead => u64::MAX,
            Firehose::Failed => FIREHOSE_FAILS.fetch_add(1, Ordering::SeqCst) + 1,
        };
        if fails >= FALLBACK_AFTER {
            per_up_round(&ctx, &conf, &bili, &appearance, &mut faces).await;
        }
    } else {
        per_up_round(&ctx, &conf, &bili, &appearance, &mut faces).await;
    }
}

/// 一轮关注流的结局，决定要不要退回逐 UP 那一路
enum Firehose {
    /// 首页拿到了（页里有没有新动态另说），水位计数清零
    Done,
    /// B 站判这份 Cookie 已死（-101 / -30）：关注流往后每轮都会空，立刻退
    Dead,
    /// 这一轮没读出来（风控、网络、Cookie 写法不对）：连着三轮才退
    Failed,
}

/// 关注流一轮：拉一到几页（什么时候停翻见 `should_fetch_more`），按订阅表分桶，再逐档推。
/// 订「全部动态」（原插件的 listenAll）也走这一份数据，只是它落在通配档 `0` 上、收所有作者。
async fn firehose_round(
    ctx: &PluginContext,
    conf: &BiliConfig,
    bili: &Arc<Bili>,
    appearance: &ImageConfig,
    faces: &mut Faces,
) -> Firehose {
    let uids = crate::store().read(|data| data.subscribed_uids());
    let wants_all = crate::store().read(|data| data.wants_all());
    if uids.is_empty() && !wants_all {
        return Firehose::Done;
    }
    let marks = watermarks();
    let mut feed: Vec<Dynamic> = Vec::new();
    let cap = conf.check.timeline_pages.max(1);
    let mut used = 0u32;
    for page in 1..=cap {
        match bili.timeline(page, "all").await {
            Ok(items) if items.is_empty() => break,
            Ok(items) => {
                used += 1;
                feed.extend(items);
            }
            Err(reason) => {
                report(&conf.check, &format!("关注时间线读不出来：{reason}"));
                // 第一页就没有，这一轮什么都判不了；后面的页翻不动就用已拿到的那些，别把整轮作废
                if page == 1 {
                    return if reason.starts_with(api::AUTH_FAILURE) {
                        Firehose::Dead
                    } else {
                        Firehose::Failed
                    };
                }
                break;
            }
        }
        if !should_fetch_more(&feed, &uids, &marks, wants_all) {
            break;
        }
    }
    if feed.is_empty() {
        return Firehose::Done;
    }
    if past_the_page_cap(used, cap, &feed, &uids, &marks, wants_all) {
        // 这一档漏掉的不是"这一轮"，是永远：水位推到本轮见过的最大 id，中间没取到的那些下一轮也不算了。
        // 走 report 那条节流——它和接口失败汇报同一种性质，一轮一条会把宿主日志刷过去，reportInterval 能压住。
        report(
            &conf.check,
            &format!(
                "关注流翻满 {cap} 页还没到底，页里最旧那条仍比最低的那条水位新——\
                 中间没取到的动态这一轮就跳过去了，订阅多就抬 check.timelinePages"
            ),
        );
    }
    let plan = plan_round(&feed, &uids, &marks, wants_all);
    for (slot, newest) in &plan.align {
        crate::store().edit_only(|data| data.advance_watermark(*slot, *newest));
        let who = if *slot == 0 {
            "关注时间线第一次跑".to_string()
        } else {
            format!("{slot} 是第一次订阅")
        };
        arona::runtime::log::info(format!(
            "bilibili：{who}，历史动态不推，水位已对齐到 {newest}"
        ));
    }
    if !plan.align.is_empty() {
        save(&conf.check);
    }
    // 排在推送**之前**：这一句要说的正是「推送这一路永远轮不到他」，所以它自己不能跟着推送一起断——
    // 一条过大的附件能把对端连接打崩，那一轮的自我交代就不该被它一起带走。
    probe_unfollowed(bili, &uids, &feed).await;
    for (slot, dynamics) in &plan.fresh {
        let targets = states_of(*slot);
        if targets.is_empty() {
            continue;
        }
        for dynamic in dynamics {
            push_one(ctx, conf, bili, appearance, faces, *slot, dynamic, &targets).await;
        }
    }
    Firehose::Done
}

/// 订了、却不在关注流里的 UP 主：关注流这一路结构上就轮不到他，检测永远不会推他一条。
///
/// `/bili add` 那句提醒只在发命令那一刻说过一次，之后在 B 站把某人取关（或当初没勾自动关注）就再没人交代。
/// 「这一轮页里没有他」不能当证据——他只是这几天没发动态时同样不在。所以去问关系接口，
/// 而且每个 uid 一生只问一次（只有 `0` 与 `128` 才是结论，关注着的那就只是没发动态，不必反复问）；
/// 一旦他在关注流里露过面就把这条记录摘掉，将来真被取关了还能再查出来。
/// 一轮只问一个：订几十个人时，第一轮回不该甩出几十次签名接口之外的请求去。
async fn probe_unfollowed(bili: &Arc<Bili>, uids: &[i64], feed: &[Dynamic]) {
    let seen: BTreeSet<i64> = feed.iter().map(|item| item.uid).collect();
    let asked = UNFOLLOW_PROBED.get_or_init(|| Mutex::new(BTreeSet::new()));
    // 挑人这一段不许横跨 await：`std::sync::MutexGuard` 不是 Send，带着它进 `relation()`
    // 就把整个任务体的 future 弄成 !Send，而 spawn_as 的边界要 Send。
    let picked = {
        let mut probed = asked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pick_probe(uids, &seen, &mut probed)
    };
    let Some(uid) = picked else {
        return;
    };
    match bili.relation(uid).await {
        // 0 是没关注、128 是拉黑，两种都让关注流永远轮不到他，只是劝的话不一样；
        // 其余取值（已关注、互粉）说明他只是这几天没发动态，不该念这一句
        Ok(attribute @ (0 | 128)) => {
            let name = crate::store()
                .read(|data| data.names.get(&uid.to_string()).cloned())
                .unwrap_or_default();
            let who = if name.is_empty() {
                format!("{uid}")
            } else {
                format!("{uid}（{name}）")
            };
            let advice = if attribute == 128 {
                "在这一份登录态的黑名单里，关注流不会给他推任何东西——要么在 B 站解除拉黑"
            } else {
                "订了却不在这一份登录态的关注列表里，关注流不会给他推任何东西——要么在 B 站关注他"
            };
            // 不走 [`report`] 那条节流：这一句一个进程只说一次，撞上节流就永久丢了，
            // 自我交代不能靠运气。收敛靠的是「一轮最多问一个、问过就不再问」。
            arona::runtime::log::warning(format!(
                "bilibili：{who}{advice}，要么 /bili del 取消这一条订阅"
            ));
        }
        Ok(_) => {}
        Err(reason) => {
            // 问不成是把标记摘掉的下一轮回重试机会，代价只是一轮一次；
            // 反过来说，把失败当结论钉死会让这条自我交代永远漏掉。
            let mut probed = asked
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            probed.remove(&uid);
            arona::runtime::log::debug(format!("bilibili：{uid} 的关注关系没查出来（{reason}）"));
        }
    }
}

/// 本轮该问谁：订了、这一轮页里没露面、且没问过的第一个，选中就把标记落下。
///
/// 摘标（这一轮露过面）排在选人前面，所以「他又开始发动态、将来再被取关」这一趟还查得出来。
fn pick_probe(uids: &[i64], seen: &BTreeSet<i64>, asked: &mut BTreeSet<i64>) -> Option<i64> {
    for uid in seen {
        asked.remove(uid);
    }
    let uid = uids
        .iter()
        .copied()
        .find(|uid| !seen.contains(uid) && !asked.contains(uid))?;
    asked.insert(uid);
    Some(uid)
}

/// 逐 UP 那一轮（只在没有登录态时用）：每个订阅者各拉一次匿名 `feed/space`
async fn per_up_round(
    ctx: &PluginContext,
    conf: &BiliConfig,
    bili: &Arc<Bili>,
    appearance: &ImageConfig,
    faces: &mut Faces,
) {
    for uid in crate::store().read(|data| data.subscribed_uids()) {
        let targets = states_of(uid);
        if targets.is_empty() {
            continue;
        }
        let items = match bili.space(uid, "").await {
            Ok(items) => items,
            Err(reason) => {
                report(
                    &conf.check,
                    &format!("订阅者 {uid} 的动态读不出来：{reason}"),
                );
                continue;
            }
        };
        let watermark = crate::store().read(|data| {
            data.watermark
                .get(&uid.to_string())
                .copied()
                .unwrap_or_default()
        });
        // `pick_fresh` 会把整页搬走，所以「这一页有没有可能没到底」得在它之前问
        let page_may_run_over = page_might_hold_more(&items, watermark);
        let Some((fresh, newest)) = pick_fresh(items, watermark) else {
            continue;
        };
        if watermark == 0 {
            crate::store().write(|data| data.advance_watermark(uid, newest));
            // 把写进去的那条 id 念出来：这一档之后几轮会不会重推历史，全看这一个值，
            // 不念出来就只能靠 `store.yml` 的终态倒推，而终态分不清「对齐写的」还是「推送顶的」
            arona::runtime::log::info(format!(
                "bilibili：{uid} 是第一次订阅，历史动态不推，水位已对齐到 {newest}"
            ));
            continue;
        }
        if page_may_run_over {
            // 水位推到本轮见过的最大 id，所以这一页底下那些没取到的更旧动态下一轮也不算新了：这一档漏是永久的
            report(
                &conf.check,
                &format!(
                    "{uid} 这一页动态整页都比水位新：匿名逐 UP 那一路每轮只打第一页，所以这一页可能没到底，\
                     没到底的那些这一轮就跳过去了、下一轮也不算新了（会漏）；\
                     隔很久才检一轮、或这个 UP 一轮里发得特别密时会这样，扫码登录换成关注流就能翻页"
                ),
            );
        }
        for dynamic in &fresh {
            push_one(ctx, conf, bili, appearance, faces, uid, dynamic, &targets).await;
        }
    }
}

/// 从一页动态里挑出比水位新的那些，并给出这一页里最新一条的 id（对齐就用它）。逐 UP 与关注流两条路共用
///
/// 手里的顺序一律不信：`per_up_round` 那份是 `parse_feed`（`bili/model.rs`）按 `time` **升序**排过的，
/// 关注流那份是多页拼接加去重，谁都不是「接口给的页序」。旧代码以为前者还是新→旧，`reverse()` 之后取 `last()`
/// 当最新，实际拿到的是**最旧**那条——水位钉在最旧动态上，下一轮整页历史就全被当成新的推出去（2026-10-03，
/// 实测 12 条的页重推出 11 条）。所以这里显式按 id 排序取 `last()`，返回的 `fresh` 也是旧→新，
/// 推出来才和读者看到的时间线一致。
fn pick_fresh(items: Vec<Dynamic>, watermark: i64) -> Option<(Vec<Dynamic>, i64)> {
    let mut fresh: Vec<Dynamic> = items
        .into_iter()
        .filter(|item| item.id > watermark)
        .collect();
    if fresh.is_empty() {
        return None;
    }
    fresh.sort_by_key(|item| item.id);
    let newest = fresh.last().map(|item| item.id).unwrap_or(watermark);
    Some((fresh, newest))
}

/// 各档位当前水位。通配档 `0` 是「全部动态」那一条，键是 uid 的十进制串，解不开的就当没有
fn watermarks() -> BTreeMap<i64, i64> {
    crate::store().read(|data| {
        data.watermark
            .iter()
            .filter_map(|(key, id)| key.parse::<i64>().ok().map(|slot| (slot, *id)))
            .collect::<BTreeMap<i64, i64>>()
    })
}

/// 一轮关注流的分桶结果
#[derive(Clone, Debug, Default)]
struct Plan {
    /// 档位 -> 这一轮要推的新动态（旧→新，跟读者看到的时间线一致）
    fresh: BTreeMap<i64, Vec<Dynamic>>,
    /// 水位还是 0 的档位 -> 页里见到的最新一条：只把水位推上去，不推历史
    align: BTreeMap<i64, i64>,
}

/// 把关注流按订阅表分成「每个档位这一轮的新动态」。纯函数，不碰 ctx，也不碰 store。
///
/// `items` 是各页拼接后的原样顺序（页内新→旧，页间可能重叠），先按 id 去重再分桶，
/// 于是这里不依赖接口给的顺序：新鲜与否只看 `id > 水位`。
fn plan_round(
    items: &[Dynamic],
    uids: &[i64],
    marks: &BTreeMap<i64, i64>,
    wants_all: bool,
) -> Plan {
    let mut seen = BTreeSet::new();
    let feed: Vec<&Dynamic> = items.iter().filter(|item| seen.insert(item.id)).collect();
    let mut slots: Vec<i64> = uids.to_vec();
    if wants_all {
        slots.push(0);
    }
    let mut plan = Plan::default();
    for slot in slots {
        // 通配档 `0` 收所有作者，按 UP 主订阅的那一档只看这个 uid
        let own: Vec<Dynamic> = feed
            .iter()
            .filter(|item| slot == 0 || item.uid == slot)
            .map(|item| (*item).clone())
            .collect();
        let watermark = marks.get(&slot).copied().unwrap_or_default();
        let Some((fresh, newest)) = pick_fresh(own, watermark) else {
            continue;
        };
        if watermark == 0 {
            // 第一次见到这一档：只对齐水位，不然一订阅就把页里那几条历史全推一遍
            plan.align.insert(slot, newest);
            continue;
        }
        plan.fresh.insert(slot, fresh);
    }
    plan
}

/// 这一页之后还要不要往下翻：页里最旧一条已经压到所有已知水位之下，再老的动态对谁都不算新了。
/// 全是第一次订阅（一个已知水位都没有）时也不翻——那一轮只对齐水位，多翻只是多打接口。
fn should_fetch_more(
    items: &[Dynamic],
    uids: &[i64],
    marks: &BTreeMap<i64, i64>,
    wants_all: bool,
) -> bool {
    let Some(oldest) = items.iter().map(|item| item.id).min() else {
        return false;
    };
    let mut floors: Vec<i64> = uids
        .iter()
        .filter_map(|uid| marks.get(uid))
        .filter(|&&mark| mark > 0)
        .copied()
        .collect();
    if wants_all {
        floors.extend(marks.get(&0).filter(|&&mark| mark > 0).copied());
    }
    match floors.into_iter().min() {
        Some(floor) => oldest > floor,
        None => false,
    }
}

/// 这一轮是「翻到底了」还是「翻页预算用完了」——只有后者才漏。
/// 水位推到本轮见过的最大 id，所以预算用完时那些没翻到的更旧动态下一轮也不算新了：这一档漏是永久的。
/// `used` 是本轮真取回的页数（空页与读不出来都提前收尾，所以它可能小于 `cap`）。
fn past_the_page_cap(
    used: u32,
    cap: u32,
    items: &[Dynamic],
    uids: &[i64],
    marks: &BTreeMap<i64, i64>,
    wants_all: bool,
) -> bool {
    used >= cap && should_fetch_more(items, uids, marks, wants_all)
}

/// 逐 UP 那一页是不是**整页**都比水位新——是就可能页底下还有更旧的动态没被取到。
/// 这一路每轮只打第一页、根本不翻页，所以「整页全新」就是「这一页没到底」的唯一可见迹象。
/// 水位为 0（第一次订阅）时整页当然全是新的，那一轮只对齐、不告警。
/// 判据用「页里最旧那条都比水位新」而不是「新条数等于页大小」：匿名页一页实测 10～12 条，页数不是常数。
fn page_might_hold_more(items: &[Dynamic], watermark: i64) -> bool {
    watermark > 0
        && items
            .iter()
            .map(|item| item.id)
            .min()
            .is_some_and(|oldest| oldest > watermark)
}

/// 直播版式要的那份房间信息。`DYNAMIC_TYPE_LIVE_RCMD` 的 content 里只有房间号、标题和封面，
/// 主播名与头像都不带，缺的那两格用动态作者那两格顶上——这一条动态本来就是主播自己发的。
fn live_of(dynamic: &Dynamic) -> Option<LiveInfo> {
    let mut live = dynamic.live.clone()?;
    if live.anchor.is_empty() {
        live.anchor = dynamic.name.clone();
    }
    if live.face.is_empty() {
        live.face = dynamic.face.clone();
    }
    if live.uid == 0 {
        live.uid = dynamic.uid;
    }
    Some(live)
}

/// 一条新动态推给一批目标：先按目标筛，再出卡（全部命中盘上那张 DRAW 就不解码素材），
/// 画完立刻把位图还掉，最后逐个发——发送那一整段只带着出门的字节走。
/// `slot` 是水位所在的那一档——按 UP 主订阅时就是那个 uid，走关注时间线时传 `0`。
// 本轮的四样上下文（配置、客户端、外观、字面）与这一条的三样坐标（档位、动态、目标）本来就该一次说全，
// 包成结构体只是把拆包搬到函数体里
#[allow(clippy::too_many_arguments)]
async fn push_one(
    ctx: &PluginContext,
    conf: &BiliConfig,
    bili: &Arc<Bili>,
    appearance: &ImageConfig,
    faces: &mut Faces,
    slot: i64,
    dynamic: &Dynamic,
    targets: &[(String, TargetState)],
) {
    let kind = kind_of(dynamic);
    let chosen: Vec<&(String, TargetState)> = targets
        .iter()
        .filter(|(_, state)| wants(state, dynamic.uid, &kind, &dynamic.text))
        .collect();
    if chosen.is_empty() {
        crate::store().edit_only(|data| data.advance_watermark(slot, dynamic.id));
        save(&conf.check);
        return;
    }
    // 归属要留在日志里：用户问「我在这个群触发，怎么其他群也收到了」时，
    // 只看素材下载数不出答案。`targets` 是 `states_of(订阅档)` 给的——订了这个 uid
    // 或订了通配档 `0`（`/bili add 0`＝我关注的所有人都推）的会话才会进来
    arona::runtime::log::info(format!(
        "bilibili：动态 {}（作者 {}，订阅档 {}）推给 {} 个会话：{}",
        dynamic.id,
        dynamic.uid,
        slot,
        chosen.len(),
        chosen
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<&str>>()
            .join(" ")
    ));
    let live = live_of(dynamic).filter(|_| kind == "直播");
    let living = live.is_some();
    // 机翻只贴动态卡：原插件那一路也只在画动态卡时调 trans，直播卡与文本模板都不带译文。
    // 绘图总开关关掉时连翻译都不必出门——那张卡本来就不会画。
    let translation = if living || !appearance.draw_enable {
        None
    } else {
        translate::translate(&conf.translate, &dynamic.text).await
    };
    let link = link_of(bili, conf.push.to_short_link, dynamic).await;
    let vals = match &live {
        Some(info) => Vals::live(info, link.clone(), &format_time(now())),
        None => Vals::dynamic(dynamic, link.clone()),
    };
    // 一、出卡。`drawEnable=false` 时这两条推送路径都不画卡（原插件 gate 的也正是这两处：
    // DynamicMessageTasker.kt:217 与 LiveMessageTasker.kt:48），命令的「先看一眼」照旧出图。
    // 同一档配色只画一张：一个群一个色，二十个群别画二十遍。
    // 模板里根本没有 `{draw}` 的目标（以及静音的）不进这张表——它们收不到卡，
    // 替它们画一张是纯浪费：一轮二十个群都只要文本时，连素材位图都不解码。
    let bucket = if living { "live" } else { "dynamic" };
    let card_kind = if living {
        push::Kind::Live
    } else {
        push::Kind::Dynamic
    };
    // 先把这一批目标里真要用卡的那些、用到的配色去重，再按配色各算一个卡片键
    let mut colors: Vec<String> = Vec::new();
    for (key, state) in &chosen {
        if !push::wants_card(ctx, state, card_kind) {
            arona::runtime::log::debug(format!(
                "bilibili：{key} 那一档模板里没有 {{draw}}，这一条不替它画卡"
            ));
            continue;
        }
        let color = color_of(appearance, state);
        if !colors.contains(&color) {
            colors.push(color);
        }
    }
    let keys: HashMap<String, String> = colors
        .into_iter()
        .map(|color| {
            let key = card_key(
                bucket,
                &dynamic.id.to_string(),
                &color,
                &link,
                translation.as_ref(),
                appearance,
            );
            (color, key)
        })
        .collect();
    // 盘上那张 DRAW 先读一遍：全部命中的这一条，素材一张也不用解码（`{images}` 的原件字节仍要下）。
    // 直播卡带着在线人数与标题，不落盘、也不在这一步复用。
    let mut drawn: HashMap<String, Option<Ready>> = HashMap::new();
    if appearance.draw_enable && !living {
        for key in keys.values() {
            if let Some(raw) = cache::read(cache::DRAW, key) {
                drawn.insert(key.clone(), Some(Ready::of(raw, None)));
            }
        }
    }
    let paints =
        appearance.draw_enable && keys.values().any(|key| !drawn.contains_key(key.as_str()));
    let material = gather(
        bili,
        &face_urls(dynamic),
        &picture_urls(dynamic),
        &emoji_pairs(dynamic),
        conf.cache.download_original,
        if paints { Want::Both } else { Want::Bytes },
    )
    .await;
    let Material { art, bytes } = material;
    if paints {
        for (color, key) in &keys {
            if drawn.contains_key(key) {
                continue;
            }
            painted(&mut drawn, key.clone(), !living, || match &live {
                Some(info) => paint_live(appearance, color, info, &art, &link, faces),
                None => paint_dynamic(
                    appearance,
                    color,
                    dynamic,
                    &art,
                    &link,
                    translation.as_ref(),
                    faces,
                ),
            });
        }
    }
    // 解码位图到此为止。发送那一段每条消息之间还要歇 `messageInterval`，
    // 不该由它替进程按着这几十 MB 的位图不放。
    drop(art);
    let images: Vec<Ready> = dynamic
        .images
        .iter()
        .filter_map(|url| bytes.get(url).cloned())
        .collect();
    let cover = live
        .as_ref()
        .and_then(|info| bytes.get(&info.cover).cloned());
    drop(bytes);
    // 二、发送。卡片与附件都是引用计数加一，不再按目标各拷一份、各编一遍
    for (key, state) in chosen {
        let card = keys
            .get(&color_of(appearance, state))
            .and_then(|key| drawn.get(key.as_str()))
            .cloned()
            .flatten();
        let post = if living {
            Post::live(vals.clone(), card, cover.clone())
        } else {
            Post::dynamic(vals.clone(), card, images.clone())
        };
        push::push_state(ctx, key, state, &post).await;
        crate::store().edit_only(|data| data.advance_watermark(slot, dynamic.id));
        save(&conf.check);
    }
}

/// 批量改水位时只在末尾落一次盘：逐条 save 会在慢盘上把一轮拖长好几秒
fn save(check: &CheckConfig) {
    if let Err(reason) = crate::store().flush() {
        report(check, &reason);
    }
}

/// 一轮直播检测：靠状态边沿判开播与结束，标题变了按一次"直播信息更新"重推
async fn run_live(ctx: PluginContext) {
    let Some(_guard) = Busy::try_lock(&LIVE_BUSY) else {
        arona::runtime::log::debug("bilibili：上一轮直播检测还没跑完，这一轮跳过");
        return;
    };
    let conf = ctx.config::<BiliConfig>(SECTION).get();
    if !conf.enable {
        return;
    }
    let uids = crate::store().read(|data| data.subscribed_uids());
    if uids.is_empty() {
        return;
    }
    let bili = api::client(conf.account.clone(), conf.check.timeout.max(5));
    let infos = match bili.live_status(&uids).await {
        Ok(infos) => infos,
        Err(reason) => {
            report(&conf.check, &format!("直播状态读不出来：{reason}"));
            return;
        }
    };
    let found: HashMap<i64, LiveInfo> = infos.into_iter().map(|live| (live.uid, live)).collect();
    let appearance = image_of(&ctx);
    let mut faces = Faces::for_round(&appearance);
    for uid in uids {
        // 这一轮接口没给这个 uid 就不判定。拿默认值顶上去会把「读不到」当成数据用：
        // 于是「在播」被判成「下播」（乱发一条结束通知），「换了标题」也被判成立（空标题不等于换标题）。
        // 批量接口对开通过直播的 uid 恒给条目、未开播也给 live_status 0，所以查不到就是这轮真的没数据。
        let Some(info) = found.get(&uid).cloned() else {
            continue;
        };
        let living = info.living();
        let prev = crate::store().read(|data| data.live.get(&uid.to_string()).cloned());
        let was_living = prev.as_ref().is_some_and(|state| state.status == 1);
        // 基线里上一次记着的标题：接口这轮给的是空壳时就靠它兜住，别让空值把基线清空
        let last_title = prev
            .as_ref()
            .map(|state| state.title.clone())
            .unwrap_or_default();
        let targets = states_of(uid);
        match (was_living, living) {
            (false, true) => {
                let state = LiveState {
                    status: 1,
                    room_id: info.room_id,
                    title: keep(info.title.clone(), &last_title),
                    started_at: if info.live_start > 0 {
                        info.live_start
                    } else {
                        now()
                    },
                    closed: false,
                };
                if prev.is_none() {
                    // 第一次观测就在播：只记状态，不然一订阅就收到一条"开播"
                    crate::store().write(|data| {
                        data.live.insert(uid.to_string(), state);
                    });
                    continue;
                }
                crate::store().write(|data| {
                    data.live.insert(uid.to_string(), state);
                });
                broadcast_live(&ctx, &conf, &bili, &appearance, &mut faces, &info, &targets).await;
            }
            (true, false) => {
                let Some(previous) = prev else { continue };
                if previous.closed {
                    continue;
                }
                crate::store().write(|data| {
                    data.live.insert(
                        uid.to_string(),
                        LiveState {
                            status: 0,
                            room_id: previous.room_id,
                            title: keep(info.title.clone(), &last_title),
                            started_at: previous.started_at,
                            closed: true,
                        },
                    );
                });
                // 原插件那档 `liveCloseNotifyEnable` 关的就是这条播报（`BiliBiliDynamic.kt:65`、
                // `LiveCheckTasker.kt:15`）：状态照记，只是不再向目标发消息
                if conf.push.live_close_notify {
                    announce_close(&ctx, &bili, uid, &previous, &info, &targets).await;
                }
            }
            (true, true) => {
                let Some(previous) = prev else { continue };
                crate::store().write(|data| {
                    data.live.insert(
                        uid.to_string(),
                        LiveState {
                            status: 1,
                            room_id: info.room_id,
                            title: keep(info.title.clone(), &last_title),
                            started_at: previous.started_at,
                            closed: false,
                        },
                    );
                });
                // 换标题就当一次开播重推（原插件那套 updateLive 的口径）。
                // 这一轮标题空着是接口没给，不是换了标题——那种不重推，不然推出去一张空卡。
                if !previous.title.is_empty()
                    && !info.title.is_empty()
                    && previous.title != info.title
                    && !targets.is_empty()
                {
                    broadcast_live(&ctx, &conf, &bili, &appearance, &mut faces, &info, &targets)
                        .await;
                }
            }
            (false, false) => {}
        }
    }
}

/// 接口这轮给的是空值就沿用基线里那个，不拿空串把已知信息抹掉。
///
/// 基线里的标题一旦被空串覆盖，下一轮接口给了真标题就又会判成「换了标题」而重推，
/// 而那张重推的卡用的还是这一轮的空数据——所以空值既不该写进基线，也不该触发推送。
fn keep(value: String, previous: &str) -> String {
    if value.is_empty() {
        previous.to_string()
    } else {
        value
    }
}

/// 开播推送：素材只有头像与封面，画一张直播卡
async fn broadcast_live(
    ctx: &PluginContext,
    conf: &BiliConfig,
    bili: &Arc<Bili>,
    appearance: &ImageConfig,
    faces: &mut Faces,
    info: &LiveInfo,
    targets: &[(String, TargetState)],
) {
    let mut avatars: Vec<String> = Vec::new();
    if !info.face.is_empty() {
        avatars.push(info.face.clone());
    } else {
        // batch 接口有时不给头像，补一次直播用户资料——名片那条要 WBI 签名，连打会被判风控
        if let Ok(anchor) = bili.master(info.uid).await {
            avatars.push(anchor.face);
        }
    }
    let link = info.link();
    let vals = Vals::live(info, link.clone(), &format_time(now()));
    // 直播卡带着实时在线人数与标题，不落到盘上，只在这一轮里按配色复用一张。
    // 与动态那条同一口径：模板里没有 `{draw}` 的目标不进这张表，一张都不该画。
    let mut colors: Vec<String> = Vec::new();
    for (_, state) in targets
        .iter()
        .filter(|(_, state)| wants(state, info.uid, "直播", &info.title))
    {
        if !push::wants_card(ctx, state, push::Kind::Live) {
            continue;
        }
        let color = color_of(appearance, state);
        if !colors.contains(&color) {
            colors.push(color);
        }
    }
    let keys: HashMap<String, String> = colors
        .into_iter()
        .map(|color| {
            let key = card_key(
                "live",
                &info.room_id.to_string(),
                &color,
                &link,
                None,
                appearance,
            );
            (color, key)
        })
        .collect();
    // 封面字节无论如何都要下（`{cover}` 是原件那条），只有"要不要解码成位图"跟着这一步走
    let drawing = appearance.draw_enable && !keys.is_empty();
    let Material { art, bytes } = gather(
        bili,
        &avatars,
        std::slice::from_ref(&info.cover),
        &[],
        conf.cache.download_original,
        if drawing { Want::Both } else { Want::Bytes },
    )
    .await;
    let mut drawn: HashMap<String, Option<Ready>> = HashMap::new();
    if drawing {
        for (color, key) in &keys {
            painted(&mut drawn, key.clone(), false, || {
                paint_live(appearance, color, info, &art, &link, faces)
            });
        }
    }
    drop(art);
    let cover = bytes.get(&info.cover).cloned();
    drop(bytes);
    for (key, state) in targets {
        if !wants(state, info.uid, "直播", &info.title) {
            continue;
        }
        let card = keys
            .get(&color_of(appearance, state))
            .and_then(|item| drawn.get(item.as_str()))
            .cloned()
            .flatten();
        let post = Post::live(vals.clone(), card, cover.clone());
        push::push_state(ctx, key, state, &post).await;
    }
}

/// 直播结束通知：模板只有纯文本那一路，字段来自上一次记着的状态
async fn announce_close(
    ctx: &PluginContext,
    bili: &Arc<Bili>,
    uid: i64,
    previous: &LiveState,
    info: &LiveInfo,
    targets: &[(String, TargetState)],
) {
    // 结束态的批量接口常不给标题分区，补一次房间详情
    let mut live = bili
        .room(previous.room_id)
        .await
        .unwrap_or_else(|_| info.clone());
    if live.uid == 0 {
        live.uid = uid;
    }
    if live.title.is_empty() {
        live.title = previous.title.clone();
    }
    // 房间详情只给 uid，主播名靠名片补；名片被风控时退回批量接口那份 uname
    if live.anchor.is_empty() {
        live.anchor = info.anchor.clone();
    }
    let vals = Vals::live_close(&live, live.link(), previous.started_at, now());
    let post = Post::live_close(vals);
    for (key, state) in targets {
        if state.muted {
            continue;
        }
        push::push_state(ctx, key, state, &post).await;
    }
}

/// 一轮番剧比对的结果
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeasonMove {
    /// 这一条本轮不作数：读不到最新集，或者跟基线一模一样
    Skip,
    /// 只记基线，不推
    Record,
    /// 换集了：记基线并推一条
    Push,
}

/// 基线认的是剧集号（`new_ep.id`）而不是集名文本：集号单调唯一，集名会撞车
/// （同一话的中文配音版号不同、名子一样）。更要紧的是别拿空串去比——2026-09-30 实测里
/// 那条尾巴空着的「更新了：」就是「接口没给最新集」被当成「换了集」推出去的。
/// 认不出是数字的基线一律只重记不推（那是老格式按集名存下来的）。
fn season_move(seen: Option<&str>, newest_ep: i64) -> SeasonMove {
    if newest_ep == 0 {
        return SeasonMove::Skip;
    }
    match seen.and_then(|seen| seen.parse::<i64>().ok()) {
        Some(seen) if seen == newest_ep => SeasonMove::Skip,
        Some(_) => SeasonMove::Push,
        None => SeasonMove::Record,
    }
}

/// 一轮番剧检测：新集标题变了才推
async fn run_seasons(ctx: PluginContext) {
    let Some(_guard) = Busy::try_lock(&SEASON_BUSY) else {
        return;
    };
    let conf = ctx.config::<BiliConfig>(SECTION).get();
    if !conf.enable {
        return;
    }
    let bili = api::client(conf.account.clone(), conf.check.timeout.max(5));
    // 按番剧号把订阅者归并：同一部剧一轮只查一次接口，持锁期间也不发消息
    let grouped: Vec<(String, Vec<(String, TargetState)>)> = crate::store().read(|data| {
        let mut by_season: BTreeMap<String, Vec<(String, TargetState)>> = BTreeMap::new();
        for (key, seasons) in data.season_targets() {
            let state = data.targets.get(&key).cloned().unwrap_or_default();
            if state.muted {
                continue;
            }
            for raw in seasons {
                by_season
                    .entry(raw)
                    .or_default()
                    .push((key.clone(), state.clone()));
            }
        }
        by_season.into_iter().collect()
    });
    for (raw, audience) in grouped {
        let Some(id) = season_id(&raw) else {
            report(
                &conf.check,
                &format!("番剧号「{raw}」认不出来，这一条不检测"),
            );
            continue;
        };
        let season = match bili.season(id).await {
            Ok(season) => season,
            Err(reason) => {
                report(&conf.check, &format!("番剧 {raw} 读不出来：{reason}"));
                continue;
            }
        };
        let seen = crate::store().read(|data| data.seasons.get(&raw).cloned());
        let verdict = season_move(seen.as_deref(), season.newest_ep);
        if verdict == SeasonMove::Skip {
            continue;
        }
        let baseline = season.newest_ep.to_string();
        crate::store().write(|data| data.seasons.insert(raw.clone(), baseline.clone()));
        if verdict == SeasonMove::Record {
            // 刚订阅：只记当前集，不把"已更新到第 N 集"当新集推一条
            continue;
        }
        let material = gather(
            &bili,
            &[],
            std::slice::from_ref(&season.cover),
            &[],
            conf.cache.download_original,
            // 番剧卡本来就不画，封面只作为 `{images}` 出门：一张都不解
            Want::Bytes,
        )
        .await;
        let link = format!("https://www.bilibili.com/bangumi/play/ss{id}");
        let vals = Vals::fill([
            ("name", season.title.clone()),
            ("uid", "0".to_string()),
            ("did", season.season_id.to_string()),
            ("time", format_time(now())),
            ("type", "番剧".to_string()),
            ("content", format!("更新了：{}", season.newest_title)),
            ("link", link.clone()),
            ("links", link),
        ]);
        // 番剧没有对应的卡片，`{draw}` 换成的文案留空——模板首条会因此被丢掉
        let supply = Supply {
            draw_missing: String::new(),
            images: material
                .bytes
                .get(&season.cover)
                .cloned()
                .map(|one| vec![one])
                .unwrap_or_default(),
            ..Default::default()
        };
        let post = Post {
            kind: Kind::Dynamic,
            vals,
            supply,
        };
        for (key, state) in &audience {
            push::push_state(&ctx, key, state, &post).await;
        }
    }
}

/// 一个订阅者的推送目标（含分组展开），命令面板与检测任务共用。
/// 分组里的新成员可能压根还没有状态记录，那就按默认值推——
/// 否则"往分组里加个群"会变成那个群永远收不到。
fn states_of(uid: i64) -> Vec<(String, TargetState)> {
    crate::store().read(|data| {
        data.targets_of(uid)
            .into_iter()
            .map(|key| {
                let state = data.targets.get(&key).cloned().unwrap_or_default();
                (key, state)
            })
            .collect::<Vec<(String, TargetState)>>()
    })
}

/// 这个目标要不要这条：没静音、且过滤器放行
fn wants(state: &TargetState, uid: i64, kind: &str, content: &str) -> bool {
    if state.muted {
        return false;
    }
    state
        .filter_for(uid)
        .is_none_or(|filter| filter.passes(kind, content))
}

/// 动态流里那条直播按哪一档处理
fn kind_of(dynamic: &Dynamic) -> String {
    if dynamic.live.is_some() || dynamic.kind == "直播" {
        "直播".to_string()
    } else {
        dynamic.kind.clone()
    }
}

/// 绘图用的素材包：解码后的图给画卡，归一过的字节给 `{images}` 直接发
struct Material {
    art: Art,
    bytes: HashMap<String, Ready>,
}

/// 画进卡片的位图最多留这么多像素。卡片里图片的落点最宽也不过整幅卡宽（1500w 那一档 1400），
/// 400 万像素足够盖住任何一格；但解码是按源图大小付钱的：一张 1440x9000 的长图原样留在素材包里
/// 就是 52 MB，一条动态九张就是半 GB。超预算的等比缩到预算内——贴图路径本来就要按框再裁一次。
const ART_PIXELS: u64 = 4_000_000;

/// 解码闸：源图超过这么多像素就根本不解。一张 8192x32768 解开是 1 GB 当量，站方给不到，
/// 但图床返回奇怪东西、或盘上那份缓存被人手放过这种图时，不能让它把进程顶爆。
/// 不解不等于不发：附件仍按原件交，卡片按缺图的样子画。
const MAX_DECODE_PIXELS: u64 = 32_000_000;

/// 一张图这一轮要取到哪一步
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Want {
    /// 只要出门的字节（全部命中盘上那张 DRAW 的这一条动态，素材不必解码）
    Bytes,
    /// 字节与位图都要：画卡用位图，`{images}` 用字节，一次解码两边复用
    Both,
    /// 只要位图：表情不单独发出去，归一附件是白烧
    Art,
}

/// 一条动态涉及哪些头像
fn face_urls(dynamic: &Dynamic) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let candidates = [
        Some(dynamic.face.as_str()),
        dynamic.origin.as_ref().map(|old| old.face.as_str()),
        dynamic.live.as_ref().map(|live| live.face.as_str()),
    ];
    for url in candidates.into_iter().flatten() {
        if !url.is_empty() && !urls.iter().any(|have| have == url) {
            urls.push(url.to_string());
        }
    }
    urls
}

/// 一条动态涉及哪些图（正文图、附属卡封面、附加卡封面、转发原文的图与封面、直播封面）
fn picture_urls(dynamic: &Dynamic) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let candidates: Vec<String> = dynamic
        .images
        .clone()
        .into_iter()
        .chain(dynamic.attached.iter().map(|item| item.cover.clone()))
        .chain(dynamic.extra.iter().map(|item| item.cover.clone()))
        .chain(dynamic.origin.iter().flat_map(|old| old.images.clone()))
        .chain(dynamic.origin.iter().filter_map(|old| old.cover()))
        .chain(dynamic.live.iter().map(|live| live.cover.clone()))
        .collect();
    for url in candidates {
        if !url.is_empty() && !urls.contains(&url) {
            urls.push(url);
        }
    }
    urls
}

/// 一条动态涉及哪些表情：正文里 `[笑]` 这种原文 → B 站给的图地址。
/// 转发动态被转的那条正文里也带表情，所以连 origin 一起收。
/// 末尾那段是 unicode 表情（❤️ 那种）：主字体没有彩色字形，按 twemoji 的 png 补。
fn emoji_pairs(dynamic: &Dynamic) -> Vec<(String, String)> {
    let mut lists: Vec<&Vec<Node>> = vec![&dynamic.nodes];
    let mut texts: Vec<&String> = vec![&dynamic.text];
    if let Some(old) = &dynamic.origin {
        lists.push(&old.nodes);
        texts.push(&old.text);
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for nodes in lists {
        for node in nodes {
            if let Node::Emoji { text, url } = node
                && !url.is_empty()
                && !out.iter().any(|(have, _)| have == text)
            {
                out.push((text.clone(), url.clone()));
            }
        }
    }
    for text in texts {
        for (code, url) in crate::draw::text::twemoji_needs(text) {
            if !out.iter().any(|(have, _)| *have == code) {
                out.push((code, url));
            }
        }
    }
    out
}

/// 逐个下素材：单张失败只跳过那张，不拖垮整条推送（缺图的样子画卡里自会体现）。
/// `want` 决定这一轮取到哪一步——全部命中盘上那张 DRAW 时按 [`Want::Bytes`]，一张都不解。
async fn gather(
    bili: &Arc<Bili>,
    faces: &[String],
    pictures: &[String],
    emojis: &[(String, String)],
    original: bool,
    want: Want,
) -> Material {
    let mut art = Art::default();
    let mut bytes: HashMap<String, Ready> = HashMap::new();
    for url in faces {
        if let Some((done, image)) = fetch(bili, url, cache::USER, original, want).await {
            bytes.insert(url.clone(), done);
            if let Some(found) = image {
                art.faces.insert(url.clone(), found);
            }
        }
    }
    for url in pictures {
        if let Some((done, image)) = fetch(bili, url, cache::IMAGES, original, want).await {
            bytes.insert(url.clone(), done);
            if let Some(found) = image {
                art.pictures.insert(url.clone(), found);
            }
        }
    }
    // 表情只画进卡片，不作为附件发出去，所以只取解码后的那一份
    if want != Want::Bytes {
        let mut set = HashMap::new();
        for (name, url) in emojis {
            if let Some((_, Some(image))) = fetch(bili, url, cache::EMOJI, true, Want::Art).await {
                set.insert(name.clone(), image);
            }
        }
        art.emojis = EmojiSet::new(set);
    }
    Material { art, bytes }
}

/// 取一张图，返回「可以直接出门的字节」与（要画进卡片时）限幅后的位图。
/// `downloadOriginal=false` 时先问图床要缩放过的 webp，拿不到再回退原图。
/// 盘上那份缓存在最前面：一条动态九张图、几十个目标，每轮都从图床重拉一遍既慢又容易撞限流。
async fn fetch(
    bili: &Arc<Bili>,
    url: &str,
    kind: &str,
    original: bool,
    want: Want,
) -> Option<(Ready, Option<image::RgbaImage>)> {
    let raw = match raw_of(bili, url, kind, original).await {
        Some(bytes) => bytes,
        None => return None,
    };
    // 只取字节、而这份字节本来就是 PNG/JPEG 时一次都不解——那是最常见的路径
    let needs_image = want != Want::Bytes
        || !(raw.starts_with(push::PNG_MAGIC) || raw.starts_with(push::JPEG_MAGIC));
    let decoded = if needs_image {
        decode(raw.as_slice())
    } else {
        None
    };
    // 解码可能是为了归一 WebP（`needs_image`），不一定是为了画卡：只要字节的这一档，
    // 解出来的那份全分辨率位图绝不能进素材包，否则这一档省下的内存又原样留着了
    let painted = if want == Want::Bytes {
        None
    } else {
        decoded.as_ref().and_then(art_of)
    };
    // 出门字节要的是**原件**（缩放那份只是给卡片用的），且一张图一轮只归一一次：
    // 归一里 WebP 转 PNG 最贵，现在复用上面这一次解码，不再为每个目标各解各编一遍。
    let done = match want {
        Want::Art => Ready::of(Vec::new(), None),
        _ => Ready::of(raw, decoded.as_ref()),
    };
    Some((done, painted))
}

/// 一张图的原件：先问盘上缓存，再问图床（带两个地址的尝试顺序见 [`fetch_attempts`]）
async fn raw_of(bili: &Arc<Bili>, url: &str, kind: &str, original: bool) -> Option<Vec<u8>> {
    if let Some(raw) = cache::read(kind, url) {
        return Some(raw);
    }
    for attempt in fetch_attempts(url, original) {
        if let Ok(raw) = bili.bytes(&attempt, "https://www.bilibili.com/").await {
            cache::write(kind, url, &raw);
            return Some(raw);
        }
    }
    None
}

/// 解码闸：源图超过 [`MAX_DECODE_PIXELS`] 就不解。判的是头里的声明尺寸，所以真解之前就把关。
fn within_decode_gate(width: u32, height: u32) -> bool {
    (width as u64) * (height as u64) <= MAX_DECODE_PIXELS
}

/// 按源图头里的尺寸决定解不解：闸不过就回 None（口径见 [`MAX_DECODE_PIXELS`] 那条说明）。
fn decode(raw: &[u8]) -> Option<image::DynamicImage> {
    let reader = match image::ImageReader::new(std::io::Cursor::new(raw)).with_guessed_format() {
        Ok(reader) => reader,
        Err(_) => return None,
    };
    let (w, h) = reader.into_dimensions().ok()?;
    if !within_decode_gate(w, h) {
        arona::runtime::log::debug(format!(
            "bilibili：一张 {w}x{h} 的图超出解码闸（{MAX_DECODE_PIXELS} 像素），卡片按缺图画，附件照发"
        ));
        return None;
    }
    image::load_from_memory(raw).ok()
}

/// 把解出来的图收成画卡用的位图，并按 [`ART_PIXELS`] 限幅。
fn art_of(any: &image::DynamicImage) -> Option<image::RgbaImage> {
    let (w, h) = (any.width(), any.height());
    let pixels = (w as u64) * (h as u64);
    if pixels <= ART_PIXELS {
        return Some(any.to_rgba8());
    }
    let k = (ART_PIXELS as f64 / pixels as f64).sqrt();
    let (tw, th) = (
        ((w as f64 * k).round() as u32).max(1),
        ((h as f64 * k).round() as u32).max(1),
    );
    Some(image::imageops::resize(
        &any.to_rgba8(),
        tw,
        th,
        image::imageops::FilterType::Triangle,
    ))
}

/// 取一张图要试哪几个地址：要原图、URL 自带尺寸档、或图床不认那套缩放语法时只走原件，
/// 否则先问 `@720w.webp` 那一份缩略，拿不到再回退原件——这是 `cache.downloadOriginal` 那句说明的兑现。
fn fetch_attempts(url: &str, original: bool) -> Vec<String> {
    if original || url.contains('@') || !is_image_host(url) {
        vec![url.to_string()]
    } else {
        vec![format!("{url}@720w.webp"), url.to_string()]
    }
}

/// 只有 B 站图床认 `@720w.webp` 那套缩放语法。
/// 认的是**主机段**，不是整串前缀：站方回包里的图 URL 两种写法都常见——scheme 是 `http://`、
/// 主机除了 i0/i1 还有 i2，先前按 `https://i0.` / `https://i1.` 两个前缀比，
/// 于是绝大多数稿件封面（实测 `http://i0.hdslb.com/bfs/archive/…`、`http://i2.hdslb.com/…`）
/// 一次都没问过缩略档，直接拿原件：1920x1080 的封面无损转 PNG 就是几 MB，长图能到几十 MB 并顶爆对端帧上限
/// （README 预留问题第 8 条的第一环）。主机段比较而不是子串，是为了不被 `i0.hdslb.com.evil.com` 这类冒名顶替。
fn is_image_host(url: &str) -> bool {
    let authority = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or_default();
    // `https://用户名@主机/` 里主机在最后一个 `@` 之后
    let host = authority.rsplit('@').next().unwrap_or_default();
    let host = host
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    host == "hdslb.com" || host.ends_with(".hdslb.com")
}

/// 动态详情链接：短链失败退回长链，推的东西不能因为短链挂了就没链接
async fn link_of(bili: &Arc<Bili>, to_short: bool, dynamic: &Dynamic) -> String {
    let long = dynamic.link();
    if !to_short {
        return long;
    }
    let id = dynamic.id.to_string();
    match bili
        .short_link(&id, &format!("opub_{id}"), Some(&long))
        .await
    {
        Some(short) => short,
        None => long,
    }
}

/// 一张卡片的取用顺序：这一轮的 memo → 盘上的 `DRAW` → 现画。
/// `sticky` 才允许读写盘：内容不会再变的东西（动态卡）才值得跨轮复用，
/// 带着实时数字的（直播卡）只在这一轮里省一省。
/// 画不出来是 `None`（字体找不到之类），那种失败不往盘上写，否则一次抖动就永久没卡。
fn painted(
    drawn: &mut HashMap<String, Option<Ready>>,
    key: String,
    sticky: bool,
    paint: impl FnOnce() -> Option<Ready>,
) -> Option<Ready> {
    if let Some(hit) = drawn.get(&key) {
        return hit.clone();
    }
    let found = match cache::read(cache::DRAW, &key) {
        Some(raw) if sticky => Some(Ready::of(raw, None)),
        _ => {
            let made = paint();
            if sticky && let Some(card) = &made {
                cache::write(cache::DRAW, &key, card.bytes());
            }
            made
        }
    };
    drawn.insert(key, found.clone());
    found
}

/// 缓存键：哪一类、哪一条、哪个配色都写进去，再把外观、链接与译文折成一小段指纹。
/// 改了主题/宽度/水印，或把 `push.toShortLink` 打开了，盘上那些旧卡就该作废——
/// 卡片画的正是那两者决定的内容，所以它们必须在键里。译文也得进去：
/// 把 `translate.enable` 关掉之后，贴过译文的旧卡不该再被复用。
fn card_key(
    bucket: &str,
    id: &str,
    color: &str,
    link: &str,
    translation: Option<&Rendered>,
    appearance: &ImageConfig,
) -> String {
    let seen = translation.map_or(String::new(), |done| done.text.clone());
    // 插件版本**必须**参与 digest：卡片是按内容算键的，而内容怎么解析、怎么画是版本决定的。
    // 少了这一项，升级后同一条动态还会命中上一版画好的图（正文丢失那种 bug 改完也看不出来），
    // 用户只能手动去删 data/bilibili/image/DRAW/ 或者等 7 天过期。
    let digest = format!(
        "{:x}",
        md5::compute(format!(
            "{version}|{appearance:?}|{link}|{seen}",
            version = env!("CARGO_PKG_VERSION")
        ))
    );
    let stamp = &digest[..8];
    format!("{bucket}/{id}/{color}#{stamp}")
}

/// 画一张动态卡。字面拿不到（字体找不到之类）就按无卡推——那句日志由 [`Faces::need`] 一轮报一次。
fn paint_dynamic(
    appearance: &ImageConfig,
    color: &str,
    dynamic: &Dynamic,
    art: &Art,
    link: &str,
    translation: Option<&Rendered>,
    faces: &mut Faces,
) -> Option<Ready> {
    let face = faces.need()?;
    let style = style_for(
        appearance,
        color,
        link,
        &appearance.footer.dynamic_footer,
        face,
    );
    Some(Ready::of(
        dynamic_card(dynamic, art, &style, translation),
        None,
    ))
}

/// 画一张直播卡
fn paint_live(
    appearance: &ImageConfig,
    color: &str,
    live: &LiveInfo,
    art: &Art,
    link: &str,
    faces: &mut Faces,
) -> Option<Ready> {
    let face = faces.need()?;
    let style = style_for(
        appearance,
        color,
        link,
        &appearance.footer.live_footer,
        face,
    );
    Some(Ready::of(live_card(live, art, &style), None))
}

/// 一份外观参数 → 画卡要的全部东西
fn style_for(
    appearance: &ImageConfig,
    color: &str,
    link: &str,
    footer: &str,
    face: &Arc<Typeface>,
) -> Style {
    let generator = &appearance.color_generator;
    let stops = background(
        color,
        generator.hue_step as f32,
        generator.lock_sb,
        generator.saturation as f32,
        generator.brightness as f32,
    );
    Style {
        quality: Quality::find(&appearance.quality),
        theme: themed(appearance),
        face: Arc::clone(face),
        background: stops,
        badge_left: appearance.badge_enable.left,
        footer: footer.to_string(),
        qr: if appearance.card_ornament.eq_ignore_ascii_case("qrcode") {
            Some(link.to_string())
        } else {
            None
        },
    }
}

/// 主题给的是整套内置配色，`image` 区那三个颜色键是**覆盖**：留空就跟随 `image.theme`。
/// 认不出的颜色只记一条日志并退回主题值——配色写错不该让这条动态不推。
///
/// 照参考实现的键形态（`DynamicImageTheme.kt` + 解析那段 `General.kt:131-150`）：
/// 透明度**没有独立的键**，它就写在色串自己的八位里（`#B4FFFFFF` 是七成不透明的白）；
/// 而描边那一键本来就是 `;` 分隔的**颜色串**（`cardOutlineColors: IntArray`），
/// 彩虹主题那档是七个色——所以覆盖底色与覆盖描边互不牵连，正如参考实现那两个字段本来就是两个。
fn themed(appearance: &ImageConfig) -> Theme {
    let mut theme = Theme::find(&appearance.theme);
    if let Some(color) = single_color("cardColor", &appearance.card_color) {
        theme.card_bg = color;
    }
    if let Some(stops) = color_stops("cardOutlineColor", &appearance.card_outline_color) {
        theme.card_outline = stops;
    }
    if let Some(color) = single_color("drawOutlineColor", &appearance.draw_outline_color) {
        theme.draw_outline = color;
    }
    theme
}

/// 单色键：`#AARRGGBB`／`#RRGGBB`（或三位缩写），留空与写错都返回 `None`＝按主题走
fn single_color(key: &str, raw: &str) -> Option<Color> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match Color::parse(raw) {
        Some(color) => Some(color),
        None => {
            miscolored(key, raw);
            None
        }
    }
}

/// 描边键：`;` 分隔的颜色串，任何一段认不出来整键作废。只给一段就是普通单色描边
fn color_stops(key: &str, raw: &str) -> Option<Vec<Color>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let wanted = raw.split(';').count();
    let stops: Vec<Color> = raw
        .split(';')
        .filter_map(|part| Color::parse(part.trim()))
        .collect();
    if stops.len() != wanted {
        miscolored(key, raw);
        return None;
    }
    Some(stops)
}

/// 与 [`single_color`]／[`color_stops`] 同一句措辞，免得三个键各报各的
fn miscolored(key: &str, raw: &str) {
    arona::runtime::log::warning(format!(
        "bilibili：image.{key} 认不出来（{raw}），这一项按主题原来的走"
    ));
}

/// 绘图配置现取（热重载之后下一次检测就用新值）
fn image_of(ctx: &PluginContext) -> ImageConfig {
    ctx.config::<ImageConfig>(IMAGE_SECTION).get()
}

/// 某个目标这一条该用哪套配色。
///
/// 唯一规则在 [`ImageConfig::color_for`]：目标自己设过（`/bili color`）就用它，没设过就用
/// `image.defaultColor`。**画卡、算缓存键、按配色取回画好的卡，三处必须用同一个值**——
/// 这里返回的是"解析后的实色"而不是 `Option`，正是为了不让空串漏进绘制层：空串在
/// [`background`](crate::draw::theme::background) 里会走另一套兜底渐变，于是配置写着粉、
/// 推送却是蓝（手动 `/bili new` 走的是 `color_for`，所以那一路一直是粉的）。
fn color_of(appearance: &ImageConfig, state: &TargetState) -> String {
    appearance.color_for(state.color.as_deref())
}

/// 命令层"先看一眼"：按给定配色把一条动态画成 Post，走的是和真实推送同一条取素材、
/// 画图、组模板的路径，只是不动水位也不动订阅关系。`color` 给空串就是默认配色。
pub(crate) async fn preview(
    ctx: &PluginContext,
    bili: &Arc<Bili>,
    dynamic: &Dynamic,
    color: &str,
) -> Post {
    let conf = ctx.config::<BiliConfig>(SECTION).get();
    let appearance = image_of(ctx);
    let mut faces = Faces::for_round(&appearance);
    let link = link_of(bili, conf.push.to_short_link, dynamic).await;
    // 只有真带着房间信息才走直播版式：没有 live 还按直播画就推出一张空名空题的卡
    if let Some(live) = live_of(dynamic).filter(|_| kind_of(dynamic) == "直播") {
        let Material { art, bytes } = gather(
            bili,
            &face_urls(dynamic),
            &picture_urls(dynamic),
            &emoji_pairs(dynamic),
            conf.cache.download_original,
            Want::Both,
        )
        .await;
        let vals = Vals::live(&live, link.clone(), &format_time(now()));
        let card = paint_live(&appearance, color, &live, &art, &link, &mut faces);
        return Post::live(vals, card, bytes.get(&live.cover).cloned());
    }
    let vals = Vals::dynamic(dynamic, link.clone());
    let translation = translate::translate(&conf.translate, &dynamic.text).await;
    let key = card_key(
        "dynamic",
        &dynamic.id.to_string(),
        color,
        &link,
        translation.as_ref(),
        &appearance,
    );
    // 先看一眼也认盘上那张 DRAW：命中的那一次一张素材都不用解
    let cached = cache::read(cache::DRAW, &key).map(|raw| Ready::of(raw, None));
    let Material { art, bytes } = gather(
        bili,
        &face_urls(dynamic),
        &picture_urls(dynamic),
        &emoji_pairs(dynamic),
        conf.cache.download_original,
        if cached.is_some() {
            Want::Bytes
        } else {
            Want::Both
        },
    )
    .await;
    let mut drawn: HashMap<String, Option<Ready>> = HashMap::new();
    if let Some(card) = &cached {
        drawn.insert(key.clone(), Some(card.clone()));
    }
    let card = painted(&mut drawn, key, true, || {
        paint_dynamic(
            &appearance,
            color,
            dynamic,
            &art,
            &link,
            translation.as_ref(),
            &mut faces,
        )
    });
    let images: Vec<Ready> = dynamic
        .images
        .iter()
        .filter_map(|url| bytes.get(url).cloned())
        .collect();
    Post::dynamic(vals, card, images)
}

/// 直播版的"先看一眼"：`/bili live` 拿到的房间信息按同一套画卡路径出图
pub(crate) async fn preview_live(
    ctx: &PluginContext,
    bili: &Arc<Bili>,
    info: &LiveInfo,
    color: &str,
) -> Post {
    let conf = ctx.config::<BiliConfig>(SECTION).get();
    let appearance = image_of(ctx);
    let mut faces = Faces::for_round(&appearance);
    let mut avatars: Vec<String> = Vec::new();
    if info.face.is_empty() {
        // batch 接口有时不给头像，补一次直播用户资料——名片那条要 WBI 签名，连打会被判风控
        if let Ok(anchor) = bili.master(info.uid).await {
            avatars.push(anchor.face);
        }
    } else {
        avatars.push(info.face.clone());
    }
    let Material { art, bytes } = gather(
        bili,
        &avatars,
        std::slice::from_ref(&info.cover),
        &[],
        conf.cache.download_original,
        Want::Both,
    )
    .await;
    let link = info.link();
    let vals = Vals::live(info, link.clone(), &format_time(now()));
    let card = paint_live(&appearance, color, info, &art, &link, &mut faces);
    Post::live(vals, card, bytes.get(&info.cover).cloned())
}

/// 北京时间口径下的当前秒级时间戳（模板与状态都用它）
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 接口失败的汇报：`reportInterval` 为 0 就每次记，否则按间隔节流。
/// 节流点记的是**上一次真记了日志**的时间（原插件 `BiliCheckTasker.lastCheck` 同口径）；
/// 早先这里每次调用都刷新时间戳，检测间隔比汇报间隔短时一条都记不出来。
fn report(check: &CheckConfig, message: &str) {
    let stamp = now() as u64;
    let last = LAST_REPORT.load(Ordering::SeqCst);
    if !throttled(check.report_interval, last, stamp) {
        LAST_REPORT.store(stamp, Ordering::SeqCst);
        arona::runtime::log::warning(format!("bilibili：{message}"));
    }
}

/// 这条汇报要不要压住。`interval` 单位分钟（与配置说明一致），0 表示不节流
fn throttled(interval: u64, last: u64, stamp: u64) -> bool {
    interval != 0 && stamp.saturating_sub(last) < interval.saturating_mul(60)
}

/// `ss12345` / `12345` → 番剧季号。`ep`/`md` 在命令层就换成 `ss` 了，这里只认这两种写法。
fn season_id(raw: &str) -> Option<i64> {
    let trimmed = raw.trim().to_lowercase();
    let digits = trimmed.strip_prefix("ss").unwrap_or(&trimmed);
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    digits.parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(low_speed: &str) -> CheckConfig {
        CheckConfig {
            low_speed: low_speed.to_string(),
            ..Default::default()
        }
    }

    fn dynamic(uid: i64, id: i64) -> Dynamic {
        Dynamic {
            uid,
            id,
            ..Default::default()
        }
    }

    /// 卡片缓存键必须带上插件版本。
    ///
    /// 键是内容的指纹：同样的动态、同样的配色就算同一个键。而"同样的内容画成什么样"
    /// 是版本决定的——正文丢了这种 bug 修完，若键里没有版本，升级后照样命中上一版
    /// 画好的旧图，用户只能手动删 DRAW 缓存或者干等过期。
    #[test]
    fn card_cache_key_carries_the_plugin_version() {
        let appearance = crate::config::ImageConfig::default();
        let link = "https://t.bilibili.com/123";
        let key = card_key("dyn", "123", "#ffffff", link, None, &appearance);
        let expected = format!(
            "{:x}",
            md5::compute(format!(
                "{}|{appearance:?}|{link}|",
                env!("CARGO_PKG_VERSION")
            ))
        );
        assert!(
            key.ends_with(&expected[..8]),
            "缓存键里没有版本号（升级后会继续用旧卡）：{key}"
        );
    }

    fn marks(rows: &[(i64, i64)]) -> BTreeMap<i64, i64> {
        rows.iter().copied().collect()
    }

    /// 推送取色必须与 `/bili new` 预览同源。
    ///
    /// 现场故障：订阅目标没设过配色，`/bili new` 出的是配置里的粉色，到点推送却是蓝色。
    /// 原因是推送这边取 `state.color.unwrap_or_default()` 拿到空串，而空串在绘制层
    /// 另有一套内置渐变。这个用例钉住"空 = 跟随 image.defaultColor"这条规则。
    #[test]
    fn push_color_follows_the_configured_default() {
        let appearance = ImageConfig {
            default_color: "#fde8ed".to_string(),
            ..Default::default()
        };
        let never_set = TargetState::default();
        assert_eq!(
            color_of(&appearance, &never_set),
            "#fde8ed",
            "目标没设配色时推送该跟 image.defaultColor 走，不能是空串"
        );

        let own = TargetState {
            color: Some("#ff8800".into()),
            ..Default::default()
        };
        assert_eq!(color_of(&appearance, &own), "#ff8800");

        // 全局那一项也空着：落出厂值，仍然不能把空串交给绘制层
        let blank = ImageConfig {
            default_color: String::new(),
            ..Default::default()
        };
        assert_eq!(
            color_of(&blank, &never_set),
            crate::config::DEFAULT_COLOR,
            "配置空着时该退到出厂色，而不是让绘制层走另一套兜底渐变"
        );
    }

    #[test]
    fn one_firehose_page_is_bucketed_per_subscriber() {
        // 页里有三个 UP：1 和 2 有人订，3 没人订；没人订的那些只可能从通配档 `0` 出去
        let feed = [
            dynamic(3, 300),
            dynamic(1, 210),
            dynamic(2, 205),
            dynamic(1, 200),
            dynamic(1, 100),
        ];
        let plan = plan_round(
            &feed,
            &[1, 2],
            &marks(&[(1, 200), (2, 100), (0, 150)]),
            true,
        );
        // 水位 200 那条本身不算新（`>` 而非 `>=`），只剩 210
        assert_eq!(ids(&plan, 1), vec![210]);
        assert_eq!(ids(&plan, 2), vec![205]);
        // 通配档收所有作者，按 id 旧→新排（推送顺序要跟读者看到的时间线一致）。
        // 1 那条 200 在这里出现却不从档位 1 出去：两档各查各的水位（0 是 150，1 是 200）。
        assert_eq!(ids(&plan, 0), vec![200, 205, 210, 300]);
        assert!(plan.align.is_empty(), "三档都有水位，不该有对齐");
    }

    #[test]
    fn a_slot_that_has_never_been_seen_only_aligns() {
        // 第一次订阅：只把水位推到页里见到的最新一条，历史一条都不推
        let feed = [dynamic(7, 500), dynamic(7, 400), dynamic(8, 300)];
        let plan = plan_round(&feed, &[7, 8], &marks(&[(8, 250)]), false);
        assert_eq!(
            plan.align.get(&7),
            Some(&500),
            "7 没见过，水位该对齐到它最新的一条"
        );
        assert!(
            ids(&plan, 7).is_empty(),
            "第一次见到就把历史全推出去，等于一订阅就刷屏"
        );
        assert_eq!(ids(&plan, 8), vec![300]);
        assert!(!plan.fresh.contains_key(&7));
    }

    #[test]
    fn the_wildcard_slot_aligns_on_its_own() {
        // 「全部动态」第一次跑：整页二十条都不该推，只对齐
        let feed = [dynamic(1, 900), dynamic(2, 800)];
        let plan = plan_round(&feed, &[1], &marks(&[(1, 100)]), true);
        assert_eq!(plan.align.get(&0), Some(&900));
        assert!(!plan.fresh.contains_key(&0));
        assert!(!plan.align.contains_key(&1), "1 有水位，走的是推送那一支");
    }

    #[test]
    fn overlapping_pages_are_merged_by_id() {
        // 翻页时两页之间又发了动态，拼接处会重复同一 id：去重，只推一次
        let feed = [
            dynamic(1, 200),
            dynamic(1, 190),
            dynamic(1, 190),
            dynamic(1, 180),
        ];
        let plan = plan_round(&feed, &[1], &marks(&[(1, 100)]), false);
        assert_eq!(ids(&plan, 1), vec![180, 190, 200]);
    }

    #[test]
    fn paging_stops_once_the_page_drops_below_every_watermark() {
        let feed = [dynamic(1, 300), dynamic(2, 200)];
        // 页里最旧 200 还高于最低水位 150，说明更老的页里可能还有没见过的
        assert!(should_fetch_more(
            &feed,
            &[1, 2],
            &marks(&[(1, 150), (2, 150)]),
            false
        ));
        // 已经压到最低水位之下：再老的动态对谁都不算新，翻下去只是多打接口
        assert!(!should_fetch_more(
            &feed,
            &[1, 2],
            &marks(&[(1, 200), (2, 250)]),
            false
        ));
        // 通配档的水位也算数：它订的是所有作者，压不到它底下就不能翻
        assert!(should_fetch_more(
            &feed,
            &[1],
            &marks(&[(1, 250), (0, 100)]),
            true
        ));
        assert!(!should_fetch_more(
            &feed,
            &[1],
            &marks(&[(1, 250), (0, 200)]),
            true
        ));
        // 全是第一次订阅（一个已知水位都没有）：那一轮只对齐，翻页白翻
        assert!(!should_fetch_more(&feed, &[1, 2], &marks(&[]), false));
        assert!(
            !should_fetch_more(&[], &[1], &marks(&[(1, 1)]), false),
            "空页没什么可翻的"
        );
    }

    /// 「翻页预算用完了」和「翻到底了」是两件事，只有前者会漏，而漏掉的那些永远补不回来
    /// （水位推到本轮见过的最大 id，下一轮那些也不算新了）。这条钉住那个区别，别让那句 warn 变成瞎报。
    #[test]
    fn only_an_exhausted_page_budget_leaks_dynamics() {
        let feed = [dynamic(1, 300), dynamic(2, 200)];
        let uids = [1, 2];
        let above = marks(&[(1, 150), (2, 150)]);
        // 三页翻满、页里最旧那条仍比最低的那条水位新：更旧的页没取到，这一轮跳过去的就是漏
        assert!(past_the_page_cap(3, 3, &feed, &uids, &above, false));
        // 同样翻满三页，但已经压到水位之下——那是翻到底，不是预算用完
        let at_bottom = marks(&[(1, 200), (2, 250)]);
        assert!(!past_the_page_cap(3, 3, &feed, &uids, &at_bottom, false));
        // 预算没用完（空页或读不出来提前收尾），剩下的页下一轮还会打
        assert!(!past_the_page_cap(2, 3, &feed, &uids, &above, false));
    }

    /// 逐 UP 那一路不翻页，所以「整页都比水位新」是「这一页可能没到底」的唯一迹象——
    /// 页里只要有一条已经见过（含水位那条本身），就说明这一页已经压进已知区，没漏。
    #[test]
    fn an_all_new_page_is_the_only_sign_the_per_up_page_ran_over() {
        let page = [dynamic(1, 300), dynamic(1, 250)];
        // 两条都比水位新：这一页底下说不定还有几条我们永远看不到的
        assert!(page_might_hold_more(&page, 200));
        // 最旧那条正好是水位那条：`id > 水位` 才算新，见过就是到底了
        assert!(!page_might_hold_more(&page, 250));
        assert!(!page_might_hold_more(&page, 300));
        // 第一次订阅那一轮整页当然全是新的，但那一轮只对齐，不该报警
        assert!(!page_might_hold_more(&page, 0));
        // 空页（匿名接口常回 `code 0`）什么都不说明
        assert!(!page_might_hold_more(&[], 200));
    }

    /// 「订了却没关注」那句自我交代的选人规则：一轮最多问一个、问过不再问、露面即摘标。
    #[test]
    fn only_a_never_asked_absent_subscriber_gets_the_relation_probe() {
        let mut asked = BTreeSet::new();
        // 1 在页里露面、2 没有 → 只可能轮到 2
        let seen = BTreeSet::from([1]);
        assert_eq!(pick_probe(&[1, 2], &seen, &mut asked), Some(2));
        // 问过就不再问：这一轮没有第二个可问的
        assert_eq!(pick_probe(&[1, 2], &seen, &mut asked), None);
        // 2 又开始发动态了：他没露面这条证据作废，摘标
        let seen = BTreeSet::from([1, 2]);
        assert_eq!(pick_probe(&[1, 2], &seen, &mut asked), None);
        assert!(
            asked.is_empty(),
            "露过面就该把标记摘掉，将来真被取关了还查得出来"
        );
        let seen = BTreeSet::from([1]);
        assert_eq!(pick_probe(&[1, 2], &seen, &mut asked), Some(2));
        // 订的人多、页里一个都没露面：一轮也只吃一个，不该甩出一串关系接口
        let none = BTreeSet::new();
        assert_eq!(pick_probe(&[7, 8, 9], &none, &mut asked), Some(7));
        assert_eq!(pick_probe(&[7, 8, 9], &none, &mut asked), Some(8));
    }

    /// 站方回包里的图 URL 写法不止一种：scheme 常是 `http://`，主机除了 i0/i1 还有 i2。
    /// 这三种都得先问 `@720w` 那份缩略——先前按两个 `https://i0.`/`https://i1.` 前缀比，
    /// 稿件封面一次都没问过缩略档，直接拿原件（1920x1080 起，长图几十 MB），
    /// `cache.downloadOriginal` 那句说明等于没写。
    #[test]
    fn a_cover_over_http_or_on_i2_still_asks_for_the_scaled_variant() {
        for url in [
            "http://i0.hdslb.com/bfs/archive/9713f8f22eff53a2ec71ead08695cc54ce656d9a.jpg",
            "http://i2.hdslb.com/bfs/archive/20f350dbb5ddf77ae86766bab425359914a136c7.jpg",
            "https://i1.hdslb.com/bfs/face/83ec10ebb202811c6d08be512275fbbff390f9b9.jpg",
            "https://I0.HDSLB.COM/bfs/archive/x.jpg",
        ] {
            let attempts = fetch_attempts(url, false);
            assert_eq!(attempts.len(), 2, "{url} 该先问缩略那一份、再回退原件");
            assert!(
                attempts[0].ends_with("@720w.webp") && attempts[1] == url,
                "{url} 的两次尝试顺序不对：{attempts:?}"
            );
        }
    }

    /// 素材那两道闸的口径：画卡用的位图超预算就等比缩到预算内，声明尺寸过了解码闸的根本不解。
    #[test]
    fn oversized_material_is_clamped_to_the_drawing_budget() {
        let big = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2000,
            3000,
            image::Rgba([9, 9, 9, 255]),
        ));
        let cut = art_of(&big).unwrap();
        assert!(
            (cut.width() as u64) * (cut.height() as u64) <= ART_PIXELS + 4000,
            "没缩进预算：{}x{}",
            cut.width(),
            cut.height()
        );
        let ratio = cut.width() as f64 / cut.height() as f64;
        assert!(
            (ratio - 2000.0 / 3000.0).abs() < 0.01,
            "等比缩出来的比例不对：{ratio}"
        );
        // 预算内的原样留，不该被无谓地过一次缩放
        let fits = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1080,
            1440,
            image::Rgba([9, 9, 9, 255]),
        ));
        let kept = art_of(&fits).unwrap();
        assert_eq!((kept.width(), kept.height()), (1080, 1440));
    }

    /// 解码闸只看头里声明的尺寸：站方真给到两万像素的长图要能画，
    /// 而一张 1 GB 当量的怪东西不能把进程顶爆。
    #[test]
    fn the_decode_gate_reads_only_the_declared_size() {
        assert!(within_decode_gate(1440, 20000), "长图被闸住了");
        assert!(within_decode_gate(4000, 4000));
        assert!(!within_decode_gate(8192, 32768), "1 GB 当量的不该解");
    }

    /// 只走原件的那三种：真要原图、URL 自带尺寸档、图床不认这套语法。
    #[test]
    fn only_the_image_host_gets_the_scaled_variant() {
        assert_eq!(
            fetch_attempts("https://i0.hdslb.com/bfs/archive/x.jpg", true),
            vec!["https://i0.hdslb.com/bfs/archive/x.jpg".to_string()]
        );
        assert_eq!(
            fetch_attempts("https://i0.hdslb.com/bfs/archive/x.jpg@720w.webp", false),
            vec!["https://i0.hdslb.com/bfs/archive/x.jpg@720w.webp".to_string()]
        );
        // 新那家 CDN 认不认这套语法没实测过，所以按原件交——宁可大，也不能拿一次注定失败的请求去挡推送
        assert!(!is_image_host(
            "https://archive.biliimg.com/bfs/archive/x.jpg"
        ));
        assert!(!is_image_host("https://example.com/x.jpg"));
    }

    /// 认的是主机段，不是整串子串：冒名顶替的那个域名不能被判成图床。
    #[test]
    fn a_lookalike_host_is_not_the_image_host() {
        assert!(!is_image_host("https://i0.hdslb.com.evil.com/x.jpg"));
        assert!(!is_image_host("https://i0.hdslb.com@evil.com/x.jpg"));
        assert!(!is_image_host("https://evil.com/?u=https://i0.hdslb.com/"));
        // 图床自己的写法都认；带端口只是同一个主机的另一个入口，不改变它是图床这件事。
        // 子域一视同仁：认的是「谁名下的主机」，不是背下 i0/i1/i2 那三个名字——
        // 站方哪天换个二级子域出来，不认就退成按原件下，那正是 #146 那次的病。
        assert!(is_image_host("https://noti0.hdslb.com/x.jpg"));
        assert!(is_image_host("http://hdslb.com/bfs/archive/x.jpg"));
        assert!(is_image_host("https://i2.hdslb.com:8443/bfs/archive/x.jpg"));
    }

    /// 顺序只能由 `pick_fresh` 自己定：`parse_feed` 交出来的是按 `time` 升序那份，接口页序又不保证什么。
    /// 这条把「任何输入顺序都取到页里最大的 id、推送都是旧→新」钉死——不打接口，也就绕开匿名空页那个抖动。
    #[test]
    fn pick_fresh_reads_the_page_by_id_whatever_order_it_comes_in() {
        // 最旧那条摆第一个，正是 `parse_feed` 排完之后逐 UP 那一页的形状
        let oldest_first = vec![
            dynamic(1, 100),
            dynamic(1, 900),
            dynamic(1, 800),
            dynamic(1, 200),
        ];
        let (fresh, newest) = pick_fresh(oldest_first, 0).expect("水位 0 时整页都算新");
        assert_eq!(newest, 900, "对齐该钉在页里 id 最大的那条，不是第一个");
        assert_eq!(ids_of(&fresh), vec![100, 200, 800, 900], "推出去要按旧→新");

        // 同一页轮转四个起点，取到的最大 id 与推送顺序都不能变
        let mut page = vec![
            dynamic(1, 900),
            dynamic(1, 100),
            dynamic(1, 800),
            dynamic(1, 200),
        ];
        for shift in 0..4 {
            page.rotate_left(1);
            let (fresh, newest) = pick_fresh(page.clone(), 150).expect("200 以上还有三条");
            assert_eq!(newest, 900, "第 {shift} 种页序取到的最大 id 不同");
            assert_eq!(ids_of(&fresh), vec![200, 800, 900]);
        }
    }

    #[test]
    fn a_page_wholly_at_or_below_the_watermark_is_not_fresh() {
        // 水位那条本身不算新（`>` 而非 `>=`）；整页都不新就返回 None，这一轮什么都不做
        assert!(pick_fresh(vec![dynamic(1, 200), dynamic(1, 100)], 200).is_none());
        let (fresh, newest) = pick_fresh(vec![dynamic(1, 300), dynamic(1, 200)], 200).unwrap();
        assert_eq!(ids_of(&fresh), vec![300]);
        assert_eq!(newest, 300);
    }

    fn ids_of(items: &[Dynamic]) -> Vec<i64> {
        items.iter().map(|item| item.id).collect()
    }

    fn ids(plan: &Plan, slot: i64) -> Vec<i64> {
        plan.fresh
            .get(&slot)
            .map(|rows| rows.iter().map(|item| item.id).collect())
            .unwrap_or_default()
    }

    #[test]
    fn a_bad_low_speed_string_leaves_the_intervals_untouched() {
        // 写错了就当没配：不能因为一个串让整个检测停摆
        assert_eq!(low_speed_factor(&check("乱写")), 1);
        assert_eq!(low_speed_factor(&check("")), 1);
        assert_eq!(current_intervals(&check("乱写")), (30, 15));
    }

    #[test]
    fn the_low_speed_factor_never_goes_below_one() {
        // `x0` 这种写法把乘以 0 挡掉，否则间隔变 0 就成了死循环轮询
        let (dynamic, live) = current_intervals(&check("0-24x0"));
        assert_eq!(dynamic, 30);
        assert_eq!(live, 15);
    }

    #[test]
    fn failure_reports_are_throttled_by_minutes_not_by_calls() {
        // 节流点必须是「上一次真记了日志」的时间：检测 30 秒一轮、汇报间隔 10 分钟，
        // 按调用刷时间戳的话每轮都差 30 秒，一条也记不出来
        assert!(!throttled(0, 0, 1_700_000_000));
        assert!(!throttled(10, 1_700_000_000 - 601, 1_700_000_000));
        assert!(throttled(10, 1_700_000_000 - 599, 1_700_000_000));
        // 间隔写得再离谱也不能溢出崩掉（debug 构建里乘法溢出会 panic）
        assert!(throttled(u64::MAX, 0, 1_700_000_000));
    }

    #[test]
    fn an_hour_spanning_midnight_is_still_one_window() {
        // 跨零点（22-3）在深夜与凌晨都算窗口内，这一条只验窗口边界解析与乘数
        let (from, to, factor) = low_speed_window("22-3x2").expect("跨零点窗口该解析出来");
        assert_eq!((from, to, factor), (22, 3, 2));
        let inside = |hour: u64| hour >= from || hour < to;
        assert!(inside(23) && inside(2) && !inside(12));
    }

    #[test]
    fn season_numbers_only_take_the_ss_form_for_now() {
        assert_eq!(season_id("ss40335"), Some(40335));
        assert_eq!(season_id("SS40335"), Some(40335));
        assert_eq!(season_id("40335"), Some(40335));
        assert_eq!(season_id("ep1234"), None);
        assert_eq!(season_id("md556"), None);
        assert_eq!(season_id(""), None);
    }

    /// 直播推荐动态的那张卡：content 里没有主播名与头像，缺的这两格由动态作者顶上，
    /// 而「没有 live 载荷」的动态绝不能走直播版式——那样推出去是一张空名空题的卡。
    #[test]
    fn live_of_borrows_the_author_slot_when_the_room_envelope_is_silent() {
        let with_room = Dynamic {
            uid: 546195,
            name: "老番茄".to_string(),
            face: "https://i0.hdslb.com/bfs/face/bc5ca1.jpg".to_string(),
            kind: "直播".to_string(),
            live: Some(LiveInfo {
                room_id: 21852,
                title: "最手忙脚乱的一集".to_string(),
                area: "其他单机".to_string(),
                status: 1,
                ..Default::default()
            }),
            ..Default::default()
        };
        let live = live_of(&with_room).expect("带着房间信息的直播该有直播版式");
        assert_eq!(live.anchor, "老番茄");
        assert_eq!(live.face, "https://i0.hdslb.com/bfs/face/bc5ca1.jpg");
        assert_eq!(live.uid, 546195);
        assert_eq!(live.title, "最手忙脚乱的一集");
        // content 自己给了主播名时不改写它
        let named = Dynamic {
            live: Some(LiveInfo {
                room_id: 21852,
                anchor: "官方账号".to_string(),
                status: 1,
                ..Default::default()
            }),
            ..with_room.clone()
        };
        assert_eq!(live_of(&named).expect("有房间信息").anchor, "官方账号");
        // 没有 live 载荷的「直播」类型动态拿不到直播版式
        assert!(
            live_of(&Dynamic {
                kind: "直播".to_string(),
                ..Default::default()
            })
            .is_none()
        );
    }

    /// 2026-09-30 实测到的坏推送：那一版按 `newest.title` 取最新集，而这个键接口压根不给，
    /// 于是空串跟基线一比就判成「换集」，推了一条尾巴空着的「更新了：」。
    #[test]
    fn a_season_without_an_episode_id_never_moves_the_baseline() {
        // 读不到最新一集：不推，也不动基线
        assert_eq!(season_move(Some("6291966"), 0), SeasonMove::Skip);
        assert_eq!(season_move(None, 0), SeasonMove::Skip);
        // 没有基线、空基线、老格式（当年按集名存下来的）都只重记不推
        assert_eq!(season_move(None, 6291966), SeasonMove::Record);
        assert_eq!(season_move(Some(""), 6291966), SeasonMove::Record);
        assert_eq!(
            season_move(Some("第01集（占位旧基线）"), 6291966),
            SeasonMove::Record
        );
        // 只有数字基线才进比对
        assert_eq!(season_move(Some("6291966"), 6291966), SeasonMove::Skip);
        assert_eq!(season_move(Some("6291908"), 6291966), SeasonMove::Push);
    }

    /// 卡片上要画的表情：正文与转发原文里的都收，同名只留一份，没地址的不收
    #[test]
    fn emoji_pairs_cover_the_quote_and_dedupe() {
        let dynamic = Dynamic {
            nodes: vec![
                Node::Text("正文 ".to_string()),
                Node::Emoji {
                    text: "[笑]".to_string(),
                    url: "https://i0.hdslb.com/e1.png".to_string(),
                },
                Node::Emoji {
                    text: "[哭]".to_string(),
                    url: "https://i0.hdslb.com/e2.png".to_string(),
                },
                Node::Emoji {
                    text: "[笑]".to_string(),
                    url: "https://i0.hdslb.com/e1.png".to_string(),
                },
                Node::Emoji {
                    text: "[没图]".to_string(),
                    url: String::new(),
                },
            ],
            origin: Some(Box::new(Dynamic {
                nodes: vec![Node::Emoji {
                    text: "[脱单delta]".to_string(),
                    url: "https://i0.hdslb.com/e3.png".to_string(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        };
        let names: Vec<String> = emoji_pairs(&dynamic)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, ["[笑]", "[哭]", "[脱单delta]"]);
    }

    #[test]
    fn picture_urls_cover_the_quote_and_the_attached_card() {
        let dynamic = Dynamic {
            id: 1,
            uid: 2,
            name: "测试Up".to_string(),
            face: "https://i0.hdslb.com/face.jpg".to_string(),
            kind: "动态".to_string(),
            type_text: "投稿视频".to_string(),
            time: 1_738_470_120,
            text: "正文".to_string(),
            nodes: Vec::new(),
            images: vec!["https://i0.hdslb.com/a.jpg".to_string()],
            links: Vec::new(),
            attached: Some(crate::bili::model::Attached {
                cover: "https://i0.hdslb.com/attach.jpg".to_string(),
                ..Default::default()
            }),
            extra: Vec::new(),
            live: None,
            origin: Some(Box::new(Dynamic {
                images: vec!["https://i0.hdslb.com/old.jpg".to_string()],
                face: "https://i0.hdslb.com/oldface.jpg".to_string(),
                ..Default::default()
            })),
            blocked: None,
            label: String::new(),
        };
        let faces = face_urls(&dynamic);
        let pictures = picture_urls(&dynamic);
        assert_eq!(faces.len(), 2, "自己与转发原作者的头像都要下");
        assert!(pictures.contains(&"https://i0.hdslb.com/a.jpg".to_string()));
        assert!(pictures.contains(&"https://i0.hdslb.com/attach.jpg".to_string()));
        assert!(pictures.contains(&"https://i0.hdslb.com/old.jpg".to_string()));
        // 原图 + 附属封面 + 转发原文那张 = 3
        assert_eq!(pictures.len(), 3);
        assert_eq!(kind_of(&dynamic), "动态");
    }

    #[test]
    fn a_live_in_the_feed_is_treated_as_a_live() {
        let dynamic = Dynamic {
            id: 1,
            uid: 2,
            name: "测试Up".to_string(),
            face: String::new(),
            kind: "直播".to_string(),
            type_text: "直播".to_string(),
            time: 1_738_470_120,
            text: String::new(),
            nodes: Vec::new(),
            images: Vec::new(),
            links: Vec::new(),
            attached: None,
            extra: Vec::new(),
            live: Some(LiveInfo::default()),
            origin: None,
            blocked: None,
            label: String::new(),
        };
        assert_eq!(kind_of(&dynamic), "直播");
        assert_eq!(face_urls(&dynamic).len(), 0, "空地址不该进下载清单");
    }

    #[test]
    fn a_muted_target_never_wants_anything() {
        let state = TargetState {
            muted: true,
            ..Default::default()
        };
        assert!(!wants(&state, 123, "动态", "正文"));
    }

    #[test]
    fn the_busy_guard_blocks_a_second_round_and_releases_on_drop() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let first = Busy::try_lock(&FLAG).expect("第一次该拿到");
        assert!(
            Busy::try_lock(&FLAG).is_none(),
            "同一轮没结束就不能再起一轮"
        );
        drop(first);
        assert!(Busy::try_lock(&FLAG).is_some(), "释放之后要能再起");
    }

    #[test]
    fn a_missing_font_reports_a_readable_reason() {
        // 指向一个肯定不存在的路径：报错文案要人能看懂，不能是裸 panic
        // （Typeface 没有 Debug，所以用 match 而不是 unwrap_err）
        match Typeface::discover("C:/does-not-exist/none.ttf") {
            Ok(_) => panic!("不存在的路径居然找到了字体"),
            Err(reason) => {
                assert!(
                    reason.contains("字体") || reason.contains("font"),
                    "{reason}"
                );
            }
        }
    }

    /// 三个颜色键各管各的，形态照参考实现：透明度没有独立的键，就写在色串自己的八位里
    /// （`#AARRGGBB`）；描边那一键本来就是 `;` 分隔的颜色串（`DynamicImageTheme.kt:135`
    /// 那份 `cardOutlineColors: IntArray`，彩虹主题七个色），所以覆盖底色抹不掉彩虹描边
    #[test]
    fn the_color_keys_govern_their_own_slots() {
        let appearance = ImageConfig {
            theme: "v3RainbowOutline".into(),
            card_color: "#80FF0000".into(),
            card_outline_color: "#FF0000; #00FF00;#0000FF".into(),
            ..Default::default()
        };
        let theme = themed(&appearance);
        assert_eq!(theme.card_bg, Color::rgba(255, 0, 0, 0x80));
        assert_eq!(theme.card_outline.len(), 3, "分号串没拆成多色描边");
        assert_eq!(
            theme.draw_outline,
            Theme::find("v3RainbowOutline").draw_outline,
            "没写的键不该跟着动"
        );
    }

    /// 任何一段认不出来就整键作废、只报日志：半条彩虹描边比主题那套更难看
    #[test]
    fn an_unreadable_palette_leaves_the_theme_outline_alone() {
        let expected = Theme::find("v3").card_outline.clone();
        let appearance = ImageConfig {
            card_outline_color: "#FF0000;不是颜色".into(),
            ..Default::default()
        };
        assert_eq!(themed(&appearance).card_outline, expected);
    }
}
