//! B 站接口层。
//!
//! 出站一律走 [`arona::runtime::http`]：插件 dll 里自己链接一份 reqwest 会把 hyper 的后台任务
//! 投到插件那份没有 reactor 上下文的 tokio 上，一发就炸（框架文档 §14.1）。这里只递请求描述，
//! 真正发请求的是宿主那份客户端。
//!
//! B 站的错误分两层：HTTP 层非 2xx 由框架归成 `Err`；业务层恒返回 200，靠 `code` 字段判，
//! 所以 [`call`] 里还要再拆一次信封。`-101` 是登录态失效，文案直接告诉用户去 `/bili login`。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use arona::runtime::http::{self, Method, Request};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::bili::model::{
    Dynamic, LiveInfo, Season, UserBrief, parse_feed, parse_live_list, parse_live_status,
    parse_master, parse_room, parse_season, parse_user, parse_wbi_img,
};
use crate::bili::wbi;
use crate::config::AccountConfig;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

// 端点：与原插件 api/Api.kt 一一对应
const NAV: &str = "https://api.bilibili.com/x/web-interface/nav";
const FEED_ALL: &str = "https://api.bilibili.com/x/polymer/web-dynamic/v1/feed/all";
const FEED_SPACE: &str = "https://api.bilibili.com/x/polymer/web-dynamic/v1/feed/space";
const DYNAMIC_DETAIL: &str = "https://api.bilibili.com/x/polymer/web-dynamic/v1/detail";
const USER_CARD: &str = "https://api.bilibili.com/x/web-interface/card";
const LIVE_LIST: &str = "https://api.live.bilibili.com/xlive/web-ucenter/v1/xfetter/GetWebList";
const LIVE_STATUS_BATCH: &str =
    "https://api.live.bilibili.com/room/v1/Room/get_status_info_by_uids";
const ROOM_INFO: &str = "https://api.live.bilibili.com/room/v1/Room/get_info";
const LIVE_MASTER: &str = "https://api.live.bilibili.com/live_user/v1/Master/info";
const SEASON_INFO: &str = "https://api.bilibili.com/pgc/view/web/season";
const PGC_REVIEW: &str = "https://api.bilibili.com/pgc/review/user";
const FOLLOW_PGC: &str = "https://api.bilibili.com/pgc/web/follow/add";
const UNFOLLOW_PGC: &str = "https://api.bilibili.com/pgc/web/follow/del";
const RELATION: &str = "https://api.bilibili.com/x/relation";
const RELATION_MODIFY: &str = "https://api.bilibili.com/x/relation/modify";
const RELATION_TAGS: &str = "https://api.bilibili.com/x/relation/tags";
const RELATION_TAG_CREATE: &str = "https://api.bilibili.com/x/relation/tag/create";
const RELATION_TAG_ADD: &str = "https://api.bilibili.com/x/relation/tags/addUsers";
const SHORT_LINK: &str = "https://api.bilibili.com/x/share/click";
const QRCODE_GENERATE: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode/generate";
const QRCODE_POLL: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll";
const FINGER_SPI: &str = "https://api.bilibili.com/x/frontend/finger/spi";

/// WBI 密钥缓存：B 站一天换一次，跟着换就行
static MIXIN: std::sync::LazyLock<RwLock<Option<(String, i64)>>> =
    std::sync::LazyLock::new(|| RwLock::new(None));

/// 匿名设备凭证缓存：`buvid3`/`buvid4` 一次取到就长期有效，进程内问一次够了
static BUVID: std::sync::LazyLock<RwLock<Option<(String, String)>>> =
    std::sync::LazyLock::new(|| RwLock::new(None));

/// 一个 B 站账号的出口。克隆便宜（内部只握字符串）。
#[derive(Clone, Debug)]
pub struct Bili {
    account: AccountConfig,
    timeout: Duration,
}

impl Bili {
    pub fn new(account: AccountConfig, timeout_secs: u64) -> Bili {
        Bili {
            account,
            timeout: Duration::from_secs(timeout_secs.clamp(5, 120)),
        }
    }

    /// 有没有登录态：没登录也能拉单人的动态流，只是关注/分组/时间线用不了
    pub fn signed(&self) -> bool {
        self.account.signed()
    }

    pub fn uid(&self) -> i64 {
        self.account.uid
    }

    async fn headers(&self, referer: &str) -> Vec<(String, String)> {
        let mut headers = vec![
            ("user-agent".to_string(), USER_AGENT.to_string()),
            ("referer".to_string(), referer.to_string()),
        ];
        let cookie = self.cookie().await;
        if !cookie.is_empty() {
            headers.push(("cookie".to_string(), cookie));
        }
        headers
    }

