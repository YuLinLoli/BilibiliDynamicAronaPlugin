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
//! * 订"全部动态"（`/bili add 0`，原插件的 listenAll）走关注时间线接口，那一个档位要有登录态。
//! * `{draw}` 卡片按目标的配色缓存，同色只画一张。

use std::collections::{BTreeMap, HashMap};
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
use crate::draw::card::{Art, Style, dynamic_card};
use crate::draw::live::live_card;
use crate::draw::quality::Quality;
use crate::draw::text::{EmojiSet, Typeface};
use crate::draw::theme::{Theme, background};
use crate::push::template::{Supply, Vals};
use crate::push::{self, Kind, Post};
use crate::store::{LiveState, TargetState};
use crate::translate::{self, Rendered};
use crate::{CACHE_JOB, DYNAMIC_JOB, LIVE_JOB, RESCHEDULE_JOB};

/// 低频窗口的复核周期（秒）：进出窗口的最大延迟就是它
const RESCHEDULE_EVERY: u64 = 300;
/// 图片缓存清理的周期。原插件 `CacheClearTasker` 也是 24 小时一轮
const CACHE_EVERY: u64 = 60 * 60 * 24;

static DYNAMIC_BUSY: AtomicBool = AtomicBool::new(false);
static LIVE_BUSY: AtomicBool = AtomicBool::new(false);
static SEASON_BUSY: AtomicBool = AtomicBool::new(false);
/// 上一轮排出去的间隔，低频重排靠它判断"该不该重排"
static LAST_DYNAMIC_INTERVAL: AtomicU64 = AtomicU64::new(0);
static LAST_LIVE_INTERVAL: AtomicU64 = AtomicU64::new(0);
/// `--test-notify` 只测一次，配置热重载不能重复推
static SELFTEST_DONE: AtomicBool = AtomicBool::new(false);
/// 接口失败的汇报节流点（unix 秒）
static LAST_REPORT: AtomicU64 = AtomicU64::new(0);
/// 字体发现要读盘，配置串 → 那一份
static FONT: OnceLock<Mutex<(String, Option<Arc<Typeface>>)>> = OnceLock::new();

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

/// 一轮动态检测：逐个订阅者拉最新几条，过滤、绘图、推送
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
    let uids = crate::store().read(|data| data.subscribed_uids());
    let bili = api::client(conf.account.clone(), conf.check.timeout.max(5));
    for uid in uids {
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
        let mut fresh: Vec<Dynamic> = items
            .into_iter()
            .filter(|item| item.id > watermark)
            .collect();
        if fresh.is_empty() {
            continue;
        }
        // 接口按新→旧给，倒过来推才和读者看到的时间线一致
        fresh.reverse();
        let newest = fresh.last().map(|item| item.id).unwrap_or(watermark);
        if watermark == 0 {
            crate::store().write(|data| data.advance_watermark(uid, newest));
            arona::runtime::log::info(format!(
                "bilibili：{uid} 是第一次订阅，历史动态不推，水位已对齐最新一条"
            ));
            continue;
        }
        for dynamic in &fresh {
            push_one(&ctx, &conf, &bili, &appearance, uid, dynamic, &targets).await;
        }
    }
    // 关注时间线那一路：有人订了"全部动态"（原插件的 listenAll）才拉，要有登录态
    if bili.signed() && crate::store().read(|data| data.wants_all()) {
        timeline_round(&ctx, &conf, &bili, &appearance).await;
    }
}

/// 时间线一轮：把 `0` 这个通配档位下的新动态推给订了它的目标
async fn timeline_round(
    ctx: &PluginContext,
    conf: &BiliConfig,
    bili: &Arc<Bili>,
    appearance: &ImageConfig,
) {
    let items = match bili.timeline(1, "all").await {
        Ok(items) => items,
        Err(reason) => {
            report(&conf.check, &format!("关注时间线读不出来：{reason}"));
            return;
        }
    };
    let watermark =
        crate::store().read(|data| data.watermark.get("0").copied().unwrap_or_default());
    let mut fresh: Vec<Dynamic> = items
        .into_iter()
        .filter(|item| item.id > watermark)
        .collect();
    if fresh.is_empty() {
        return;
    }
    fresh.reverse();
    let newest = fresh.last().map(|item| item.id).unwrap_or(watermark);
    if watermark == 0 {
        // 第一次开这条：只对齐水位，不然一上来就把时间线首页二十条全推一遍
        crate::store().write(|data| data.advance_watermark(0, newest));
        arona::runtime::log::info("bilibili：关注时间线第一次跑，历史动态不推，水位已对齐最新一条");
        return;
    }
    let targets = states_of(0);
    for dynamic in &fresh {
        push_one(ctx, conf, bili, appearance, 0, dynamic, &targets).await;
    }
}

