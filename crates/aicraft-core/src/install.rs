//! 版本安装 —— 从零装一个 Minecraft 版本。
//!
//! 流程分两段，刻意解耦：
//!
//! 1. **规划（`plan_install`）**：拉版本清单 → 解析目标版本 JSON →
//!    算出「需要下载哪些文件、落到哪里」。纯计算，不碰磁盘写入，
//!    可以脱离网络单独测试（喂一个 JSON 就能验）。
//! 2. **执行（`execute_install`）**：按计划下载并落盘。
//!
//! 这样拆分的好处：真正的复杂度在规划（rules 求值、路径推导、资源索引展开），
//! 而那部分完全可测；下载只是重复劳动。
//!
//! 一个版本安装下来包含四类文件：
//!
//! - 版本 JSON 本身（`versions/<id>/<id>.json`）
//! - 客户端 jar（`versions/<id>/<id>.jar`）
//! - 库（`libraries/`）
//! - 资源索引 + 索引里的每个资源对象（`assets/`）
//!
//! 其中资源对象动辄几千个，是耗时主体。
//!
//! 这里的 `<id>` 是**实例名**，不一定等于官方版本号：同一个官方版本可以装成
//! 好几份，各自一个目录（`1.20.1`、`1.20.1-原版`、`1.20.1-测试`）。装成别的
//! 名字时，写进磁盘的版本 JSON 里的顶层 `"id"` 也会一起改成实例名 —— 否则
//! `resolved.id` 和目录名不一致，客户端 jar 与 `${version_name}` 都会指错地方。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::json::{self, Json};
use crate::rules::{RuleContext, is_allowed};
use crate::settings::Mirror;
use crate::cancel::CancelToken;
use crate::speed::Budget;
use crate::transport;
use crate::version::parse_version;

/// Mojang 官方版本清单地址。
pub const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// 清单里的一个可用版本。
#[derive(Debug, Clone)]
pub struct VersionSummary {
    pub id: String,
    /// release / snapshot / old_beta / old_alpha
    pub version_type: String,
    pub url: String,
    pub release_time: String,
}

/// 官方清单。
#[derive(Debug, Clone, Default)]
pub struct VersionManifest {
    pub latest_release: Option<String>,
    pub latest_snapshot: Option<String>,
    pub versions: Vec<VersionSummary>,
}

impl VersionManifest {
    /// 只看正式版。
    pub fn releases(&self) -> Vec<&VersionSummary> {
        self.versions
            .iter()
            .filter(|v| v.version_type == "release")
            .collect()
    }

    /// 按 id 精确查找。
    pub fn find(&self, id: &str) -> Option<&VersionSummary> {
        self.versions.iter().find(|v| v.id == id)
    }
}

/// 解析官方版本清单。
pub fn parse_manifest(text: &str) -> Result<VersionManifest, String> {
    let root = json::parse(text).map_err(|e| e.to_string())?;

    let mut manifest = VersionManifest::default();
    if let Some(latest) = root.get("latest") {
        manifest.latest_release = latest.get("release").and_then(Json::as_str).map(str::to_string);
        manifest.latest_snapshot = latest
            .get("snapshot")
            .and_then(Json::as_str)
            .map(str::to_string);
    }

    let Some(versions) = root.get("versions").and_then(Json::as_array) else {
        return Err("版本清单缺少 versions 字段".to_string());
    };

    for v in versions {
        let Some(id) = v.get("id").and_then(Json::as_str) else {
            continue;
        };
        let Some(url) = v.get("url").and_then(Json::as_str) else {
            continue;
        };
        manifest.versions.push(VersionSummary {
            id: id.to_string(),
            version_type: v
                .get("type")
                .and_then(Json::as_str)
                .unwrap_or("release")
                .to_string(),
            url: url.to_string(),
            release_time: v
                .get("releaseTime")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }

    Ok(manifest)
}

/// 一个待下载条目。
#[derive(Debug, Clone)]
pub struct InstallItem {
    /// 目标绝对路径。
    pub dest: PathBuf,
    /// 下载地址；None 表示本地生成（如版本 JSON 由启动器写入）。
    pub url: Option<String>,
    /// 期望 SHA-1。
    pub sha1: Option<String>,
    /// 期望字节数。
    pub size: Option<u64>,
    /// 类别，用于统计与进度展示。
    pub kind: InstallKind,
}

/// 安装条目的类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// 版本 JSON
    VersionJson,
    /// 客户端 jar
    ClientJar,
    /// 依赖库
    Library,
    /// 资源索引
    AssetIndex,
    /// 单个资源对象
    Asset,
    /// 日志配置文件
    LogConfig,
}

impl InstallKind {
    pub fn as_str(self) -> &'static str {
        match self {
            InstallKind::VersionJson => "版本清单",
            InstallKind::ClientJar => "客户端",
            InstallKind::Library => "库",
            InstallKind::AssetIndex => "资源索引",
            InstallKind::Asset => "资源",
            InstallKind::LogConfig => "日志配置",
        }
    }
}

/// 远古版本（`pre-1.6` / `legacy`）的资源要多铺一份。
///
/// 这两份索引里的对象是**按逻辑名**登记的（`sound/ambient/cave/cave1.ogg`），
/// 而 `objects/<hash>` 只有哈希、没有名字。当年的启动器会照名字铺出一棵目录树：
///
/// - `virtual: true`（1.6.x 的 `legacy`）→ `<assets>/virtual/<索引>/`，
///   游戏从 `--assetsDir` 指过去的正是这里；
/// - `map_to_resources: true`（b1.x / 1.5 的 `pre-1.6`）→ `<gameDir>/resources/`，
///   b1.7.3 这种老版本的启动参数里连 `--assetsDir` 都没有，只能读这里；
///   但 1.5/1.6 又会用 `--assetsDir` 指 `virtual/pre-1.6`，所以两个地方都铺。
///
/// 不铺的后果不影响启动，只是进游戏听不到声音、界面没有语言文件。
#[derive(Debug, Clone)]
pub struct VirtualAssets {
    /// `<assets>` 根目录；源文件在它下面的 `objects/<前两位>/<hash>`。
    ///
    /// 存根目录而不是 `objects` 本身，是为了能直接交给
    /// [`crate::verify::asset_object_path`] —— 那个函数自己会拼 `objects`。
    pub assets_root: PathBuf,
    /// 要铺开的目录；每个目录都收一份完整的副本。
    pub dirs: Vec<PathBuf>,
    /// (逻辑名, hash)。
    pub entries: Vec<(String, String)>,
}

/// 安装计划。
#[derive(Debug, Default)]
pub struct InstallPlan {
    /// 实例名，也就是落盘目录名（`versions/<version_id>/`）。
    pub version_id: String,
    /// 官方版本号。装成别的实例名时它才是「装的是哪个版本」的答案。
    pub official_id: String,
    pub items: Vec<InstallItem>,
    /// 版本 JSON 的原文，安装时要原样写入磁盘。
    pub version_json: String,
    /// 版本 JSON 的目标路径。
    pub version_json_path: PathBuf,
    /// 汇总：各类别条目数。
    pub stats: Vec<(InstallKind, usize)>,
    /// 预计总字节数（仅统计已知 size 的条目）。
    pub total_bytes: u64,
    /// 远古版本的资源铺开任务；现代版本为 `None`。
    pub virtual_assets: Option<VirtualAssets>,
    /// 游戏根目录（`.minecraft`）。装完之后要把清单压平，那一步只知道
    /// 实例名不够用，得能回到 `versions/`。
    pub game_dir: PathBuf,
}

impl InstallPlan {
    /// 需要实际下载的条目（排除本地生成的）。
    pub fn downloadable(&self) -> Vec<&InstallItem> {
        self.items.iter().filter(|i| i.url.is_some()).collect()
    }
}

