//! 从模组 jar 里取出作者放的那张图标。
//!
//! 三家的声明方式各不相同，但都指向 jar 内的一个图片路径：
//!
//! - **Fabric**：`fabric.mod.json` 的 `icon` —— 可以是一个字符串，也可以是
//!   `{"128": "assets/icon_128.png", "64": ...}` 这样按尺寸给多份；
//! - **Forge / NeoForge**：`META-INF/mods.toml`（NeoForge 是
//!   `META-INF/neoforge.mods.toml`）里的 `logoFile = "..."`；
//! - 两样都没有时返回 `None`，界面画占位。
//!
//! 只做「找路径 + 把那一个条目读出来」，解不解得开是调用方的事（UI 那边
//! 已经有 `image` crate 在做解码）。jar 是 ZIP，复用 [`crate::natives`] 里
//! 那个自己写的读取器——不为一张图标再引一个 zip 库。

use std::io;
use std::path::Path;

use crate::json;
use crate::natives::{self, ZipEntry};

/// 图标本身超过这个大小就当没有：真图标不会这么大，超了多半是把整个
/// 纹理包放进了 jar 根目录，读进来只会拖慢列表。
const MAX_ICON_BYTES: u64 = 2 * 1024 * 1024;

/// 在一组条目里找「图标是哪个路径」。
///
/// 抽成纯函数是为了能直接喂条目名做测试，不必真的造一个 jar。
pub fn icon_path(entries: &[ZipEntry], read: &dyn Fn(&ZipEntry) -> Option<Vec<u8>>) -> Option<String> {
    // Fabric 最常见，先试它
    if let Some(entry) = find(entries, "fabric.mod.json") {
        if let Some(text) = read(entry).and_then(|b| String::from_utf8(b).ok()) {
            if let Some(path) = fabric_icon(&text) {
                return Some(path);
            }
        }
    }
    // NeoForge 的文件名优先于 Forge 的：同一个 jar 两个都带时，NeoForge
    // 那份才是这个模组现在维护的那份。
    for name in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        if let Some(entry) = find(entries, name) {
            if let Some(text) = read(entry).and_then(|b| String::from_utf8(b).ok()) {
                if let Some(path) = toml_logo(&text) {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// 从 jar 里取出图标字节。
///
/// 返回的是**原始图片字节**（png / jpg / webp 都有可能），交给调用方解码。
pub fn from_jar(jar: &Path) -> io::Result<Option<Vec<u8>>> {
    let entries = natives::read_zip_entries(jar)?;
    let read = |entry: &ZipEntry| entry.read_data(jar).ok();
    let Some(path) = icon_path(&entries, &read) else {
        return Ok(None);
    };
    let Some(entry) = find(&entries, &path) else {
        return Ok(None);
    };
    if entry.uncompressed_size > MAX_ICON_BYTES {
        return Ok(None);
    }
    let Ok(bytes) = entry.read_data(jar) else {
        return Ok(None);
    };
    Ok(Some(bytes))
}

/// 按条目名查一条（ZIP 里重名不常见，取第一条就够）。
fn find<'a>(entries: &'a [ZipEntry], name: &str) -> Option<&'a ZipEntry> {
    entries
        .iter()
        .find(|e| !e.is_dir && e.name.trim_start_matches('/') == name)
}

/// `fabric.mod.json` 里的 `icon`。
pub fn fabric_icon(text: &str) -> Option<String> {
    let root = json::parse(text).ok()?;
    let icon = root.get("icon")?;
    if let Some(path) = icon.as_str() {
        return clean(path);
    }
    // 按尺寸给多份时挑最大的那个键
    let obj = icon.as_object()?;
    let mut best: Option<(u64, &str)> = None;
    for (key, value) in obj {
        let Some(path) = value.as_str() else {
            continue;
        };
        // 键一般是 "16"/"32"/"128"；解析不出数字的当成 0 排最后
        let size = key.parse::<u64>().unwrap_or(0);
        if best.map(|(s, _)| size > s).unwrap_or(true) {
            best = Some((size, path));
        }
    }
    best.and_then(|(_, path)| clean(path))
}

/// `mods.toml` 里的 `logoFile = "..."`。
///
/// 不引 TOML 解析器：整份文件里我们只关心这一个键，而 TOML 的值就是
/// 一个带引号的字符串（也可能是一段的起头），扫一遍足够。
pub fn toml_logo(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("logoFile") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = rest.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.split('"').next())
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|v| v.split('\'').next())
            })?;
        return clean(value);
    }
    None
}

