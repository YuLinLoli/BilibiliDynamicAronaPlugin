//! 机翻：给非中文正文补一段中文译文，画在卡片正文之后。
//!
//! 口径逐条抄原插件 `utils/translate/TransApi.kt`：
//! * 先把 `[笑]` `[群公告]` 这类中括号整段摘掉——它们是 B 站的表情占位与话题标记，
//!   喂给翻译接口只会污染结果，原插件那步循环替换成两个空格就是为这个。
//! * 判该不该译：**含假名就一定译**（中日文混写时"有没有汉字"判不出来），
//!   **整篇没汉字也译**（纯英文、纯韩文、纯假名都算）。两者都不满足就是中文原文，不占配额。
//! * 回包里的 `from` 是接口对原文语种判定，判成 `zh` 就丢掉——正文里夹了大段中文时接口会这么回。
//! * 只画在卡片上：原插件那一路 `trans(text)` 只喂给 `DynamicModuleDraw`，
//!   文本模板里的 `{content}` 从来不带译文，这里保持一致。

use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;

use arona::runtime::http::{self, Method, Request};

use crate::config::TranslateConfig;

/// 百度翻译给的 https 域名。原插件那条是 http——签名与 APP ID 都在查询串里，
/// 明文跑公网等于把它们摊给沿途每一跳看，这里换 https。
const API: &str = "https://fanyi-api.baidu.com/api/trans/vip/translate";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

/// 平假名、片假名（含半角假名与扩展区）、长音符与日文中点
static KANA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\u3040-\u30ff\u31f0-\u31ffー・]").unwrap());
/// CJK 统一表意文字：整篇一个都没有就认定不是中文
static HAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\u4e00-\u9fa5]").unwrap());
/// `[...]` 那一整段：表情占位、话题、系统标记
static BRACKET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[[^\]]*\]").unwrap());

/// 卡片正文后面追加的那两格。带着分割线一起走，绘图层不必再去读配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub cut_line: String,
    pub text: String,
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    from: Option<String>,
    #[serde(default, rename = "trans_result")]
    trans_result: Option<Vec<Item>>,
    #[serde(default, rename = "error_code")]
    error_code: Option<String>,
    #[serde(default, rename = "error_msg")]
    error_msg: Option<String>,
}

#[derive(Deserialize)]
struct Item {
    dst: String,
}

/// 该不该拿去译。返回摘掉中括号之后的查询串，不该译（或摘完就空了）时给 `None`。
pub fn query(text: &str) -> Option<String> {
    let stripped = BRACKET.replace_all(text, "  ").trim().to_string();
    if stripped.is_empty() {
        return None;
    }
    if KANA.is_match(&stripped) || !HAN.is_match(&stripped) {
        Some(stripped)
    } else {
        None
    }
}

/// 那句签名：`appid + 原文 + 盐 + 密钥` 连成一串取 MD5 小写。少拼一段就 401。
fn sign(app_id: &str, query: &str, salt: &str, security_key: &str) -> String {
    format!(
        "{:x}",
        md5::compute(format!("{app_id}{query}{salt}{security_key}"))
    )
}

/// 译一段正文。开关没开、密钥没填、原文本来就是中文、接口出错都回 `None`——
/// 翻译是锦上添花，任何一种失败都不该拦住这条动态。
pub async fn translate(conf: &TranslateConfig, text: &str) -> Option<Rendered> {
    if !conf.enable {
        return None;
    }
    // 密钥留空时原插件那句判定是 `SECURITY_KEY != ""`，APP_ID 一起看更省事
    if conf.app_id.is_empty() || conf.security_key.is_empty() {
        return None;
    }
    let query = query(text)?;
    let salt = chrono::Local::now().timestamp_millis().to_string();
    let url = format!(
        "{API}?q={}&from=auto&to=zh&appid={}&salt={}&sign={}",
        encode(&query),
        conf.app_id,
        salt,
        sign(&conf.app_id, &query, &salt, &conf.security_key)
    );
    let request = Request {
        method: Method::Get,
        url: url.clone(),
        headers: vec![("user-agent".to_string(), USER_AGENT.to_string())],
        body: Default::default(),
        timeout: Some(std::time::Duration::from_secs(10)),
        ..Default::default()
    };
    let raw = match http::send(request).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        Err(reason) => {
            arona::runtime::log::warning(format!("bilibili：翻译接口没答上（{reason}）"));
            return None;
        }
    };
    let reply: Reply = match serde_json::from_str(&raw) {
        Ok(reply) => reply,
        Err(reason) => {
            arona::runtime::log::warning(format!(
                "bilibili：翻译回包不是 JSON（{reason}）：{}",
                raw.chars().take(80).collect::<String>()
            ));
            return None;
        }
    };
    if let (Some(code), Some(message)) = (reply.error_code, reply.error_msg) {
        arona::runtime::log::warning(format!("bilibili：翻译被拒 code: {code} msg: {message}"));
        return None;
    }
    // 接口自己判定原文就是中文：那段"译文"就是原文，不必再贴一遍
    if reply.from.as_deref() == Some("zh") {
        return None;
    }
    let text = reply
        .trans_result?
        .iter()
        .map(|item| item.dst.as_str())
        .collect::<Vec<&str>>()
        .join("\n");
    if text.trim().is_empty() {
        return None;
    }
    Some(Rendered {
        cut_line: conf.cut_line.clone(),
        text,
    })
}