    /// 发出去的 Cookie：配置里那份，前面补上匿名设备凭证。
    ///
    /// 必须带 `buvid3`。2026-09-30 实测：不带它时动态流直接 HTTP 412、nav 回 `-352 风控校验失败`，
    /// 看起来像"要登录"，其实缺的是这个匿名浏览器指纹；带上了未登录也能正常回话。
    /// 取不到指纹时不拦请求——手填 Cookie 的人本来就不靠 buvid。
    async fn cookie(&self) -> String {
        let (b3, b4) = self.buvid().await.unwrap_or_default();
        let configured = self.account.cookie.trim();
        let mut parts = Vec::new();
        if !b3.is_empty() && !configured.contains("buvid3") {
            parts.push(format!("buvid3={b3}"));
        }
        if !b4.is_empty() && !configured.contains("buvid4") {
            parts.push(format!("buvid4={b4}"));
        }
        if !configured.is_empty() {
            parts.push(configured.to_string());
        }
        parts.join("; ")
    }

    /// 匿名设备指纹：`x/frontend/finger/spi` 不要登录态，取到就缓存到进程结束
    async fn buvid(&self) -> Result<(String, String), String> {
        {
            let cache = BUVID.read().await;
            if let Some(pair) = cache.as_ref() {
                return Ok(pair.clone());
            }
        }
        // 这条请求自己拼头，不走 headers()——否则取指纹要先有指纹，绕成环
        let bytes = http::send(Request {
            method: Method::Get,
            url: FINGER_SPI.to_string(),
            headers: vec![
                ("user-agent".to_string(), USER_AGENT.to_string()),
                (
                    "referer".to_string(),
                    "https://www.bilibili.com/".to_string(),
                ),
            ],
            timeout: Some(self.timeout),
            ..Default::default()
        })
        .await?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|err| format!("指纹接口返回的不是 JSON（{err}）"))?;
        let data = value.get("data").cloned().unwrap_or(Value::Null);
        let read = |key: &str| {
            data.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let pair = (read("b_3"), read("b_4"));
        if pair.0.is_empty() {
            return Err("指纹接口没给 buvid3".to_string());
        }
        *BUVID.write().await = Some(pair.clone());
        Ok(pair)
    }

    /// GET 一个接口，拆掉 `{code,message,data}` 信封
    async fn call(
        &self,
        url: &str,
        params: &[(&str, &str)],
        referer: &str,
    ) -> Result<Value, String> {
        unwrap_envelope(self.call_raw(url, params, referer).await?)
    }

    /// GET 但不判 `code`，把整个信封原样交出去。
    ///
    /// 目前只有 nav 用得上：未登录时它回 `code -101`，可同一份响应里 `data.wbi_img` 照样带着，
    /// 而 WBI 密钥本来就是公开值。走 [`call`] 的话这条随错误返回的载荷会被当成失败丢掉，
    /// 于是所有签名接口在没有 Cookie 时都连密钥这一步都到不了（2026-09-30 curl 实测）。
    async fn call_raw(
        &self,
        url: &str,
        params: &[(&str, &str)],
        referer: &str,
    ) -> Result<Value, String> {
        self.send(Request {
            method: Method::Get,
            url: full_url(url, params),
            headers: self.headers(referer).await,
            timeout: Some(self.timeout),
            ..Default::default()
        })
        .await
    }

    /// 带 WBI 签名的 GET
    async fn call_wbi(
        &self,
        url: &str,
        params: &[(&str, &str)],
        referer: &str,
    ) -> Result<Value, String> {
        let mut raw = wbi::params(params);
        raw.insert("web_location".to_string(), "1550101".to_string());
        let mixin = self.mixin_key().await?;
        let wts = wbi::now_seconds();
        let (signed, w_rid) = wbi::sign(&raw, &mixin, wts);
        let mut all = signed;
        all.insert("w_rid".to_string(), w_rid);
        let query = wbi::encode_query(&all);
        let request = Request {
            method: Method::Get,
            url: format!("{url}?{query}"),
            headers: self.headers(referer).await,
            timeout: Some(self.timeout),
            ..Default::default()
        };
        unwrap_envelope(self.send(request).await?)
    }

    /// 表单 POST：改关系、生成短链都是这一路
    async fn post_form(
        &self,
        url: &str,
        pairs: &[(&str, &str)],
        referer: &str,
    ) -> Result<Value, String> {
        let form = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let request = Request {
            method: Method::Post,
            url: url.to_string(),
            headers: self.headers(referer).await,
            body: arona::runtime::http::Body::Form(form),
            timeout: Some(self.timeout),
        };
        unwrap_envelope(self.send(request).await?)
    }

    async fn post_json(&self, url: &str, json: &str, referer: &str) -> Result<Value, String> {
        let request = Request {
            method: Method::Post,
            url: url.to_string(),
            headers: self.headers(referer).await,
            body: arona::runtime::http::Body::Raw {
                content_type: "application/json".to_string(),
                text: json.to_string(),
            },
            timeout: Some(self.timeout),
        };
        unwrap_envelope(self.send(request).await?)
    }

    async fn send(&self, request: Request) -> Result<Value, String> {
        let bytes = http::send(request).await?;
        serde_json::from_slice(&bytes).map_err(|err| format!("B 站返回的不是 JSON（{err}）"))
    }

    /// 拿图片字节：动态封面、头像、用户主页背景都走这里
    pub async fn bytes(&self, url: &str, referer: &str) -> Result<Vec<u8>, String> {
        http::send(Request {
            method: Method::Get,
            url: url.to_string(),
            headers: self.headers(referer).await,
            timeout: Some(self.timeout),
            ..Default::default()
        })
        .await
    }