/// 求主构件的落盘路径。
fn artifact_dest(
    _raw: &crate::version::RawVersion,
    lib: &crate::version::Library,
    artifact: &crate::version::Download,
    game_dir: &Path,
) -> Option<PathBuf> {
    if let Some(rel) = &artifact.path {
        return Some(
            game_dir
                .join("libraries")
                .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)),
        );
    }
    let coord = lib.coord.as_ref()?;
    Some(
        game_dir
            .join("libraries")
            .join(coord.to_path().replace('/', std::path::MAIN_SEPARATOR_STR)),
    )
}

/// 规划一次安装。
///
/// `version_json` 是该版本的完整清单原文（已从 `url` 取得）。
/// 这里不再发网络请求，纯解析 + 计算，便于测试。
///
/// `instance_name` 是落盘用的实例名（`versions/<name>/`）。给 `None` 就用
/// 官方版本号，行为和以前一样；给别的名字就能把同一个版本再装一份。
/// 名字不合法（空、带路径分隔符、`.`/`..`）时退回官方版本号并记一条警告。
pub fn plan_install(
    game_dir: &Path,
    version_json: &str,
    asset_index_json: Option<&str>,
    ctx: &RuleContext,
) -> Result<InstallPlan, String> {
    plan_install_as(game_dir, version_json, asset_index_json, ctx, None)
}

/// 同 [`plan_install`]，但可以指定实例名。
pub fn plan_install_as(
    game_dir: &Path,
    version_json: &str,
    asset_index_json: Option<&str>,
    ctx: &RuleContext,
    instance_name: Option<&str>,
) -> Result<InstallPlan, String> {
    let raw = parse_version(version_json).map_err(|e| format!("解析版本清单失败: {}", e))?;

    let official_id = raw.id.clone();
    let version_id = match instance_name.map(str::trim).filter(|n| is_valid_instance_name(n)) {
        Some(name) => name.to_string(),
        None => {
            if let Some(name) = instance_name.map(str::trim).filter(|n| !n.is_empty()) {
                crate::logging::warn(format!(
                    "实例名「{}」不合法（不能含 \\ / : * ? \" < > |，也不能是 . 或 ..），改用 {}",
                    name, official_id
                ));
            }
            official_id.clone()
        }
    };

    // 装成别的名字时，磁盘上的版本 JSON 顶层 `id` 必须跟着改：
    // `resolve` 出的 `resolved.id` 参与拼 jar 路径和 `${version_name}`，
    // 不改就会指回 `versions/<官方 id>/`。
    let version_json = if version_id == official_id {
        version_json.to_string()
    } else {
        retag_version_json(version_json, &version_id, &official_id)?
    };

    let mut plan = InstallPlan {
        version_id: version_id.clone(),
        official_id: official_id.clone(),
        version_json,
        version_json_path: game_dir
            .join("versions")
            .join(&version_id)
            .join(format!("{}.json", version_id)),
        game_dir: game_dir.to_path_buf(),
        ..Default::default()
    };

    // ---- 1. 客户端 jar ----
    if let Some(client) = &raw.downloads_client {
        if !client.url.is_empty() {
            plan.items.push(InstallItem {
                dest: game_dir
                    .join("versions")
                    .join(&version_id)
                    .join(format!("{}.jar", version_id)),
                url: Some(client.url.clone()),
                sha1: client.sha1.clone(),
                size: client.size,
                kind: InstallKind::ClientJar,
            });
        }
    }

    // ---- 2. 库 ----
    for lib in &raw.libraries {
        if !is_allowed(&lib.rules, ctx) {
            continue;
        }

        // 一个老式条目可能**同时**需要下载两个文件：
        //   artifact    = 主 jar（进 classpath）
        //   classifier  = 原生库（解压出 dll）
        // 所以这里不能二选一，两部分都要处理。
        let mut pushed_main = false;

        if let Some(artifact) = &lib.artifact {
            if !artifact.url.is_empty() {
                if let Some(dest) = artifact_dest(&raw, lib, artifact, game_dir) {
                    plan.items.push(InstallItem {
                        dest,
                        url: Some(artifact.url.clone()),
                        sha1: artifact.sha1.clone(),
                        size: artifact.size,
                        kind: InstallKind::Library,
                    });
                    pushed_main = true;
                }
            }
        }

        // 原生库（classifier）
        if lib.has_natives() {
            if let Some(classifier) = lib.native_classifier(ctx.os.name.as_str()) {
                let classifier = expand_arch_placeholder(&classifier, ctx);
                if let Some((_, dl)) = lib.classifiers.iter().find(|(k, _)| k == &classifier) {
                    let rel = dl.path.clone().unwrap_or_else(|| {
                        let coord = lib.coord.as_ref().map(|c| c.to_path()).unwrap_or_default();
                        format!("{}-{}.jar", coord.trim_end_matches(".jar"), classifier)
                    });
                    plan.items.push(InstallItem {
                        dest: game_dir
                            .join("libraries")
                            .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)),
                        url: if dl.url.is_empty() {
                            None
                        } else {
                            Some(dl.url.clone())
                        },
                        sha1: dl.sha1.clone(),
                        size: dl.size,
                        kind: InstallKind::Library,
                    });
                }
            }
        }

        // 已有主构件，本条处理完毕
        if pushed_main {
            continue;
        }

        // 到这里说明该库没有 `artifact`。两种情况要区分开：
        //
        // 1. 它只有 `classifiers`（纯原生库，如 twitch-platform）。
        //    这类库**没有主构件**，Mojang 服务器上也不存在
        //    `<artifact>-<version>.jar`。若按坐标硬推 URL 就会 404
        //    （实测 1.8.9 会因此产生 3 个必然失败的任务）。
        //    正确做法是只下载当前平台对应的 classifier（上面已处理）。
        //
        // 2. 它既无 artifact 也无 classifiers（老式坐标库）。
        // 到这里说明该库没有 `artifact` 且已在上面的 `has_natives` 分支
        // 处理过原生库。若它确实带 classifiers，就不该再按坐标推导
        // （那会编出不存在的 URL，实测 1.8.9 会 404 三次）。
        if !lib.classifiers.is_empty() {
            continue;
        }

        // 老版清单：没有 artifact，只有坐标 + 仓库基址
        let Some(coord) = &lib.coord else { continue };
        let base = lib
            .url
            .clone()
            .unwrap_or_else(|| "https://libraries.minecraft.net/".to_string());
        let base = if base.ends_with('/') {
            base
        } else {
            format!("{}/", base)
        };
        let rel = coord.to_path();
        plan.items.push(InstallItem {
            dest: game_dir
                .join("libraries")
                .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)),
            url: Some(format!("{}{}", base, rel)),
            sha1: None,
            size: None,
            kind: InstallKind::Library,
        });
    }

    // ---- 3. 资源索引 ----
    let index_path = game_dir
        .join("assets")
        .join("indexes")
        .join(format!("{}.json", raw.asset_index.id));

    if let Some(url) = &raw.asset_index.url {
        plan.items.push(InstallItem {
            dest: index_path.clone(),
            url: Some(url.clone()),
            sha1: raw.asset_index.sha1.clone(),
            size: raw.asset_index.size,
            kind: InstallKind::AssetIndex,
        });
    }

    // ---- 4. 资源对象 ----
    // 优先用调用方传入的索引内容（已下载）；否则尝试读本地已有的。
    let index_text = asset_index_json
        .map(str::to_string)
        .or_else(|| std::fs::read_to_string(&index_path).ok());

    if let Some(text) = index_text {
        let index = crate::verify::parse_asset_index(&text)
            .map_err(|e| format!("解析资源索引失败: {}", e))?;
        // 同一 hash 可能被多个逻辑名引用，去重避免重复下载
        let mut seen: HashSet<String> = HashSet::new();
        for asset in &index.objects {
            if !seen.insert(asset.hash.clone()) {
                continue;
            }
            let prefix = if asset.hash.len() >= 2 {
                &asset.hash[..2]
            } else {
                "00"
            };
            plan.items.push(InstallItem {
                dest: game_dir
                    .join("assets")
                    .join("objects")
                    .join(prefix)
                    .join(&asset.hash),
                url: Some(format!(
                    "https://resources.download.minecraft.net/{}/{}",
                    prefix, asset.hash
                )),
                sha1: Some(asset.hash.clone()),
                size: Some(asset.size),
                kind: InstallKind::Asset,
            });
        }

        // 远古版本：另记一份「逻辑名 → hash」，装完按名字铺出去。
        if index.virtual_assets || index.map_to_resources {
            let mut dirs: Vec<PathBuf> = Vec::new();
            let virtual_dir = game_dir
                .join("assets")
                .join("virtual")
                .join(&raw.asset_index.id);
            if index.virtual_assets {
                dirs.push(virtual_dir.clone());
            }
            if index.map_to_resources {
                dirs.push(virtual_dir);
                dirs.push(game_dir.join("resources"));
            }
            dirs.dedup();
            plan.virtual_assets = Some(VirtualAssets {
                assets_root: game_dir.join("assets"),
                dirs,
                entries: index
                    .objects
                    .iter()
                    .map(|a| (a.name.clone(), a.hash.clone()))
                    .collect(),
            });
        }
    }

    // ---- 5. 日志配置 ----
    if let Some(logging) = &raw.logging {
        if let Some(file) = &logging.file {
            if let Some(url) = Some(&file.url) {
                if !url.is_empty() {
                    let name = Path::new(&file.url)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "client-log.xml".to_string());
                    plan.items.push(InstallItem {
                        dest: game_dir.join("assets").join("log_configs").join(name),
                        url: Some(url.clone()),
                        sha1: file.sha1.clone(),
                        size: file.size,
                        kind: InstallKind::LogConfig,
                    });
                }
            }
        }
    }

    // 统计
    for kind in [
        InstallKind::VersionJson,
        InstallKind::ClientJar,
        InstallKind::Library,
        InstallKind::AssetIndex,
        InstallKind::Asset,
        InstallKind::LogConfig,
    ] {
        let n = plan.items.iter().filter(|i| i.kind == kind).count();
        if n > 0 {
            plan.stats.push((kind, n));
        }
    }
    // 同一个目标文件可能被清单里两个条目同时指向。
    // 老清单尤其常见：`b1.7.3` / `1.5.2` 把 `net.java.jinput:jinput-platform:2.0.5`
    // 写了两遍（一次带 natives、一次不带），两条都算出同一个
    // `...-natives-windows.jar`；1.12.2 的 text2speech 也是同理。
    // 两个线程同时往一个 dest 下载，后收尾的那个 rename 就会撞上
    // 「os error 32 另一个程序正在使用此文件」或者干脆找不到文件。
    // 下载是按内容寻址的、写的是同一份字节，去重只保留第一条即可。
    {
        let mut seen: HashSet<PathBuf> = HashSet::new();
        plan.items.retain(|item| seen.insert(item.dest.clone()));
    }
    plan.total_bytes = plan.items.iter().filter_map(|i| i.size).sum();

    Ok(plan)
}

