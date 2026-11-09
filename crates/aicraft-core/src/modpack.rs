//! 整合包导入（Modrinth `.mrpack` 与 HMCL 导出格式）。
//!
//! 只认两种布局，靠压缩包里那个「清单元文件」的名字就能分辨：
//!
//! | 格式 | 清单元 | 内容前缀 |
//! |---|---|---|
//! | Modrinth | `modrinth.index.json` | `overrides/`、`client-overrides/` |
//! | HMCL | `modpack.json` + `minecraft/pack.json` | `minecraft/` |
//!
//! 判断顺序照抄 PCL 的（`ModModpack.vb` 里那张表），只是把用不上的
//! MCBBS / CurseForge / MultiMC 几种先跳过——多认一种格式就要多一套
//! 字段映射，而玩家手上的整合包绝大多数落在这两类里。
//!
//! 这里只做「读清单、算要下什么、铺文件」，真正下载与装加载器由调用方
//! 编排（见 `install` 与 `loader`）：解析和网络混在一起就没法单测了。

use std::path::{Path, PathBuf};

use crate::json::{self, Json};
use crate::loader::LoaderKind;
use crate::natives::ZipEntry;

/// 清单元文件名：见到哪个就是哪种包。
pub const MODRINTH_INDEX: &str = "modrinth.index.json";
pub const HMCL_MANIFEST: &str = "modpack.json";
pub const HMCL_PACK: &str = "minecraft/pack.json";

/// 整合包格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackFormat {
    Modrinth,
    Hmcl,
    CurseForge,
}

impl PackFormat {
    pub const ALL: [PackFormat; 3] = [
        PackFormat::Modrinth,
        PackFormat::Hmcl,
        PackFormat::CurseForge,
    ];

    pub fn id(self) -> &'static str {
        match self {
            PackFormat::Modrinth => "modrinth",
            PackFormat::Hmcl => "hmcl",
            PackFormat::CurseForge => "curseforge",
        }
    }

    /// 界面上怎么称呼。
    pub fn label(self) -> &'static str {
        match self {
            PackFormat::Modrinth => "Modrinth 整合包",
            PackFormat::Hmcl => "HMCL 整合包",
            PackFormat::CurseForge => "CurseForge 整合包",
        }
    }

    /// 这种格式要铺进实例目录的前缀（按顺序覆盖，后者盖前者）。
    pub fn overrides(self) -> &'static [&'static str] {
        match self {
            PackFormat::Modrinth => &["overrides/", "client-overrides/"],
            PackFormat::Hmcl => &["minecraft/"],
            PackFormat::CurseForge => &["overrides/"],
        }
    }

    /// 前缀之内、但不该铺进实例的文件（清单本身留着没用）。
    pub fn skipped(self) -> &'static [&'static str] {
        match self {
            PackFormat::Modrinth => &[],
            PackFormat::Hmcl => &[HMCL_PACK, "minecraft/modpack.json"],
            PackFormat::CurseForge => &[crate::curseforge::CURSEFORGE_MANIFEST],
        }
    }
}

/// 清单里点名要下载的一个文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackFile {
    /// 相对实例目录的落盘位置，如 `mods/xxx.jar`
    pub path: String,
    /// 从哪里下（清单里给了多个时取第一个能用的）
    pub url: String,
    /// 校验用；清单没给就是 `None`
    pub sha1: Option<String>,
    pub size: u64,
}

/// 解析出来的整合包摘要。
#[derive(Debug, Clone)]
pub struct PackManifest {
    pub format: PackFormat,
    /// 整合包自己的名字（不是文件名）
    pub name: String,
    /// 整合包版本号
    pub version: String,
    /// 它要的 Minecraft 版本
    pub game_version: String,
    /// 它要的加载器与版本
    pub loader: Option<(LoaderKind, String)>,
    /// 需要联网下载的文件
    pub files: Vec<PackFile>,
    /// CurseForge 专用的文件引用（projectID + fileID）。
    ///
    /// 别的格式这里是空的。CurseForge 的清单不给 URL，得再查一次 API——
    /// 那一步放到真正开装时做（`download_files` 里），因为 `inspect()` 是
    /// 「拖进来先看看」，不该在这时候就去打网络。
    pub cf_files: Vec<crate::curseforge::ManifestFile>,
}

impl PackManifest {
    /// 一句话概括「这是什么包」，给确认框用。
    pub fn summary(&self) -> String {
        let mut text = format!(
            "{} {}（{}，Minecraft {}",
            self.format.label(),
            self.name,
            self.version,
            self.game_version
        );
        match &self.loader {
            Some((kind, version)) => text.push_str(&format!("，{} {}", kind.label(), version)),
            None => text.push_str("，原版"),
        }
        let count = self.files.len() + self.cf_files.len();
        text.push_str(&format!("，{} 个文件）", count));
        text
    }
}

/// 看一眼压缩包里有什么，判断是哪种整合包。
///
/// 只认顶层（PCL 还会看一层目录，但那种包少见，多一层判断出错时
/// 反而会把普通压缩包误判成整合包）。
pub fn sniff(entries: &[ZipEntry]) -> Option<PackFormat> {
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e.name.trim_start_matches("./"))
        .collect();
    // 先判 Modrinth：PCL 导出的 .mrpack 也满足 HMCL 那条（有些包两个
    // 清单元都带），而 Modrinth 的清单信息更全，优先按它读。
    if names.contains(&MODRINTH_INDEX) {
        return Some(PackFormat::Modrinth);
    }
    if names.contains(&HMCL_MANIFEST) {
        return Some(PackFormat::Hmcl);
    }
    // CurseForge 的 `manifest.json` 和别的启动器重名，光看名字定不了，
    // 这里只当候选；`read_manifest` 会真读一遍确认 `manifestType`。
    if names.iter().any(|n| crate::curseforge::is_manifest_name(n)) {
        return Some(PackFormat::CurseForge);
    }
    None
}

