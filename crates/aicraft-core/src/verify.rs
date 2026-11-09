//! 资源校验与补全。
//!
//! 设计取向参考 HMCL 与 PCL 的实际做法：**都不在启动路径上校验**。
//!
//! - HMCL：启动时完全不碰资源，日志里一条 assets 记录都没有。
//! - PCL：有「补全文件」功能，但定位是故障排查工具，默认关闭
//!   （`LaunchAdvanceAssets:False`），文案写的是「若游戏缺失音效，请尝试此项」。
//!
//! 原因很实在：资源文件不会凭空消失，真会变的只有手动删除、磁盘故障、
//! 下载中断这三种情况，都不该由「每次启动扫一遍」来防。而代价是实打实的：
//! 本机 `5.json` 的 3587 个对象逐个 stat 就要 1.4 秒，全量 SHA-1 要 83 秒。
//!
//! 所以这里提供的是**显式命令**，不挂在 launch 上。

use std::path::{Path, PathBuf};

use crate::json::{self, Json};
use crate::resolve::ResolvedVersion;
use crate::rules::RuleContext;
use crate::version::Library;

/// 校验模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// 只检查文件是否存在、尺寸是否吻合。快（本机约 1.4 秒/索引）。
    Existence,
    /// 额外计算 SHA-1。慢（本机约 83 秒/全量），但能发现内容损坏。
    Hash,
}

/// 一个缺失或损坏的文件。
#[derive(Debug, Clone)]
pub struct MissingItem {
    /// 显示用的类别：assets / libraries / client-jar / index / logging
    pub kind: String,
    /// 本地期望路径。
    pub path: PathBuf,
    /// 下载地址（若有）。
    pub url: Option<String>,
    /// 期望的 SHA-1。
    pub sha1: Option<String>,
    /// 期望的字节数。
    pub size: Option<u64>,
    /// 问题描述。
    pub reason: Problem,
}

/// 问题类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// 文件不存在。
    Missing,
    /// 尺寸不符。
    WrongSize,
    /// SHA-1 不符。
    HashMismatch,
}

impl Problem {
    pub fn as_str(self) -> &'static str {
        match self {
            Problem::Missing => "缺失",
            Problem::WrongSize => "尺寸不符",
            Problem::HashMismatch => "校验和不符",
        }
    }
}

/// 校验报告。
#[derive(Debug, Default)]
pub struct VerifyReport {
    /// 检查过的文件总数。
    pub checked: usize,
    /// 有问题的条目。
    pub problems: Vec<MissingItem>,
    /// 按类别统计的问题数。
    pub counts: Vec<(String, usize)>,
    /// assets 中确认存在的对象数。
    pub assets_ok: usize,
    /// 库中确认存在的数量。
    pub libs_ok: usize,
}

impl VerifyReport {
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }

    pub fn total_problems(&self) -> usize {
        self.problems.len()
    }
}

/// 资源索引里的一个对象。
///
/// `name` 是**逻辑路径**（`minecraft/sounds/x.ogg` 或远古版本的 `sound/x.ogg`）。
/// 现代版本只按 hash 取文件、名字没用；但 `pre-1.6` / `legacy` 这两份索引靠
/// 逻辑名把文件铺进 `assets/virtual/<索引>/`，游戏是照那个目录读的，
/// 所以名字不能在这一层就丢掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetObject {
    pub name: String,
    pub hash: String,
    pub size: u64,
}

/// 单个资源索引（`assets/indexes/<id>.json`）的内容。
#[derive(Debug, Clone, Default)]
pub struct AssetIndexFile {
    pub objects: Vec<AssetObject>,
    /// 索引里的 `virtual: true`：老的「虚拟资源」，要按逻辑名铺开。
    pub virtual_assets: bool,
    /// 索引里的 `map_to_resources: true`：更老，铺进 `resources/`。
    pub map_to_resources: bool,
}

/// 解析资源索引文件。
pub fn parse_asset_index(text: &str) -> Result<AssetIndexFile, String> {
    let root = json::parse(text).map_err(|e| e.to_string())?;
    let objects_node = root
        .get("objects")
        .ok_or_else(|| "资源索引缺少 objects 字段".to_string())?;

    let mut objects = Vec::new();
    // objects 是 { "minecraft/sounds/x.ogg": { "hash": "...", "size": 123 } }
    if let Some(entries) = objects_node.as_object() {
        for (name, value) in entries {
            let hash = value
                .get("hash")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string();
            if hash.is_empty() {
                continue;
            }
            let size = value
                .get("size")
                .and_then(Json::as_i64)
                .unwrap_or(0) as u64;
            objects.push(AssetObject {
                name: name.to_string(),
                hash,
                size,
            });
        }
    }
    let virtual_assets = root
        .get("virtual")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let map_to_resources = root
        .get("map_to_resources")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    Ok(AssetIndexFile {
        objects,
        virtual_assets,
        map_to_resources,
    })
}