/// 实例名是否可用作目录名。
///
/// 只排掉真正会出问题的字符：Windows 上 `\ / : * ? " < > |` 非法，
/// `.` 与 `..` 会造成路径穿越，空名没有意义。中文、空格、`-`、`_` 都放行。
pub fn is_valid_instance_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    !name
        .chars()
        .any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control())
}

/// 版本 JSON 里记录「原始官方版本号」的私有字段。
///
/// 装成别的实例名时顶层 `"id"` 被改成实例名，官方版本号就没了；
/// 这个字段把它留下来，`installed_count` 之类才能按「同一个官方版本」聚合。
/// 前缀 `_aicraft` 是刻意的：原版游戏与 Forge 都不读未知名段。
pub const OFFICIAL_ID_KEY: &str = "_aicraftOfficialId";

/// 把版本 JSON 顶层的 `"id"` 换成实例名，并把官方版本号记进私有字段。
///
/// 安装流程内部会调它（[`plan_install_as`]），Fabric 的 profile 也要先按这个
/// 规则改好再交进来 —— 所以它对外可见。
pub fn retag_version_json(
    version_json: &str,
    new_id: &str,
    official_id: &str,
) -> Result<String, String> {
    let mut value = json::parse(version_json).map_err(|e| format!("解析版本清单失败: {}", e))?;
    match &mut value {
        Json::Object(entries) => {
            match entries.iter_mut().find(|(k, _)| k == "id") {
                Some((_, slot)) => *slot = Json::Str(new_id.to_string()),
                None => entries.insert(0, ("id".to_string(), Json::Str(new_id.to_string()))),
            }
            let official = Json::Str(official_id.to_string());
            match entries.iter_mut().find(|(k, _)| k == OFFICIAL_ID_KEY) {
                Some((_, slot)) => *slot = official,
                None => entries.push((OFFICIAL_ID_KEY.to_string(), official)),
            }
        }
        _ => return Err("版本清单顶层不是对象".to_string()),
    }
    Ok(json::write(&value))
}

/// 列出 `versions/` 下所有实例名。
///
/// 只认「目录里有同名 `.json`」的那些，和 `rescan` 的判定保持一致。
pub fn installed_instances(game_dir: &Path) -> Vec<String> {
    let dir = game_dir.join("versions");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|name| dir.join(name).join(format!("{}.json", name)).is_file())
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// 某个官方版本已经装了几份（含用官方版本号本身那一份）。
pub fn installed_count(game_dir: &Path, official_id: &str) -> usize {
    instances_of(game_dir, official_id).len()
}

/// 列出某个官方版本装成的所有实例名（按名字排序）。
pub fn instances_of(game_dir: &Path, official_id: &str) -> Vec<String> {
    installed_instances(game_dir)
        .into_iter()
        .filter(|name| instance_official_id(game_dir, name).as_deref() == Some(official_id))
        .collect()
}

/// 读实例目录里的版本 JSON，返回它对应的官方版本号。
///
/// 装成别的名字时顶层 `"id"` 是实例名，官方版本号存在私有字段
/// [`OFFICIAL_ID_KEY`] 里；老实例（直接用官方版本号做目录名的那种）
/// 没有这个字段，`"id"` 本身就是答案。
pub fn instance_official_id(game_dir: &Path, instance_name: &str) -> Option<String> {
    let path = game_dir
        .join("versions")
        .join(instance_name)
        .join(format!("{}.json", instance_name));
    let text = std::fs::read_to_string(path).ok()?;
    let value = json::parse(&text).ok()?;
    if let Some(official) = value.get(OFFICIAL_ID_KEY).and_then(Json::as_str) {
        return Some(official.to_string());
    }
    value
        .get("id")
        .and_then(Json::as_str)
        .map(str::to_string)
        .or_else(|| Some(instance_name.to_string()))
}

/// 这个实例对应哪个原版游戏版本。
///
/// 不能用 `instance_official_id`：那个返回的是加载器那一层的 id
/// （`fabric-loader-0.20.1-26.3`），拿去和 Modrinth 的版本号比是比不上的。
pub fn instance_game_version(game_dir: &Path, instance_name: &str) -> Option<String> {
    let path = game_dir
        .join("versions")
        .join(instance_name)
        .join(format!("{}.json", instance_name));
    let text = std::fs::read_to_string(path).ok()?;
    crate::loader::detect_game_version(&text)
}