/// 统一收拾一下路径：去掉开头的 `./` 和 `/`，空的当没有。
fn clean(path: &str) -> Option<String> {
    let path = path.trim();
    let path = path.strip_prefix("./").unwrap_or(path);
    let path = path.strip_prefix('/').unwrap_or(path);
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> ZipEntry {
        ZipEntry {
            name: name.to_string(),
            compression: 0,
            compressed_size: 0,
            uncompressed_size: 0,
            local_header_offset: 0,
            is_dir: name.ends_with('/'),
        }
    }

    #[test]
    fn a_fabric_pack_points_at_one_file() {
        let entries = vec![
            entry("fabric.mod.json"),
            entry("assets/icon.png"),
            entry("META-INF/MANIFEST.MF"),
        ];
        let read = |e: &ZipEntry| -> Option<Vec<u8>> {
            if e.name == "fabric.mod.json" {
                Some(br#"{"id":"sodium","icon":"assets/icon.png"}"#.to_vec())
            } else {
                None
            }
        };
        assert_eq!(
            icon_path(&entries, &read).as_deref(),
            Some("assets/icon.png")
        );
    }

    #[test]
    fn a_fabric_pack_with_sizes_takes_the_biggest() {
        let entries = vec![entry("fabric.mod.json")];
        let read = |_: &ZipEntry| -> Option<Vec<u8>> {
            Some(
                br#"{"icon":{"16":"assets/icon16.png","128":"assets/icon128.png","64":"assets/icon64.png"}}"#
                    .to_vec(),
            )
        };
        assert_eq!(
            icon_path(&entries, &read).as_deref(),
            Some("assets/icon128.png")
        );
    }

    #[test]
    fn a_forge_mod_declares_a_logo_file() {
        let entries = vec![entry("META-INF/mods.toml"), entry("logo.png")];
        let read = |_: &ZipEntry| -> Option<Vec<u8>> {
            Some(
                br#"modLoader="javafml"
[[mods]]
modId="jei"
displayName="Just Enough Items"
logoFile = "logo.png"
"#
                .to_vec(),
            )
        };
        assert_eq!(icon_path(&entries, &read).as_deref(), Some("logo.png"));
    }

    #[test]
    fn neoforge_wins_over_forge_when_both_are_there() {
        let entries = vec![
            entry("META-INF/mods.toml"),
            entry("META-INF/neoforge.mods.toml"),
            entry("old.png"),
            entry("new.png"),
        ];
        let read = |e: &ZipEntry| -> Option<Vec<u8>> {
            match e.name.as_str() {
                "META-INF/mods.toml" => Some(b"logoFile=\"old.png\"".to_vec()),
                "META-INF/neoforge.mods.toml" => Some(b"logoFile=\"new.png\"".to_vec()),
                _ => None,
            }
        };
        assert_eq!(icon_path(&entries, &read).as_deref(), Some("new.png"));
    }

    #[test]
    fn a_pack_without_any_icon_says_so() {
        let entries = vec![entry("fabric.mod.json")];
        let read = |_: &ZipEntry| -> Option<Vec<u8>> {
            Some(br#"{"id":"x"}"#.to_vec())
        };
        assert_eq!(icon_path(&entries, &read), None);
    }

    #[test]
    fn a_broken_manifest_is_not_a_crash() {
        let entries = vec![entry("fabric.mod.json")];
        let read = |_: &ZipEntry| -> Option<Vec<u8>> { Some(b"{not json".to_vec()) };
        assert_eq!(icon_path(&entries, &read), None);
    }

    #[test]
    fn single_quoted_logo_is_understood() {
        assert_eq!(
            toml_logo("logoFile = 'gfx/logo.png'\n").as_deref(),
            Some("gfx/logo.png")
        );
    }

    #[test]
    fn a_leading_dot_slash_is_dropped() {
        assert_eq!(fabric_icon(r#"{"icon":"./icon.png"}"#).as_deref(), Some("icon.png"));
    }
}
