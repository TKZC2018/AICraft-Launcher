//! 实例隔离。
//!
//! 同一个 `.minecraft` 下可以装很多版本，如果它们共用一份游戏目录，
//! 存档、模组、配置文件会互相污染 —— 换个版本发现模组全变、存档没了，
//! 是最容易劝退人的体验。
//!
//! 这里沿用 HMCL 的「版本隔离」布局约定：
//!
//! ```text
//! .minecraft/
//! ├── assets/                 共享（只读，体积大）
//! ├── libraries/              共享（只读，体积大）
//! └── versions/
//!     ├── 1.20.1_Fabric/
//!     │   ├── 1.20.1_Fabric.json
//!     │   ├── 1.20.1_Fabric.jar
//!     │   ├── mods/            ← 实例私有
//!     │   ├── saves/           ← 实例私有
//!     │   ├── config/          ← 实例私有
//!     │   └── options.txt      ← 实例私有
//!     └── 1.21.7/
//!         ├── 1.21.7.json
//!         └── 1.21.7.jar       ← 没有实例数据 = 共享模式
//! ```
//!
//! 为什么沿用它而不是另造 `instances/`：用户的现有库已经是这个布局，
//! 换约定等于让所有存档和模组「消失」，必须兼容。

use std::path::{Path, PathBuf};

/// 判断隔离时看的标记。
///
/// 只要版本目录里出现其中任意一项，就认为这个版本是独立的实例。
/// 这些都是游戏运行时才会产生的用户数据，不会由版本安装生成 ——
/// 所以刚装好的纯净版本不会被误判为隔离。
const INSTANCE_MARKERS: &[&str] = &[
    "options.txt",
    "mods",
    "saves",
    "config",
    "resourcepacks",
    "shaderpacks",
    "screenshots",
    "crash-reports",
    "schematics",
];

/// 实例的运行模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceMode {
    /// 独立实例：游戏目录指向 `versions/<id>/`。
    Isolated,
    /// 共享：游戏目录指向 `.minecraft`。
    Shared,
}

impl InstanceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            InstanceMode::Isolated => "隔离",
            InstanceMode::Shared => "共享",
        }
    }
}

/// 某个版本的目录。
pub fn version_dir(game_dir: &Path, version_id: &str) -> PathBuf {
    game_dir.join("versions").join(version_id)
}

/// 探测某个版本是否已经是独立实例。
///
/// 只看版本目录里有没有用户数据，不做任何猜测。
pub fn detect_mode(game_dir: &Path, version_id: &str) -> InstanceMode {
    let dir = version_dir(game_dir, version_id);
    for marker in INSTANCE_MARKERS {
        if dir.join(marker).exists() {
            return InstanceMode::Isolated;
        }
    }
    InstanceMode::Shared
}

/// 按模式求游戏实际使用的工作目录。
pub fn game_dir_for(game_dir: &Path, version_id: &str, mode: InstanceMode) -> PathBuf {
    match mode {
        InstanceMode::Isolated => version_dir(game_dir, version_id),
        InstanceMode::Shared => game_dir.to_path_buf(),
    }
}

/// 把一个版本转成独立实例。
///
/// 只是建出目录结构，不改动任何已有文件 —— 游戏第一次以隔离模式启动时，
/// 会自己生成 `options.txt`、`logs/` 等。
///
/// 返回实际创建了哪些目录，便于调用方展示。
pub fn ensure_instance(game_dir: &Path, version_id: &str) -> std::io::Result<Vec<PathBuf>> {
    let dir = version_dir(game_dir, version_id);
    let mut created = Vec::new();
    for sub in ["", "mods", "saves", "config"] {
        let p = if sub.is_empty() { dir.clone() } else { dir.join(sub) };
        if !p.exists() {
            std::fs::create_dir_all(&p)?;
            created.push(p);
        }
    }
    Ok(created)
}

/// 实例目录里的一个条目：一个模组、一个资源包、一个存档。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    /// 修改时间（UTC 秒）。读不到就是 None，界面显示成「—」。
    pub modified: Option<u64>,
    pub is_dir: bool,
}

/// 列出目录里的文件，只收指定扩展名。
///
/// `extensions` 传空就是全都要。名字以 `.` 开头的（下载残留、编辑器临时文件）
/// 一律跳过 —— 那些不是用户装进去的东西，列出来只会让人误删。
///
/// 排序：按修改时间新的在前；时间一样或都没有就按名字，结果稳定。
pub fn list_files(dir: &Path, extensions: &[&str]) -> Vec<Item> {
    let mut items = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return items;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if !extensions.is_empty() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .unwrap_or_default();
            if !extensions.iter().any(|want| *want == ext) {
                continue;
            }
        }
        items.push(Item {
            name: name.to_string(),
            path,
            bytes: meta.len(),
            modified: modified_secs(&meta),
            is_dir: false,
        });
    }
    sort_items(&mut items);
    items
}

/// 列出目录里的子目录（存档就是一个目录一个世界）。
pub fn list_dirs(dir: &Path) -> Vec<Item> {
    let mut items = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return items;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        items.push(Item {
            name: name.to_string(),
            path,
            bytes: dir_size(&entry.path()),
            modified: modified_secs(&meta),
            is_dir: true,
        });
    }
    sort_items(&mut items);
    items
}