/// 一条新动态推给一批目标：先按目标筛，再取素材，卡片按配色缓存，最后逐个发。
/// `slot` 是水位所在的那一档——按 UP 主订阅时就是那个 uid，走关注时间线时传 `0`。
async fn push_one(
    ctx: &PluginContext,
    conf: &BiliConfig,
    bili: &Arc<Bili>,
    appearance: &ImageConfig,
    slot: i64,
    dynamic: &Dynamic,
    targets: &[(String, TargetState)],
) {
    let kind = kind_of(dynamic);
    let chosen: Vec<(String, TargetState)> = targets
        .iter()
        .filter(|(_, state)| wants(state, dynamic.uid, &kind, &dynamic.text))
        .map(|(key, state)| (key.clone(), state.clone()))
        .collect();
    if chosen.is_empty() {
        crate::store().edit_only(|data| data.advance_watermark(slot, dynamic.id));
        save(&conf.check);
        return;
    }
    let living = kind == "直播";
    let material = gather(
        bili,
        &face_urls(dynamic),
        &picture_urls(dynamic),
        &emoji_pairs(dynamic),
        conf.cache.download_original,
    )
    .await;
    let link = link_of(bili, conf.push.to_short_link, dynamic).await;
    // 机翻只贴动态卡：原插件那一路也只在画动态卡时调 trans，直播卡与文本模板都不带译文
    let translation = if living {
        None
    } else {
        translate::translate(&conf.translate, &dynamic.text).await
    };
    let vals = if living {
        let live = dynamic.live.clone().unwrap_or_default();
        Vals::live(&live, link.clone(), &format_time(now()))
    } else {
        Vals::dynamic(dynamic, link.clone())
    };
    let images: Vec<Vec<u8>> = dynamic
        .images
        .iter()
        .filter_map(|url| material.bytes.get(url).cloned())
        .collect();
    let cover = dynamic
        .live
        .as_ref()
        .and_then(|live| material.bytes.get(&live.cover).cloned());
    // 同一档配色只画一张卡：一个群一个色，二十个群别画二十遍
    let mut drawn: HashMap<String, Option<Vec<u8>>> = HashMap::new();
    for (key, state) in chosen {
        let color = state.color.clone().unwrap_or_default();
        let bucket = if living { "live" } else { "dynamic" };
        let card = painted(
            &mut drawn,
            card_key(
                bucket,
                &dynamic.id.to_string(),
                &color,
                &link,
                translation.as_ref(),
                appearance,
            ),
            // 动态内容不会再变，画好的卡值得落到盘上；直播卡带着在线人数与标题，只在这一轮里复用
            !living,
            || {
                if living {
                    let live = dynamic.live.clone().unwrap_or_default();
                    paint_live(appearance, &color, &live, &material, &link)
                } else {
                    paint_dynamic(
                        appearance,
                        &color,
                        dynamic,
                        &material,
                        &link,
                        translation.as_ref(),
                    )
                }
            },
        );
        let post = if living {
            Post::live(vals.clone(), card, cover.clone())
        } else {
            Post::dynamic(vals.clone(), card, images.clone())
        };
        push::push_state(ctx, &key, &state, &post).await;
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
    for uid in uids {
        let info = found.get(&uid).cloned().unwrap_or_default();
        let living = info.living();
        let prev = crate::store().read(|data| data.live.get(&uid.to_string()).cloned());
        let was_living = prev.as_ref().is_some_and(|state| state.status == 1);
        let targets = states_of(uid);
        match (was_living, living) {
            (false, true) => {
                let state = LiveState {
                    status: 1,
                    room_id: info.room_id,
                    title: info.title.clone(),
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
                broadcast_live(&ctx, &conf, &bili, &appearance, &info, &targets).await;
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
                            title: info.title.clone(),
                            started_at: previous.started_at,
                            closed: true,
                        },
                    );
                });
                announce_close(&ctx, &bili, uid, &previous, &info, &targets).await;
            }
            (true, true) => {
                let Some(previous) = prev else { continue };
                crate::store().write(|data| {
                    data.live.insert(
                        uid.to_string(),
                        LiveState {
                            status: 1,
                            room_id: info.room_id,
                            title: info.title.clone(),
                            started_at: previous.started_at,
                            closed: false,
                        },
                    );
                });
                // 换标题就当一次开播重推（原插件那套 updateLive 的口径）
                if !previous.title.is_empty() && previous.title != info.title && !targets.is_empty()
                {
                    broadcast_live(&ctx, &conf, &bili, &appearance, &info, &targets).await;
                }
            }
            (false, false) => {}
        }
    }
}