/// 把清单里写的相对路径变成安全的落盘路径。
///
/// 整合包是别人做的，`path` 完全由作者控制。放行 `..` 或者绝对路径，
/// 一个包就能往实例目录外面写文件（PCL 也专门拦了这一手，报「整合包
/// 的文件路径超出了版本文件夹」）。这里宁可拒绝整包，也不修正路径——
/// 修正过的路径和作者预期不符，装出来一样是坏的。
pub fn safe_relative(path: &str) -> Option<PathBuf> {
    let text = path.trim().replace('\\', "/");
    if text.is_empty() {
        return None;
    }
    // 盘符（`C:`）与 UNC / 绝对路径
    if text.contains(':') || text.starts_with('/') {
        return None;
    }
    let mut out = PathBuf::new();
    for part in text.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 解析 `modrinth.index.json`。
pub fn parse_modrinth(text: &str) -> Result<PackManifest, String> {
    let root = json::parse(text).map_err(|e| format!("整合包清单不是合法 JSON: {}", e))?;

    let game_version = root
        .get("dependencies")
        .and_then(|d| d.get("minecraft"))
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| "整合包没写要哪个 Minecraft 版本".to_string())?;

    let loader = modrinth_loader(&root);

    let mut files = Vec::new();
    if let Some(list) = root.get("files").and_then(Json::as_array) {
        for item in list {
            let Some(path) = item.get("path").and_then(Json::as_str) else {
                continue;
            };
            // 服务端专用文件（`env.client == "unsupported"`）客户端不需要
            if let Some(client) = item
                .get("env")
                .and_then(|e| e.get("client"))
                .and_then(Json::as_str)
            {
                if client.eq_ignore_ascii_case("unsupported") {
                    continue;
                }
            }
            let Some(url) = item
                .get("downloads")
                .and_then(Json::as_array)
                .and_then(|urls| urls.first())
                .and_then(Json::as_str)
            else {
                // 没有下载地址的条目（少数包把它们全放 overrides 里）跳过
                continue;
            };
            if safe_relative(path).is_none() {
                return Err(format!("整合包的文件路径不合法：{}", path));
            }
            files.push(PackFile {
                path: path.to_string(),
                url: url.to_string(),
                sha1: item
                    .get("hashes")
                    .and_then(|h| h.get("sha1"))
                    .and_then(Json::as_str)
                    .map(str::to_string),
                size: item
                    .get("fileSize")
                    .and_then(Json::as_i64)
                    .unwrap_or(0)
                    .max(0) as u64,
            });
        }
    }

    Ok(PackManifest {
        format: PackFormat::Modrinth,
        name: text_of(&root, "name").unwrap_or_else(|| "未命名整合包".to_string()),
        version: text_of(&root, "versionId").unwrap_or_else(|| "未知版本".to_string()),
        game_version,
        loader,
        files,
        cf_files: Vec::new(),
    })
}

/// Modrinth 清单里的加载器：`fabric-loader` / `forge` / `neoforge`（也见过 `neo-forge`）。
fn modrinth_loader(root: &Json) -> Option<(LoaderKind, String)> {
    let deps = root.get("dependencies")?;
    for (key, kind) in [
        ("fabric-loader", LoaderKind::Fabric),
        ("forge", LoaderKind::Forge),
        ("neoforge", LoaderKind::NeoForge),
        ("neo-forge", LoaderKind::NeoForge),
    ] {
        if let Some(version) = deps.get(key).and_then(Json::as_str) {
            if !version.trim().is_empty() {
                return Some((kind, version.to_string()));
            }
        }
    }
    None
}

/// 解析 HMCL 整合包（`modpack.json` + `minecraft/pack.json`）。
///
/// `pack` 可以为空：有些导出只留 `modpack.json`，那就只拿得到名字和
/// 游戏版本，装成原版实例。
pub fn parse_hmcl(modpack: &str, pack: Option<&str>) -> Result<PackManifest, String> {
    let root =
        json::parse(modpack).map_err(|e| format!("整合包清单不是合法 JSON: {}", e))?;

    let pack_json = match pack {
        Some(text) if !text.trim().is_empty() => {
            Some(json::parse(text).map_err(|e| format!("pack.json 不是合法 JSON: {}", e))?)
        }
        _ => None,
    };

    // 三处都可能写着游戏版本，按准确性排：`jar.id` 是实例自己的底座，
    // 最准；`modpack.json` 的 `gameVersion` 是导出时记下的；最后才从
    // `inheritsFrom` 那串加载器 id 里剥（写法各家不同，最容易出错）。
    let game_version = pack_json
        .as_ref()
        .and_then(hmcl_jar_id)
        .or_else(|| text_of(&root, "gameVersion"))
        .or_else(|| pack_json.as_ref().and_then(hmcl_inherits_game))
        .ok_or_else(|| "整合包没写要哪个 Minecraft 版本".to_string())?;

    let loader = pack_json.as_ref().and_then(loader_from_pack);

    Ok(PackManifest {
        format: PackFormat::Hmcl,
        name: text_of(&root, "name").unwrap_or_else(|| "未命名整合包".to_string()),
        version: text_of(&root, "version").unwrap_or_else(|| "未知版本".to_string()),
        game_version,
        loader,
        // HMCL 把文件都打在压缩包里，不列下载清单
        files: Vec::new(),
        cf_files: Vec::new(),
    })
}