/// 资源对象在 `assets/objects` 下的落盘位置。
pub fn asset_object_path(assets_dir: &Path, hash: &str) -> PathBuf {
    let prefix = if hash.len() >= 2 { &hash[..2] } else { "00" };
    assets_dir.join("objects").join(prefix).join(hash)
}

/// 校验一个版本的资源与库。
///
/// `game_dir` 是 .minecraft 根目录。
pub fn verify_version(
    game_dir: &Path,
    version: &ResolvedVersion,
    ctx: &RuleContext,
    mode: VerifyMode,
) -> VerifyReport {
    let mut report = VerifyReport::default();
    let assets_dir = game_dir.join("assets");
    let libraries_dir = game_dir.join("libraries");

    // ---- 1. 资源索引文件本身 ----
    let index_path = assets_dir
        .join("indexes")
        .join(format!("{}.json", version.asset_index.id));

    let index_text = match std::fs::read_to_string(&index_path) {
        Ok(t) => Some(t),
        Err(_) => {
            report.problems.push(MissingItem {
                kind: "index".into(),
                path: index_path.clone(),
                url: version.asset_index.url.clone(),
                sha1: version.asset_index.sha1.clone(),
                size: version.asset_index.size,
                reason: Problem::Missing,
            });
            None
        }
    };

    // ---- 2. 索引里的每个对象 ----
    if let Some(text) = &index_text {
        match parse_asset_index(text) {
            Ok(index) => {
                report.checked += index.objects.len();
                for asset in &index.objects {
                    let (hash, size) = (&asset.hash, &asset.size);
                    let path = asset_object_path(&assets_dir, hash);
                    match std::fs::metadata(&path) {
                        Ok(meta) if meta.is_file() => {
                            // 同样：size == 0 视为「未知」，不当作期望值
                            let size_ok = if *size > 0 { meta.len() == *size } else { meta.len() > 0 };
                            if !size_ok {
                                report.problems.push(MissingItem {
                                    kind: "assets".into(),
                                    path,
                                    url: Some(asset_url(hash)),
                                    sha1: Some(hash.clone()),
                                    size: Some(*size),
                                    reason: Problem::WrongSize,
                                });
                            } else if mode == VerifyMode::Hash {
                                match crate::sha1::sha1_file_hex(&path) {
                                    Ok(actual) if actual.eq_ignore_ascii_case(hash) => {
                                        report.assets_ok += 1;
                                    }
                                    _ => report.problems.push(MissingItem {
                                        kind: "assets".into(),
                                        path,
                                        url: Some(asset_url(hash)),
                                        sha1: Some(hash.clone()),
                                        size: Some(*size),
                                        reason: Problem::HashMismatch,
                                    }),
                                }
                            } else {
                                report.assets_ok += 1;
                            }
                        }
                        _ => {
                            report.problems.push(MissingItem {
                                kind: "assets".into(),
                                path,
                                url: Some(asset_url(hash)),
                                sha1: Some(hash.clone()),
                                size: Some(*size),
                                reason: Problem::Missing,
                            });
                        }
                    }
                }
            }
            Err(e) => {
                report.problems.push(MissingItem {
                    kind: "index".into(),
                    path: index_path.clone(),
                    url: version.asset_index.url.clone(),
                    sha1: None,
                    size: None,
                    reason: Problem::HashMismatch,
                });
                let _ = e;
            }
        }
    }

    // ---- 3. 库文件 ----
    for lib in version.effective_libraries(ctx) {
        let (path, url, sha1, size) = library_target(lib, ctx, &libraries_dir);
        let Some(path) = path else { continue };
        report.checked += 1;

        // 本地库（natives）不解压到 libraries，跳过存在性判定
        if lib.is_native() {
            continue;
        }

        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => {
                // size == 0 表示「清单没提供尺寸」，不是「期望零字节」。
                // 第三方源（BMCLAPI 的 liteloader 接口）就只给 url 不给 size，
                // 把它们当成真实期望会产生误报。
                let size_ok = match size {
                    Some(s) if s > 0 => meta.len() == s,
                    // 没有可信尺寸信息时，只要求非空
                    _ => meta.len() > 0,
                };
                if !size_ok {
                    report.problems.push(MissingItem {
                        kind: "libraries".into(),
                        path,
                        url,
                        sha1,
                        size,
                        reason: Problem::WrongSize,
                    });
                } else if mode == VerifyMode::Hash {
                    match (&sha1, crate::sha1::sha1_file_hex(&path)) {
                        (Some(want), Ok(actual)) if actual.eq_ignore_ascii_case(want) => {
                            report.libs_ok += 1;
                        }
                        (Some(_), _) => report.problems.push(MissingItem {
                            kind: "libraries".into(),
                            path,
                            url,
                            sha1,
                            size,
                            reason: Problem::HashMismatch,
                        }),
                        // 没有 sha1 信息时不做哈希判定
                        (None, _) => report.libs_ok += 1,
                    }
                } else {
                    report.libs_ok += 1;
                }
            }
            _ => {
                report.problems.push(MissingItem {
                    kind: "libraries".into(),
                    path,
                    url,
                    sha1,
                    size,
                    reason: Problem::Missing,
                });
            }
        }
    }

    // ---- 4. 客户端 jar ----
    if let Some(jar_id) = &version.jar_id {
        let jar = game_dir
            .join("versions")
            .join(jar_id)
            .join(format!("{}.jar", jar_id));
        if !jar.is_file() {
            report.problems.push(MissingItem {
                kind: "client-jar".into(),
                path: jar,
                url: None,
                sha1: None,
                size: None,
                reason: Problem::Missing,
            });
        }
    }

    // 汇总
    for item in &report.problems {
        match report.counts.iter_mut().find(|(k, _)| k == &item.kind) {
            Some((_, n)) => *n += 1,
            None => report.counts.push((item.kind.clone(), 1)),
        }
    }

    report
}

