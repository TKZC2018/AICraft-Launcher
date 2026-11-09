//! 把登录凭证封在本机：换台电脑就解不开。
//!
//! 玩家的诉求是「这份令牌只在这台机器上有效」。做法是两层：
//!
//! 1. **随机种子**：第一次写账号文件时生成 32 字节，存在数据目录里。
//! 2. **机器指纹**：`MachineGuid` + 计算机名 + 用户目录，运行时现取。
//!
//! 密钥 = `SHA256(种子 ‖ 指纹)`，正文用它的密钥流异或。把 `accounts.json`
//! 拷到另一台电脑，指纹不同 → 密钥不同 → 校验位对不上 → 当作「这份凭证
//! 不是这台机器的」，玩家重新登录即可。
//!
//! 不假装这是加密：这层防线挡的是「把文件夹复制走」，挡不住愿意改二进制的
//! 人。真要防那个，得用 TPM 或 DPAPI，而我们连一个外部 crate 都不引，
//! 也没法在 `unsafe_code = "forbid"` 下调 Win32。这一点在文档里说清楚，
//! 比写一句「已加密」更诚实。

use crate::sha256;

/// 种子文件名（放在数据目录里，和 `accounts.json` 同一个文件夹）。
pub const SEED_FILE: &str = "machine.key";

/// 封好的正文头上带的标记，用来区分「封过的」和「老版本明文」。
const HEADER: &str = "aicraft-seal-1";

/// 同一个标记的公开副本：测试与排查时拿来判断「这文件封过没有」。
pub const SEAL_MARKER: &str = HEADER;

/// 机器指纹：换机器（或换 Windows 用户）就会变的那些东西。
///
/// 结果缓存：每次调用都要跑一次 `reg query`，而账号文件一改就要取密钥。
pub fn machine_hint() -> &'static str {
    use std::sync::OnceLock;
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED.get_or_init(|| {
        let guid = registry_machine_guid().unwrap_or_default();
        let computer = std::env::var("COMPUTERNAME").unwrap_or_default();
        let user = std::env::var("USERPROFILE").unwrap_or_default();
        format!("{}|{}|{}", guid, computer, user)
    })
}

/// 从注册表取 `MachineGuid`。
///
/// 不用 winreg：那要引依赖，而出问题的机器上恰恰是拉不到依赖的那种。
/// `reg.exe` 到处都有，输出也稳定。
fn registry_machine_guid() -> Option<String> {
    let output = crate::spawn::silent("reg.exe")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Cryptography",
            "/v",
            "MachineGuid",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if !line.contains("MachineGuid") {
            continue;
        }
        // 形如 `    MachineGuid    REG_SZ    xxxx-xxxx-...`
        if let Some(value) = line.split_whitespace().last() {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// 读出数据目录里的种子；没有就生成一份。
///
/// 种子本身不是秘密（和账号文件放在一起），它的作用是让两台装了同一份
/// 启动器的机器即使指纹撞了也对不上密钥，同时避免「所有用户共用一个密钥」。
pub fn load_or_create_seed(dir: &std::path::Path) -> Result<[u8; 32], String> {
    let path = dir.join(SEED_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Some(seed) = decode_seed(text.trim()) {
            return Ok(seed);
        }
    }
    let raw = crate::auth::random_bytes(32);
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&raw);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录 {} 失败：{}", parent.display(), e))?;
    }
    std::fs::write(&path, to_hex(&seed))
        .map_err(|e| format!("写入 {} 失败：{}", path.display(), e))?;
    Ok(seed)
}