/// `pack.json` 自己的底座 id（`jar.id`），有它就不用猜。
fn hmcl_jar_id(pack: &Json) -> Option<String> {
    pack.get("jar")
        .and_then(|j| j.get("id"))
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// 从 `inheritsFrom`（`fabric-loader-0.15.11-1.20.1` 这类）里剥出游戏版本。
///
/// Fabric 的格式是「加载器版本在前、游戏版本在后」，所以取最后一段；
/// 纯版本号（`1.20.1`）原样返回。
fn hmcl_inherits_game(pack: &Json) -> Option<String> {
    let base = pack.get("inheritsFrom").and_then(Json::as_str)?.trim();
    if base.is_empty() {
        return None;
    }
    let tail = base.rsplit('-').next()?.trim();
    if tail.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        Some(tail.to_string())
    } else {
        None
    }
}

/// 从 `pack.json` 里认出加载器与版本。
///
/// 先看 `inheritsFrom`（HMCL 命名是 `fabric-loader-0.20.1-26.3` 这种），
/// 再看 `libraries` 里的坐标——两种写法 HMCL 都用过。
fn loader_from_pack(pack: &Json) -> Option<(LoaderKind, String)> {
    if let Some(base) = pack.get("inheritsFrom").and_then(Json::as_str) {
        if let Some(found) = parse_loader_id(base) {
            return Some(found);
        }
    }
    let libraries = pack.get("libraries").and_then(Json::as_array)?;
    for lib in libraries {
        let Some(name) = lib.get("name").and_then(Json::as_str) else {
            continue;
        };
        if let Some(found) = parse_loader_coordinate(name) {
            return Some(found);
        }
    }
    None
}

/// 认 `fabric-loader-0.20.1-26.3` / `forge-26.3-61.0.1` / `neoforge-21.1.72` 这种 id。
pub fn parse_loader_id(id: &str) -> Option<(LoaderKind, String)> {
    let lower = id.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("fabric-loader-") {
        // `fabric-loader-<loader>-<game>`，加载器版本里通常没有 `-`
        let version = rest.split('-').next()?;
        if !version.is_empty() {
            return Some((LoaderKind::Fabric, version.to_string()));
        }
        return None;
    }
    if let Some(rest) = lower.strip_prefix("neoforge-") {
        if !rest.is_empty() {
            return Some((LoaderKind::NeoForge, rest.to_string()));
        }
        return None;
    }
    if let Some(rest) = lower.strip_prefix("forge-") {
        // `forge-<game>-<forge>`，中间那段是游戏版本
        let mut parts = rest.split('-');
        let _game = parts.next()?;
        let version = parts.next()?;
        if !version.is_empty() {
            return Some((LoaderKind::Forge, version.to_string()));
        }
    }
    None
}

