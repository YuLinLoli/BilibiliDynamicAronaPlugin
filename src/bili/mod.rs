//! B 站接口层：WBI 签名、动态/直播/番剧、关系与扫码登录。

pub mod api;
pub mod model;
pub mod wbi;

/// 百分号编码集合：`A-Za-z0-9` 与 `-_.~` 之外全编。
///
/// 不直接用 `percent_encoding::NON_ALPHANUMERIC`——它连下划线也编掉，`host_mid` 会变成
/// `host%5Fmid`，而 B 站校验 WBI 时比对的是参数名本身，编码前后两套名字对不上就被判成伪造请求。
/// 相比 JS 的 `encodeURIComponent` 这里多编了 `!~*'()`，签名前 wbi 的清洗步骤已经把这几个字符滤掉了，
/// 所以两边算出的串仍然一致。
pub const URI_COMPONENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');
