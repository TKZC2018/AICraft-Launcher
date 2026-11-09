//! 编码工具：Base64URL 与 URL 百分号编码。
//!
//! 这两样东西在登录流程里到处都是：
//!
//! - PKCE 的 `code_challenge` 要 `BASE64URL_NOPAD(SHA256(verifier))`；
//! - `/token` 端点要求 `application/x-www-form-urlencoded`，参数值必须百分号编码，
//!   否则 `refresh_token` 里出现的 `+ / =` 会被对端解析错；
//! - 浏览器回调的查询串要解码回来（`code=...&state=...`）。
//!
//! 零依赖自实现，理由同其他模块：本机 cargo 拉不到 crates.io。

/// Base64URL（RFC 4648 §5，无填充）编码。
///
/// 与标准 Base64 的差别只有两点：字符表里 `+` → `-`、`/` → `_`，
/// 并且不写 `=` 填充。
pub fn base64url_nopad(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    let mut out = String::with_capacity((data.len() * 4).div_ceil(3));
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();

        // 每 3 字节产出 4 个字符（末尾不足时少产出）
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
        if b1.is_some() {
            out.push(ALPHABET[(((b1.unwrap_or(0) & 0x0F) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char);
        }
        if b2.is_some() {
            out.push(ALPHABET[(b2.unwrap_or(0) & 0x3F) as usize] as char);
        }
    }
    out
}

/// Base64URL（无填充）解码。主要服务于测试与调试，但对端发来的
/// `id_token` 分段解码也用得上。
pub fn base64url_nopad_decode(text: &str) -> Result<Vec<u8>, String> {
    let mut vals: Vec<u8> = Vec::with_capacity(text.len() + 4);
    for ch in text.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            // 容忍标准 Base64 的字符，方便手工调试时混用
            b'+' => 62,
            b'/' => 63,
            b'=' => continue,
            other => return Err(format!("非法的 Base64URL 字符: {:?}", other as char)),
        };
        vals.push(v);
    }
    if vals.len() % 4 == 1 {
        return Err("Base64URL 长度非法".to_string());
    }

    let mut out = Vec::with_capacity(vals.len() * 3 / 4);
    for chunk in vals.chunks(4) {
        let n = chunk.len();
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let b3 = chunk.get(3).copied().unwrap_or(0);

        out.push((b0 << 2) | (b1 >> 4));
        if n >= 3 {
            out.push((b1 << 4) | (b2 >> 2));
        }
        if n >= 4 {
            out.push((b2 << 6) | b3);
        }
    }
    Ok(out)
}

/// URL 百分号编码（`application/x-www-form-urlencoded` 的键与值）。
///
/// 只保留 RFC 3986 的 unreserved 字符 `A-Z a-z 0-9 - . _ ~`，
/// 空格编码为 `%20`（不是 `+`：两者在服务端都合法，但 `%20` 不会
/// 因为表单解码顺序的差异被误读）。
pub fn urlencode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 把一组键值编成 `a=1&b=2` 这种表单串。
pub fn urlencode_pairs(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&urlencode(k));
        out.push('=');
        out.push_str(&urlencode(v));
    }
    out
}

/// 百分号解码。回调里的 `code` / `state` 需要它。
///
/// 非法转义（`%ZZ`）时返回 `None` 而不是替换成替换字符 —— 登录回调
/// 被篡改时宁可报错，也不要拿到一个「看起来正常」的错误值。
pub fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hi = hex_val(bytes[i + 1])?;
                let lo = hex_val(bytes[i + 2])?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b'+' => {
                // 表单语义：+ 代表空格
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 解析 `a=1&b=2` 形式的表单串，返回键值对。
///
/// 重复的键按出现顺序保留，由调用方决定取哪个。
pub fn parse_form(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in text.split('&') {
        if part.is_empty() {
            continue;
        }
        let (k, v) = match part.split_once('=') {
            Some((k, v)) => (k, v),
            None => (part, ""),
        };
        if let (Some(k), Some(v)) = (percent_decode(k), percent_decode(v)) {
            out.push((k, v));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_rfc4648_vectors() {
        // RFC 4648 §10 的测试向量换成 URL 安全字母表
        let cases = [
            ("", ""),
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
        ];
        for (input, want) in cases {
            assert_eq!(base64url_nopad(input.as_bytes()), want, "编码 {:?}", input);
            assert_eq!(
                base64url_nopad_decode(want).unwrap(),
                input.as_bytes(),
                "解码 {:?}",
                want
            );
        }
    }

    #[test]
    fn base64url_uses_url_safe_alphabet() {
        // 0xFF 0xFE -> 标准 Base64 是 "//4="，URL 安全应为 "__4"
        assert_eq!(base64url_nopad(&[0xFF, 0xFE]), "__4");
        assert!(!base64url_nopad(&[0xFF, 0xFE]).contains(['+', '/', '=']));
    }

    #[test]
    fn base64url_rejects_bad_input() {
        assert!(base64url_nopad_decode("Zg$").is_err());
        assert!(base64url_nopad_decode("Z").is_err()); // 长度 %4 == 1
    }

    #[test]
    fn urlencode_keeps_unreserved_and_escapes_rest() {
        assert_eq!(urlencode("abcXYZ019-._~"), "abcXYZ019-._~");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("a+b"), "a%2Bb");
        assert_eq!(urlencode("a/b?c=d&e"), "a%2Fb%3Fc%3Dd%26e");
        // 多字节 UTF-8 逐字节编码
        assert_eq!(urlencode("é"), "%C3%A9");
        assert_eq!(urlencode("中"), "%E4%B8%AD");
    }

    #[test]
    fn urlencode_pairs_builds_form() {
        assert_eq!(
            urlencode_pairs(&[("grant_type", "authorization_code"), ("code", "a+b/c=")]),
            "grant_type=authorization_code&code=a%2Bb%2Fc%3D"
        );
        assert_eq!(urlencode_pairs(&[]), "");
    }

    #[test]
    fn percent_decode_roundtrip() {
        let raw = "a b+c/d=é中";
        assert_eq!(percent_decode(&urlencode(raw)).unwrap(), raw);
        // + 在表单语义里是空格
        assert_eq!(percent_decode("a+b").unwrap(), "a b");
        assert_eq!(percent_decode("%C3%A9").unwrap(), "é");
        assert_eq!(percent_decode("%ZZ"), None);
        assert_eq!(percent_decode("%4"), None);
    }

    #[test]
    fn parse_form_splits_pairs() {
        let form = parse_form("code=abc&state=x%2By&empty=");
        assert_eq!(
            form,
            vec![
                ("code".to_string(), "abc".to_string()),
                ("state".to_string(), "x+y".to_string()),
                ("empty".to_string(), String::new()),
            ]
        );
        assert!(parse_form("").is_empty());
    }
}