/// 认 maven 坐标 `net.fabricmc:fabric-loader:0.20.1` 这种。
pub fn parse_loader_coordinate(name: &str) -> Option<(LoaderKind, String)> {
    let mut parts = name.split(':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    let version = parts.next()?;
    if version.is_empty() {
        return None;
    }
    let group = group.to_ascii_lowercase();
    let artifact = artifact.to_ascii_lowercase();
    let kind = match (group.as_str(), artifact.as_str()) {
        ("net.fabricmc", "fabric-loader") => LoaderKind::Fabric,
        ("net.minecraftforge", "forge") => LoaderKind::Forge,
        ("net.neoforged", "neoforge") => LoaderKind::NeoForge,
        _ => return None,
    };
    // forge 的坐标版本是 `<game>-<forge>`，只要后半段
    let version = if kind == LoaderKind::Forge {
        version.rsplit_once('-').map(|(_, v)| v).unwrap_or(version)
    } else {
        version
    };
    Some((kind, version.to_string()))
}

/// 把压缩包里的 `overrides/`（或 `minecraft/`）铺进实例目录。
///
/// 返回铺了多少个文件。已存在的同名文件会被覆盖——整合包自带的就是
/// 它想让你用的那份配置。
pub fn extract_overrides(
    jar: &Path,
    entries: &[ZipEntry],
    manifest: &PackManifest,
    dest: &Path,
) -> Result<usize, String> {
    let prefixes = manifest.format.overrides();
    let skipped = manifest.format.skipped();
    let mut written = 0usize;

    for entry in entries {
        if entry.is_dir {
            continue;
        }
        let name = entry.name.trim_start_matches("./").replace('\\', "/");
        if skipped.iter().any(|s| *s == name) {
            continue;
        }
        let Some(prefix) = prefixes.iter().find(|p| name.starts_with(*p)) else {
            continue;
        };
        let rel = &name[prefix.len()..];
        let Some(rel) = safe_relative(rel) else {
            return Err(format!("整合包的文件路径不合法：{}", name));
        };
        let target = dest.join(&rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建目录失败: {}（{}）", e, parent.display()))?;
        }
        let data = entry
            .read_data(jar)
            .map_err(|e| format!("解压 {} 失败: {}", name, e))?;
        std::fs::write(&target, data)
            .map_err(|e| format!("写入 {} 失败: {}", target.display(), e))?;
        written += 1;
    }
    Ok(written)
}

fn text_of(root: &Json, key: &str) -> Option<String> {
    root.get(key)
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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

    const MODRINTH: &str = r##"{
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": "15.0.0",
        "name": "Fabulously Optimized",
        "dependencies": { "minecraft": "26.3", "fabric-loader": "0.20.1" },
        "files": [
            {
                "path": "mods/sodium.jar",
                "hashes": { "sha1": "abc123" },
                "env": { "client": "required", "server": "required" },
                "downloads": ["https://cdn.modrinth.com/data/AANobbMI/versions/x/sodium.jar"],
                "fileSize": 1234567
            },
            {
                "path": "mods/serveronly.jar",
                "hashes": {},
                "env": { "client": "unsupported", "server": "required" },
                "downloads": ["https://example.invalid/serveronly.jar"],
                "fileSize": 10
            },
            {
                "path": "mods/optional.jar",
                "hashes": {},
                "env": { "client": "optional", "server": "unsupported" },
                "downloads": ["https://example.invalid/optional.jar"]
            }
        ]
    }"##;

    #[test]
    fn a_modrinth_pack_reads_what_the_wizard_needs() {
        let pack = parse_modrinth(MODRINTH).unwrap();
        assert_eq!(pack.format, PackFormat::Modrinth);
        assert_eq!(pack.name, "Fabulously Optimized");
        assert_eq!(pack.version, "15.0.0");
        assert_eq!(pack.game_version, "26.3");
        assert_eq!(
            pack.loader,
            Some((LoaderKind::Fabric, "0.20.1".to_string()))
        );
        // 服务端专用那条被跳过；optional 的留着（客户端也要）
        assert_eq!(pack.files.len(), 2);
        assert_eq!(pack.files[0].path, "mods/sodium.jar");
        assert_eq!(pack.files[0].sha1.as_deref(), Some("abc123"));
        assert_eq!(pack.files[0].size, 1_234_567);
    }

    #[test]
    fn client_only_files_are_kept_and_optional_ones_too() {
        let text = r##"{
            "dependencies": { "minecraft": "1.21.1", "neoforge": "21.1.72" },
            "files": [
                { "path": "mods/a.jar", "env": { "client": "required" },
                  "downloads": ["https://a"] },
                { "path": "mods/b.jar", "env": { "client": "optional" },
                  "downloads": ["https://b"] }
            ]
        }"##;
        let pack = parse_modrinth(text).unwrap();
        assert_eq!(pack.files.len(), 2);
        assert_eq!(
            pack.loader,
            Some((LoaderKind::NeoForge, "21.1.72".to_string()))
        );
    }

    #[test]
    fn a_pack_without_a_game_version_is_refused() {
        let err = parse_modrinth(r##"{ "name": "x", "dependencies": {} }"##).unwrap_err();
        assert!(err.contains("Minecraft 版本"), "{}", err);
        assert!(parse_modrinth("not json").is_err());
    }

    #[test]
    fn one_bad_path_rejects_the_whole_pack() {
        // 修正过的路径和作者预期不符，装出来一样是坏的；宁可整包不收。
        let text = r##"{
            "dependencies": { "minecraft": "1.20.1" },
            "files": [
                { "path": "mods/ok.jar", "downloads": ["https://ok"] },
                { "path": "../escape.jar", "downloads": ["https://bad"] }
            ]
        }"##;
        let err = parse_modrinth(text).unwrap_err();
        assert!(err.contains("不合法"), "{}", err);
    }

    #[test]
    fn a_file_path_may_not_climb_out_of_the_instance() {
        assert!(safe_relative("mods/a.jar").is_some());
        assert_eq!(
            safe_relative("./mods/a.jar").unwrap(),
            PathBuf::from("mods/a.jar")
        );
        assert!(safe_relative("mods\\a.jar").is_some());
        // 想往上跑的、绝对路径、盘符，全部拒绝
        assert!(safe_relative("../a.jar").is_none());
        assert!(safe_relative("mods/../../a.jar").is_none());
        assert!(safe_relative("/etc/passwd").is_none());
        assert!(safe_relative("C:/Windows/a.jar").is_none());
        assert!(safe_relative("").is_none());
        assert!(safe_relative("   ").is_none());
    }

    const HMCL_MODPACK: &str = r##"{
        "manifestType": "HMCL",
        "manifestVersion": 1,
        "name": "我的整合包",
        "version": "1.2.3",
        "author": "某人",
        "description": "",
        "gameVersion": "1.20.1"
    }"##;

    const HMCL_PACK_JSON: &str = r##"{
        "id": "1.20.1-Fabric",
        "inheritsFrom": "fabric-loader-0.15.11-1.20.1",
        "libraries": [
            { "name": "net.fabricmc:intermediary:1.20.1" }
        ]
    }"##;

    #[test]
    fn an_hmcl_pack_reads_name_version_and_loader() {
        let pack = parse_hmcl(HMCL_MODPACK, Some(HMCL_PACK_JSON)).unwrap();
        assert_eq!(pack.format, PackFormat::Hmcl);
        assert_eq!(pack.name, "我的整合包");
        assert_eq!(pack.version, "1.2.3");
        assert_eq!(pack.game_version, "1.20.1");
        assert_eq!(
            pack.loader,
            Some((LoaderKind::Fabric, "0.15.11".to_string()))
        );
        assert!(pack.files.is_empty());
    }

    #[test]
    fn an_hmcl_pack_without_pack_json_still_imports_as_vanilla() {
        let pack = parse_hmcl(HMCL_MODPACK, None).unwrap();
        assert_eq!(pack.game_version, "1.20.1");
        assert!(pack.loader.is_none());
    }

    #[test]
    fn loader_ids_and_coordinates_are_both_understood() {
        assert_eq!(
            parse_loader_id("fabric-loader-0.20.1-26.3"),
            Some((LoaderKind::Fabric, "0.20.1".to_string()))
        );
        assert_eq!(
            parse_loader_id("forge-1.20.1-47.2.0"),
            Some((LoaderKind::Forge, "47.2.0".to_string()))
        );
        assert_eq!(
            parse_loader_id("neoforge-21.1.72"),
            Some((LoaderKind::NeoForge, "21.1.72".to_string()))
        );
        assert_eq!(parse_loader_id("26.3"), None);

        assert_eq!(
            parse_loader_coordinate("net.fabricmc:fabric-loader:0.20.1"),
            Some((LoaderKind::Fabric, "0.20.1".to_string()))
        );
        assert_eq!(
            parse_loader_coordinate("net.minecraftforge:forge:1.20.1-47.2.0"),
            Some((LoaderKind::Forge, "47.2.0".to_string()))
        );
        assert_eq!(
            parse_loader_coordinate("net.neoforged:neoforge:21.1.72"),
            Some((LoaderKind::NeoForge, "21.1.72".to_string()))
        );
        assert_eq!(parse_loader_coordinate("com.google.guava:guava:32.1"), None);
        assert_eq!(parse_loader_coordinate("bad"), None);
    }

    #[test]
    fn a_curseforge_pack_is_recognised_by_its_manifest() {
        let entries = vec![
            entry("manifest.json"),
            entry("overrides/config/x.toml"),
        ];
        assert_eq!(sniff(&entries), Some(PackFormat::CurseForge));
    }

    #[test]
    fn a_curseforge_manifest_becomes_a_manifest() {
        let text = r##"{
            "manifestType": "minecraftModpack",
            "manifestVersion": 1,
            "name": "All the Mods 10",
            "version": "2.0.0",
            "minecraft": {
                "version": "1.21.1",
                "modLoaders": [{ "id": "fabric-0.15.11", "primary": true }]
            },
            "files": [
                { "projectID": 394468, "fileID": 8756581, "required": true }
            ]
        }"##;
        let found = crate::curseforge::parse_manifest(text).unwrap().unwrap();
        let manifest = parse_curseforge(found);
        assert_eq!(manifest.format, PackFormat::CurseForge);
        assert_eq!(manifest.name, "All the Mods 10");
        assert_eq!(manifest.game_version, "1.21.1");
        assert_eq!(
            manifest.loader,
            Some((LoaderKind::Fabric, "0.15.11".to_string()))
        );
        // CurseForge 的文件走 cf_files，不带 URL
        assert!(manifest.files.is_empty());
        assert_eq!(manifest.cf_files.len(), 1);
    }

    #[test]
    fn curseforge_loader_ids_are_translated() {
        assert_eq!(cf_loader_id("fabric-0.15.11"), "fabric-loader-0.15.11");
        assert_eq!(cf_loader_id("neoforge-21.1.72"), "neoforge-21.1.72");
        assert_eq!(cf_loader_id("forge-47.2.0"), "forge-x-47.2.0");
        // 认不出的原样返回，别把信息弄丢
        assert_eq!(cf_loader_id("quilt-1.0"), "quilt-1.0");
        // 走一遍解析链，确认三种都能落到对的加载器上
        assert_eq!(
            parse_loader_id(&cf_loader_id("fabric-0.15.11")),
            Some((LoaderKind::Fabric, "0.15.11".to_string()))
        );
        assert_eq!(
            parse_loader_id(&cf_loader_id("neoforge-21.1.72")),
            Some((LoaderKind::NeoForge, "21.1.72".to_string()))
        );
        assert_eq!(
            parse_loader_id(&cf_loader_id("forge-47.2.0")),
            Some((LoaderKind::Forge, "47.2.0".to_string()))
        );
    }

    #[test]
    fn the_manifest_file_decides_the_format() {
        assert_eq!(
            sniff(&[entry("modrinth.index.json")]),
            Some(PackFormat::Modrinth)
        );
        assert_eq!(
            sniff(&[entry("modpack.json"), entry("minecraft/pack.json")]),
            Some(PackFormat::Hmcl)
        );
        assert_eq!(sniff(&[entry("readme.txt")]), None);
        // 只认顶层：藏在子目录里的不算
        assert_eq!(sniff(&[entry("deep/modrinth.index.json")]), None);
    }

    #[test]
    fn the_summary_says_what_will_be_installed() {
        let pack = parse_modrinth(MODRINTH).unwrap();
        let text = pack.summary();
        assert!(text.contains("Fabulously Optimized"));
        assert!(text.contains("26.3"));
        assert!(text.contains("Fabric 0.20.1"));
        let vanilla = parse_hmcl(HMCL_MODPACK, None).unwrap();
        assert!(vanilla.summary().contains("原版"));
    }
}