/// 开播推送：素材只有头像与封面，画一张直播卡
async fn broadcast_live(
    ctx: &PluginContext,
    conf: &BiliConfig,
    bili: &Arc<Bili>,
    appearance: &ImageConfig,
    info: &LiveInfo,
    targets: &[(String, TargetState)],
) {
    let mut faces: Vec<String> = Vec::new();
    if !info.face.is_empty() {
        faces.push(info.face.clone());
    } else {
        // batch 接口有时不给头像，补一次用户名片
        if let Ok(brief) = bili.user(info.uid).await {
            faces.push(brief.face);
        }
    }
    let material = gather(
        bili,
        &faces,
        std::slice::from_ref(&info.cover),
        &[],
        conf.cache.download_original,
    )
    .await;
    let link = info.link();
    let vals = Vals::live(info, link.clone(), &format_time(now()));
    let mut drawn: HashMap<String, Option<Vec<u8>>> = HashMap::new();
    for (key, state) in targets {
        if !wants(state, info.uid, "直播", &info.title) {
            continue;
        }
        let color = state.color.clone().unwrap_or_default();
        let card = painted(
            &mut drawn,
            card_key(
                "live",
                &info.room_id.to_string(),
                &color,
                &link,
                None,
                appearance,
            ),
            false,
            || paint_live(appearance, &color, info, &material, &link),
        );
        let post = Post::live(vals.clone(), card, material.bytes.get(&info.cover).cloned());
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
    let vals = Vals::live_close(&live, live.link(), previous.started_at, now());
    let post = Post::live_close(vals);
    for (key, state) in targets {
        if state.muted {
            continue;
        }
        push::push_state(ctx, key, state, &post).await;
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
        if seen.as_deref() == Some(season.newest_title.as_str()) {
            continue;
        }
        let first = seen.is_none();
        crate::store().write(|data| {
            data.seasons
                .insert(raw.clone(), season.newest_title.clone());
        });
        if first {
            // 刚订阅：只记当前集，不把"已更新到第 N 集"当新集推一条
            continue;
        }
        let material = gather(
            &bili,
            &[],
            std::slice::from_ref(&season.cover),
            &[],
            conf.cache.download_original,
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

/// 绘图用的素材包：解码后的图给画卡，原始字节给 `{images}` 直接发
struct Material {
    art: Art,
    bytes: HashMap<String, Vec<u8>>,
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
fn emoji_pairs(dynamic: &Dynamic) -> Vec<(String, String)> {
    let mut lists: Vec<&Vec<Node>> = vec![&dynamic.nodes];
    if let Some(old) = &dynamic.origin {
        lists.push(&old.nodes);
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
    out
}

/// 逐个下素材：单张失败只跳过那张，不拖垮整条推送（缺图的样子画卡里自会体现）
async fn gather(
    bili: &Arc<Bili>,
    faces: &[String],
    pictures: &[String],
    emojis: &[(String, String)],
    original: bool,
) -> Material {
    let mut art = Art::default();
    let mut bytes: HashMap<String, Vec<u8>> = HashMap::new();
    for url in faces {
        if let Some((raw, image)) = fetch(bili, url, cache::USER, original).await {
            bytes.insert(url.clone(), raw);
            art.faces.insert(url.clone(), image);
        }
    }
    for url in pictures {
        if let Some((raw, image)) = fetch(bili, url, cache::IMAGES, original).await {
            bytes.insert(url.clone(), raw);
            art.pictures.insert(url.clone(), image);
        }
    }
    // 表情只画进卡片，不作为附件发出去，所以只要解码后的那一份
    let mut set = HashMap::new();
    for (name, url) in emojis {
        if let Some((_, image)) = fetch(bili, url, cache::EMOJI, true).await {
            set.insert(name.clone(), image);
        }
    }
    art.emojis = EmojiSet::new(set);
    Material { art, bytes }
}

/// 取一张图。`downloadOriginal=false` 时先问图床要缩放过的 webp，拿不到再回退原图。
/// 盘上那份缓存在最前面：一条动态九张图、几十个目标，每轮都从图床重拉一遍既慢又容易撞限流。
async fn fetch(
    bili: &Arc<Bili>,
    url: &str,
    kind: &str,
    original: bool,
) -> Option<(Vec<u8>, image::RgbaImage)> {
    if let Some(raw) = cache::read(kind, url)
        && let Ok(decoded) = image::load_from_memory(&raw)
    {
        return Some((raw, decoded.to_rgba8()));
    }
    let attempts: Vec<String> = if original || url.contains('@') || !is_image_host(url) {
        vec![url.to_string()]
    } else {
        vec![format!("{url}@720w.webp"), url.to_string()]
    };
    for attempt in attempts {
        if let Ok(raw) = bili.bytes(&attempt, "https://www.bilibili.com/").await
            && let Ok(decoded) = image::load_from_memory(&raw)
        {
            cache::write(kind, url, &raw);
            return Some((raw, decoded.to_rgba8()));
        }
    }
    None
}

/// 只有 B 站图床认 `@720w.webp` 那套缩放语法
fn is_image_host(url: &str) -> bool {
    url.starts_with("https://i0.hdslb.com/") || url.starts_with("https://i1.hdslb.com/")
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
    drawn: &mut HashMap<String, Option<Vec<u8>>>,
    key: String,
    sticky: bool,
    paint: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    if let Some(hit) = drawn.get(&key) {
        return hit.clone();
    }
    let found = match cache::read(cache::DRAW, &key) {
        Some(raw) if sticky => Some(raw),
        _ => {
            let made = paint();
            if sticky && let Some(png) = &made {
                cache::write(cache::DRAW, &key, png);
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
    let digest = format!(
        "{:x}",
        md5::compute(format!("{appearance:?}|{link}|{seen}"))
    );
    let stamp = &digest[..8];
    format!("{bucket}/{id}/{color}#{stamp}")
}

/// 画一张动态卡。外观坏了（字体找不到之类）记一条日志然后按无卡推。
fn paint_dynamic(
    appearance: &ImageConfig,
    color: &str,
    dynamic: &Dynamic,
    material: &Material,
    link: &str,
    translation: Option<&Rendered>,
) -> Option<Vec<u8>> {
    let style = match style_for(
        appearance,
        color,
        link,
        &appearance.footer.dynamic_footer,
        true,
    ) {
        Ok(style) => style,
        Err(reason) => {
            arona::runtime::log::warning(format!(
                "bilibili：动态卡画不出来（{reason}），这条只发文本"
            ));
            return None;
        }
    };
    Some(dynamic_card(dynamic, &material.art, &style, translation))
}

/// 画一张直播卡
fn paint_live(
    appearance: &ImageConfig,
    color: &str,
    live: &LiveInfo,
    material: &Material,
    link: &str,
) -> Option<Vec<u8>> {
    let style = match style_for(
        appearance,
        color,
        link,
        &appearance.footer.live_footer,
        false,
    ) {
        Ok(style) => style,
        Err(reason) => {
            arona::runtime::log::warning(format!(
                "bilibili：直播卡画不出来（{reason}），这条只发文本"
            ));
            return None;
        }
    };
    Some(live_card(live, &material.art, &style))
}

/// 一份外观参数 → 画卡要的全部东西
fn style_for(
    appearance: &ImageConfig,
    color: &str,
    link: &str,
    footer: &str,
    show_id: bool,
) -> Result<Style, String> {
    let generator = &appearance.color_generator;
    let stops = background(
        color,
        generator.hue_step as f32,
        generator.lock_sb,
        generator.saturation as f32,
        generator.brightness as f32,
    );
    Ok(Style {
        quality: Quality::find(&appearance.quality),
        theme: Theme::find(&appearance.theme),
        face: typeface(&appearance.font)?,
        background: stops,
        badge: appearance.badge_enable,
        show_id,
        footer: footer.to_string(),
        qr: if appearance.card_ornament.eq_ignore_ascii_case("qrcode") {
            Some(link.to_string())
        } else {
            None
        },
    })
}

/// 字体只发现一次，配置串换了才重新找
fn typeface(configured: &str) -> Result<Arc<Typeface>, String> {
    let slot = FONT.get_or_init(|| Mutex::new((String::new(), None)));
    let mut cache = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.1.is_none() || cache.0 != configured {
        cache.1 = Some(Typeface::discover(configured)?);
        cache.0 = configured.to_string();
    }
    cache
        .1
        .clone()
        .ok_or_else(|| "字体还没找到，检查 bili-image.font".to_string())
}

/// 绘图配置现取（热重载之后下一次检测就用新值）
fn image_of(ctx: &PluginContext) -> ImageConfig {
    ctx.config::<ImageConfig>(IMAGE_SECTION).get()
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
    let material = gather(
        bili,
        &face_urls(dynamic),
        &picture_urls(dynamic),
        &emoji_pairs(dynamic),
        conf.cache.download_original,
    )
    .await;
    let link = link_of(bili, conf.push.to_short_link, dynamic).await;
    if kind_of(dynamic) == "直播" {
        let live = dynamic.live.clone().unwrap_or_default();
        let vals = Vals::live(&live, link.clone(), &format_time(now()));
        let card = paint_live(&appearance, color, &live, &material, &link);
        return Post::live(vals, card, material.bytes.get(&live.cover).cloned());
    }
    let vals = Vals::dynamic(dynamic, link.clone());
    let translation = translate::translate(&conf.translate, &dynamic.text).await;
    let mut drawn: HashMap<String, Option<Vec<u8>>> = HashMap::new();
    let card = painted(
        &mut drawn,
        card_key(
            "dynamic",
            &dynamic.id.to_string(),
            color,
            &link,
            translation.as_ref(),
            &appearance,
        ),
        true,
        || {
            paint_dynamic(
                &appearance,
                color,
                dynamic,
                &material,
                &link,
                translation.as_ref(),
            )
        },
    );
    let images: Vec<Vec<u8>> = dynamic
        .images
        .iter()
        .filter_map(|url| material.bytes.get(url).cloned())
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
    let mut faces: Vec<String> = Vec::new();
    if info.face.is_empty() {
        // batch 接口有时不给头像，补一次用户名片
        if let Ok(brief) = bili.user(info.uid).await {
            faces.push(brief.face);
        }
    } else {
        faces.push(info.face.clone());
    }
    let material = gather(
        bili,
        &faces,
        std::slice::from_ref(&info.cover),
        &[],
        conf.cache.download_original,
    )
    .await;
    let link = info.link();
    let vals = Vals::live(info, link.clone(), &format_time(now()));
    let card = paint_live(&appearance, color, info, &material, &link);
    Post::live(vals, card, material.bytes.get(&info.cover).cloned())
}

/// 北京时间口径下的当前秒级时间戳（模板与状态都用它）
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 接口失败的汇报：`reportInterval` 为 0 就每次记，否则按间隔节流
fn report(check: &CheckConfig, message: &str) {
    let stamp = now() as u64;
    let last = LAST_REPORT.swap(stamp, Ordering::SeqCst);
    if check.report_interval == 0 || stamp.saturating_sub(last) >= check.report_interval {
        arona::runtime::log::warning(format!("bilibili：{message}"));
    }
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
            interval: 30,
            live_interval: 15,
            low_speed: low_speed.to_string(),
            report_interval: 0,
            timeout: 20,
        }
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
        match typeface("C:/does-not-exist/none.ttf") {
            Ok(_) => panic!("不存在的路径居然找到了字体"),
            Err(reason) => {
                assert!(
                    reason.contains("字体") || reason.contains("font"),
                    "{reason}"
                );
            }
        }
    }
}
