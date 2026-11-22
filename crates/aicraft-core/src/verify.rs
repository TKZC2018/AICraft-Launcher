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
//! 本机 `5.json` 的 3587 个对象逐个 stat 就要 1.4 秒（冷缓存）。
//!
//! 所以这里提供的是**显式命令**，不挂在 launch 上。
//!
//! 检查本身是只读的、逐文件互不相干，所以摊给多个线程跑（见 `run_checks`）。
//! 本机 1.20.1-Fabric 全量 `--hash`（3587 个资源 + 54 个库，热缓存）
//! 单线程 2.28 秒、8 线程 0.45 秒。`VerifyMode::Existence` 只做 stat，
//! 线程再多也受限于文件系统本身，收益小得多。

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
    /// 额外计算 SHA-1。比 Existence 慢得多（本机全量约 0.45 秒），
    /// 但能发现内容损坏。
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
    // 先把「要查哪些文件」列全，再统一下发。检查本身是只读的、彼此无关，
    // 所以可以摊给多个线程 —— 全量 SHA-1 的时间几乎全花在这里。
    let mut checks: Vec<Check> = Vec::new();

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
                    checks.push(Check {
                        kind: "assets",
                        path: asset_object_path(&assets_dir, &asset.hash),
                        url: Some(asset_url(&asset.hash)),
                        sha1: Some(asset.hash.clone()),
                        size: Some(asset.size),
                        hash: mode == VerifyMode::Hash,
                    });
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

        checks.push(Check {
            kind: "libraries",
            path,
            url,
            sha1,
            size,
            hash: mode == VerifyMode::Hash,
        });
    }

    // ---- 3.5 逐个体检（多线程） ----
    run_checks(checks, &mut report);

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

/// 一项待检查的文件。
struct Check {
    kind: &'static str,
    path: PathBuf,
    url: Option<String>,
    sha1: Option<String>,
    size: Option<u64>,
    /// 是否要算 SHA-1（VerifyMode::Hash 才有）。
    hash: bool,
}

/// 检查一个文件：先看存在与尺寸，需要时再算 SHA-1。
fn inspect(check: &Check) -> Option<Problem> {
    let meta = match std::fs::metadata(&check.path) {
        Ok(meta) if meta.is_file() => meta,
        _ => return Some(Problem::Missing),
    };
    // size == 0 表示「清单没提供尺寸」，不是「期望零字节」。
    // 第三方源（BMCLAPI 的 liteloader 接口）就只给 url 不给 size，
    // 把它们当成真实期望会产生误报。
    let size_ok = match check.size {
        Some(s) if s > 0 => meta.len() == s,
        // 没有可信尺寸信息时，只要求非空
        _ => meta.len() > 0,
    };
    if !size_ok {
        return Some(Problem::WrongSize);
    }
    if !check.hash {
        return None;
    }
    let Some(want) = &check.sha1 else {
        // 没有 sha1 信息时不做哈希判定
        return None;
    };
    match crate::sha1::sha1_file_hex(&check.path) {
        Ok(actual) if actual.eq_ignore_ascii_case(want) => None,
        _ => Some(Problem::HashMismatch),
    }
}

/// 检查所有文件并写进报告。
///
/// 线程数按 CPU 核数取，上限 8：再往上收益就抵不过机械盘与杀毒软件
/// 的争抢了。结果按原顺序归位，报告里的条目次序因此与单线程时完全一致。
fn run_checks(checks: Vec<Check>, report: &mut VerifyReport) {
    if checks.is_empty() {
        return;
    }
    let total = checks.len();
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 8)
        .min(total);

    let results: Vec<std::sync::Mutex<Option<Problem>>> =
        checks.iter().map(|_| std::sync::Mutex::new(None)).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let checks = &checks;
            let results = &results;
            let next = &next;
            scope.spawn(move || loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= total {
                    break;
                }
                let problem = inspect(&checks[i]);
                *results[i].lock().unwrap() = problem;
            });
        }
    });

    for (check, slot) in checks.iter().zip(results) {
        match slot.into_inner().unwrap() {
            None => {
                if check.kind == "assets" {
                    report.assets_ok += 1;
                } else {
                    report.libs_ok += 1;
                }
            }
            Some(reason) => report.problems.push(MissingItem {
                kind: check.kind.into(),
                path: check.path.clone(),
                url: check.url.clone(),
                sha1: check.sha1.clone(),
                size: check.size,
                reason,
            }),
        }
    }
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


    /// 多线程检查之后，报告里的条目必须仍是「索引顺序 + 库顺序」，
    /// 否则界面上的列表会在每次校验之间跳来跳去。
    #[test]
    fn problems_keep_the_order_of_the_manifest() {
        let tmp = crate::testutil::fresh_dir("aicraft_verify_order");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("assets/indexes")).unwrap();
        // 24 个对象：偶数号故意做成尺寸不符（放 50 字节，期望 100），
        // 奇数号干脆不落盘。数量超过线程数，才能覆盖「抢任务」的顺序问题。
        let mut objects = String::new();
        for i in 0..24 {
            if i > 0 {
                objects.push(',');
            }
            objects.push_str(&format!(
                "\"obj/{i}\":{{\"hash\":\"{i:02}0000000000000000000000000000000000000\",\"size\":100}}"
            ));
        }
        let index = format!("{{\"objects\":{{{objects}}}}}");
        std::fs::write(tmp.join("assets/indexes/5.json"), index).unwrap();
        for i in (0..24).step_by(2) {
            let hash = format!("{i:02}0000000000000000000000000000000000000");
            let path = asset_object_path(&tmp.join("assets"), &hash);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, vec![0u8; 50]).unwrap();
        }

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

        assert_eq!(report.total_problems(), 24);
        let order: Vec<String> = report
            .problems
            .iter()
            .map(|p| p.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let want: Vec<String> = (0..24)
            .map(|i| format!("{i:02}0000000000000000000000000000000000000"))
            .collect();
        assert_eq!(order, want);
        // 偶数号尺寸不符、奇数号缺失，且不串位
        for (i, p) in report.problems.iter().enumerate() {
            let expected = if i % 2 == 0 {
                Problem::WrongSize
            } else {
                Problem::Missing
            };
            assert_eq!(p.reason, expected, "第 {i} 条的判定不对");
        }

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