// ---------------------------------------------------------------------------
// 安装编排
// ---------------------------------------------------------------------------

use crate::cancel::CancelToken;
use crate::loader::LoaderProgress;
use crate::settings::Mirror;
use crate::speed::Budget;

/// 导入一份整合包要用的全部设置。
pub struct ImportConfig<'a> {
    pub game_dir: &'a Path,
    /// 要装成的实例名（调用方已经处理过重名）
    pub instance: &'a str,
    pub mirror: Mirror,
    /// 加载器自己的下载源（可以是与 `mirror` 不同的镜像）
    pub loader_mirror: Mirror,
    pub concurrency: usize,
    pub limit_kbps: Option<u32>,
    pub cancel: &'a CancelToken,
}

/// 导入过程中给界面的进度。
#[derive(Debug, Clone)]
pub enum PackProgress {
    /// 正在做哪一步（阶段名）
    Stage(String),
    /// 版本文件的下载进度
    Install { done: usize, total: usize, bytes: u64 },
    /// 整合包自带文件的下载进度
    Pack { done: usize, total: usize },
    /// 加载器安装进度
    Loader(LoaderProgress),
}

/// 导入结果。
#[derive(Debug, Clone)]
pub struct ImportReport {
    /// 最终落盘的实例名
    pub instance: String,
    /// 从压缩包里铺出来的文件数
    pub overrides: usize,
    /// 按清单下载成功的文件数
    pub downloaded: usize,
    /// 下载失败的条目
    pub failures: Vec<(String, String)>,
    /// 玩家中途取消
    pub cancelled: bool,
}

impl ImportReport {
    pub fn is_success(&self) -> bool {
        !self.cancelled && self.failures.is_empty()
    }
}