/// 资源对象的官方下载地址。
///
/// Mojang 的 CDN 按 hash 的前两位分桶，这是固定规则。
pub fn asset_url(hash: &str) -> String {
    let prefix = if hash.len() >= 2 { &hash[..2] } else { "00" };
    format!(
        "https://resources.download.minecraft.net/{}/{}",
        prefix, hash
    )
}

/// 求一个库在本地应有的路径与下载信息。
fn library_target(
    lib: &Library,
    ctx: &RuleContext,
    libraries_dir: &Path,
) -> (Option<PathBuf>, Option<String>, Option<String>, Option<u64>) {
    if let Some(dl) = crate::resolve::library_download(lib, ctx) {
        if let Some(rel) = &dl.path {
            let path = libraries_dir.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            let url = if dl.url.is_empty() { None } else { Some(dl.url.clone()) };
            return (Some(path), url, dl.sha1.clone(), dl.size);
        }
        let url = if dl.url.is_empty() { None } else { Some(dl.url.clone()) };
        // 没有 path 时按坐标推导
        if let Some(coord) = &lib.coord {
            let path = libraries_dir.join(
                coord.to_path().replace('/', std::path::MAIN_SEPARATOR_STR),
            );
            return (Some(path), url, dl.sha1.clone(), dl.size);
        }
    }

    // 全无下载信息：按坐标推导路径，用默认仓库地址
    let coord = match lib.coord.as_ref() {
        Some(c) => c,
        None => return (None, None, None, None),
    };
    let rel = coord.to_path();
    let path = libraries_dir.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    let base = lib
        .url
        .clone()
        .unwrap_or_else(|| "https://libraries.minecraft.net/".to_string());
    let url = format!("{}{}", ensure_trailing_slash(&base), rel);
    (Some(path), Some(url), None, None)
}