/// 把一个已经装好的实例改名。
///
/// Forge / NeoForge 的安装器会自己决定落盘目录名（`1.20.1-forge-47.0.1`
/// 这种），玩家没法在安装器里指定。想让实例叫什么就自己起名，只能装完再搬：
///
/// 1. `versions/<from>/` → `versions/<to>/`
/// 2. 目录里 `<from>.json` / `<from>.jar` 跟着改文件名（`rescan` 与启动都按
///    「目录名 = 文件名」找，不改就找不到）
/// 3. 版本 JSON 的 `"id"` 改成新名字 —— 启动时 `${version_name}` 与客户端 jar
///    的兜底查找都用它
///
/// `from == to` 时什么都不做；目标已存在时报错，不覆盖别人的实例。
pub fn rename_instance(game_dir: &Path, from: &str, to: &str) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    if !is_valid_instance_name(to) {
        return Err(format!("实例名「{}」不能用作目录名", to));
    }
    let versions = game_dir.join("versions");
    let src = versions.join(from);
    let dst = versions.join(to);
    if !src.is_dir() {
        return Err(format!("没有找到实例 {}", from));
    }
    if dst.exists() {
        return Err(format!("实例名 {} 已经被占用了", to));
    }

    std::fs::rename(&src, &dst).map_err(|e| format!("给实例改名失败: {}", e))?;

    // 改目录里的文件：<from>.json / <from>.jar / <from>.txt …
    if let Ok(entries) = std::fs::read_dir(&dst) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(ext) = name.strip_prefix(from).and_then(|rest| rest.strip_prefix('.')) else {
                continue;
            };
            let target = dst.join(format!("{}.{}", to, ext));
            std::fs::rename(&path, &target)
                .map_err(|e| format!("重命名 {} 失败: {}", path.display(), e))?;
        }
    }

    // 改清单里的 id，官方版本号仍然记在私有字段里
    let json_path = dst.join(format!("{}.json", to));
    let text = std::fs::read_to_string(&json_path)
        .map_err(|e| format!("读取 {} 失败: {}", json_path.display(), e))?;
    let official = instance_official_id_of(&text, to);
    let retagged = retag_version_json(&text, to, &official)?;
    std::fs::write(&json_path, retagged)
        .map_err(|e| format!("写入 {} 失败: {}", json_path.display(), e))?;
    Ok(())
}

/// 从版本 JSON 文本里取官方版本号（读不到就用实例名兜底）。
fn instance_official_id_of(text: &str, instance: &str) -> String {
    let Ok(value) = json::parse(text) else {
        return instance.to_string();
    };
    value
        .get(OFFICIAL_ID_KEY)
        .and_then(Json::as_str)
        .or_else(|| value.get("id").and_then(Json::as_str))
        .unwrap_or(instance)
        .to_string()
}

/// 展开 classifier 里的 `${arch}` 占位符。
///
/// 老版本清单（1.8 时代）把 native 分类符写成
/// `natives-windows-${arch}`，需要替换成 `64` 或 `32` ——
/// 注意这里用的是 **位数**，不是 `x86_64`/`arm64` 这种架构名。
fn expand_arch_placeholder(classifier: &str, ctx: &RuleContext) -> String {
    if !classifier.contains("${arch}") {
        return classifier.to_string();
    }
    let bits = match ctx.os.arch {
        crate::rules::Arch::X86 => "32",
        // 64 位 ARM 在 Minecraft 老版本里没有对应 native，
        // 按 64 位 Intel 处理是唯一可行的兜底。
        _ => "64",
    };
    classifier.replace("${arch}", bits)
}

/// 下载版本清单。
pub fn fetch_manifest() -> Result<VersionManifest, String> {
    fetch_manifest_in(None)
}

/// 下载版本清单。
///
/// 文本类文件直接进内存（见 `transport::fetch_text`），
/// `scratch` 参数保留是为了兼容旧调用点，现在不再需要临时目录。
pub fn fetch_manifest_in(_scratch: Option<&Path>) -> Result<VersionManifest, String> {
    let text = transport::fetch_text(VERSION_MANIFEST_URL)
        .map_err(|e| format!("下载版本清单失败: {}", e))?;
    let manifest = parse_manifest(&text)?;
    crate::logging::debug(format!("版本清单共 {} 个版本", manifest.versions.len()));
    Ok(manifest)
}

/// 下载某个版本的 JSON 原文。
///
/// 同样直接进内存，不落盘。
pub fn fetch_version_json_in(url: &str, _scratch: Option<&Path>) -> Result<String, String> {
    transport::fetch_text(url).map_err(|e| format!("下载版本 JSON 失败: {}", e))
}

/// 安装过程中发生的事件。
#[derive(Debug, Clone)]
pub enum InstallProgress {
    /// 阶段说明（如「正在下载客户端」）。
    Stage(String),
    /// 已处理 / 总数 / 累计字节。
    Tick {
        done: usize,
        total: usize,
        bytes: u64,
    },
    /// 跳过（已存在且完整）。
    Skipped(PathBuf),
    /// 失败。
    Failed(PathBuf, String),
}

/// 安装结果。
#[derive(Debug, Default)]
pub struct InstallReport {
    pub downloaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub bytes: u64,
    pub failures: Vec<(PathBuf, String)>,
    /// 是不是被用户取消打断的。为真时 `failed` 里可能有一条「已取消」，
    /// 调用方不该把它当安装失败报给用户。
    pub cancelled: bool,
}

impl InstallReport {
    pub fn is_success(&self) -> bool {
        self.failed == 0
    }
}

/// 执行安装。
///
/// 先写版本 JSON（让版本立刻出现在列表里），再并发下载其余文件。
/// 单个文件失败不影响其他文件——尽量让用户能启动，
/// 缺的东西事后可以用「校验资源」查出来，再按提示补全。
pub fn execute_install<F>(
    plan: &InstallPlan,
    concurrency: usize,
    on_progress: F,
) -> Result<InstallReport, String>
where
    F: FnMut(InstallProgress),
{
    execute_install_with_mirror(plan, concurrency, &Mirror::Official, on_progress)
}

/// 把「写不进去」翻译成用户能照着做的一句话。
///
/// 访问被拒（`os error 5`）在这个项目里遇到过一次真实案例：启动器所在的
/// 文件夹被打了低完整性标记（Windows 的 MIC，沙箱/安全软件会这么干），
/// 于是进程只能写那个文件夹内部，写游戏目录一律被拒 —— 看起来像权限问题，
/// 其实是进程被降权了。权限本身没有问题，改 ACL 也没用，只能换地方跑。
fn write_hint(e: &std::io::Error) -> &'static str {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        "\n原因可能是：启动器所在的文件夹带了低完整性标记（沙箱或安全策略），进程写不进游戏目录。\
         \n可以试试：把启动器换个文件夹再启动，或用「以管理员身份运行」，或在「目录列表」里换一个能写的游戏目录。"
    } else if e.kind() == std::io::ErrorKind::NotFound {
        "\n目标路径的上一级不存在，可能是游戏目录被删掉或挪走了。"
    } else {
        ""
    }
}

/// 执行安装，并在发请求前按版本下载源改写地址。
///
/// 计划里存的始终是官方地址（便于缓存与校验），镜像只在下载这一层生效；
/// 换源不会让「已安装」判断失效。
pub fn execute_install_with_mirror<F>(
    plan: &InstallPlan,
    concurrency: usize,
    mirror: &Mirror,
    on_progress: F,
) -> Result<InstallReport, String>
where
    F: FnMut(InstallProgress),
{
    execute_install_with_budget(plan, concurrency, mirror, &Budget::unlimited(), on_progress)
}

/// 执行安装，同时受下载限速约束。
///
/// `budget` 是**总带宽**：每个下载线程进场时按「总量 / 在场数」领一份，
/// 下完把名额还回去，后来的线程就能拿到更大的一份。
///
/// 进度事件是**攒到最后一次性回放**的：这个签名收的是 `FnMut`，
/// 没法同时给多个工作线程用。要在下载过程中实时看到进度，用
/// [`execute_install_live`]。
pub fn execute_install_with_budget<F>(
    plan: &InstallPlan,
    concurrency: usize,
    mirror: &Mirror,
    budget: &Budget,
    mut on_progress: F,
) -> Result<InstallReport, String>
where
    F: FnMut(InstallProgress),
{
    // `FnMut` 不是 `Sync`，没法直接交给工作线程，所以先攒进队列、跑完再回放。
    // 想在下载过程中实时拿到进度，请用 [`execute_install_live`]。
    let events = std::sync::Mutex::new(Vec::new());
    let cancel = CancelToken::new();
    let report = execute_install_live(plan, concurrency, mirror, budget, &cancel, &|ev| {
        if let Ok(mut queue) = events.lock() {
            queue.push(ev);
        }
    })?;
    if let Ok(events) = events.into_inner() {
        for ev in events {
            on_progress(ev);
        }
    }
    Ok(report)
}