/// 导入一份整合包（从压缩包文件一路装到能启动）。
///
/// 顺序有讲究：**先装原版**再铺文件。`ensure_instance` 会建 `mods/`
/// `config/` 这些子目录，先铺文件再建目录的话，用户自己塞进去的东西
/// 不会丢，但整合包带的 `config/` 就被当成「已存在」了——反过来说，
/// 先建目录再铺文件更符合「整合包说了算」的语义。
///
/// 加载器最后装：Forge/NeoForge 的安装器要往 `versions/<实例>/` 里写
/// 版本 JSON，必须在原版那一份已经就位之后。
pub fn import_pack<F>(
    archive: &Path,
    config: &ImportConfig<'_>,
    on_progress: &(dyn Fn(PackProgress) + Send + Sync),
    mut on_log: F,
) -> Result<ImportReport, String>
where
    F: FnMut(&str),
{
    if config.cancel.is_cancelled() {
        return Ok(ImportReport {
            instance: config.instance.to_string(),
            overrides: 0,
            downloaded: 0,
            failures: Vec::new(),
            cancelled: true,
        });
    }

    on_progress(PackProgress::Stage("读取整合包".to_string()));
    let entries = crate::natives::read_zip_entries(archive)
        .map_err(|e| format!("这个文件不是能读的压缩包: {}", e))?;
    let format = sniff(&entries).ok_or_else(|| {
        "认不出这是什么整合包。目前支持 Modrinth（.mrpack）和 HMCL 导出的整合包。".to_string()
    })?;
    on_log(&format!("认出来了：{}", format.label()));

    let manifest = read_manifest(archive, &entries, format)?;
    on_log(&manifest.summary());

    if config.cancel.is_cancelled() {
        return Ok(cancelled(config.instance));
    }

    // 1. 原版：整合包只写「我要 26.3」，底座得自己装。
    on_progress(PackProgress::Stage(format!(
        "安装 Minecraft {}",
        manifest.game_version
    )));
    install_base(config, &manifest, on_progress, &mut on_log)?;
    if config.cancel.is_cancelled() {
        return Ok(cancelled(config.instance));
    }

    // 2. 加载器
    if let Some((kind, version)) = manifest.loader.clone() {
        on_progress(PackProgress::Stage(format!(
            "安装 {} {}",
            kind.label(),
            version
        )));
        install_loader(config, &manifest.game_version, kind, &version, on_progress, &mut on_log)?;
        if config.cancel.is_cancelled() {
            return Ok(cancelled(config.instance));
        }
    }

    // 3. 铺 overrides。目录在装原版时已经建好（见 `install_base` 里的
    //    `ensure_instance`），这里只往里写文件。
    on_progress(PackProgress::Stage("铺开整合包自带的文件".to_string()));
    let instance_dir = crate::instance::version_dir(config.game_dir, config.instance);
    std::fs::create_dir_all(&instance_dir)
        .map_err(|e| format!("创建实例目录失败: {}（{}）", e, instance_dir.display()))?;
    let overrides = extract_overrides(archive, &entries, &manifest, &instance_dir)?;
    on_log(&format!("铺开 {} 个自带文件", overrides));

    // 4. 下载清单点名的文件
    let (downloaded, failures) = download_files(config, &manifest, &instance_dir, on_progress, &mut on_log)?;

    Ok(ImportReport {
        instance: config.instance.to_string(),
        overrides,
        downloaded,
        failures,
        cancelled: config.cancel.is_cancelled(),
    })
}

fn cancelled(instance: &str) -> ImportReport {
    ImportReport {
        instance: instance.to_string(),
        overrides: 0,
        downloaded: 0,
        failures: Vec::new(),
        cancelled: true,
    }
}

/// 按格式读清单。
/// 只读清单，不碰磁盘。
///
/// 界面在「拖进来之后、真正开装之前」要先把它摊给玩家看：什么包、要哪个
/// 游戏版本、要不要装加载器。这一步不该有任何副作用——玩家看完可能就
/// 点取消了。
pub fn inspect(archive: &Path) -> Result<PackManifest, String> {
    let entries = crate::natives::read_zip_entries(archive)
        .map_err(|e| format!("这个文件不是能读的压缩包: {}", e))?;
    let format = sniff(&entries).ok_or_else(|| {
        "认不出这是什么整合包。目前支持 Modrinth（.mrpack）和 HMCL 导出的整合包。".to_string()
    })?;
    read_manifest(archive, &entries, format)
}

fn read_manifest(
    archive: &Path,
    entries: &[ZipEntry],
    format: PackFormat,
) -> Result<PackManifest, String> {
    let read = |name: &str| -> Result<Option<String>, String> {
        let Some(entry) = entries.iter().find(|e| e.name == name) else {
            return Ok(None);
        };
        let bytes = entry
            .read_data(archive)
            .map_err(|e| format!("解压 {} 失败: {}", name, e))?;
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| format!("{} 不是 UTF-8 文本", name))
    };

    match format {
        PackFormat::Modrinth => {
            let text = read(MODRINTH_INDEX)?.ok_or_else(|| "压缩包里没有清单".to_string())?;
            parse_modrinth(&text)
        }
        PackFormat::Hmcl => {
            let text = read(HMCL_MANIFEST)?.ok_or_else(|| "压缩包里没有清单".to_string())?;
            let pack = read(HMCL_PACK)?;
            parse_hmcl(&text, pack.as_deref())
        }
        PackFormat::CurseForge => {
            let text = read(crate::curseforge::CURSEFORGE_MANIFEST)?
                .ok_or_else(|| "压缩包里没有清单".to_string())?;
            match crate::curseforge::parse_manifest(&text)? {
                Some(found) => Ok(parse_curseforge(found)),
                // `sniff` 只是按文件名挑的候选，这里发现不是 CurseForge
                // 就得说清楚，不然玩家只会看到「认不出这是什么整合包」。
                None => Err(
                    "这个 manifest.json 不是 CurseForge 的整合包清单（manifestType 对不上）"
                        .to_string(),
                ),
            }
        }
    }
}