/// 删掉一个条目：文件删文件，目录递归删。
///
/// 只接受 `list_files` / `list_dirs` 给出来的那种路径 —— 调用方不该拿它去删
/// 游戏目录本身，所以这里再加一道「不能删到只剩盘符」的检查。
pub fn remove_item(path: &Path) -> std::io::Result<()> {
    if path.parent().is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "拒绝删除根目录",
        ));
    }
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn modified_secs(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

fn sort_items(items: &mut [Item]) {
    items.sort_by(|a, b| match (b.modified, a.modified) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.name.cmp(&b.name)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.name.cmp(&b.name),
    });
}

/// 目录总大小。只为显示，遇到读不了的项就跳过，不报错。
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_keeps_only_the_wanted_files() {
        let tmp = crate::testutil::fresh_dir("inst-list");
        let mods = tmp.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("sodium.jar"), b"12345").unwrap();
        std::fs::write(mods.join("notes.txt"), b"x").unwrap();
        std::fs::write(mods.join(".hidden.jar"), b"x").unwrap();

        let items = list_files(&mods, &["jar"]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "sodium.jar");
        assert_eq!(items[0].bytes, 5);
        assert!(!items[0].is_dir);

        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn listing_saves_counts_the_whole_folder() {
        let tmp = crate::testutil::fresh_dir("inst-saves");
        let world = tmp.join("saves").join("世界一");
        std::fs::create_dir_all(world.join("region")).unwrap();
        std::fs::write(world.join("level.dat"), b"abcd").unwrap();
        std::fs::write(world.join("region").join("r.0.0.mca"), b"abcdef").unwrap();

        let items = list_dirs(&tmp.join("saves"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "世界一");
        assert_eq!(items[0].bytes, 10);
        assert!(items[0].is_dir);

        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn listing_a_missing_folder_is_empty_not_an_error() {
        let tmp = crate::testutil::fresh_dir("inst-missing");
        assert!(list_files(&tmp.join("mods"), &["jar"]).is_empty());
        assert!(list_dirs(&tmp.join("saves")).is_empty());
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn removing_an_item_handles_files_and_folders() {
        let tmp = crate::testutil::fresh_dir("inst-rm");
        let jar = tmp.join("a.jar");
        std::fs::write(&jar, b"x").unwrap();
        let world = tmp.join("saves").join("w");
        std::fs::create_dir_all(&world).unwrap();
        std::fs::write(world.join("level.dat"), b"x").unwrap();

        remove_item(&jar).unwrap();
        remove_item(&world).unwrap();
        assert!(!jar.exists());
        assert!(!world.exists());

        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn removing_a_root_path_is_refused() {
        assert!(remove_item(Path::new("C:\\")).is_err());
    }

    #[test]
    fn fresh_version_is_shared() {
        let tmp = crate::testutil::fresh_dir("inst-fresh");
        let vdir = tmp.join("versions").join("1.20.1");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("1.20.1.json"), "{}").unwrap();
        std::fs::write(vdir.join("1.20.1.jar"), b"x").unwrap();

        // 只有清单和 jar，没有用户数据 -> 共享
        assert_eq!(detect_mode(&tmp, "1.20.1"), InstanceMode::Shared);
        assert_eq!(game_dir_for(&tmp, "1.20.1", InstanceMode::Shared), *tmp);

        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn version_with_options_is_isolated() {
        let tmp = crate::testutil::fresh_dir("inst-opt");
        let vdir = tmp.join("versions").join("1.20.1_Fabric");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("options.txt"), "fov:0\n").unwrap();

        assert_eq!(detect_mode(&tmp, "1.20.1_Fabric"), InstanceMode::Isolated);
        assert_eq!(
            game_dir_for(&tmp, "1.20.1_Fabric", InstanceMode::Isolated),
            vdir
        );

        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn mods_dir_also_marks_isolated() {
        let tmp = crate::testutil::fresh_dir("inst-mods");
        let vdir = tmp.join("versions").join("ATM10");
        std::fs::create_dir_all(vdir.join("mods")).unwrap();

        assert_eq!(detect_mode(&tmp, "ATM10"), InstanceMode::Isolated);
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn ensure_instance_creates_dirs() {
        let tmp = crate::testutil::fresh_dir("inst-ensure");
        let created = ensure_instance(&tmp, "1.8.9").unwrap();
        assert!(!created.is_empty(), "应创建目录");

        let vdir = tmp.join("versions").join("1.8.9");
        assert!(vdir.join("mods").is_dir());
        assert!(vdir.join("saves").is_dir());
        assert!(vdir.join("config").is_dir());
        assert_eq!(detect_mode(&tmp, "1.8.9"), InstanceMode::Isolated);

        // 再次调用不应重复创建
        let again = ensure_instance(&tmp, "1.8.9").unwrap();
        assert!(again.is_empty(), "已存在时不应再创建: {:?}", again);

        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn shared_and_isolated_differ() {
        let tmp = crate::testutil::fresh_dir("inst-diff");
        let iso = game_dir_for(&tmp, "v", InstanceMode::Isolated);
        let sha = game_dir_for(&tmp, "v", InstanceMode::Shared);
        assert_ne!(iso, sha);
        assert!(iso.ends_with("v"));

        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn labels_are_readable() {
        assert_eq!(InstanceMode::Isolated.as_str(), "隔离");
        assert_eq!(InstanceMode::Shared.as_str(), "共享");
    }
}