/// 执行安装，一边下一边把进度交出去，并且可以中途取消。
///
/// 与 [`execute_install_with_budget`] 的区别有两点，都是为了「下载弹窗」：
///
/// - `on_progress` 要求 `Sync`，工作线程直接调它（不再攒到最后回放），
///   所以界面能实时看到「下了多少、多快」。
/// - `cancel` 被点掉之后剩下的队列直接放弃，正在跑的那几个 `curl` 会被杀掉；
///   返回的报告里 `cancelled` 为真。
pub fn execute_install_live(
    plan: &InstallPlan,
    concurrency: usize,
    mirror: &Mirror,
    budget: &Budget,
    cancel: &CancelToken,
    on_progress: &(dyn Fn(InstallProgress) + Send + Sync),
) -> Result<InstallReport, String> {
    // 1. 写版本 JSON
    if let Some(parent) = plan.version_json_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "创建版本目录失败: {}（{}）{}",
                e,
                parent.display(),
                write_hint(&e)
            )
        })?;
    }
    std::fs::write(&plan.version_json_path, &plan.version_json).map_err(|e| {
        format!(
            "写入版本 JSON 失败: {}（{}）{}",
            e,
            plan.version_json_path.display(),
            write_hint(&e)
        )
    })?;

    if cancel.is_cancelled() {
        return Ok(InstallReport {
            cancelled: true,
            ..InstallReport::default()
        });
    }

    let items: Vec<&InstallItem> = plan.downloadable();
    let total = items.len();
    if total == 0 {
        return Ok(InstallReport::default());
    }

    crate::logging::info(format!(
        "开始安装：{} 个文件，并发 {}",
        total, concurrency
    ));
    on_progress(InstallProgress::Stage(format!(
        "开始下载 {} 个文件",
        total
    )));

    // 2. 建目录
    for item in &items {
        if let Some(parent) = item.dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "创建目录 {} 失败: {}{}",
                    parent.display(),
                    e,
                    write_hint(&e)
                )
            })?;
        }
    }

    // 3. 决定顺序：小文件（索引、日志配置）先下，让启动更早可用；
    //    大文件（客户端 jar、库）随后。排序在下面与并发下载一起处理。

    // 4. 并发下载
    //
    // 队列里放**拥有所有权**的条目副本，而不是 `&InstallItem`：
    // 工作线程要求 `'static`，借用 `plan` 无法跨越线程边界。
    // 条目本身只有几个 PathBuf 和 String，克隆成本可忽略。
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    let mut ordered: Vec<InstallItem> = items.into_iter().cloned().collect();
    ordered.sort_by_key(|i| match i.kind {
        InstallKind::LogConfig => 0,
        InstallKind::AssetIndex => 1,
        InstallKind::Library => 2,
        InstallKind::ClientJar => 3,
        InstallKind::Asset => 4,
        InstallKind::VersionJson => 5,
    });

    let queue = Arc::new(Mutex::new(ordered));
    let mirror = Arc::new(*mirror);
    let budget = budget.clone();
    let report = Arc::new(Mutex::new(InstallReport::default()));
    let done = Arc::new(AtomicUsize::new(0));
    let bytes = Arc::new(AtomicUsize::new(0));

    let workers = concurrency.max(1).min(total);

    // 用 `scope` 而不是 `spawn`：工作线程要借用 `on_progress`（一个共享引用），
    // 而 `spawn` 要求线程体是 `'static` 的。
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let mirror = Arc::clone(&mirror);
            let budget = budget.clone();
            let report = Arc::clone(&report);
            let done = Arc::clone(&done);
            let bytes = Arc::clone(&bytes);
            let cancel = cancel.clone();

            scope.spawn(move || loop {
                if cancel.is_cancelled() {
                    break;
                }
                let item = {
                    let mut q = queue.lock().unwrap();
                    if q.is_empty() {
                        break;
                    }
                    q.remove(0)
                };

                let url = match &item.url {
                    Some(u) => mirror.rewrite(u),
                    None => continue,
                };

                // 占一个限速名额；`share` 作用域结束时归还，后面的线程就能多分一点。
                let share = budget.enter();
                let result = transport::download_with_cancel(
                    &url,
                    &item.dest,
                    item.sha1.as_deref(),
                    share.kbps(),
                    &cancel,
                );
                drop(share);

                // 被取消时这一条可能留下了半截 `.part`，由调用方清理；
                // 这里不再把它算进失败，免得用户看到一堆红字。
                if cancel.is_cancelled() {
                    break;
                }

                let event = {
                    let mut rep = report.lock().unwrap();
                    match result {
                        Ok(fetched) => {
                            if fetched.skipped {
                                rep.skipped += 1;
                                Some(InstallProgress::Skipped(item.dest.clone()))
                            } else {
                                rep.downloaded += 1;
                                rep.bytes += fetched.bytes;
                                bytes.fetch_add(fetched.bytes as usize, Ordering::Relaxed);
                                None
                            }
                        }
                        Err(e) => {
                            rep.failed += 1;
                            rep.failures.push((item.dest.clone(), e.to_string()));
                            Some(InstallProgress::Failed(item.dest.clone(), e.to_string()))
                        }
                    }
                };
                if let Some(ev) = event {
                    on_progress(ev);
                }

                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                on_progress(InstallProgress::Tick {
                    done: n,
                    total,
                    bytes: bytes.load(Ordering::Relaxed) as u64,
                });
            });
        }
    });

    let mut report = Arc::try_unwrap(report)
        .map(|m| m.into_inner().unwrap())
        .unwrap_or_default();
    report.cancelled = cancel.is_cancelled();

    // 远古版本最后一步：把 objects 里的哈希文件按逻辑名铺一份。
    // 不做这一步游戏照样能开，只是没有声音、没有语言文件。
    if !report.cancelled {
        if let Some(virtual_assets) = &plan.virtual_assets {
            let (linked, copied, failed) = expand_virtual_assets(virtual_assets);
            crate::logging::info(format!(
                "远古版本资源：铺开 {} 份（硬链接 {}，复制 {}，失败 {}）",
                linked + copied,
                linked,
                copied,
                failed
            ));
        }
    }

    // 最后一步：把清单压平成自包含形式（HMCL 那种写法）。
    //
    // 失败不报错 —— 薄清单照样能启动，只是多一个「父版本必须在」的约束。
    // 装到一半（缺父版本 / 被取消 / 有失败条目）时不压，免得写出一份
    // 指向不存在文件的清单。
    if !report.cancelled && report.failed == 0 {
        match crate::flatten::rewrite_instance(&plan.game_dir, &plan.version_id) {
            Ok(bytes) => crate::logging::debug(format!(
                "{} 的版本清单已压平（{} 字节）",
                plan.version_id, bytes
            )),
            Err(e) => crate::logging::debug(format!(
                "{} 的版本清单保持原样：{}",
                plan.version_id, e
            )),
        }
    }

    Ok(report)
}

/// 把 `objects/<hash>` 按逻辑名铺进目标目录。
///
/// 优先硬链接（同一块盘、零额外空间），跨盘或文件系统不支持时退回复制。
/// 已经铺过且大小一致的直接跳过，所以重复安装不会反复搬文件。
///
/// 返回 (硬链接数, 复制数, 失败数)。
fn expand_virtual_assets(virtual_assets: &VirtualAssets) -> (usize, usize, usize) {
    let mut linked = 0usize;
    let mut copied = 0usize;
    let mut failed = 0usize;

    for dir in &virtual_assets.dirs {
        if let Err(e) = std::fs::create_dir_all(dir) {
            crate::logging::warn(format!("创建 {} 失败: {}", dir.display(), e));
            failed += virtual_assets.entries.len();
            continue;
        }
        for (name, hash) in &virtual_assets.entries {
            let source = crate::verify::asset_object_path(&virtual_assets.assets_root, hash);
            // 逻辑名用的是 `/`，Windows 上要换成当前分隔符
            let relative: PathBuf = name.split('/').collect();
            let dest = dir.join(relative);
            if let Some(parent) = dest.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    crate::logging::warn(format!("创建 {} 失败: {}", parent.display(), e));
                    failed += 1;
                    continue;
                }
            }
            if let (Ok(a), Ok(b)) = (std::fs::metadata(&source), std::fs::metadata(&dest)) {
                if a.len() == b.len() {
                    continue;
                }
            }
            let _ = std::fs::remove_file(&dest);
            if std::fs::hard_link(&source, &dest).is_ok() {
                linked += 1;
                continue;
            }
            match std::fs::copy(&source, &dest) {
                Ok(_) => copied += 1,
                Err(_) => failed += 1,
            }
        }
    }

    (linked, copied, failed)
}