/// 把 CurseForge 清单折成统一的 [`PackManifest`]。
fn parse_curseforge(found: crate::curseforge::Manifest) -> PackManifest {
    // `modLoaders` 里是 `fabric-0.15.11` / `forge-47.2.0` / `neoforge-21.1.72`
    // 这种写法，和 `parse_loader_id` 认的格式差一段，先按它试，
    // 不行再从项目里的库坐标找。
    let loader = found
        .loaders
        .iter()
        .find_map(|id| parse_loader_id(&cf_loader_id(id)));

    PackManifest {
        format: PackFormat::CurseForge,
        name: found.name,
        version: found.version,
        game_version: found.game_version,
        loader,
        files: Vec::new(),
        cf_files: found.files,
    }
}

/// CurseForge 的加载器 id → 我们认的那种写法。
///
/// `fabric-0.15.11` 里没有游戏版本，而 `parse_loader_id` 对 fabric 要求
/// `fabric-loader-<ver>-<game>`。所以这里做个中转：把 `forge-47.2.0`
/// 也当成 `neoforge` 之外的同构写法单独认，认不出的原样返回。
fn cf_loader_id(id: &str) -> String {
    let id = id.trim();
    let lower = id.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("fabric-") {
        // 统一成 `fabric-loader-<ver>`，`parse_loader_id` 会取第一段
        return format!("fabric-loader-{}", rest);
    }
    if lower.starts_with("neoforge-") {
        return lower;
    }
    if let Some(rest) = lower.strip_prefix("forge-") {
        // `forge-47.2.0`：`parse_loader_id` 的格式是 `forge-<game>-<ver>`，
        // 缺游戏版本，用 `-` 占位让它按「第二段是版本」取到。
        return format!("forge-x-{}", rest);
    }
    id.to_string()
}

/// 装原版底座，并顺手把实例目录建出来。
///
/// 版本号不在官方清单里说明是坏包（或者是很老的版本），直接报错比装一半好。
fn install_base<F>(
    config: &ImportConfig<'_>,
    manifest: &PackManifest,
    on_progress: &(dyn Fn(PackProgress) + Send + Sync),
    on_log: &mut F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    let game_version = manifest.game_version.as_str();
    let manifest_text = crate::transport::fetch_text(
        &config.mirror.rewrite(crate::install::VERSION_MANIFEST_URL),
    )
    .map_err(|e| format!("获取版本清单失败: {}", e))?;
    let list = crate::install::parse_manifest(&manifest_text)?;
    let summary = list.find(game_version).ok_or_else(|| {
        format!(
            "官方版本清单里没有 {}，可能整合包写的版本号不对",
            game_version
        )
    })?;

    on_log(&format!("下载 {} 的清单…", game_version));
    let version_json = crate::transport::fetch_text(&config.mirror.rewrite(&summary.url))
        .map_err(|e| format!("下载版本 JSON 失败: {}", e))?;
    let parsed = crate::version::parse_version(&version_json)
        .map_err(|e| format!("解析版本 JSON 失败: {}", e))?;

    // 加载器层要 `inheritsFrom` 指向原版，所以原版必须以官方名装一份。
    let base_instance = if manifest.loader.is_some() {
        game_version.to_string()
    } else {
        config.instance.to_string()
    };

    if !crate::install::is_installed(config.game_dir, &base_instance) {
        let index_text = match &parsed.asset_index.url {
            Some(url) => {
                on_log("下载资源索引…");
                Some(
                    crate::transport::fetch_text(&config.mirror.rewrite(url))
                        .map_err(|e| format!("下载资源索引失败: {}", e))?,
                )
            }
            None => None,
        };

        let ctx = crate::rules::RuleContext::current();
        let plan = crate::install::plan_install_as(
            config.game_dir,
            &version_json,
            index_text.as_deref(),
            &ctx,
            Some(&base_instance),
        )?;
        on_log(&format!(
            "需要下载 {} 个文件，约 {:.1} MB",
            plan.items.len(),
            plan.total_bytes as f64 / 1_048_576.0
        ));

        let total = plan.items.len();
        let budget = Budget::new(config.limit_kbps);
        let report = crate::install::execute_install_live(
            &plan,
            config.concurrency,
            &config.mirror,
            &budget,
            config.cancel,
            &|ev| {
                if let crate::install::InstallProgress::Tick { done, bytes, .. } = ev {
                    on_progress(PackProgress::Install { done, total, bytes });
                }
            },
        )?;
        if report.cancelled {
            return Ok(());
        }
        if !report.is_success() {
            return Err(format!("{} 个文件下载失败", report.failed));
        }
    } else {
        on_log(&format!("{} 已经装过，跳过", base_instance));
    }

    // 实例目录（`versions/<实例>/mods` 等）得先有，否则下一步铺文件时
    // 会散落在版本目录里。
    let _ = crate::instance::ensure_instance(config.game_dir, config.instance);
    Ok(())
}