fn decode_seed(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let bytes = from_hex(text)?;
    if bytes.len() != 32 {
        return None;
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    Some(seed)
}

/// 由种子和机器指纹算出实际使用的密钥。
pub fn derive_key(seed: &[u8; 32], hint: &str) -> [u8; 32] {
    let mut hasher = sha256::Sha256Stream::new();
    hasher.update(seed);
    hasher.update(b"|aicraft-seal|");
    hasher.update(hint.as_bytes());
    hasher.finish()
}

/// 封起来：返回带标记的 JSON，可以直接当文件内容写。
pub fn seal(plain: &str, key: &[u8; 32]) -> String {
    let data = xor_keystream(plain.as_bytes(), key);
    // 校验位：解封后拿明文再算一次，用来判断「密钥对不对」
    let check = &sha256::sha256(plain.as_bytes())[..8];
    format!(
        r#"{{"format":"{}","check":"{}","data":"{}"}}"#,
        HEADER,
        to_hex(check),
        to_hex(&data)
    )
}

/// 解开；对不上（换了机器、文件被改过）返回错误。
pub fn unseal(text: &str, key: &[u8; 32]) -> Result<String, String> {
    let value = crate::json::parse(text).map_err(|e| format!("包裹格式损坏：{}", e))?;
    let format = value
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if format != HEADER {
        return Err(format!("不认识的包裹格式：{}", format));
    }
    let check = value
        .get("check")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "包裹缺少校验位".to_string())?;
    let data = value
        .get("data")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "包裹缺少正文".to_string())?;
    let bytes = from_hex(data).ok_or_else(|| "包裹正文不是十六进制".to_string())?;
    let plain = xor_keystream(&bytes, key);
    let expected = from_hex(check).ok_or_else(|| "校验位不是十六进制".to_string())?;
    let actual = &sha256::sha256(&plain)[..8];
    if expected != actual {
        return Err("这份登录凭证不是这台电脑上的".to_string());
    }
    String::from_utf8(plain).map_err(|_| "解出来的内容不是文本".to_string())
}

/// 判断一段文件内容是不是老版本的明文账号文件。
pub fn looks_plain(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with('{') && !trimmed.contains(HEADER)
}

fn xor_keystream(data: &[u8], key: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut counter: u32 = 0;
    while out.len() < data.len() {
        let mut hasher = sha256::Sha256Stream::new();
        hasher.update(key);
        hasher.update(&counter.to_le_bytes());
        let block = hasher.finish();
        counter = counter.wrapping_add(1);
        for byte in block.iter() {
            if out.len() >= data.len() {
                break;
            }
            out.push(*byte);
        }
    }
    for (index, slot) in out.iter_mut().enumerate() {
        *slot ^= data[index];
    }
    out
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0F) as usize] as char);
    }
    out
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        out.push((high << 4) | low);
    }
    Some(out)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; 32] {
        derive_key(&[7u8; 32], "test-machine")
    }

    #[test]
    fn sealing_round_trips() {
        let plain = r#"{"version":1,"accounts":[]}"#;
        let sealed = seal(plain, &key());
        assert!(sealed.starts_with(HEADER) || sealed.contains(HEADER));
        assert!(!sealed.contains("accounts\":[]"), "正文不该是明文");
        assert_eq!(unseal(&sealed, &key()).unwrap(), plain);
    }

    #[test]
    fn another_machine_cannot_open_it() {
        let sealed = seal("秘密", &key());
        let other = derive_key(&[7u8; 32], "another-machine");
        let err = unseal(&sealed, &other).unwrap_err();
        assert!(err.contains("不是这台电脑"), "实际是：{}", err);
    }

    #[test]
    fn a_different_seed_cannot_open_it() {
        let sealed = seal("秘密", &key());
        let other = derive_key(&[8u8; 32], "test-machine");
        assert!(unseal(&sealed, &other).is_err());
    }

    #[test]
    fn tampered_bodies_are_rejected() {
        let sealed = seal("秘密", &key());
        // 改掉正文里一个十六进制字符
        let mut chars: Vec<char> = sealed.chars().collect();
        let position = chars
            .iter()
            .rposition(|c| c.is_ascii_hexdigit())
            .expect("总有十六进制字符");
        let replacement = if chars[position] == 'a' { 'b' } else { 'a' };
        chars[position] = replacement;
        let broken: String = chars.into_iter().collect();
        assert!(unseal(&broken, &key()).is_err());
    }

    #[test]
    fn the_machine_hint_is_not_empty() {
        let hint = machine_hint();
        assert!(!hint.is_empty(), "机器指纹不该是空的");
        // 三项里至少得有一项取到值，否则「绑机器」就名不副实
        assert!(
            hint.split('|').any(|part| !part.is_empty()),
            "机器指纹一项都没取到：{}",
            hint
        );
    }

    #[test]
    fn plain_legacy_files_are_recognised() {
        assert!(looks_plain(r#"{"version":1,"accounts":[]}"#));
        assert!(!looks_plain(&seal("{}", &key())));
    }
}
