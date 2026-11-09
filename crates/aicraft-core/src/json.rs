//! 最小 JSON 解析器。
//!
//! core 只需要读取版本清单这类结构固定的文档，引入 serde 会带进一整套
//! derive 宏和依赖树，而本机 cargo 目前无法访问 crates.io。这里实现一个
//! 够用的解析器：对象 / 数组 / 字符串 / 数字 / 布尔 / null，支持全部
//! JSON 转义序列。
//!
//! 解析结果保留插入顺序（对象内部用 Vec），因为版本清单里 `rules` 的
//! 判定顺序是有语义的，不能被打乱。

use std::fmt;

/// 一个 JSON 值。
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// 数字统一按 f64 存放；需要整数时调用方自行转换。
    Number(f64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

/// 解析错误。
#[derive(Debug, Clone, PartialEq)]
pub struct JsonError {
    pub message: String,
    /// 出错位置在输入中的字节偏移。
    pub offset: usize,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON 解析错误 (偏移 {}): {}", self.offset, self.message)
    }
}

impl std::error::Error for JsonError {}

impl Json {
    /// 按 key 取对象成员。非对象返回 None。
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// 取字符串。
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// 取布尔。
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// 取整数，小数会被截断。
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Number(n) => Some(*n as i64),
            _ => None,
        }
    }

    /// 取数组。
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    /// 取对象成员列表。
    pub fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(entries) => Some(entries),
            _ => None,
        }
    }

    /// 是否为空值。
    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }
}

/// 把一个 JSON 值写回文本。
///
/// 对象成员保持插入顺序，输出是紧凑格式（不带缩进）——它只用来把
/// 改过顶层 `id` 的版本清单写回磁盘，格式无关紧要。
pub fn write(value: &Json) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &Json, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Number(n) => out.push_str(&format_number(*n)),
        Json::Str(s) => write_string(s, out),
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Json::Object(entries) => {
            out.push('{');
            for (i, (key, item)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(item, out);
            }
            out.push('}');
        }
    }
}

/// 整数按整数写（`4096` 而不是 `4096.0`），其余交给浮点默认格式。
fn format_number(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.0e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// 解析一个完整的 JSON 文档。
pub fn parse(input: &str) -> Result<Json, JsonError> {
    let mut p = Parser {
        bytes: input.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let value = p.parse_value()?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return Err(p.err("文档结束后存在多余内容"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, message: &str) -> JsonError {
        JsonError {
            message: message.to_string(),
            offset: self.pos,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    /// 跳过空白字符。
    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            match b {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn expect(&mut self, want: u8) -> Result<(), JsonError> {
        match self.bump() {
            Some(b) if b == want => Ok(()),
            Some(b) => Err(self.err(&format!(
                "期望 '{}'，实际是 '{}'",
                want as char, b as char
            ))),
            None => Err(self.err(&format!("期望 '{}'，但已到文档末尾", want as char))),
        }
    }

    fn parse_value(&mut self) -> Result<Json, JsonError> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => Ok(Json::Str(self.parse_string()?)),
            Some(b't') => self.parse_literal("true", Json::Bool(true)),
            Some(b'f') => self.parse_literal("false", Json::Bool(false)),
            Some(b'n') => self.parse_literal("null", Json::Null),
            Some(b) if b == b'-' || b.is_ascii_digit() => self.parse_number(),
            Some(b) => Err(self.err(&format!("无法识别的字符 '{}'", b as char))),
            None => Err(self.err("文档意外结束")),
        }
    }

    fn parse_literal(&mut self, word: &str, value: Json) -> Result<Json, JsonError> {
        for expected in word.bytes() {
            if self.bump() != Some(expected) {
                return Err(self.err(&format!("期望字面量 '{}'", word)));
            }
        }
        Ok(value)
    }

    fn parse_object(&mut self) -> Result<Json, JsonError> {
        self.expect(b'{')?;
        let mut entries = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(entries));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b'}') => break,
                Some(b) => return Err(self.err(&format!("对象中期望 ',' 或 '}}'，实际是 '{}'", b as char))),
                None => return Err(self.err("对象未闭合")),
            }
        }
        Ok(Json::Object(entries))
    }

    fn parse_array(&mut self) -> Result<Json, JsonError> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b']') => break,
                Some(b) => return Err(self.err(&format!("数组中期望 ',' 或 ']'，实际是 '{}'", b as char))),
                None => return Err(self.err("数组未闭合")),
            }
        }
        Ok(Json::Array(items))
    }

    fn parse_string(&mut self) -> Result<String, JsonError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let b = match self.bump() {
                Some(b) => b,
                None => return Err(self.err("字符串未闭合")),
            };
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = self.bump().ok_or_else(|| self.err("转义序列未完成"))?;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let cp = self.parse_hex4()?;
                            // 处理 UTF-16 代理对
                            if (0xD800..0xDC00).contains(&cp) {
                                if self.bump() != Some(b'\\') || self.bump() != Some(b'u') {
                                    return Err(self.err("高代理项后缺少低代理项"));
                                }
                                let low = self.parse_hex4()?;
                                if !(0xDC00..0xE000).contains(&low) {
                                    return Err(self.err("非法的低代理项"));
                                }
                                let combined =
                                    0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00);
                                out.push(
                                    char::from_u32(combined)
                                        .ok_or_else(|| self.err("非法码点"))?,
                                );
                            } else {
                                out.push(
                                    char::from_u32(cp).ok_or_else(|| self.err("非法码点"))?,
                                );
                            }
                        }
                        other => {
                            return Err(
                                self.err(&format!("未知转义字符 '\\{}'", other as char))
                            )
                        }
                    }
                }
                // 多字节 UTF-8：收下首字节，后续字节原样并入
                _ => {
                    let start = self.pos - 1;
                    let width = utf8_width(b);
                    if width > 1 {
                        self.pos = start + width;
                        if self.pos > self.bytes.len() {
                            return Err(self.err("UTF-8 序列被截断"));
                        }
                    }
                    let piece = std::str::from_utf8(&self.bytes[start..self.pos])
                        .map_err(|_| self.err("非法 UTF-8"))?;
                    out.push_str(piece);
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u32, JsonError> {
        let mut v: u32 = 0;
        for _ in 0..4 {
            let b = self.bump().ok_or_else(|| self.err("\\u 转义不完整"))?;
            let digit = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return Err(self.err("\\u 转义中出现非十六进制字符")),
            };
            v = v * 16 + digit as u32;
        }
        Ok(v)
    }

    fn parse_number(&mut self) -> Result<Json, JsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        if let Some(b'e' | b'E') = self.peek() {
            self.pos += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.pos += 1;
            }
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| self.err("数字不是合法 UTF-8"))?;
        let value: f64 = text
            .parse()
            .map_err(|_| self.err(&format!("无法解析数字 '{}'", text)))?;
        Ok(Json::Number(value))
    }
}

