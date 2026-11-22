//! 存档与备份的脸面：世界目录里那张 `icon.png`。
//!
//! 游戏存世界时会顺手截一张俯视图，存成 `<存档>/icon.png`（本机 16 个存档里
//! 14 个有）。备份是把整个世界目录压成 zip，同一张图就落在 `<世界名>/`
//! 或 `saves/<世界名>/` 下面 —— 层级不固定，这里按「最浅的那一个」取。
//!
//! 备份包可能几个 G，所以只读 ZIP 中央目录，别的条目一个都不解压。

use std::path::Path;

use crate::natives;

/// 单张图标的上限。超过这个数说明读到的不是那张截图。
const MAX_ICON_BYTES: u64 = 4 * 1024 * 1024;

/// `icon.png` 最多允许埋在几层下面。
///
/// `saves/world/icon.png` 就是三层，本机的备份里最深的就是这一种。
const MAX_DEPTH: usize = 3;

/// 存档目录（或者解压开的备份）里的 `icon.png`。
pub fn icon_dir(dir: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(dir.join("icon.png")).ok()?;
    if !meta.is_file() || meta.len() > MAX_ICON_BYTES {
        return None;
    }
    std::fs::read(dir.join("icon.png")).ok()
}

/// 备份 zip 里的 `icon.png`。
///
/// 备份工具不统一：有的把世界目录整个压进来（`<世界名>/icon.png`），
/// 有的先套一层 `saves/`。取层级最浅的那一个。
pub fn icon_zip(zip: &Path) -> Option<Vec<u8>> {
    let entries = natives::read_zip_index(zip).ok()?;
    let (_, entry) = entries
        .iter()
        .filter(|entry| !entry.is_dir && entry.uncompressed_size <= MAX_ICON_BYTES)
        .filter_map(|entry| {
            let parts: Vec<&str> = entry.name.trim_start_matches('/').split('/').collect();
            match parts.last() {
                Some(name) if *name == "icon.png" && parts.len() <= MAX_DEPTH => {
                    Some((parts.len(), entry))
                }
                _ => None,
            }
        })
        .min_by_key(|(depth, _)| *depth)?;
    entry.read_data(zip).ok()
}

/// 目录按存档读、文件按备份读，一个入口。
pub fn icon_at(path: &Path) -> Option<Vec<u8>> {
    if path.is_dir() {
        icon_dir(path)
    } else {
        icon_zip(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拼一个只装 stored 记录的最小 ZIP（偏移全按顺序算）。
    ///
    /// 真备份动辄几个 G，测试里自己造：本地头 + 中央目录 + EOCD。
    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = Vec::new();
        let mut offsets = Vec::new();
        for (name, content) in entries {
            offsets.push(z.len() as u32);
            let name = name.as_bytes();
            z.extend_from_slice(b"PK\x03\x04");
            z.extend_from_slice(&20u16.to_le_bytes());
            z.extend_from_slice(&0u16.to_le_bytes());
            z.extend_from_slice(&0u16.to_le_bytes());
            z.extend_from_slice(&[0u8; 4]);
            z.extend_from_slice(&0u32.to_le_bytes());
            z.extend_from_slice(&(content.len() as u32).to_le_bytes());
            z.extend_from_slice(&(content.len() as u32).to_le_bytes());
            z.extend_from_slice(&(name.len() as u16).to_le_bytes());
            z.extend_from_slice(&0u16.to_le_bytes());
            z.extend_from_slice(name);
            z.extend_from_slice(content);
        }

        let cd_start = z.len() as u32;
        for ((name, content), offset) in entries.iter().zip(&offsets) {
            let name = name.as_bytes();
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
            z.extend_from_slice(&offset.to_le_bytes());
            z.extend_from_slice(name);
        }
        let cd_size = z.len() as u32 - cd_start;

        z.extend_from_slice(b"PK\x05\x06");
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        z.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        z.extend_from_slice(&cd_size.to_le_bytes());
        z.extend_from_slice(&cd_start.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z
    }

    #[test]
    fn a_save_gives_up_its_icon() {
        let tmp = crate::testutil::fresh_dir("save-icon");
        let world = tmp.join("世界");
        std::fs::create_dir_all(&world).unwrap();
        std::fs::write(world.join("icon.png"), b"SHOT").unwrap();
        std::fs::write(world.join("level.dat"), b"data").unwrap();
        assert_eq!(icon_at(&world).as_deref(), Some(&b"SHOT"[..]));
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn a_save_that_was_never_screenshotted_has_none() {
        let tmp = crate::testutil::fresh_dir("save-noicon");
        let world = tmp.join("老世界");
        std::fs::create_dir_all(&world).unwrap();
        std::fs::write(world.join("level.dat"), b"data").unwrap();
        assert!(icon_at(&world).is_none());
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn a_backup_finds_the_icon_however_deep_it_sits() {
        let tmp = crate::testutil::fresh_dir("backup-icon");
        for (name, body) in [
            ("world-one", make_zip(&[("world-one/level.dat", b"d"), ("world-one/icon.png", b"ONE")])),
            (
                "world-two",
                make_zip(&[("saves/world-two/level.dat", b"d"), ("saves/world-two/icon.png", b"TWO")]),
            ),
        ] {
            let path = tmp.join(format!("{}.zip", name));
            std::fs::write(&path, &body).unwrap();
            let want = if name == "world-one" { b"ONE" } else { b"TWO" };
            assert_eq!(icon_at(&path).as_deref(), Some(&want[..]), "{} 的图标没取到", name);
        }
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn a_backup_without_a_shot_stays_blank() {
        let tmp = crate::testutil::fresh_dir("backup-noicon");
        let path = tmp.join("a.zip");
        std::fs::write(&path, make_zip(&[("world/level.dat", b"d")])).unwrap();
        assert!(icon_at(&path).is_none());
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn a_deep_or_oversized_entry_is_not_mistaken_for_the_shot() {
        let tmp = crate::testutil::fresh_dir("backup-deep");
        // 四层以下的不认（本机的备份没有这么深），免得抠出某张内部贴图。
        let path = tmp.join("deep.zip");
        std::fs::write(
            &path,
            make_zip(&[("a/b/c/d/icon.png", b"DEEP"), ("a/level.dat", b"d")]),
        )
        .unwrap();
        assert!(icon_at(&path).is_none());
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn a_broken_zip_does_not_panic() {
        let tmp = crate::testutil::fresh_dir("backup-broken");
        let path = tmp.join("a.zip");
        std::fs::write(&path, b"not a zip").unwrap();
        assert!(icon_at(&path).is_none());
        crate::testutil::cleanup(&tmp);
    }
}