/// 判断某个版本是否已安装。
pub fn is_installed(game_dir: &Path, version_id: &str) -> bool {
    game_dir
        .join("versions")
        .join(version_id)
        .join(format!("{}.json", version_id))
        .is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Arch, Features, OsInfo, OsName};

    fn win_ctx() -> RuleContext {
        RuleContext::new(
            OsInfo::for_test(OsName::Windows, Arch::X86_64),
            Features::default(),
        )
    }

    #[test]
    fn instance_game_version_skips_the_loader_prefix() {
        let dir = crate::testutil::fresh_dir("instance-game-version");
        let root = dir.as_ref();
        let version = root.join("versions").join("26.3-Fabric");
        std::fs::create_dir_all(&version).unwrap();
        std::fs::write(
            version.join("26.3-Fabric.json"),
            r#"{
                "id": "26.3-Fabric",
                "_aicraftOfficialId": "fabric-loader-0.20.1-26.3",
                "libraries": [{"name": "net.fabricmc:intermediary:26.3"}]
            }"#,
        )
        .unwrap();

        // 原版 id 是加载器那一层，游戏版本得另外认出来
        assert_eq!(
            instance_official_id(root, "26.3-Fabric").as_deref(),
            Some("fabric-loader-0.20.1-26.3")
        );
        assert_eq!(
            instance_game_version(root, "26.3-Fabric").as_deref(),
            Some("26.3")
        );
        assert_eq!(instance_game_version(root, "不存在"), None);
    }

    const MANIFEST: &str = r#"{
        "latest": {"release":"1.21.7","snapshot":"26.4-snapshot-5"},
        "versions": [
            {"id":"26.4-snapshot-5","type":"snapshot","url":"https://x/s.json","time":"t","releaseTime":"r","sha1":"a"},
            {"id":"1.21.7","type":"release","url":"https://x/1.json","time":"t","releaseTime":"2025-06-17","sha1":"b"}
        ]
    }"#;

    #[test]
    fn parses_manifest() {
        let m = parse_manifest(MANIFEST).unwrap();
        assert_eq!(m.latest_release.as_deref(), Some("1.21.7"));
        assert_eq!(m.versions.len(), 2);
        assert_eq!(m.releases().len(), 1);
        assert_eq!(m.releases()[0].id, "1.21.7");
        assert!(m.find("1.21.7").is_some());
        assert!(m.find("nope").is_none());
    }

    #[test]
    fn manifest_requires_versions() {
        assert!(parse_manifest(r#"{"latest":{}}"#).is_err());
    }

    const VERSION: &str = r#"{
        "id":"1.20.1-test",
        "mainClass":"net.minecraft.client.main.Main",
        "assetIndex":{"id":"5","url":"https://x/5.json","sha1":"idxsha","size":100},
        "assets":"5",
        "javaVersion":{"majorVersion":17},
        "downloads":{"client":{"url":"https://x/c.jar","sha1":"csha","size":1000}},
        "libraries":[
            {"name":"g:a:1.0","downloads":{"artifact":{"path":"g/a/1.0/a-1.0.jar","url":"https://x/a.jar","sha1":"asha","size":10}}},
            {"name":"g:mac:1.0","rules":[{"action":"allow","os":{"name":"osx"}}],
             "downloads":{"artifact":{"path":"g/mac/1.0/m.jar","url":"https://x/m.jar","sha1":"msha","size":20}}},
            {"name":"g:old:2.0","url":"https://maven.example/"}
        ],
        "logging":{"client":{"argument":"-Dlog4j.configurationFile=${path}",
            "file":{"id":"client-1.12.xml","url":"https://x/log.xml","sha1":"lsha","size":50},
            "type":"log4j2-xml"}},
        "arguments":{"game":["--username","${auth_player_name}"],"jvm":["-cp","${classpath}"]}
    }"#;

    const INDEX: &str = r#"{"objects":{
        "minecraft/a.ogg":{"hash":"aaaabbbbccccddddeeeeffff0000111122223333","size":5},
        "minecraft/b.ogg":{"hash":"1111222233334444555566667777888899990000","size":7}
    }}"#;

    const INDEX_PRE_1_6: &str = r#"{"map_to_resources":true,"objects":{
        "minecraft/a.ogg":{"hash":"aaaabbbbccccddddeeeeffff0000111122223333","size":5},
        "minecraft/b.ogg":{"hash":"1111222233334444555566667777888899990000","size":7}
    }}"#;

    #[test]
    fn plans_client_jar_and_index() {
        let plan = plan_install(
            Path::new("C:\\mc"),
            VERSION,
            Some(INDEX),
            &win_ctx(),
        )
        .unwrap();

        assert_eq!(plan.version_id, "1.20.1-test");
        assert!(plan
            .items
            .iter()
            .any(|i| i.kind == InstallKind::ClientJar));
        assert!(plan.items.iter().any(|i| i.kind == InstallKind::AssetIndex));
        // 客户端 jar 落点是 versions/<id>/<id>.jar
        let jar = plan
            .items
            .iter()
            .find(|i| i.kind == InstallKind::ClientJar)
            .unwrap();
        assert!(jar.dest.ends_with("1.20.1-test.jar"));
    }

    #[test]
    fn filters_libraries_by_platform() {
        let plan =
            plan_install(Path::new("C:\\mc"), VERSION, Some(INDEX), &win_ctx()).unwrap();

        let names: Vec<String> = plan
            .items
            .iter()
            .filter(|i| i.kind == InstallKind::Library)
            .map(|i| i.dest.to_string_lossy().into_owned())
            .collect();

        // macOS 专属的库不应出现
        assert!(
            !names.iter().any(|n| n.contains("m.jar")),
            "macOS 库被错误包含: {:?}",
            names
        );
        // 坐标推导的老式库应当出现
        assert!(
            names.iter().any(|n| n.contains("old")),
            "老式坐标库缺失: {:?}",
            names
        );
    }

    #[test]
    fn expands_asset_objects_with_cdn_urls() {
        let plan =
            plan_install(Path::new("C:\\mc"), VERSION, Some(INDEX), &win_ctx()).unwrap();
        let assets: Vec<_> = plan
            .items
            .iter()
            .filter(|i| i.kind == InstallKind::Asset)
            .collect();
        assert_eq!(assets.len(), 2);

        let a = assets
            .iter()
            .find(|i| i.sha1.as_deref() == Some("aaaabbbbccccddddeeeeffff0000111122223333"))
            .unwrap();
        assert_eq!(
            a.url.as_deref(),
            Some("https://resources.download.minecraft.net/aa/aaaabbbbccccddddeeeeffff0000111122223333")
        );
        // 落点按 hash 前两位分桶
        assert!(a.dest.to_string_lossy().contains("objects"));
        assert!(a.dest.to_string_lossy().ends_with("aaaabbbbccccddddeeeeffff0000111122223333"));
    }

    #[test]
    fn dedups_repeated_asset_hash() {
        let idx = r#"{"objects":{
            "a/x.ogg":{"hash":"aaaabbbbccccddddeeeeffff0000111122223333","size":5},
            "b/x.ogg":{"hash":"aaaabbbbccccddddeeeeffff0000111122223333","size":5}
        }}"#;
        let plan = plan_install(Path::new("C:\\mc"), VERSION, Some(idx), &win_ctx()).unwrap();
        let assets = plan
            .items
            .iter()
            .filter(|i| i.kind == InstallKind::Asset)
            .count();
        assert_eq!(assets, 1, "相同 hash 只应下载一次");
    }

    #[test]
    fn includes_log_config() {
        let plan =
            plan_install(Path::new("C:\\mc"), VERSION, Some(INDEX), &win_ctx()).unwrap();
        let logs: Vec<_> = plan
            .items
            .iter()
            .filter(|i| i.kind == InstallKind::LogConfig)
            .collect();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].dest.to_string_lossy().contains("log_configs"));
    }

    #[test]
    fn computes_stats_and_total_bytes() {
        let plan =
            plan_install(Path::new("C:\\mc"), VERSION, Some(INDEX), &win_ctx()).unwrap();
        assert!(!plan.stats.is_empty());
        // 至少包含客户端 1000 + 索引 100 + 两个资源 12 + 日志 50
        assert!(plan.total_bytes >= 1162, "total_bytes={}", plan.total_bytes);
        let downloadable = plan.downloadable().len();
        assert!(downloadable > 0);
    }

    #[test]
    fn duplicate_download_targets_appear_once() {
        // b1.7.3 / 1.5.2 的清单把 jinput-platform 写了两遍，两条算出同一个 dest。
        // 不去重的话两个线程会同时往一个文件下载，收尾时 rename 撞车。
        const DUP: &str = r#"{
            "id":"dup-test",
            "mainClass":"net.minecraft.client.main.Main",
            "assetIndex":{"id":"pre-1.6","url":"https://x/p.json","sha1":"idxsha","size":100},
            "assets":"pre-1.6",
            "javaVersion":{"majorVersion":8},
            "libraries":[
                {"name":"net.java.jinput:jinput-platform:2.0.5",
                 "natives":{"windows":"natives-windows"},
                 "downloads":{"classifiers":{"natives-windows":{
                     "path":"net/java/jinput/jinput-platform/2.0.5/jinput-platform-2.0.5-natives-windows.jar",
                     "url":"https://x/nat.jar","sha1":"nsha","size":30}}}},
                {"name":"net.java.jinput:jinput-platform:2.0.5",
                 "natives":{"windows":"natives-windows"},
                 "downloads":{"classifiers":{"natives-windows":{
                     "path":"net/java/jinput/jinput-platform/2.0.5/jinput-platform-2.0.5-natives-windows.jar",
                     "url":"https://x/nat.jar","sha1":"nsha","size":30}}}}
            ]
        }"#;
        let plan = plan_install(Path::new("C:\\mc"), DUP, Some(INDEX), &win_ctx()).unwrap();
        let mut dests: Vec<_> = plan.items.iter().map(|i| i.dest.clone()).collect();
        let total = dests.len();
        dests.sort();
        dests.dedup();
        assert_eq!(dests.len(), total, "同一个落点出现了多次: {:?}", dests);
    }

    #[test]
    fn a_legacy_index_asks_for_the_virtual_tree() {
        // pre-1.6：map_to_resources，虚拟目录与 resources/ 都要铺
        let plan = plan_install(
            Path::new("C:\\mc"),
            VERSION,
            Some(INDEX_PRE_1_6),
            &win_ctx(),
        )
        .unwrap();
        let assets = plan.virtual_assets.expect("pre-1.6 应当产生铺开任务");
        assert_eq!(assets.assets_root, Path::new("C:\\mc").join("assets"));
        assert_eq!(assets.entries.len(), 2);
        assert_eq!(assets.entries[0].0, "minecraft/a.ogg");
        assert!(
            assets
                .dirs
                .iter()
                .any(|d| d.ends_with("resources")),
            "resources/ 缺失: {:?}",
            assets.dirs
        );
        // 目录名跟着索引 id 走（这里 VERSION 用的是 "5"，真实远古版本是 pre-1.6）
        assert!(
            assets
                .dirs
                .iter()
                .any(|d| d.ends_with(Path::new("virtual").join("5"))),
            "virtual/<索引>/ 缺失: {:?}",
            assets.dirs
        );
    }

    #[test]
    fn a_modern_index_has_nothing_to_expand() {
        let plan = plan_install(Path::new("C:\\mc"), VERSION, Some(INDEX), &win_ctx()).unwrap();
        assert!(plan.virtual_assets.is_none());
    }

    #[test]
    fn version_json_path_is_correct() {
        let plan =
            plan_install(Path::new("C:\\mc"), VERSION, Some(INDEX), &win_ctx()).unwrap();
        assert!(plan.version_json_path.ends_with("1.20.1-test.json"));
        assert_eq!(plan.version_json, VERSION);
    }

    #[test]
    fn is_installed_detects_presence() {
        let tmp = crate::testutil::fresh_dir("install-check");
        assert!(!is_installed(&tmp, "1.20.1"));
        let dir = tmp.join("versions").join("1.20.1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("1.20.1.json"), "{}").unwrap();
        assert!(is_installed(&tmp, "1.20.1"));
        crate::testutil::cleanup(&tmp);
    }

    /// 没有索引内容时，资源对象无法展开，其余部分照常规划。
    #[test]
    fn works_without_asset_index() {
        let plan = plan_install(Path::new("C:\\mc"), VERSION, None, &win_ctx()).unwrap();
        assert!(
            !plan.items.iter().any(|i| i.kind == InstallKind::Asset),
            "没有索引时不应凭空产生资源条目"
        );
        assert!(plan.items.iter().any(|i| i.kind == InstallKind::ClientJar));
    }

    #[test]
    fn kind_labels_are_human_readable() {
        assert_eq!(InstallKind::Asset.as_str(), "资源");
        assert_eq!(InstallKind::ClientJar.as_str(), "客户端");
    }

    /// 只有 classifiers、没有 artifact 的纯原生库：
    /// 不应按坐标编出一个不存在的 URL（实测会让 1.8.9 必然失败 3 次），
    /// 而应只取当前平台的 native。
    #[test]
    fn native_only_library_uses_classifier_not_guessed_path() {
        let vjson = r#"{
            "id":"1.8.9-test",
            "mainClass":"net.minecraft.client.main.Main",
            "assetIndex":{"id":"1.8","url":"https://x/i.json"},
            "libraries":[{
                "name":"tv.twitch:twitch-platform:6.5",
                "natives":{"windows":"natives-windows-${arch}"},
                "downloads":{"classifiers":{
                    "natives-windows-64":{
                        "path":"tv/twitch/twitch-platform/6.5/twitch-platform-6.5-natives-windows-64.jar",
                        "url":"https://x/twitch-n64.jar","sha1":"aaaa","size":580098}
                }}
            }]
        }"#;

        let plan = plan_install(Path::new("C:\\mc"), vjson, None, &win_ctx()).unwrap();
        let libs: Vec<_> = plan
            .items
            .iter()
            .filter(|i| i.kind == InstallKind::Library)
            .collect();

        assert_eq!(libs.len(), 1, "应只有分类符对应的一个条目，实际 {:?}", libs);
        let item = libs[0];
        assert_eq!(item.url.as_deref(), Some("https://x/twitch-n64.jar"));
        assert!(
            item.dest.to_string_lossy().contains("natives-windows-64"),
            "落点应为 classifier 文件: {}",
            item.dest.display()
        );
        // 关键：绝不能出现按坐标拼出来的 twitch-platform-6.5.jar
        assert!(
            !plan.items.iter().any(|i| i
                .dest
                .to_string_lossy()
                .ends_with("twitch-platform-6.5.jar")),
            "不应凭坐标猜出不存在的 URL"
        );
    }

    /// 既无 artifact 也无 classifiers 的老式库，仍按坐标推导。
    #[test]
    fn plain_coordinate_library_still_derives_url() {
        let vjson = r#"{
            "id":"x",
            "mainClass":"M",
            "assetIndex":{"id":"1"},
            "libraries":[{"name":"g:plain:1.0","url":"https://maven.example/"}]
        }"#;
        let plan = plan_install(Path::new("C:\\mc"), vjson, None, &win_ctx()).unwrap();
        let item = plan
            .items
            .iter()
            .find(|i| i.kind == InstallKind::Library)
            .unwrap();
        assert_eq!(
            item.url.as_deref(),
            Some("https://maven.example/g/plain/1.0/plain-1.0.jar")
        );
    }

    // ---- 同一版本装成多个实例 ----

    #[test]
    fn instance_name_decides_the_directory() {
        let plan = plan_install_as(
            Path::new("C:\\mc"),
            VERSION,
            Some(INDEX),
            &win_ctx(),
            Some("1.20.1-copy"),
        )
        .unwrap();
        assert_eq!(plan.version_id, "1.20.1-copy");
        assert_eq!(plan.official_id, "1.20.1-test");
        assert!(plan.version_json_path.ends_with("1.20.1-copy.json"));
        assert!(plan
            .version_json_path
            .to_string_lossy()
            .contains("versions\\1.20.1-copy"));
        // 客户端 jar 也跟着实例名走
        let jar = plan
            .items
            .iter()
            .find(|i| i.kind == InstallKind::ClientJar)
            .unwrap();
        assert!(jar.dest.ends_with("1.20.1-copy.jar"), "{}", jar.dest.display());
    }

    #[test]
    fn retagging_rewrites_id_and_keeps_the_official_one() {
        let plan = plan_install_as(
            Path::new("C:\\mc"),
            VERSION,
            Some(INDEX),
            &win_ctx(),
            Some("copy-1"),
        )
        .unwrap();
        let text = &plan.version_json;
        // 顶层 id 换成实例名，否则 jar 路径与 ${version_name} 都会指错
        assert!(text.contains("\"id\":\"copy-1\""), "{}", text);
        // 官方版本号留在私有字段里，便于按版本聚合
        assert!(text.contains(OFFICIAL_ID_KEY), "{}", text);
        assert!(text.contains("\"1.20.1-test\""), "{}", text);
        // 其余字段原样保留
        assert!(text.contains("net.minecraft.client.main.Main"), "{}", text);
        assert!(text.contains("\"majorVersion\":17"), "{}", text);
    }

    #[test]
    fn no_instance_name_keeps_the_original_json_verbatim() {
        let plan = plan_install(Path::new("C:\\mc"), VERSION, Some(INDEX), &win_ctx()).unwrap();
        assert_eq!(plan.version_id, "1.20.1-test");
        assert_eq!(plan.official_id, "1.20.1-test");
        // 没改名就不该重写 JSON（保持与官方原文逐字节一致）
        assert_eq!(plan.version_json, VERSION);
    }

    #[test]
    fn same_name_as_official_is_not_retagged() {
        let plan = plan_install_as(
            Path::new("C:\\mc"),
            VERSION,
            Some(INDEX),
            &win_ctx(),
            Some("1.20.1-test"),
        )
        .unwrap();
        assert_eq!(plan.version_json, VERSION);
        assert!(!plan.version_json.contains(OFFICIAL_ID_KEY));
    }

    #[test]
    fn invalid_instance_names_fall_back_to_the_official_one() {
        for bad in ["", "  ", "a/b", "a\\b", "..", ".", "x:y", "x?y", "x|y"] {
            let plan = plan_install_as(
                Path::new("C:\\mc"),
                VERSION,
                Some(INDEX),
                &win_ctx(),
                Some(bad),
            )
            .unwrap();
            assert_eq!(plan.version_id, "1.20.1-test", "名字 {:?} 应被拒", bad);
        }
    }

    #[test]
    fn instance_names_allow_chinese_and_spaces() {
        for good in ["原版", "1.20.1 原版", "1.20.1-测试_2"] {
            assert!(is_valid_instance_name(good), "{:?} 应放行", good);
        }
    }

    #[test]
    fn installed_count_groups_instances_by_official_id() {
        let tmp = crate::testutil::fresh_dir("install-count");
        assert_eq!(installed_count(&tmp, "26.3"), 0);

        // 直接用官方名的实例
        let plain = tmp.join("versions").join("26.3");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("26.3.json"), r#"{"id":"26.3"}"#).unwrap();
        // 改过名的实例
        let copy = tmp.join("versions").join("26.3-2");
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::write(
            copy.join("26.3-2.json"),
            format!(
                r#"{{"id":"26.3-2","{}":"26.3"}}"#,
                OFFICIAL_ID_KEY
            ),
        )
        .unwrap();
        // 完全无关的版本
        let other = tmp.join("versions").join("1.20.1");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("1.20.1.json"), r#"{"id":"1.20.1"}"#).unwrap();

        assert_eq!(installed_count(&tmp, "26.3"), 2);
        assert_eq!(installed_count(&tmp, "1.20.1"), 1);
        assert_eq!(installed_count(&tmp, "26.4"), 0);
        assert_eq!(
            instance_official_id(&tmp, "26.3-2").as_deref(),
            Some("26.3")
        );
        assert_eq!(installed_instances(&tmp).len(), 3);
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn unwritable_game_dir_reports_the_path() {
        let tmp = crate::testutil::fresh_dir("test-install-unwritable");
        // 拿一个普通文件当游戏目录，`create_dir_all` 必然失败。
        let blocker = tmp.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();

        let plan = plan_install(&blocker, VERSION, Some(INDEX), &win_ctx()).unwrap();
        let err = execute_install_with_budget(
            &plan,
            1,
            &Mirror::Official,
            &Budget::unlimited(),
            |_| {},
        )
        .unwrap_err();

        // 报错要说清是哪个路径写不进去，而不是一句光秃秃的「拒绝访问」。
        assert!(err.contains("创建版本目录失败"), "{}", err);
        assert!(err.contains("blocker"), "{}", err);
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn permission_denied_carries_an_actionable_hint() {
        let denied = std::io::Error::from_raw_os_error(5);
        let hint = write_hint(&denied);
        // 「拒绝访问」对用户毫无信息量，得告诉他这是被降权了、能怎么绕。
        assert!(hint.contains("可以试试"), "{}", hint);
        assert!(hint.contains("完整性"), "{}", hint);
        // 其他错误不硬凑建议
        assert_eq!(write_hint(&std::io::Error::other("别的")), "");
    }

    #[test]
    fn renaming_moves_the_directory_and_retags_the_json() {
        let tmp = crate::testutil::fresh_dir("install-rename");
        let old = tmp.join("versions").join("1.20.1-forge-47.0.1");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(
            old.join("1.20.1-forge-47.0.1.json"),
            r#"{"id":"1.20.1-forge-47.0.1","mainClass":"x"}"#,
        )
        .unwrap();
        std::fs::write(old.join("1.20.1-forge-47.0.1.jar"), b"jar").unwrap();
        std::fs::write(old.join("notes.txt"), b"keep").unwrap();

        rename_instance(&tmp, "1.20.1-forge-47.0.1", "生存整合").unwrap();

        let new = tmp.join("versions").join("生存整合");
        assert!(!old.exists());
        assert!(new.join("生存整合.json").is_file());
        assert!(new.join("生存整合.jar").is_file());
        // 无关文件原样留着
        assert_eq!(std::fs::read(new.join("notes.txt")).unwrap(), b"keep");

        let text = std::fs::read_to_string(new.join("生存整合.json")).unwrap();
        assert!(text.contains(r#""id":"生存整合""#), "{}", text);
        // 官方版本号留在私有字段里，统计还能聚合
        assert_eq!(
            instance_official_id(&tmp, "生存整合").as_deref(),
            Some("1.20.1-forge-47.0.1")
        );
        crate::testutil::cleanup(&tmp);
    }

    #[test]
    fn renaming_refuses_to_clobber_an_existing_instance() {
        let tmp = crate::testutil::fresh_dir("install-rename-clash");
        for name in ["a", "b"] {
            let dir = tmp.join("versions").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{}.json", name)), "{}").unwrap();
        }
        let err = rename_instance(&tmp, "a", "b").unwrap_err();
        assert!(err.contains("已经被占用"), "{}", err);
        // 两个都还在，一个都没被吃掉
        assert!(tmp.join("versions").join("a").is_dir());
        assert!(tmp.join("versions").join("b").is_dir());
        crate::testutil::cleanup(&tmp);
    }
}