fn install_loader<F>(
    config: &ImportConfig<'_>,
    game_version: &str,
    kind: LoaderKind,
    version: &str,
    on_progress: &(dyn Fn(PackProgress) + Send + Sync),
    on_log: &mut F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    let mut line = |s: &str| on_log(s);
    let progress = |ev: LoaderProgress| on_progress(PackProgress::Loader(ev));

    let outcome = match kind {
        LoaderKind::Fabric => crate::loader::install_fabric(
            config.game_dir,
            game_version,
            version,
            &config.loader_mirror,
            config.concurrency,
            Some(config.instance),
            config.cancel,
            &progress,
            &mut line,
        )?,
        LoaderKind::Forge => crate::loader::install_forge(
            config.game_dir,
            game_version,
            version,
            config.loader_mirror,
            Some(config.instance),
            config.cancel,
            &progress,
            &mut line,
        )?,
        LoaderKind::NeoForge => crate::loader::install_neoforge(
            config.game_dir,
            version,
            config.loader_mirror,
            Some(config.instance),
            config.cancel,
            &progress,
            &mut line,
        )?,
    };

    if outcome.cancelled {
        return Ok(());
    }
    if outcome.instance != config.instance {
        on_log(&format!(
            "注意：加载器实际落成了 {}，不是 {}",
            outcome.instance, config.instance
        ));
    }
    Ok(())
}

/// 把清单里点名的文件逐个下到实例目录。
///
/// 单个文件失败不打断整包——一个模组下不下来，玩家至少还能进游戏，
/// 失败清单在最后一起报。
fn download_files(
    config: &ImportConfig<'_>,
    manifest: &PackManifest,
    instance_dir: &Path,
    on_progress: &(dyn Fn(PackProgress) + Send + Sync),
    on_log: &mut dyn FnMut(&str),
) -> Result<(usize, Vec<(String, String)>), String> {
    // CurseForge 的清单只有 projectID/fileID，下载地址要现查。
    // 先解析成统一的任务列表，后面的循环对两种格式一视同仁。
    let tasks = gather_tasks(config, manifest, on_progress, on_log)?;
    let total = tasks.len();
    if total == 0 {
        return Ok((0, Vec::new()));
    }
    on_log(&format!("下载 {} 个清单文件…", total));

    let budget = Budget::new(config.limit_kbps);
    let mut downloaded = 0;
    let mut failures = Vec::new();

    for (i, file) in tasks.iter().enumerate() {
        if config.cancel.is_cancelled() {
            break;
        }
        on_progress(PackProgress::Pack {
            done: i,
            total,
        });

        let Some(rel) = safe_relative(&file.path) else {
            failures.push((file.path.clone(), "路径不合法".to_string()));
            continue;
        };
        let dest = instance_dir.join(rel);

        // 已经下过的（同一个整合包导两次）跳过
        if dest.exists() {
            if let Some(sha1) = &file.sha1 {
                if let Ok(actual) = crate::sha1::sha1_file_hex(&dest) {
                    if actual.eq_ignore_ascii_case(sha1) {
                        downloaded += 1;
                        continue;
                    }
                }
            } else {
                downloaded += 1;
                continue;
            }
        }

        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                failures.push((file.path.clone(), format!("建目录失败: {}", e)));
                continue;
            }
        }

        let share = budget.enter();
        let result = crate::transport::download_with_cancel(
            &file.url,
            &dest,
            file.sha1.as_deref(),
            share.kbps(),
            config.cancel,
        );
        drop(share);

        match result {
            Ok(_) => downloaded += 1,
            Err(e) if e.is_cancelled() => break,
            Err(e) => {
                on_log(&format!("  {} 下载失败：{}", file.path, e));
                failures.push((file.path.clone(), e.to_string()));
            }
        }
    }

    on_progress(PackProgress::Pack {
        done: total,
        total,
    });
    Ok((downloaded, failures))
}

/// 把一份清单变成「下哪个地址、放哪个文件」的列表。
///
/// Modrinth / HMCL 的清单自带 URL，照搬就行；CurseForge 得先查一次 API。
/// CurseForge 的清单不写落盘路径——那边约定俗成全是模组，一律进 `mods/`。
fn gather_tasks(
    config: &ImportConfig<'_>,
    manifest: &PackManifest,
    on_progress: &(dyn Fn(PackProgress) + Send + Sync),
    on_log: &mut dyn FnMut(&str),
) -> Result<Vec<PackFile>, String> {
    let mut tasks = manifest.files.clone();

    if !manifest.cf_files.is_empty() {
        if config.cancel.is_cancelled() {
            return Ok(tasks);
        }
        on_log(&format!(
            "查 {} 个 CurseForge 文件的下载地址…",
            manifest.cf_files.len()
        ));
        on_progress(PackProgress::Stage(
            "查 CurseForge 下载地址".to_string(),
        ));
        let resolved = crate::curseforge::resolve_files(&manifest.cf_files)?;

        // 连文件名都查不到的，只能让玩家自己去网页下。
        // 这种必须点名说出来：悄悄少装一个模组，玩家得进游戏才发现。
        let blocked: Vec<String> = resolved
            .iter()
            .filter(|f| f.url.is_empty())
            .map(|f| {
                if f.project_name.is_empty() {
                    format!("{}/{}", f.project_id, f.file_id)
                } else {
                    format!("{}（{}/{}）", f.project_name, f.project_id, f.file_id)
                }
            })
            .collect();
        if !blocked.is_empty() {
            return Err(format!(
                "这 {} 个文件查不到下载地址，需要你手动去 CurseForge 网页下：{}",
                blocked.len(),
                blocked.join("、")
            ));
        }

        for file in resolved {
            // 落哪个目录看项目的分类：`.zip` 可能是资源包、数据包或光影，
            // 一律丢进 `mods/` 会让游戏根本不认。
            let dir = crate::curseforge::target_dir(file.class_id);
            let path = format!("{}/{}", dir, file.filename);
            on_log(&format!(
                "  {}/{} -> {}",
                file.project_id, file.file_id, path
            ));
            tasks.push(PackFile {
                path,
                url: file.url,
                sha1: file.sha1,
                size: file.size,
            });
        }
    }
    Ok(tasks)
}