    /// WBI 密钥：优先读缓存，跨天才重新问一次 nav
    async fn mixin_key(&self) -> Result<String, String> {
        let today = wbi::now_seconds() / 86_400;
        {
            let cache = MIXIN.read().await;
            if let Some((key, day)) = cache.as_ref()
                && *day == today
            {
                return Ok(key.clone());
            }
        }
        let nav = self.call_raw(NAV, &[], "https://www.bilibili.com/").await?;
        let data = nav.get("data").cloned().unwrap_or(Value::Null);
        let (img, sub) = parse_wbi_img(&data).ok_or("nav 没返回 WBI 水印图，签不了名")?;
        let key = wbi::mixin_key(&img, &sub);
        *MIXIN.write().await = Some((key.clone(), today));
        Ok(key)
    }

    // ==================== 动态 ====================

    /// 本账号关注时间线。要登录态；一次拿回所有订阅者的新动态，是检测任务的主路径。
    pub async fn timeline(&self, page: u32, kind: &str) -> Result<Vec<Dynamic>, String> {
        let data = self
            .call(
                FEED_ALL,
                &[
                    ("timezone_offset", "-480"),
                    ("type", kind),
                    ("page", &page.to_string()),
                    ("features", "itemOpusStyle"),
                ],
                "https://t.bilibili.com/",
            )
            .await?;
        Ok(parse_feed(&data))
    }

    /// 单个 UP 主的动态流：没有登录态时的替代路径，也能拿置顶
    pub async fn space(&self, uid: i64, offset: &str) -> Result<Vec<Dynamic>, String> {
        let data = self
            .call_wbi(
                FEED_SPACE,
                &[
                    ("timezone_offset", "-480"),
                    ("host_mid", &uid.to_string()),
                    ("offset", offset),
                    ("features", "itemOpusStyle"),
                ],
                &format!("https://space.bilibili.com/{uid}/dynamic"),
            )
            .await?;
        Ok(parse_feed(&data))
    }

    /// 按动态 id 取一条（`/bili search <动态ID>` 用）
    pub async fn detail(&self, did: i64) -> Result<Option<Dynamic>, String> {
        let data = self
            .call(
                DYNAMIC_DETAIL,
                &[
                    ("timezone_offset", "-480"),
                    ("id", &did.to_string()),
                    ("features", "itemOpusStyle"),
                ],
                "https://t.bilibili.com/",
            )
            .await?;
        Ok(data.get("item").and_then(crate::bili::model::parse_item))
    }

    // ==================== 用户 / 直播 / 番剧 ====================

    /// 用户名片：昵称、头像、签名、等级、直播间号。
    ///
    /// 走 `x/web-interface/card`——不签名，带上匿名设备指纹就能取。以前走 `x/space/wbi/acc/info`，
    /// 那条要 WBI 签名，一轮链接解析里同秒连打就被判风控（2026-09-30 两次实测 `-352`），
    /// 匿名时等级还会掉成 Lv0。card 这一版不给房间号，那一格从主播资料接口补。
    pub async fn user(&self, uid: i64) -> Result<UserBrief, String> {
        let data = self
            .call(
                USER_CARD,
                &[("mid", &uid.to_string())],
                &format!("https://space.bilibili.com/{uid}"),
            )
            .await?;
        let mut brief = parse_user(&data).ok_or_else(|| format!("B 站没给 {uid} 的名片"))?;
        if brief.room_id == 0
            && let Ok(anchor) = self.master(uid).await
        {
            brief.room_id = anchor.room_id;
        }
        Ok(brief)
    }

    /// 关注列表里的直播间（要登录态）
    pub async fn live_list(&self, page: u32) -> Result<Vec<LiveInfo>, String> {
        let data = self
            .call(
                LIVE_LIST,
                &[("page", &page.to_string()), ("page_size", "20")],
                "https://link.bilibili.com/p/center/index",
            )
            .await?;
        Ok(parse_live_list(&data))
    }