/// 依首个字节判断 UTF-8 序列长度。非法首字节按 1 处理，交给后续校验。
fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars() {
        assert_eq!(parse("null").unwrap(), Json::Null);
        assert_eq!(parse("true").unwrap(), Json::Bool(true));
        assert_eq!(parse(" -12.5 ").unwrap(), Json::Number(-12.5));
        assert_eq!(parse(r#""hi""#).unwrap(), Json::Str("hi".into()));
    }

    #[test]
    fn parses_nested() {
        let v = parse(r#"{"a":[1,{"b":"x"}],"c":false}"#).unwrap();
        assert_eq!(v.get("c").unwrap().as_bool(), Some(false));
        let arr = v.get("a").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1].get("b").unwrap().as_str(), Some("x"));
    }

    #[test]
    fn handles_escapes_and_unicode() {
        assert_eq!(parse(r#""a\nb""#).unwrap().as_str(), Some("a\nb"));
        assert_eq!(parse(r#""\u4e2d\u6587""#).unwrap().as_str(), Some("中文"));
        // 代理对 -> U+1F600
        assert_eq!(
            parse(r#""\ud83d\ude00""#).unwrap().as_str(),
            Some("\u{1F600}")
        );
        // 直接出现的多字节 UTF-8
        assert_eq!(parse(r#""版本""#).unwrap().as_str(), Some("版本"));
    }

    #[test]
    fn rejects_trailing_garbage() {
        assert!(parse("{} x").is_err());
        assert!(parse("[1,2").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn preserves_object_order() {
        let v = parse(r#"{"z":1,"a":2}"#).unwrap();
        let entries = v.as_object().unwrap();
        assert_eq!(entries[0].0, "z");
        assert_eq!(entries[1].0, "a");
    }

    #[test]
    fn write_round_trips() {
        let text = r#"{"id":"1.20.1","n":4096,"f":1.5,"b":true,"nul":null,"arr":[1,"x"],"s":"a\"b\\c\n"}"#;
        let value = parse(text).unwrap();
        let again = parse(&write(&value)).unwrap();
        assert_eq!(value, again);
    }

    #[test]
    fn write_keeps_integers_looking_like_integers() {
        // 版本清单里 size / majorVersion 这类字段是整数，
        // 写成 4096.0 会让别的解析器把它们当浮点。
        let value = parse(r#"{"size":4096,"ratio":0.5}"#).unwrap();
        let text = write(&value);
        assert!(text.contains("\"size\":4096"), "{}", text);
        assert!(text.contains("\"ratio\":0.5"), "{}", text);
    }

    #[test]
    fn empty_containers_survive_a_round_trip() {
        for text in ["{}", "[]", r#"{"a":{},"b":[]}"#] {
            let value = parse(text).unwrap();
            assert_eq!(parse(&write(&value)).unwrap(), value);
        }
    }
}