fn encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TranslateConfig;

    #[test]
    fn brackets_are_stripped_before_deciding() {
        // 只剩中括号的那条：摘完就空了，不该去译
        assert_eq!(query("[笑][脱单delta]"), None);
        // 括号换成两个空格，前后各留一个的空格由 trim 收掉
        assert_eq!(query("[笑]hello world").as_deref(), Some("hello world"));
        assert_eq!(
            query("本日です[笑]皆さん").as_deref(),
            Some("本日です  皆さん")
        );
    }

    #[test]
    fn plain_chinese_is_not_translated() {
        assert_eq!(query("今天开了新活动，大家来玩"), None);
    }

    #[test]
    fn kana_makes_it_translate_even_with_hanzi_in_it() {
        // 中日混写：只看"有没有汉字"会判成中文而漏译
        assert_eq!(
            query("本日の新活動です").as_deref(),
            Some("本日の新活動です")
        );
    }

    #[test]
    fn text_without_any_hanzi_translates() {
        assert_eq!(query("Hello everyone").as_deref(), Some("Hello everyone"));
        assert_eq!(query("한국어 안녕").as_deref(), Some("한국어 안녕"));
    }

    #[test]
    fn long_sound_mark_and_middle_dot_count_as_japanese() {
        assert!(KANA.is_match("ー"));
        assert!(KANA.is_match("・"));
        assert!(KANA.is_match("カ"));
        assert_eq!(query("greetings ー").as_deref(), Some("greetings ー"));
    }

    #[test]
    fn sign_is_appid_then_query_then_salt_then_key() {
        // 顺序错一位接口就回 54001，这里把那一串原文钉住
        assert_eq!(
            sign("123", "abc", "999", "key"),
            format!("{:x}", md5::compute("123abc999key"))
        );
        assert_eq!(sign("123", "abc", "999", "key").len(), 32);
    }

    #[tokio::test]
    async fn disabled_or_unconfigured_never_leaves_the_process() {
        assert_eq!(
            translate(&TranslateConfig::default(), "こんにちは").await,
            None
        );
        let filled = TranslateConfig {
            enable: true,
            app_id: "1".into(),
            security_key: "k".into(),
            cut_line: "\n---\n".into(),
        };
        // 中文原文：判都不判，不该发请求
        assert_eq!(translate(&filled, "今天开了新活动").await, None);
        let no_keys = TranslateConfig {
            enable: true,
            ..Default::default()
        };
        assert_eq!(translate(&no_keys, "こんにちは").await, None);
    }

    #[test]
    fn reply_shape_matches_baidu() {
        let reply: Reply = serde_json::from_str(
            r#"{"from":"jp","to":"zh","trans_result":[{"src":"こんにちは","dst":"你好"}]}"#,
        )
        .unwrap();
        assert_eq!(reply.from.as_deref(), Some("jp"));
        assert_eq!(reply.trans_result.unwrap()[0].dst, "你好");
    }

    #[test]
    fn error_reply_shape_matches_baidu() {
        let reply: Reply = serde_json::from_str(
            r#"{"error_code":"54003","error_msg":"USER_REQUESTS_EXCEED_MAX_LIMIT"}"#,
        )
        .unwrap();
        assert_eq!(reply.error_code.as_deref(), Some("54003"));
        assert!(reply.trans_result.is_none());
    }
}
