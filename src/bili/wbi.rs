//! B 站 WBI 签名。
//!
//! 2024 年起 `x/space/wbi/*`、`x/web-interface/wbi/*` 这类接口要求带 `w_rid` + `wts`，
//! 算法是把 `nav` 返回的两张水印图名拼成 64 字符的源串，按固定索引表重排后取前 32 位当密钥，
//! 再把查询参数按 key 字典序拼成 `k=v&k=v`（**原值，不做百分号编码**）接上密钥做 MD5。
//!
//! 索引表与原插件 `api/General.kt` 的 `getVerifyString` 逐字一致；这张表是 B 站前端硬编码的，
//! 换站方改版才会变，届时只需替换 [`MIXIN_TABLE`]。

use std::collections::BTreeMap;

/// 固定重排表（64 项，取值范围 0..64）
const MIXIN_TABLE: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// 官方文档要求从参与签名的值里滤掉的字符。请求也按滤后的值发，两边才对得上。
const FILTERED: [char; 18] = [
    '!', '\'', '(', ')', '*', '+', ',', '.', '/', ':', ';', '=', '?', '@', '&', '%', '#', '~',
];

/// `…/msgface/v1/daccf0a8c5f1e9b02d3c4e5f6a7b8c9d.png` 里那段文件名就是 key
pub fn key_of(url: &str) -> String {
    url.split('/')
        .next_back()
        .unwrap_or_default()
        .split('.')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// 由 img_key + sub_key 拼出 32 位 mixinKey
pub fn mixin_key(img_key: &str, sub_key: &str) -> String {
    let source = format!("{img_key}{sub_key}");
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(32);
    for index in MIXIN_TABLE {
        if let Some(ch) = chars.get(index) {
            out.push(*ch);
        }
    }
    out.chars().take(32).collect()
}

/// 把值里 B 站要滤的字符去掉
fn clean(value: &str) -> String {
    value.chars().filter(|ch| !FILTERED.contains(ch)).collect()
}

/// 签好名的查询串：返回按 key 字典序排好的参数（含 wts）与 w_rid。
/// 调用方把这份 map 直接拼成 URL，签名与实发才是同一串。
pub fn sign(
    params: &BTreeMap<String, String>,
    mixin: &str,
    wts: i64,
) -> (BTreeMap<String, String>, String) {
    let mut signed = BTreeMap::new();
    for (key, value) in params {
        signed.insert(key.clone(), clean(value));
    }
    signed.insert("wts".to_string(), wts.to_string());
    let query = signed
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let w_rid = format!("{:x}", md5::compute(format!("{query}{mixin}")));
    (signed, w_rid)
}

/// 拼查询串：key 与 value 都做百分号编码（签名用的是原值，这两件事互不干扰）
///
/// 编码集合用 `super::URI_COMPONENT`：`host_mid` 一旦编成 `host%5Fmid`，B 站收到的 key 就和
/// 参与签名的 key 不再是同一串写法。
pub fn encode_query(params: &BTreeMap<String, String>) -> String {
    use percent_encoding::utf8_percent_encode;
    params
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                utf8_percent_encode(key, super::URI_COMPONENT),
                utf8_percent_encode(value, super::URI_COMPONENT)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// 当前秒级时间戳
pub fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs() as i64)
        .unwrap_or_default()
}

/// 把查询参数装成表（值原样存，签名时才清洗）
pub fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 重排表逐字来自可工作的原插件；这组 img_key/sub_key 是把两边的实现各跑一遍对出来的，
    /// 期望值换不了——改了就意味着 Rust 侧的表序与原插件不再一致。
    #[test]
    fn mixin_key_matches_the_reference_implementation() {
        let img = key_of("https://i0.hdslb.com/bfs/wbi/7cd71926bcd147c6b5e90b611c0b8d31.png");
        let sub = key_of("https://i0.hdslb.com/bfs/wbi/594527442bcf4e6b826b1606987ac484.png");
        assert_eq!(img, "7cd71926bcd147c6b5e90b611c0b8d31");
        assert_eq!(sub, "594527442bcf4e6b826b1606987ac484");
        assert_eq!(
            mixin_key(&img, &sub),
            "6bed6b1566d177e5bf929cc9d8c444b7",
            "mixinKey 必须是表序重排后的前 32 位"
        );
        // 源串短于表长时只能凑出命中的那几个字符，不能 panic
        assert_eq!(mixin_key("abcdef", ""), "cdfabe");
    }

    #[test]
    fn url_key_extraction() {
        assert_eq!(key_of("https://a.com/b/face123.png"), "face123");
        assert_eq!(key_of("https://a.com/b/face123"), "face123");
        assert_eq!(key_of(""), "");
    }

    /// 签名必须是"字典序 + 原值 + 接密钥 + MD5 小写"，少一步就 403
    #[test]
    fn signature_is_deterministic() {
        let mut map = params(&[("mid", "31244122"), ("foo", "bar")]);
        map.insert("z".into(), "1".into());
        let (signed, w_rid) = sign(&map, "testmixinkey32charslongenough1234", 1_700_000_000);
        assert_eq!(w_rid.chars().count(), 32);
        assert!(
            w_rid
                .chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
        );
        // 同一份输入再算一次必须一模一样
        assert_eq!(
            sign(&map, "testmixinkey32charslongenough1234", 1_700_000_000).1,
            w_rid
        );
        // wts 由签名函数补进去，不该出现在原始参数里
        assert_eq!(signed.get("wts").map(String::as_str), Some("1700000000"));
    }

    #[test]
    fn filtered_chars_leave_both_query_and_request() {
        let map = params(&[("keyword", "a!b'c(d)")]);
        let (signed, _) = sign(&map, "mixin", 1);
        assert_eq!(signed.get("keyword").map(String::as_str), Some("abcd"));
    }

    #[test]
    fn query_encoding_keeps_values_intact() {
        let map = params(&[("type", "ALL"), ("keyword", "你好 world")]);
        let text = encode_query(&map);
        assert!(text.starts_with("keyword="), "{text}");
        assert!(text.contains("type=ALL"), "{text}");
        assert!(!text.contains(' '), "{text}");
    }
}
