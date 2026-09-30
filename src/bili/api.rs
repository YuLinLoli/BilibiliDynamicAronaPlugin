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
use std::time::Duration;

use arona::runtime::http::{self, Method, Request};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::bili::model::{
    Dynamic, LiveInfo, Season, UserBrief, parse_feed, parse_live_list, parse_live_status,
    parse_season, parse_user, parse_wbi_img,
};
use crate::bili::wbi;
use crate::config::AccountConfig;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

// 端点：与原插件 api/Api.kt 一一对应
const NAV: &str = "https://api.bilibili.com/x/web-interface/nav";
const FEED_ALL: &str = "https://api.bilibili.com/x/polymer/web-dynamic/v1/feed/all";
const FEED_SPACE: &str = "https://api.bilibili.com/x/polymer/web-dynamic/v1/feed/space";
const DYNAMIC_DETAIL: &str = "https://api.bilibili.com/x/polymer/web-dynamic/v1/detail";
const USER_INFO_WBI: &str = "https://api.bilibili.com/x/space/wbi/acc/info";
const LIVE_LIST: &str = "https://api.live.bilibili.com/xlive/web-ucenter/v1/xfetter/GetWebList";
const LIVE_STATUS_BATCH: &str =
    "https://api.live.bilibili.com/room/v1/Room/get_status_info_by_uids";
const ROOM_INFO: &str = "https://api.live.bilibili.com/room/v1/Room/get_info";
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

/// WBI 密钥缓存：B 站一天换一次，跟着换就行
static MIXIN: std::sync::LazyLock<RwLock<Option<(String, i64)>>> =
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

    fn headers(&self, referer: &str) -> Vec<(String, String)> {
        let mut headers = vec![
            ("user-agent".to_string(), USER_AGENT.to_string()),
            ("referer".to_string(), referer.to_string()),
        ];
        if !self.account.cookie.is_empty() {
            headers.push(("cookie".to_string(), self.account.cookie.clone()));
        }
        headers
    }

    /// GET 一个接口，拆掉 `{code,message,data}` 信封
    async fn call(
        &self,
        url: &str,
        params: &[(&str, &str)],
        referer: &str,
    ) -> Result<Value, String> {
        let mut query: Vec<String> = params
            .iter()
            .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
            .collect();
        query.retain(|item| !item.is_empty());
        let full = if query.is_empty() {
            url.to_string()
        } else {
            format!("{url}?{}", query.join("&"))
        };
        let request = Request {
            method: Method::Get,
            url: full,
            headers: self.headers(referer),
            timeout: Some(self.timeout),
            ..Default::default()
        };
        unwrap_envelope(self.send(request).await?)
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
            headers: self.headers(referer),
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
            headers: self.headers(referer),
            body: arona::runtime::http::Body::Form(form),
            timeout: Some(self.timeout),
        };
        unwrap_envelope(self.send(request).await?)
    }

    async fn post_json(&self, url: &str, json: &str, referer: &str) -> Result<Value, String> {
        let request = Request {
            method: Method::Post,
            url: url.to_string(),
            headers: self.headers(referer),
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
            headers: self.headers(referer),
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
        let nav = self.call(NAV, &[], "https://www.bilibili.com/").await?;
        let (img, sub) = parse_wbi_img(&nav).ok_or("nav 没返回 WBI 水印图，签不了名")?;
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

    pub async fn user(&self, uid: i64) -> Result<UserBrief, String> {
        let data = self
            .call_wbi(
                USER_INFO_WBI,
                &[
                    ("mid", &uid.to_string()),
                    ("token", ""),
                    ("platform", "web"),
                ],
                &format!("https://space.bilibili.com/{uid}"),
            )
            .await?;
        parse_user(&data).ok_or_else(|| format!("B 站没给 {uid} 的名片"))
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
    pub async fn live_status(&self, uids: &[i64]) -> Result<Vec<LiveInfo>, String> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::to_string(&BTreeMap::from([("uid_list", uids.to_vec())]))
            .map_err(|err| err.to_string())?;
        let data = self
            .post_json(LIVE_STATUS_BATCH, &body, "https://live.bilibili.com/")
            .await?;
        Ok(parse_live_status(&data))
    }

    /// 房间详情（直播结束通知补标题/分区用）
    pub async fn room(&self, room_id: i64) -> Result<LiveInfo, String> {
        let data = self
            .call(
                ROOM_INFO,
                &[("room_id", &room_id.to_string())],
                &format!("https://live.bilibili.com/{room_id}"),
            )
            .await?;
        Ok(LiveInfo {
            uid: data
                .get("uid")
                .and_then(Value::as_i64)
                .or_else(|| {
                    data.get("room_info")
                        .and_then(|v| v.get("uid"))
                        .and_then(Value::as_i64)
                })
                .unwrap_or_default(),
            room_id: data
                .get("roomid")
                .and_then(Value::as_i64)
                .unwrap_or(room_id),
            anchor: data
                .get("anchor_info")
                .and_then(|v| v.get("base_info"))
                .and_then(|v| v.get("uname"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            face: data
                .get("anchor_info")
                .and_then(|v| v.get("base_info"))
                .and_then(|v| v.get("face"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            area: data
                .get("area_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            title: data
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            cover: data
                .get("cover")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            online: data
                .get("online")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
            live_start: 0,
            status: data
                .get("live_status")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
        })
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
        parse_season(&data).ok_or_else(|| format!("B 站没给剧集 {ep_id} 所属的番剧"))
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
            // 这份接口只给换算用的字段，最新集数由检测任务再查一次 season 接口
            newest_title: String::new(),
            publish_text: String::new(),
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
        let mut pairs: Vec<(String, String)> = vec![
            ("build".to_string(), "6880300".to_string()),
            ("buvid".to_string(), "abcdefg".to_string()),
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
            headers: self.headers("https://www.bilibili.com/"),
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

fn encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// 给命令与任务用的便捷构造：从三块配置里拼出一个出口
pub fn client(account: AccountConfig, timeout_secs: u64) -> Arc<Bili> {
    Arc::new(Bili::new(account, timeout_secs))
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
    }

    /// 真联网：只在手动跑的时候验，避免 CI 与日常 `cargo test` 依赖外网
    #[test]
    #[ignore = "要联网访问 B 站"]
    fn nav_returns_wbi_keys() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let bili = Bili::new(AccountConfig::default(), 20);
            let mixin = bili.mixin_key().await.expect("匿名也能从 nav 拿到水印图");
            assert_eq!(mixin.chars().count(), 32);
            let user = bili.user(546195).await.expect("咬人酱的公开名片");
            assert!(!user.name.is_empty());
        });
    }

    #[test]
    #[ignore = "要联网访问 B 站"]
    fn space_feed_works_without_login() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let bili = Bili::new(AccountConfig::default(), 20);
            let feed = bili.space(546195, "").await.unwrap();
            assert!(!feed.is_empty(), "匿名拉单人动态应该有内容");
            assert!(feed.windows(2).all(|pair| pair[0].time <= pair[1].time));
        });
    }
}
