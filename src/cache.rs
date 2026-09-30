//! 图片缓存：按类别落盘、按天过期清理。
//!
//! 目录形状与原插件一致：`data/bilibili/image/<类别>/<md5>.<扩展名>`，
//! 类别名就是 `bili.cache.expires` 里那五个键（`DRAW`/`IMAGES`/`EMOJI`/`USER`/`OTHER`）。
//! 缓存的意义在网络而不是磁盘：一条动态九张图、一个群几十个目标，
//! 每轮检测都从 B 站图床重拉一遍既慢又容易被限流。
//!
//! 与原插件的一处不同，记进 README：
//! * 卡片画好之后也写进 `DRAW`，于是重复看同一条动态（`/bili search`、链接解析）
//!   不必重画一遍。原插件只缓存素材，不缓存成品卡。
//! * `OTHER` 这一路本插件没有对应素材（番剧封面、直播封面都归 `IMAGES`），
//!   配置项留着是为了和原插件的键一致，清不到东西也不报错。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// 画好的动态/直播卡片
pub(crate) const DRAW: &str = "DRAW";
/// 动态配图、各种封面
pub(crate) const IMAGES: &str = "IMAGES";
/// 正文里的表情（`[笑]` 那种）
pub(crate) const EMOJI: &str = "EMOJI";
/// UP 主头像
pub(crate) const USER: &str = "USER";

/// 一天有多少秒。`expires` 的单位是"天"，与原插件那份配置对齐
const DAY: u64 = 60 * 60 * 24;

fn dir(kind: &str) -> PathBuf {
    crate::image_dir().join(kind)
}

/// 一张图在盘上的位置。键里带斜杠、问号、中文都有，所以文件名只用哈希；
/// 扩展名从原始 URL 的尾巴上取，认不出来按 png 存（发出去都是位图，不影响显示）。
fn file(kind: &str, key: &str) -> PathBuf {
    let digest = format!("{:x}", md5::compute(key.as_bytes()));
    dir(kind).join(format!("{digest}.{}", extension(key)))
}

/// URL 尾巴上的扩展名，只认图片那几种
fn extension(key: &str) -> &'static str {
    let head = key.split(['?', '#']).next().unwrap_or_default();
    let name = head.rsplit('/').next().unwrap_or_default();
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => match ext.to_ascii_lowercase().as_str() {
            "png" => "png",
            "jpg" | "jpeg" => "jpg",
            "gif" => "gif",
            "webp" => "webp",
            "bmp" => "bmp",
            _ => "png",
        },
        _ => "png",
    }
}

/// 读缓存。没命中就 None，让调用方去下载。
pub(crate) fn read(kind: &str, key: &str) -> Option<Vec<u8>> {
    fs::read(file(kind, key)).ok()
}

/// 写缓存。写不进去不是错误——图已经拿到手了，照样能发出去，只记一条 debug。
pub(crate) fn write(kind: &str, key: &str, bytes: &[u8]) {
    let path = file(kind, key);
    if let Some(parent) = path.parent()
        && let Err(reason) = fs::create_dir_all(parent)
    {
        arona::runtime::log::debug(format!("bilibili：缓存目录开不出来（{reason}）"));
        return;
    }
    if let Err(reason) = fs::write(&path, bytes) {
        arona::runtime::log::debug(format!("bilibili：图没进缓存（{reason}）"));
    }
}

/// 按 `bili.cache.expires` 清过期：每个类别按自己的天数删修改时间够老的文件。
/// 天数写 0（或键不存在）表示这个类别不清。返回删掉的文件数。
pub(crate) fn clear(expires: &BTreeMap<String, u64>) -> usize {
    let mut removed = 0usize;
    for (kind, days) in expires {
        if *days == 0 {
            continue;
        }
        removed += wipe(&dir(kind), Duration::from_secs(days * DAY));
    }
    removed
}

/// 递归删。目录不存在直接算 0——首次启动时那五个目录本来就还没建。
fn wipe(dir: &Path, life: Duration) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            removed += wipe(&path, life);
        } else if let Ok(age) = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .map(|modified| now.duration_since(modified).unwrap_or(Duration::ZERO))
            && age > life
            && fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_become_one_file_each() {
        let left = file(IMAGES, "https://i0.hdslb.com/bfs/face/abc.jpg@720w.webp");
        let right = file(IMAGES, "https://i0.hdslb.com/bfs/face/def.jpg");
        assert_ne!(left, right);
        assert!(left.to_string_lossy().ends_with(".webp"));
        assert!(right.to_string_lossy().ends_with(".jpg"));
    }

    #[test]
    fn unknown_extensions_fall_back_to_png() {
        assert_eq!(extension("https://t.bilibili.com/123"), "png");
        assert_eq!(extension("https://face/1.gif?a=1"), "gif");
        assert_eq!(extension("https://face/1.exe"), "png");
    }

    #[test]
    fn a_zero_expiry_clears_nothing() {
        let mut expires = BTreeMap::new();
        expires.insert("DRAW".to_string(), 0u64);
        assert_eq!(clear(&expires), 0);
    }
}
