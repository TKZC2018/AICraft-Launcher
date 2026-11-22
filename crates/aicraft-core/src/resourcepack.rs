//! 资源包的脸面：从包里把 `pack.png` 抠出来。
//!
//! 游戏里资源包列表那一行显示的是 `pack.mcmeta` 里写的描述，作者会在里面塞
//! `§6`、`§l` 这类格式代码。启动器这边不复刻那一套：列表里显示的是文件名，
//! 玩家一眼就知道自己往文件夹里丢了什么，也好对着文件名去找。
//!
//! 剩下的活只有一件——把列表左边那张 64×64 的图标取出来。资源包可以是一个
//! `.zip`，也可以是一个**目录**（开发时、或者从别处解压来的），两种形态都认。
//!
//! 大包很常见（本机就有一个 877 MB 的），所以绝不能为了看一眼图标把整包读进
//! 内存 —— 目录在 ZIP 尾部，走 [`crate::natives::read_zip_index`] 只读那几段。

use std::path::Path;

use crate::natives;

/// `pack.png` 的单条上限。
///
/// 图标是 64×64 的小图；超过这个数说明读到的不是它，宁可不显示。
const MAX_ICON_BYTES: u64 = 1024 * 1024;

/// 不关心它是 zip 还是目录，直接取图标字节。
pub fn icon_at(path: &Path) -> Option<Vec<u8>> {
    if path.is_dir() {
        icon_dir(path)
    } else {
        icon(path)
    }
}

/// 这个目录像不像一个资源包：游戏认的判据就是有没有 `pack.mcmeta`。
///
/// 资源包可以是解压开的文件夹，而 `resourcepacks/` 里也常躺着别的杂项目录；
/// 只认有 `pack.mcmeta` 的，免得把一堆无关文件夹列进列表。
pub fn is_pack_dir(dir: &Path) -> bool {
    dir.is_dir() && dir.join("pack.mcmeta").is_file()
}

/// 从 `.zip` 里取出 `pack.png` 的原始字节。
pub fn icon(path: &Path) -> Option<Vec<u8>> {
    let entries = natives::read_zip_index(path).ok()?;
    entry_data(path, &entries, "pack.png", MAX_ICON_BYTES)
}

/// 从目录里取出 `pack.png` 的原始字节。
pub fn icon_dir(dir: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(dir.join("pack.png")).ok()?;
    if meta.len() > MAX_ICON_BYTES {
        return None;
    }
    std::fs::read(dir.join("pack.png")).ok()
}

/// 按条目名读一条（ZIP 里重名不常见，取第一条就够）。
fn find<'a>(entries: &'a [natives::ZipEntry], name: &str) -> Option<&'a natives::ZipEntry> {
    entries
        .iter()
        .find(|e| !e.is_dir && e.name.trim_start_matches('/') == name)
}

fn entry_data(
    archive: &Path,
    entries: &[natives::ZipEntry],
    name: &str,
    limit: u64,
) -> Option<Vec<u8>> {
    let entry = find(entries, name)?;
    if entry.uncompressed_size > limit {
        return None;
    }
    entry.read_data(archive).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个只装一条 stored 记录的最小 ZIP。
    ///
    /// 真包动辄几十上百 MB，测试里自己拼一个：本地头 + 中央目录 + EOCD，
    /// 偏移全按顺序算，`read_zip_index` 读到的就该是这一条。
    fn make_zip(name: &str, content: &[u8]) -> Vec<u8> {
        let name = name.as_bytes();
        let mut z = Vec::new();
        z.extend_from_slice(b"PK\x03\x04");
        z.extend_from_slice(&20u16.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes()); // stored
        z.extend_from_slice(&[0u8; 4]); // time + date
        z.extend_from_slice(&0u32.to_le_bytes()); // crc（本模块不解压，随便）
        z.extend_from_slice(&(content.len() as u32).to_le_bytes());
        z.extend_from_slice(&(content.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(name);
        z.extend_from_slice(content);

        let cd_start = z.len();
        z.extend_from_slice(b"PK\x01\x02");
        z.extend_from_slice(&20u16.to_le_bytes());
        z.extend_from_slice(&20u16.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&[0u8; 4]);
        z.extend_from_slice(&0u32.to_le_bytes());
        z.extend_from_slice(&(content.len() as u32).to_le_bytes());
        z.extend_from_slice(&(content.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&0u32.to_le_bytes());
        z.extend_from_slice(&0u32.to_le_bytes());
        z.extend_from_slice(name);
        let cd_size = z.len() - cd_start;

        z.extend_from_slice(b"PK\x05\x06");
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&1u16.to_le_bytes());
        z.extend_from_slice(&1u16.to_le_bytes());
        z.extend_from_slice(&(cd_size as u32).to_le_bytes());
        z.extend_from_slice(&(cd_start as u32).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z
    }

    #[test]
    fn an_icon_comes_out_of_the_zip() {
        let tmp = crate::testutil::fresh_dir("pack-icon");
        let pack = tmp.join("a.zip");
        std::fs::write(&pack, make_zip("pack.png", b"PNGDATA")).unwrap();
        assert_eq!(icon(&pack).as_deref(), Some(&b"PNGDATA"[..]));
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn a_pack_without_an_icon_has_none() {
        let tmp = crate::testutil::fresh_dir("pack-noicon");
        let pack = tmp.join("a.zip");
        std::fs::write(&pack, make_zip("pack.mcmeta", b"{}")).unwrap();
        assert!(icon(&pack).is_none());
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn an_unreadable_zip_does_not_panic() {
        let tmp = crate::testutil::fresh_dir("pack-broken");
        let pack = tmp.join("a.zip");
        std::fs::write(&pack, b"this is not a zip").unwrap();
        assert!(icon(&pack).is_none());
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn a_folder_pack_gives_its_icon_too() {
        let tmp = crate::testutil::fresh_dir("pack-dir");
        let dir = tmp.join("解压开的包");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pack.mcmeta"), b"{}").unwrap();
        std::fs::write(dir.join("pack.png"), b"ICON").unwrap();
        assert!(is_pack_dir(&dir), "有 pack.mcmeta 就该算资源包");
        assert_eq!(icon_at(&dir).as_deref(), Some(&b"ICON"[..]));
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn a_plain_folder_is_not_a_pack() {
        let tmp = crate::testutil::fresh_dir("pack-plain");
        let dir = tmp.join("别的杂项");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_pack_dir(&dir));
        assert!(icon_at(&dir).is_none());
        crate::testutil::cleanup(&tmp);
    }
}