    /// 批量查直播状态。这个是 POST + JSON，GET 那份 shape 早就不给全了。
    ///
    /// 请求体的键必须是 `uids`：写成 `uid_list` 时 B 站回 `code 1: invalid params`（2026-09-30 实测），
    /// 返回的 `data` 是以 uid 字符串为键的表。
    pub async fn live_status(&self, uids: &[i64]) -> Result<Vec<LiveInfo>, String> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::to_string(&BTreeMap::from([("uids", uids.to_vec())]))
            .map_err(|e| e.to_string())?;
        let data = self
            .post_json(LIVE_STATUS_BATCH, &body, "https://live.bilibili.com/")
            .await?;
        Ok(parse_live_status(&data))
    }

    /// 房间详情（直播结束通知补标题/分区、`/bili add <直播链接>` 画直播卡都走这里）
    pub async fn room(&self, room_id: i64) -> Result<LiveInfo, String> {
        let data = self
            .call(
                ROOM_INFO,
                &[("room_id", &room_id.to_string())],
                &format!("https://live.bilibili.com/{room_id}"),
            )
            .await?;
        let mut info = parse_room(&data, room_id);
        // 这个接口只给 uid，名字头像要再跑一次直播用户资料。取不到就留空：
        // 卡片退回画灰圆、模板那格空着，不值得为它让整条链接解析失败。
        if info.uid > 0
            && let Ok(anchor) = self.master(info.uid).await
        {
            info.anchor = anchor.name;
            info.face = anchor.face;
        }
        Ok(info)
    }

    /// 主播资料（`live_user/v1/Master/info`）：房间详情只给 uid，名字和头像从这儿补。
    ///
    /// 不用 [`user`]（名片接口）：那条要 WBI 签名，一轮链接解析里同秒连打两次就被判风控。
    /// 2026-09-30 实测两次 `wbi/acc/info?mid=11153765` 都只回 120 字节，直播卡的主播名因此一直是空的；
    /// 这个接口匿名可取、不签名，一次就回 `info.uname` + `info.face`。
    pub async fn master(&self, uid: i64) -> Result<UserBrief, String> {
        let data = self
            .call(
                LIVE_MASTER,
                &[("uid", &uid.to_string())],
                "https://live.bilibili.com/",
            )
            .await?;
        parse_master(&data).ok_or_else(|| format!("B 站没给 {uid} 的主播资料"))
    }

    /// 番剧季度信息（`/bili add ss11111` 用；season_id 由 ss 后面的数字给出）
    pub async fn season(&self, season_id: i64) -> Result<Season, String> {
        let data = self
            .call(
                SEASON_INFO,
                &[("season_id", &season_id.to_string())],
                "https://www.bilibili.com/bangumi/",
            )
            .await?;
        let result = data.get("result").unwrap_or(&data);
        parse_season(result).ok_or_else(|| format!("B 站没给番剧 {season_id} 的信息"))
    }

    /// `ep` 号那一路：同一个 season 接口换 `ep_id` 查，拿回来的就是季度信息
    pub async fn season_by_ep(&self, ep_id: i64) -> Result<Season, String> {
        let data = self
            .call(
                SEASON_INFO,
                &[("ep_id", &ep_id.to_string())],
                "https://www.bilibili.com/bangumi/",
            )
            .await?;
        // 这一路跟 `season()` 一样包在 `result` 里，直接递整份 data 会连 season_id 都取不到
        let result = data.get("result").unwrap_or(&data);
        parse_season(result).ok_or_else(|| format!("B 站没给剧集 {ep_id} 所属的番剧"))
    }

    /// `md` 号那一路：港澳台番剧在 `season` 表里查不到，只能从 review 接口按 media_id 换算
    pub async fn media(&self, media_id: i64) -> Result<Season, String> {
        let data = self
            .call(
                PGC_REVIEW,
                &[("media_id", &media_id.to_string())],
                "https://www.bilibili.com/bangumi/",
            )
            .await?;
        let media = data
            .get("media")
            .ok_or_else(|| format!("B 站没给媒体 {media_id} 的信息"))?;
        Ok(Season {
            season_id: media.get("season_id").and_then(Value::as_i64).unwrap_or(0),
            title: media
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            cover: media
                .get("cover")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            // 这份接口只给换算用的字段，最新集由检测任务再查一次 season 接口
            newest_ep: 0,
            newest_title: String::new(),
        })
    }

    /// 在 B 站那边也追上这部番。要有登录态；命令层即使它失败也只记日志，本地订阅照样成立。
    pub async fn follow_pgc(&self, season_id: i64) -> Result<(), String> {
        let csrf = self.csrf()?;
        self.post_form(
            FOLLOW_PGC,
            &[
                ("season_id", &season_id.to_string()),
                ("platform", "web"),
                ("csrf", &csrf),
            ],
            "https://www.bilibili.com/bangumi/",
        )
        .await?;
        Ok(())
    }

    pub async fn unfollow_pgc(&self, season_id: i64) -> Result<(), String> {
        let csrf = self.csrf()?;
        self.post_form(
            UNFOLLOW_PGC,
            &[
                ("season_id", &season_id.to_string()),
                ("platform", "web"),
                ("csrf", &csrf),
            ],
            "https://www.bilibili.com/bangumi/",
        )
        .await?;
        Ok(())
    }

    // ==================== 关系 ====================

    /// 关注关系：0 未关注 / 2 已关注 / 1 被粉丝 / 3 互粉
    pub async fn relation(&self, uid: i64) -> Result<i64, String> {
        let data = self
            .call(
                RELATION,
                &[("fid", &uid.to_string())],
                "https://space.bilibili.com/",
            )
            .await?;
        Ok(data.get("attribute").and_then(Value::as_i64).unwrap_or(0))
    }

    pub async fn follow(&self, uid: i64) -> Result<(), String> {
        let csrf = self.csrf()?;
        self.post_form(
            RELATION_MODIFY,
            &[
                ("fid", &uid.to_string()),
                ("act", "1"),
                ("re_src", "11"),
                ("csrf", &csrf),
            ],
            &format!("https://space.bilibili.com/{uid}"),
        )
        .await?;
        Ok(())
    }

    /// B 站分组列表
    pub async fn tags(&self) -> Result<Vec<(i64, String)>, String> {
        let data = self
            .call(
                RELATION_TAGS,
                &[("tag", "1")],
                "https://space.bilibili.com/",
            )
            .await?;
        Ok(data
            .get("list")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|item| {
                        let id = item.get("tagid").and_then(Value::as_i64)?;
                        let name = item.get("name").and_then(Value::as_str)?.to_string();
                        Some((id, name))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn create_tag(&self, name: &str) -> Result<(), String> {
        let csrf = self.csrf()?;
        self.post_form(
            RELATION_TAG_CREATE,
            &[("tag", name), ("csrf", &csrf)],
            "https://space.bilibili.com/",
        )
        .await?;
        Ok(())
    }

    /// 把人归进 B 站分组
    pub async fn add_to_tag(&self, uid: i64, tag_id: i64) -> Result<(), String> {
        let csrf = self.csrf()?;
        self.post_form(
            RELATION_TAG_ADD,
            &[
                ("fids", &uid.to_string()),
                ("tagids", &tag_id.to_string()),
                ("csrf", &csrf),
            ],
            "https://space.bilibili.com/",
        )
        .await?;
        Ok(())
    }

    fn csrf(&self) -> Result<String, String> {
        self.account.csrf_value().ok_or_else(|| {
            "改关系要有 bili_jct：在配置里填 cookie（含 bili_jct=…），或用 /bili login 扫码登录"
                .to_string()
        })
    }

    // ==================== 其它 ====================

    /// 长链换短链。失败就返回 None，调用方退回长链——推的东西不能因为短链挂了就没链接。
    pub async fn short_link(
        &self,
        oid: &str,
        share_id: &str,
        origin: Option<&str>,
    ) -> Option<String> {
        // 分享接口不校验这一位（原插件就写死 abcdefg），但既然手上有真指纹就没必要递假值
        let buvid = {
            let mine = self.buvid().await.unwrap_or_default().0;
            if mine.is_empty() {
                "abcdefg".to_string()
            } else {
                mine
            }
        };
        let mut pairs: Vec<(String, String)> = vec![
            ("build".to_string(), "6880300".to_string()),
            ("buvid".to_string(), buvid),
            ("platform".to_string(), "android".to_string()),
            ("oid".to_string(), oid.to_string()),
            ("share_channel".to_string(), "QQ".to_string()),
            ("share_id".to_string(), share_id.to_string()),
            ("share_mode".to_string(), "3".to_string()),
        ];
        if let Some(origin) = origin {
            pairs.push(("share_origin".to_string(), origin.to_string()));
        }
        let request = Request {
            method: Method::Post,
            url: SHORT_LINK.to_string(),
            headers: self.headers("https://www.bilibili.com/").await,
            body: arona::runtime::http::Body::Form(pairs),
            timeout: Some(self.timeout),
        };
        let value = self.send(request).await.ok()?;
        unwrap_envelope(value)
            .ok()?
            .get("link")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    // ==================== 扫码登录 ====================

    /// 生成登录二维码：返回 (待编码的内容, qrcode_key)
    pub async fn qrcode(&self) -> Result<(String, String), String> {
        let data = self
            .call(QRCODE_GENERATE, &[], "https://passport.bilibili.com/login")
            .await?;
        Ok((
            data.get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            data.get("qrcode_key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ))
    }

    /// 轮询扫码结果。返回 (状态码, 提示, 拿到手的 cookie)
    pub async fn poll(&self, key: &str) -> Result<(i64, String, Option<String>), String> {
        let data = self
            .call(
                QRCODE_POLL,
                &[("qrcode_key", key)],
                "https://passport.bilibili.com/login",
            )
            .await?;
        let code = data.get("code").and_then(Value::as_i64).unwrap_or(-1);
        let message = data
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let cookie = match code {
            0 => cookie_from_url(data.get("url").and_then(Value::as_str).unwrap_or_default()),
            _ => None,
        };
        Ok((code, message, cookie))
    }
}

/// 从登录跳转 URL 的 query 里捞 SESSDATA / bili_jct / DedeUserID 拼成 cookie 串
fn cookie_from_url(url: &str) -> Option<String> {
    let query = url.split_once('?').map(|(_, rest)| rest).unwrap_or(url);
    let mut picked = BTreeMap::new();
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=')
            && ["SESSDATA", "bili_jct", "DedeUserID", "sid"].contains(&key)
        {
            picked.insert(
                key.to_string(),
                percent_encoding::percent_decode_str(value)
                    .decode_utf8_lossy()
                    .into_owned(),
            );
        }
    }
    if picked.contains_key("SESSDATA") {
        Some(
            picked
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    } else {
        None
    }
}

/// B 站的业务信封：code != 0 一律算失败，-101 单独给一句能照着做的话
fn unwrap_envelope(value: Value) -> Result<Value, String> {
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code == 0 {
        // 多数接口把载荷放在 `data`，pgc 那一批放在 `result`，两种都当成"拆掉信封"
        return Ok(value
            .get("data")
            .or_else(|| value.get("result"))
            .cloned()
            .unwrap_or(Value::Null));
    }
    let message = value
        .get("message")
        .or_else(|| value.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("无提示");
    if code == -101 || code == -30 {
        return Err(format!(
            "账号登录失效（code {code}），请用 /bili login 重新扫码登录"
        ));
    }
    if code == -403 || code == -352 {
        return Err(format!(
            "被 B 站风控拦了（code {code}: {message}），把检测间隔调大一些再试"
        ));
    }
    Err(format!("B 站返回 code {code}: {message}"))
}

/// 拼查询串。值为空也照样发出去（`offset=` 在 B 站表示首页，参数不能缺席）
fn full_url(url: &str, params: &[(&str, &str)]) -> String {
    let query: Vec<String> = params
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect();
    if query.is_empty() {
        url.to_string()
    } else {
        format!("{url}?{}", query.join("&"))
    }
}

fn encode(value: &str) -> String {
    // URI_COMPONENT 不编码 `-._~`，与 JS 的 encodeURIComponent 同口径。
    // 换成 NON_ALPHANUMERIC 会把 `host_mid` 发成 `host%5Fmid`，参数名就和签名时算的那串对不上了。
    percent_encoding::utf8_percent_encode(value, crate::bili::URI_COMPONENT).to_string()
}

/// 给命令与任务用的便捷构造：从三块配置里拼出一个出口
pub fn client(account: AccountConfig, timeout_secs: u64) -> Arc<Bili> {
    Arc::new(Bili::new(with_cookie_file(account), timeout_secs))
}

/// 原插件启动时会读 `dataFolder/cookies.json`——浏览器扩展 EditThisCookie 导出的那一份
/// （`Init.kt:24-36`），并且**文件优先**：拼得出登录态就不用 `accountConfig.cookie`。
/// 挂在这唯一的构造口上，任务与命令两条路径都吃到，热重载之后下一轮也会重读。
fn with_cookie_file(mut account: AccountConfig) -> AccountConfig {
    let path = crate::data_dir().join("cookies.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return account;
    };
    let found: Vec<Value> = match serde_json::from_str(&raw) {
        Ok(list) => list,
        Err(reason) => {
            warn_cookie(
                modified(&path),
                format!("bilibili：cookies.json 读不出来（{reason}），这一轮仍用配置里的 cookie"),
            );
            return account;
        }
    };
    let Some(cookie) = cookie_from_editthis(&found) else {
        warn_cookie(
            modified(&path),
            "bilibili：cookies.json 里缺少 SESSDATA 或 bili_jct，这一轮仍用配置里的 cookie".into(),
        );
        return account;
    };
    account.cookie = cookie;
    account
}

/// 文件的修改时间（拿不到就记 0）
fn modified(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// 同一份坏文件只提醒一次：检测每 15 秒就要构造一个出口，文件没换过就别把日志刷满
fn warn_cookie(stamp: u64, reason: String) {
    if COOKIE_WARNED_ON.swap(stamp, Ordering::SeqCst) != stamp {
        arona::runtime::log::warning(reason);
    }
}

/// 上次提醒针对的是哪一份文件（初值取不可能出现的时间戳，保证头一次一定提醒）
static COOKIE_WARNED_ON: AtomicU64 = AtomicU64::new(u64::MAX);

/// EditThisCookie 那份数组 → 能直接发出去的 Cookie 串。只取 SESSDATA 与 bili_jct 两项
/// （原插件的 `toCookie()` 也只要这两项，`BiliCookie.kt:60`），缺任何一项就不算登录态。
fn cookie_from_editthis(items: &[Value]) -> Option<String> {
    let pick = |name: &str| {
        items
            .iter()
            .find(|item| item.get("name").and_then(Value::as_str) == Some(name))
            .and_then(|item| item.get("value").and_then(Value::as_str))
            .filter(|value| !value.is_empty())
    };
    let sess = pick("SESSDATA")?;
    let jct = pick("bili_jct")?;
    // 扩展导出的是解码过的值，发回 B 站要原样那串：SESSDATA 里的 `,` 与 `*` 得还原成
    // `%2C`/`%2A`（原插件同一对手工替换在 BiliCookie.kt:17）
    Some(format!(
        "SESSDATA={}; bili_jct={}",
        sess.replace(',', "%2C").replace('*', "%2A"),
        jct
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_passes_data_through() {
        let ok =
            unwrap_envelope(serde_json::json!({"code": 0, "message": "0", "data": {"mid": 1}}))
                .unwrap();
        assert_eq!(ok.get("mid").and_then(Value::as_i64), Some(1));
        // 有的接口没有 data，那也不能算错
        assert_eq!(
            unwrap_envelope(serde_json::json!({"code": 0, "ttl": 1})).unwrap(),
            Value::Null
        );
    }

    /// cookies.json（EditThisCookie 导出）只认 SESSDATA 与 bili_jct 两项，且要还原 `%2C`/`%2A`
    #[test]
    fn cookie_file_needs_both_keys_and_keeps_them_encoded() {
        let list: Vec<Value> = serde_json::from_str(
            r#"[{"name":"SESSDATA","value":"abc,def*gh"},{"name":"bili_jct","value":"jct"},{"name":"buvid3","value":"别的都不看"}]"#,
        )
        .unwrap();
        assert_eq!(
            cookie_from_editthis(&list).as_deref(),
            Some("SESSDATA=abc%2Cdef%2Agh; bili_jct=jct")
        );
        // 缺 bili_jct 不算登录态：宁可继续用配置里那串，也别发出去一份半残的 Cookie
        let partial: Vec<Value> =
            serde_json::from_str(r#"[{"name":"SESSDATA","value":"abc"}]"#).unwrap();
        assert_eq!(cookie_from_editthis(&partial), None);
        let blank: Vec<Value> = serde_json::from_str(
            r#"[{"name":"SESSDATA","value":""},{"name":"bili_jct","value":"jct"}]"#,
        )
        .unwrap();
        assert_eq!(cookie_from_editthis(&blank), None);
    }

    #[test]
    fn expired_login_tells_the_user_what_to_do() {
        let reason = unwrap_envelope(
            serde_json::json!({"code": -101, "message": "账号未登录", "data": null}),
        )
        .unwrap_err();
        assert!(reason.contains("/bili login"), "{reason}");
    }

    #[test]
    fn other_codes_keep_the_server_message() {
        let reason = unwrap_envelope(
            serde_json::json!({"code": -412, "message": "请求被拦截", "data": null}),
        )
        .unwrap_err();
        assert!(reason.contains("-412"), "{reason}");
        assert!(reason.contains("请求被拦截"), "{reason}");
    }

    #[test]
    fn login_cookie_is_mined_from_the_redirect_url() {
        let cookie = cookie_from_url(
            "https://www.bilibili.com/?DedeUserID=12345&SESSDATA=abc%2C1700000000%2C01b34&bili_jct=def&mid=12345&new_hint=1",
        )
        .unwrap();
        assert!(cookie.contains("SESSDATA=abc,1700000000,01b34"), "{cookie}");
        assert!(cookie.contains("bili_jct=def"), "{cookie}");
        assert!(cookie.contains("DedeUserID=12345"), "{cookie}");
        // 没带 SESSDATA 就等于没登录成，不能装作拿到了
        assert!(cookie_from_url("https://www.bilibili.com/?mid=1").is_none());
        assert!(cookie_from_url("").is_none());
    }

    #[test]
    fn csrf_needs_a_real_value() {
        let anonymous = Bili::new(AccountConfig::default(), 20);
        assert!(anonymous.csrf().is_err());
        assert!(!anonymous.signed());
        let account = AccountConfig {
            cookie: "SESSDATA=s; bili_jct=j".into(),
            ..Default::default()
        };
        let signed = Bili::new(account, 20);
        assert_eq!(signed.csrf().unwrap(), "j");
        assert!(signed.signed());
    }

    #[test]
    fn query_values_get_encoded() {
        assert_eq!(encode("你好"), "%E4%BD%A0%E5%A5%BD");
        assert_eq!(encode("a b&c=d"), "a%20b%26c%3Dd");
        // 参数名里的下划线必须原样发出去，否则 B 站收到的 key 与签名时算的不是同一个
        assert_eq!(encode("host_mid"), "host_mid");
        assert_eq!(encode("timezone_offset"), "timezone_offset");
    }

    /// nav 未登录那份响应：`code` 是 `-101`，可 `data.wbi_img` 就在同一份包里。
    /// WBI 密钥是公开值，签名这一步不该要登录态——这条钉住它，哪天有人把 `mixin_key`
    /// 改回走 `call()`（非零 code 一律判失败），这里就会挂。
    #[test]
    fn wbi_keys_survive_the_not_logged_in_envelope() {
        let envelope = serde_json::json!({
            "code": -101, "message": "账号未登录", "ttl": 1,
            "data": {"isLogin": false, "wbi_img": {
                "img_url": "https://i0.hdslb.com/bfs/wbi/7cd71926bcd147c6b5e90b611c0b8d31.png",
                "sub_url": "https://i0.hdslb.com/bfs/wbi/594527442bcf4e6b826b1606987ac484.png"
            }}
        });
        assert!(
            unwrap_envelope(envelope.clone()).is_err(),
            "信封本身仍判为未登录"
        );
        let data = envelope.get("data").cloned().unwrap();
        let (img, sub) = parse_wbi_img(&data).expect("未登录的 nav 也该给出水印图");
        assert_eq!(wbi::mixin_key(&img, &sub).chars().count(), 32);
    }

    /// 真联网：只在手动跑的时候验，避免 CI 与日常 `cargo test` 依赖外网。
    ///
    /// 匿名能拿到什么，B 站一年改好几回，所以这里只对"必然该成立"的两件事下手（指纹可取、
    /// 水印图拼得出 32 位密钥），动态流和名片只打印结果不定死方向。
    ///
    /// 上一版这里钉的是"匿名一定拿不到水印图"，那是个误判：`nav` 未登录时回 `-101`，
    /// 可同一份响应里 `data.wbi_img` 照样带着，是我们拆信封的分支把它丢了，
    /// 于是把自己的实现缺陷钉成了"B 站接口如此"（2026-09-30 curl 实测）。
    #[test]
    #[ignore = "要联网访问 B 站"]
    fn anonymous_access_reports_its_real_verdict() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let bili = Bili::new(AccountConfig::default(), 20);
            let (b3, b4) = bili.buvid().await.expect("指纹接口匿名可取");
            assert!(!b3.is_empty() && !b4.is_empty(), "两个指纹都得有值");

            let mixin = bili.mixin_key().await.expect("nav 该给出水印图");
            assert_eq!(mixin.chars().count(), 32, "两张图名重排后取前 32 位当密钥");

            let feed = bili.space(546195, "").await;
            match &feed {
                Ok(list) => println!("动态流：匿名可拉，{} 条", list.len()),
                Err(reason) => println!("动态流：匿名被拒，{reason}"),
            }
            match bili.user(546195).await {
                Ok(user) => println!(
                    "名片：匿名可拉，昵称「{}」签名 {} 字，Lv{}，房间 {}",
                    user.name,
                    user.sign.chars().count(),
                    user.level,
                    user.room_id
                ),
                Err(reason) => println!("名片：匿名被拒，{reason}"),
            }
            match bili.timeline(1, "ALL").await {
                Ok(list) => println!("关注时间线：匿名可拉，{} 条", list.len()),
                Err(reason) => println!("关注时间线：匿名被拒，{reason}"),
            }
            // 开播检测、直播链接解析、单条动态这三条只读路径也一并记录，别只测动态流
            match bili
                .live_status(&[546195, 168887495, 209895665, 651852799])
                .await
            {
                Ok(list) => {
                    println!("开播状态：匿名可拉，{} 间", list.len());
                    for live in &list {
                        println!(
                            "  uid {} 房 {} 状态 {} 封面{}\
                             开播时间 {} 标题「{}」",
                            live.uid,
                            live.room_id,
                            live.status,
                            if live.cover.is_empty() { "缺" } else { "有" },
                            live.live_start,
                            live.title
                        );
                    }
                }
                Err(reason) => println!("开播状态：匿名被拒，{reason}"),
            }
            match bili.room(23058).await {
                Ok(live) => println!(
                    "房间：匿名可拉，主播「{}」头像 {} 字，标题「{}」分区「{}」状态 {} 开播 {}",
                    live.anchor,
                    live.face.chars().count(),
                    live.title,
                    live.area,
                    live.status,
                    live.started_text()
                ),
                Err(reason) => println!("房间：匿名被拒，{reason}"),
            }
            // 单条动态拿刚拉到的真实 id 去查，编造的 id 只会测到 B 站的报错分支
            match feed.as_ref().ok().and_then(|list| list.first()) {
                Some(first) => match bili.detail(first.id).await {
                    Ok(Some(dy)) => {
                        println!(
                            "单条动态：匿名可拉，{} 字，{} 张图",
                            dy.text.chars().count(),
                            dy.images.len()
                        );
                    }
                    Ok(None) => println!("单条动态：匿名可拉但这条查不到 id {}", first.id),
                    Err(reason) => println!("单条动态：匿名被拒，{reason}"),
                },
                None => println!("单条动态：没拿到可查的 id，跳过"),
            }
        });
    }

    /// 真联网的一次形状核对：动态流里那条直播推荐把房间信息放在哪一层、`parse_feed` 认不认。
    /// 只打印不定死方向（B 站一年改好几回），但 2026-09-30 就是靠它抓出
    /// 「feed 这份包里 `major` 的兄弟键全是 null 占位」这一条，
    /// 离线 fixture 只照 `detail` 写、照不出这个差别。
    #[test]
    #[ignore = "要联网访问 B 站"]
    fn dump_the_live_item_shape_from_the_space_feed() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let bili = Bili::new(AccountConfig::default(), 20);
            let data = bili
                .call_wbi(
                    FEED_SPACE,
                    &[
                        ("timezone_offset", "-480"),
                        ("host_mid", "546195"),
                        ("offset", ""),
                        ("features", "itemOpusStyle"),
                    ],
                    "https://space.bilibili.com/546195/dynamic",
                )
                .await
                .expect("动态流匿名可拉");
            let items = data
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            println!("动态流原始 {} 条", items.len());
            for item in &items {
                let kind = item.get("type").and_then(Value::as_str).unwrap_or("?");
                if !kind.contains("LIVE") {
                    continue;
                }
                println!(
                    "--- {kind} id {}",
                    item.get("id_str").and_then(Value::as_str).unwrap_or("?")
                );
                // module_author 那一大坨头像图层先把输出挤满，只看动态正文这一层
                match item.pointer("/modules/module_dynamic") {
                    Some(module) => {
                        println!(
                            "module_dynamic 键：{:?}",
                            module
                                .as_object()
                                .map(|m| m.keys().cloned().collect::<Vec<_>>())
                        );
                        let dump = serde_json::to_string_pretty(module).unwrap_or_default();
                        println!("{}", dump.chars().take(2600).collect::<String>());
                    }
                    None => println!("这条没有 module_dynamic"),
                }
            }
            for dynamic in parse_feed(&data) {
                if dynamic.kind == "直播" {
                    println!("解析结果 id {} live {:?}", dynamic.id, dynamic.live);
                }
            }
        });
    }
}