fn ensure_trailing_slash(s: &str) -> String {
    if s.ends_with('/') {
        s.to_string()
    } else {
        format!("{}/", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"{
        "objects": {
            "minecraft/sounds/a.ogg": {"hash":"aaaabbbbccccddddeeeeffff0000111122223333","size":100},
            "minecraft/textures/b.png": {"hash":"1111222233334444555566667777888899990000","size":200}
        }
    }"#;

    #[test]
    fn parses_asset_index() {
        let idx = parse_asset_index(INDEX).unwrap();
        assert_eq!(idx.objects.len(), 2);
        assert_eq!(idx.objects[0].size, 100);
        assert_eq!(idx.objects[0].name, "minecraft/sounds/a.ogg");
        assert!(!idx.virtual_assets);
        assert!(!idx.map_to_resources);
    }

    #[test]
    fn legacy_indexes_announce_their_layout() {
        let legacy = parse_asset_index(r#"{"virtual":true,"objects":{}}"#).unwrap();
        assert!(legacy.virtual_assets);
        assert!(!legacy.map_to_resources);

        let old = parse_asset_index(r#"{"map_to_resources":true,"objects":{}}"#).unwrap();
        assert!(old.map_to_resources);
        assert!(!old.virtual_assets);
    }

    #[test]
    fn asset_object_path_uses_two_char_prefix() {
        let p = asset_object_path(Path::new("C:\\mc\\assets"), "abcdef1234");
        assert!(p.ends_with("objects\\ab\\abcdef1234") || p.ends_with("objects/ab/abcdef1234"));
    }

    #[test]
    fn asset_url_follows_cdn_layout() {
        assert_eq!(
            asset_url("abcdef1234"),
            "https://resources.download.minecraft.net/ab/abcdef1234"
        );
    }

    #[test]
    fn verify_reports_missing_objects() {
        let tmp = crate::testutil::fresh_dir("aicraft_verify_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("assets/indexes")).unwrap();
        std::fs::write(tmp.join("assets/indexes/5.json"), INDEX).unwrap();

        let version = crate::resolve::ResolvedVersion {
            id: "t".into(),
            main_class: "M".into(),
            jar_id: None,
            asset_index: crate::version::AssetIndex {
                id: "5".into(),
                ..Default::default()
            },
            java_version: Default::default(),
            libraries: vec![],
            game_args: vec![],
            jvm_args: vec![],
            logging: None,
            version_type: None,
        };

        let report = verify_version(
            &tmp,
            &version,
            &RuleContext::current(),
            VerifyMode::Existence,
        );
        // 两个对象都不存在
        assert_eq!(report.total_problems(), 2);
        assert!(report.problems.iter().all(|p| p.kind == "assets"));
        assert!(report.problems.iter().all(|p| p.reason == Problem::Missing));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn verify_detects_wrong_size() {
        let tmp = crate::testutil::fresh_dir("aicraft_verify_size");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("assets/indexes")).unwrap();
        std::fs::write(tmp.join("assets/indexes/5.json"), INDEX).unwrap();
        // 放一个尺寸不对的文件
        let hash = "aaaabbbbccccddddeeeeffff0000111122223333";
        let p = asset_object_path(&tmp.join("assets"), hash);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, vec![0u8; 50]).unwrap(); // 期望 100

        let version = crate::resolve::ResolvedVersion {
            id: "t".into(),
            main_class: "M".into(),
            jar_id: None,
            asset_index: crate::version::AssetIndex {
                id: "5".into(),
                ..Default::default()
            },
            java_version: Default::default(),
            libraries: vec![],
            game_args: vec![],
            jvm_args: vec![],
            logging: None,
            version_type: None,
        };
        let report = verify_version(
            &tmp,
            &version,
            &RuleContext::current(),
            VerifyMode::Existence,
        );
        assert_eq!(report.total_problems(), 2);
        // 其中一个尺寸不符，一个缺失
        assert!(report.problems.iter().any(|p| p.reason == Problem::WrongSize));
        assert!(report.problems.iter().any(|p| p.reason == Problem::Missing));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn verify_reports_missing_index() {
        let tmp = crate::testutil::fresh_dir("aicraft_verify_noidx");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let version = crate::resolve::ResolvedVersion {
            id: "t".into(),
            main_class: "M".into(),
            jar_id: None,
            asset_index: crate::version::AssetIndex {
                id: "999".into(),
                ..Default::default()
            },
            java_version: Default::default(),
            libraries: vec![],
            game_args: vec![],
            jvm_args: vec![],
            logging: None,
            version_type: None,
        };
        let report = verify_version(
            &tmp,
            &version,
            &RuleContext::current(),
            VerifyMode::Existence,
        );
        assert_eq!(report.total_problems(), 1);
        assert_eq!(report.problems[0].kind, "index");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn trailing_slash_is_normalized() {
        assert_eq!(ensure_trailing_slash("https://x/"), "https://x/");
        assert_eq!(ensure_trailing_slash("https://x"), "https://x/");
    }

    /// size == 0 表示「清单没给尺寸」，不是「期望零字节」。
    /// 第三方源（如 BMCLAPI 的 liteloader 接口）只给 url 不给 size，
    /// 若当成真实期望值就会误报（本机实际踩到过）。
    #[test]
    fn zero_size_means_unknown_not_zero() {
        let tmp = crate::testutil::fresh_dir("aicraft_verify_zerosize");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("assets/indexes")).unwrap();
        // 索引里声明 size=0，但本地有一个非空文件
        let index = r#"{"objects":{"a/b.ogg":{"hash":"aaaabbbbccccddddeeeeffff0000111122223333","size":0}}}"#;
        std::fs::write(tmp.join("assets/indexes/5.json"), index).unwrap();

        let hash = "aaaabbbbccccddddeeeeffff0000111122223333";
        let p = asset_object_path(&tmp.join("assets"), hash);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, vec![0u8; 123]).unwrap();

        let version = crate::resolve::ResolvedVersion {
            id: "t".into(),
            main_class: "M".into(),
            jar_id: None,
            asset_index: crate::version::AssetIndex {
                id: "5".into(),
                ..Default::default()
            },
            java_version: Default::default(),
            libraries: vec![],
            game_args: vec![],
            jvm_args: vec![],
            logging: None,
            version_type: None,
        };
        let report = verify_version(
            &tmp,
            &version,
            &RuleContext::current(),
            VerifyMode::Existence,
        );
        assert!(
            report.is_clean(),
            "size=0 不应被当成期望尺寸而误报: {:?}",
            report.problems
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
