//! 字节转小写十六进制。
//!
//! 哈希值、离线 UUID、clientToken 都要把字节串变成十六进制。以前每处都写
//! `format!("{:02x}", b)` —— 每个字节都走一遍格式化机制，本机实测 200 万次
//! 20 字节转 hex 要 2.05 秒，换成查表只要 0.26 秒。校验整个游戏目录时这段
//! 是实打实的热路径（每个文件都要算一次 SHA-1 再转字符串），所以单独收成
//! 一处，别再让各处各自展开。
//!
//! 查表版直接 `push(char)` 而不碰 `String` 的字节缓冲：core 全局禁止
//! `unsafe`，而 `as_mut_vec` 需要它。每个字节两次 push 依然比格式化快一个
//! 数量级，够用。

/// 十六进制数字表（小写）。
const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// 把一段字节转成小写十六进制字符串。
pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    push_hex(&mut out, bytes);
    out
}

/// 把一段字节以小写十六进制追加到已有的字符串后面。
pub fn push_hex(out: &mut String, bytes: &[u8]) {
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0F) as usize] as char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_become_lowercase_hex() {
        assert_eq!(to_hex(&[]), "");
        assert_eq!(to_hex(&[0x00]), "00");
        assert_eq!(to_hex(&[0x0f, 0xf0, 0xff]), "0ff0ff");
        assert_eq!(to_hex(&[0x12, 0x34, 0xab, 0xcd]), "1234abcd");
    }

    #[test]
    fn appending_keeps_what_was_there() {
        let mut out = String::from("sha1:");
        push_hex(&mut out, &[0xde, 0xad]);
        assert_eq!(out, "sha1:dead");
    }
}
