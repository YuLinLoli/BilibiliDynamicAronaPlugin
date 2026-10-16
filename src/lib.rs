//! bilibili-dynamic-arona-plugin —— B 站动态/直播订阅推送插件。
//!
//! 这是 mirai 插件 `Colter23/bilibili-dynamic-mirai-plugin` 的 Arona 重写版：
//! 功能面（订阅 UP 主动态与直播、番剧订阅、类型/正则双过滤器、@全体、分组、模板推送、
//! 卡片绘图、扫码登录、链接解析、机翻补译文、图片缓存清理、`/bili` 那一整棵子命令树）都对齐原插件，
//! 但**不依赖任何前置插件**——原插件要 chat-command 才能解析命令、要 mirai-skia-plugin 才能绘图，
//! 这里命令走框架的分发器，绘图用纯 Rust 的 image + fontdue 在插件里画完再交框架发图。
//!
//! 形态与所有 Arona 插件一样：本 crate 编译成 `bilibili_dynamic_plugin.dll`，用户在**启动前**
//! 把它放进框架运行目录的 `plugins/bilibili/`，框架启动时扫描、握手、装配。
//! 本插件没有 `main`，也不能独立运行——OneBot 连接、运行时、配置读写、发消息全部用框架的预制出口。
//!
//! 配置：`config/bilibili/arona.yml`（三块区，见 [`config`]）；
//! 运行数据：`data/bilibili/store.yml`（见 [`store`]）。

mod bili;
mod cache;
mod commands;
mod config;
mod draw;
mod link;
mod push;
/// 把订阅表交给框架的展示板（GUI 的群管理页读那张板子，框架不读本插件的 store.yml）
mod report;
mod store;
mod tasks;
mod translate;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use arona::plugin::{AronaPlugin, PluginContext, PluginMeta, PluginRegistrar};
use arona::runtime::config::Feature;

use crate::config::{IMAGE_SECTION, SECTION, TEMPLATE_SECTION};
use crate::store::Store;

/// 本插件的稳定 id：框架按它规定落盘位置（`plugins/bilibili/`、`config/bilibili/arona.yml`、
/// `data/bilibili/`），`disabled_plugins` 里也写它。改名等于换一份用户数据，别动。
pub(crate) const PLUGIN_ID: &str = "bilibili";

/// 内存探针：只在测试构建里编译，用来把"常驻/峰值/用完有没有回落"落成数字，见模块说明
#[cfg(test)]
mod memprobe;

/// 探针用的计数分配器。生产 dll 里不存在这一项，所以插件仍然用系统分配器。
#[cfg(test)]
#[global_allocator]
static MEMORY_PROBE: memprobe::Counting = memprobe::Counting;

/// 动态检测任务的登记名（同名任务会被直接替换，配置热重载后照新值重排）
pub(crate) const DYNAMIC_JOB: &str = "BiliDynamicCheck";
/// 直播检测任务
pub(crate) const LIVE_JOB: &str = "BiliLiveCheck";
/// 低频时段重排任务
pub(crate) const RESCHEDULE_JOB: &str = "BiliReschedule";
/// 图片缓存清理任务
pub(crate) const CACHE_JOB: &str = "BiliImageCacheClear";

/// 功能开关 key：命令与钩子的 `with_feature` 用它，GUI 功能页据此生成一行
pub(crate) const FEATURE: &str = "bilibili";

/// 本插件的数据目录（data/bilibili）
pub(crate) fn data_dir() -> PathBuf {
    arona::runtime::paths::plugin_data_dir(PLUGIN_ID)
}

/// 本插件的图片缓存目录（data/bilibili/image）
pub(crate) fn image_dir() -> PathBuf {
    arona::runtime::paths::plugin_image_dir(PLUGIN_ID)
}

/// 本插件的配置目录（config/bilibili：字体、随包资源都放这）
pub(crate) fn config_dir() -> PathBuf {
    arona::runtime::paths::plugin_config_dir(PLUGIN_ID)
}

/// 运行数据文件（data/bilibili/store.yml）
fn store_file() -> PathBuf {
    data_dir().join("store.yml")
}

/// 订阅关系那份状态。插件内单例：dll 只会被装载一次，命令与检测任务共用同一份，
/// 不跨 FFI 边界，所以不需要框架那套实例登记。
static STORE: OnceLock<Arc<Store>> = OnceLock::new();

/// 取状态库（首次调用时从盘上加载）
pub(crate) fn store() -> Arc<Store> {
    STORE
        .get_or_init(|| Arc::new(Store::open(store_file())))
        .clone()
}

/// B 站动态订阅插件。
#[derive(Default)]
pub struct BiliDynamicPlugin;

impl AronaPlugin for BiliDynamicPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta::new(
            PLUGIN_ID,
            "BilibiliDynamic",
            env!("CARGO_PKG_VERSION"),
            "B 站动态/直播订阅推送：订阅、分组、过滤器、模板、卡片绘图、扫码登录，全部自带命令不依赖前置插件",
        )
        .with_author("岚雨凛")
    }

    fn install(&self, reg: &PluginRegistrar) -> Result<(), String> {
        reg.feature(Feature {
            key: FEATURE,
            name: "B 站动态订阅",
            description: "订阅 UP 主动态/直播/番剧，定时检测并按模板推送到群或好友",
        });
        reg.config::<config::BiliConfig>(SECTION);
        reg.config::<config::ImageConfig>(IMAGE_SECTION);
        reg.config::<config::TemplateConfig>(TEMPLATE_SECTION);
        Ok(())
    }

    fn configure(&self, ctx: &PluginContext) -> Result<(), String> {
        commands::register(ctx);
        link::register(ctx);
        Ok(())
    }

    fn start(&self, ctx: &PluginContext) -> Result<(), String> {
        store();
        report::attach(ctx);
        tasks::schedule(ctx);
        Ok(())
    }

    /// 配置热重载：检测间隔、低频时段、绘图外观都可能变了，按新值重排任务
    fn on_config_reload(&self, ctx: &PluginContext) {
        // 上下文每轮都不一样（装载器可能换了实例），展示板的上报口跟着重挂一次并补报
        report::attach(ctx);
        tasks::schedule(ctx);
    }
}

arona::export_arona_plugin!(BiliDynamicPlugin);
