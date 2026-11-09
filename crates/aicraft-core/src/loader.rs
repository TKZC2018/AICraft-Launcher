//! 模组加载器与 Fabric API 的安装。
//!
//! 三种加载器走的路子完全不同，这里如实照它们各自官方的做法来：
//!
//! - **Fabric**：官方提供「一装即用」的版本清单
//!   （`/v2/versions/loader/<游戏版本>/<加载器版本>/profile/json`），
//!   里面已经写好了 `mainClass` 与要下的库，直接喂给
//!   [`crate::install::plan_install_as`] 就行 —— 和装一个原版版本是同一条路。
//! - **Forge / NeoForge**：没有这种清单，只有安装器 jar。安装器会自己做
//!   binary patch（`processors`），所以必须**真的把 java 跑起来**：
//!   `java -jar <installer> --installClient <游戏目录>`。
//!   两个坑写在各自的 `install_*` 里：安装器要求目录里先有
//!   `launcher_profiles.json`；NeoForge 的安装器只会去
//!   `maven.neoforged.net` 拿库，而那个域在部分网络下不可达，
//!   所以要先按官方 CDN 把库预置进 `libraries/`，让安装器只剩下校验与 patch。
//!
//! 这些流程都依赖网络与子进程，不适合在单测里跑；可测的部分（版本号映射、
//! URL 拼装、清单解析）都抽成了纯函数，见文件末尾。

use std::path::{Path, PathBuf};

use crate::json::{self, Json};
use crate::version::parse_version;

/// Fabric 官方 meta 服务。
pub const FABRIC_META: &str = "https://meta.fabricmc.net";
/// Fabric meta 的 BMCLAPI 镜像。
pub const FABRIC_META_MIRROR: &str = "https://bmclapi2.bangbang93.com/fabric-meta";
/// Forge 官方 maven。
pub const FORGE_MAVEN: &str = "https://maven.minecraftforge.net";
/// BMCLAPI 的 maven 镜像（也是 Forge 版本列表的来源）。
pub const BMCLAPI: &str = "https://bmclapi2.bangbang93.com";
/// BMCLAPI 的 maven 前缀。官方 maven 的 `/<path>` 在这里是 `/maven/<path>`。
pub const BMCLAPI_MAVEN: &str = "https://bmclapi2.bangbang93.com/maven";
/// BMCLAPI 给 NeoForge 单开的接口前缀（`/neoforge/list/<mc>`、`/neoforge/version/<v>/download/<file>`）。
///
/// 这个前缀比 `/maven` 靠谱得多：NeoForge 的 `maven-metadata.xml` 在 BMCLAPI
/// 上会 302 到一台经常连不上的镜像机，而 `/neoforge/list` 是它自己整理的索引。
pub const BMCLAPI_NEOFORGE: &str = "https://bmclapi2.bangbang93.com/neoforge";
/// NeoForge 官方 CDN。`maven.neoforged.net` 在不少网络下不可达，这个是能用的那个。
pub const NEOFORGE_CDN: &str = "https://neoforged.forgecdn.net";
/// NeoForge 安装器默认会去的主机前缀 —— 预置库时要把这个换成可用的地址。
pub const NEOFORGE_DEAD_MAVEN: &str = "https://maven.neoforged.net/releases/";
/// Modrinth API（Fabric API 的版本与下载地址都从这里查）。
///
/// BMCLAPI 没有 Modrinth 的镜像（`/modrinth/...` 是 404），所以这一项只有官方。
pub const MODRINTH_API: &str = "https://api.modrinth.com/v2";

/// 能装的加载器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderKind {
    Fabric,
    Forge,
    NeoForge,
}

impl LoaderKind {
    pub const ALL: [LoaderKind; 3] = [
        LoaderKind::Fabric,
        LoaderKind::Forge,
        LoaderKind::NeoForge,
    ];

    /// 存进配置/日志用的短名。
    pub fn id(self) -> &'static str {
        match self {
            LoaderKind::Fabric => "fabric",
            LoaderKind::Forge => "forge",
            LoaderKind::NeoForge => "neoforge",
        }
    }

    /// 界面上显示的名字。
    pub fn label(self) -> &'static str {
        match self {
            LoaderKind::Fabric => "Fabric",
            LoaderKind::Forge => "Forge",
            LoaderKind::NeoForge => "NeoForge",
        }
    }

    pub fn from_id(id: &str) -> Option<LoaderKind> {
        LoaderKind::ALL
            .into_iter()
            .find(|kind| kind.id().eq_ignore_ascii_case(id.trim()))
    }
}

/// 一个可选的加载器版本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderVersion {
    /// 加载器自己的版本号，如 `0.16.10` / `47.0.1` / `21.1.72`。
    pub version: String,
    /// 它对应的游戏版本。Fabric 的列表是按游戏版本查的，这里填请求的那个；
    /// NeoForge 的版本号本身就编码了游戏版本，从里面解出来。
    pub game_version: String,
    /// 官方是否标为稳定版。查不到就当稳定（Forge 的列表没有这个信息）。
    pub stable: bool,
}

/// 加载器安装过程中能报给界面的进度。
///
/// 为什么加载器这块要单独一套：它有「说不清进度」的阶段（跑 Forge/NeoForge
/// 的 java 安装器，解包、打补丁，几分钟都有可能），那种时候硬塞一个百分比
/// 是在骗人，不如让界面显示一个转动的条加一句说明。
#[derive(Debug, Clone)]
pub enum LoaderProgress {
    /// 能算出比例的时候：已完成 / 总数 / 累计字节。
    Tick {
        done: usize,
        total: usize,
        bytes: u64,
    },
    /// 正在做一件说不清进度的事。
    Working(String),
}

/// 安装结果。
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    /// 落盘的实例名，即 `versions/<instance>/` 的目录名。
    /// 被取消时是空串——那时候没有东西留下。
    pub instance: String,
    /// 是不是玩家中途点了取消。为真时 `instance` 为空，已下的文件留给调用方清理。
    pub cancelled: bool,
}

/// 从一组候选地址里挑第一个能用的取文本。
///
/// `rest` 是拼在基址后面的路径（含查询串），比如 `/v2/versions/loader/1.20.1`。
pub fn fetch_text_any(bases: &[&str], rest: &str) -> Result<String, String> {
    let mut errors = Vec::new();
    for base in bases {
        let url = format!("{}{}", base.trim_end_matches('/'), rest);
        match crate::transport::fetch_text(&url) {
            Ok(text) => return Ok(text),
            Err(e) => errors.push(format!("{}: {}", url, e)),
        }
    }
    Err(format!("都拿不到，试过：{}", errors.join("；")))
}

/// 从一组候选地址里挑第一个能用的下载到 `dest`。
pub fn download_any(bases: &[&str], rest: &str, dest: &Path) -> Result<PathBuf, String> {
    let mut errors = Vec::new();
    for base in bases {
        let url = format!("{}{}", base.trim_end_matches('/'), rest);
        match crate::transport::download_to(&url, dest, None) {
            Ok(_) => return Ok(dest.to_path_buf()),
            Err(e) => errors.push(format!("{}: {}", url, e)),
        }
    }
    Err(format!("都下不下来，试过：{}", errors.join("；")))
}

// ---------------------------------------------------------------- 下载源

/// Fabric 的候选基址（按用户选的源排）。
fn fabric_bases(mirror: crate::settings::Mirror) -> [&'static str; 2] {
    if mirror.resolved(crate::settings::ProbeKind::Fabric) == crate::settings::Mirror::Bmclapi {
        [FABRIC_META_MIRROR, FABRIC_META]
    } else {
        [FABRIC_META, FABRIC_META_MIRROR]
    }
}

/// Forge 安装器的候选基址。BMCLAPI 的 maven 路径要加 `/maven`。
fn forge_bases(mirror: crate::settings::Mirror) -> [&'static str; 2] {
    if mirror.resolved(crate::settings::ProbeKind::Forge) == crate::settings::Mirror::Bmclapi {
        [BMCLAPI_MAVEN, FORGE_MAVEN]
    } else {
        [FORGE_MAVEN, BMCLAPI_MAVEN]
    }
}

/// NeoForge 安装器的候选地址（已经拼好，不用再按 `rest` 拼）。
fn neoforge_installer_candidates(
    version: &str,
    mirror: crate::settings::Mirror,
) -> Vec<String> {
    let maven_url = format!(
        "{}/net/neoforged/neoforge/{v}/neoforge-{v}-installer.jar",
        BMCLAPI_MAVEN,
        v = version
    );
    let official = neoforge_installer_url(version);
    if mirror.resolved(crate::settings::ProbeKind::NeoForge) == crate::settings::Mirror::Bmclapi {
        vec![
            format!("{}/{}/download/installer", BMCLAPI_NEOFORGE, version),
            maven_url,
            official,
        ]
    } else {
        vec![official, maven_url]
    }
}

// ---------------------------------------------------------------- Fabric

/// 从一组完整地址里挑第一个能用的取文本。
pub fn fetch_text_urls(urls: &[String]) -> Result<String, String> {
    let mut errors = Vec::new();
    for url in urls {
        match crate::transport::fetch_text(url) {
            Ok(text) => return Ok(text),
            Err(e) => errors.push(format!("{}: {}", url, e)),
        }
    }
    Err(format!("都拿不到，试过：{}", errors.join("；")))
}

/// 从一组完整地址里挑第一个能用的下载到 `dest`。
pub fn download_urls(urls: &[String], dest: &Path) -> Result<PathBuf, String> {
    let mut errors = Vec::new();
    for url in urls {
        match crate::transport::download_to(url, dest, None) {
            Ok(_) => return Ok(dest.to_path_buf()),
            Err(e) => errors.push(format!("{}: {}", url, e)),
        }
    }
    Err(format!("都下不下来，试过：{}", errors.join("；")))
}

/// 列出某个游戏版本可用的 Fabric 加载器。
pub fn list_fabric_loaders(
    mc_version: &str,
    mirror: crate::settings::Mirror,
) -> Result<Vec<LoaderVersion>, String> {
    let rest = format!("/v2/versions/loader/{}", mc_version);
    let text = fetch_text_any(&fabric_bases(mirror), &rest)?;
    parse_fabric_loaders(&text, mc_version)
}

/// 解析 Fabric 的加载器列表。
///
/// 形状是数组，每项 `{"loader":{"version":"0.16.10","stable":true},...}`。
pub fn parse_fabric_loaders(text: &str, mc_version: &str) -> Result<Vec<LoaderVersion>, String> {
    let root = json::parse(text).map_err(|e| format!("解析 Fabric 列表失败: {}", e))?;
    let Some(items) = root.as_array() else {
        return Err("Fabric 列表不是数组".to_string());
    };
    let mut out = Vec::new();
    for item in items {
        let Some(loader) = item.get("loader") else {
            continue;
        };
        let Some(version) = loader.get("version").and_then(Json::as_str) else {
            continue;
        };
        out.push(LoaderVersion {
            version: version.to_string(),
            game_version: mc_version.to_string(),
            stable: loader.get("stable").and_then(Json::as_bool).unwrap_or(true),
        });
    }
    if out.is_empty() {
        return Err(format!("Fabric 没有适用于 {} 的加载器", mc_version));
    }
    Ok(out)
}

/// 取 Fabric 为「某游戏版本 + 某加载器版本」生成的版本清单。
pub fn fabric_profile(
    mc_version: &str,
    loader_version: &str,
    mirror: crate::settings::Mirror,
) -> Result<String, String> {
    let rest = format!(
        "/v2/versions/loader/{}/{}/profile/json",
        mc_version, loader_version
    );
    fetch_text_any(&fabric_bases(mirror), &rest)
}

/// 装 Fabric：拿 profile 清单直接走版本安装流程。
///
/// `instance` 是玩家给这一份起的名字（`None` 就用清单里的 `id`，
/// `fabric-loader-0.16.10-1.20.1`，和 Fabric 官方启动器一致）。
// 参数和另外两个加载器长得一样，为了调用处看着整齐，就不合成结构体了。
#[allow(clippy::too_many_arguments)]
pub fn install_fabric<F>(
    game_dir: &Path,
    mc_version: &str,
    loader_version: &str,
    mirror: &crate::settings::Mirror,
    concurrency: usize,
    instance: Option<&str>,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(LoaderProgress) + Send + Sync),
    mut on_log: F,
) -> Result<InstallOutcome, String>
where
    F: FnMut(&str),
{
    on_log(&format!(
        "获取 Fabric {} 的清单（游戏 {}）…",
        loader_version, mc_version
    ));
    let profile_json = fabric_profile(mc_version, loader_version, *mirror)?;

    let parsed =
        parse_version(&profile_json).map_err(|e| format!("解析 Fabric 清单失败: {}", e))?;
    let default_name = if parsed.id.is_empty() {
        format!("fabric-loader-{}-{}", loader_version, mc_version)
    } else {
        parsed.id.clone()
    };
    let instance = clean_instance_name(instance, &default_name);
    let retag = instance != default_name;
    let profile_json = if retag {
        crate::install::retag_version_json(&profile_json, &instance, &default_name)?
    } else {
        profile_json
    };

    let ctx = crate::rules::RuleContext::current();
    let plan = crate::install::plan_install_as(
        game_dir,
        &profile_json,
        None,
        &ctx,
        Some(&instance),
    )?;

    on_log(&format!(
        "Fabric 需要 {} 个文件，约 {:.1} MB",
        plan.items.len(),
        plan.total_bytes as f64 / 1_048_576.0
    ));

    let budget = crate::speed::Budget::unlimited();
    let report = crate::install::execute_install_live(
        &plan,
        concurrency,
        mirror,
        &budget,
        cancel,
        &|ev| match ev {
            crate::install::InstallProgress::Stage(text) => crate::logging::info(text),
            crate::install::InstallProgress::Tick {
                done,
                total,
                bytes,
            } => on_progress(LoaderProgress::Tick {
                done,
                total,
                bytes,
            }),
            _ => {}
        },
    )?;

    if report.cancelled {
        on_log("Fabric 安装被取消");
        return Ok(InstallOutcome {
            instance: String::new(),
            cancelled: true,
        });
    }

    if report.failed > 0 {
        let first = report
            .failures
            .first()
            .map(|(p, m)| format!("{}（{}）", p.display(), m))
            .unwrap_or_default();
        return Err(format!(
            "{} 个库下载失败，第一个是 {}",
            report.failed, first
        ));
    }

    on_log(&format!(
        "Fabric 装好了：versions/{}（下载 {} 个文件）",
        plan.version_id, report.downloaded
    ));
    Ok(InstallOutcome {
        instance: plan.version_id,
        cancelled: false,
    })
}

/// 玩家给的实例名合不合用；不合用就退回默认名。
fn clean_instance_name(wanted: Option<&str>, default_name: &str) -> String {
    match wanted.map(str::trim) {
        Some(name) if !name.is_empty() && crate::install::is_valid_instance_name(name) => {
            name.to_string()
        }
        _ => default_name.to_string(),
    }
}

// ---------------------------------------------------------------- Forge

/// 列出某个游戏版本可用的 Forge 版本（按版本号从新到旧）。
///
/// 两条路：
/// - 镜像：BMCLAPI 的 `/forge/minecraft/<mc>`，它已经按游戏版本分好组了；
/// - 官方：Forge 没有「按游戏版本列加载器」的接口，只能拉整份
///   `maven-metadata.xml`（5000 多个版本）再按 `<mc>-` 前缀筛。
///
/// 官方那条失败时退回 BMCLAPI —— 列表内容两边是同一份，区别只在谁更快。
pub fn list_forge_versions(
    mc_version: &str,
    mirror: crate::settings::Mirror,
) -> Result<Vec<LoaderVersion>, String> {
    if mirror.resolved(crate::settings::ProbeKind::Forge) == crate::settings::Mirror::Bmclapi {
        let rest = format!("/forge/minecraft/{}", mc_version);
        let text = fetch_text_any(&[BMCLAPI], &rest)?;
        parse_forge_versions(&text, mc_version)
    } else {
        let rest = "/net/minecraftforge/forge/maven-metadata.xml";
        // 官方 maven 偶尔超时，镜像那份内容一模一样，兜一下底。
        let text = fetch_text_any(&[FORGE_MAVEN, BMCLAPI_MAVEN], rest)?;
        parse_forge_metadata(&text, mc_version)
    }
}

/// 从 Forge 的 `maven-metadata.xml` 里筛出某个游戏版本的构建。
///
/// 里面全是 `<version>1.20.1-47.0.1</version>` 这样的行，按 `<mc>-` 前缀取剩下
/// 的部分就是 Forge 版本号。这里不引 XML 库：文档结构固定，扫标签就够。
pub fn parse_forge_metadata(text: &str, mc_version: &str) -> Result<Vec<LoaderVersion>, String> {
    let prefix = format!("{}-", mc_version);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for chunk in text.split("<version>").skip(1) {
        let Some(end) = chunk.find("</version>") else {
            continue;
        };
        let full = chunk[..end].trim();
        let Some(rest) = full.strip_prefix(&prefix) else {
            continue;
        };
        if rest.is_empty() || !seen.insert(rest.to_string()) {
            continue;
        }
        out.push(LoaderVersion {
            version: rest.to_string(),
            game_version: mc_version.to_string(),
            stable: !rest.contains("beta"),
        });
    }
    if out.is_empty() {
        return Err(format!("Forge 没有适用于 {} 的版本", mc_version));
    }
    out.sort_by(|a, b| compare_versions(&b.version, &a.version));
    Ok(out)
}

/// 解析 BMCLAPI 的 Forge 版本列表。
///
/// 形状是数组，每项 `{"version":"47.0.1","mcversion":"1.20.1","build":47000001,...}`。
pub fn parse_forge_versions(text: &str, mc_version: &str) -> Result<Vec<LoaderVersion>, String> {
    let root = json::parse(text).map_err(|e| format!("解析 Forge 列表失败: {}", e))?;
    let Some(items) = root.as_array() else {
        return Err("Forge 列表不是数组".to_string());
    };
    let mut out: Vec<(i64, LoaderVersion)> = Vec::new();
    for item in items {
        let Some(version) = item.get("version").and_then(Json::as_str) else {
            continue;
        };
        let build = item.get("build").and_then(Json::as_i64).unwrap_or(0);
        out.push((
            build,
            LoaderVersion {
                version: version.to_string(),
                game_version: mc_version.to_string(),
                stable: true,
            },
        ));
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.0));
    let out: Vec<LoaderVersion> = out.into_iter().map(|(_, v)| v).collect();
    if out.is_empty() {
        return Err(format!("Forge 没有适用于 {} 的版本", mc_version));
    }
    Ok(out)
}

/// Forge 安装器的下载地址。
pub fn forge_installer_url(mc_version: &str, forge_version: &str) -> String {
    format!(
        "{}/net/minecraftforge/forge/{mc}-{fv}/forge-{mc}-{fv}-installer.jar",
        FORGE_MAVEN,
        mc = mc_version,
        fv = forge_version
    )
}

/// Forge 安装器在 BMCLAPI 镜像上的地址。
pub fn forge_installer_mirror_url(mc_version: &str, forge_version: &str) -> String {
    format!(
        "{}/maven/net/minecraftforge/forge/{mc}-{fv}/forge-{mc}-{fv}-installer.jar",
        BMCLAPI,
        mc = mc_version,
        fv = forge_version
    )
}

/// 装 Forge。
///
/// 两个必须做的事：
/// 1. 目录里要先有 `launcher_profiles.json`，否则安装器直接报
///    `There is no minecraft launcher profile in "…", you need to run the launcher first!`
/// 2. 安装器会真的去 patch 原版 class 文件，所以得用 java 跑起来，跑完才有
///    `versions/<mc>-forge-<版本>/` 与 `libraries/` 里那堆 patched 产物。
///
/// 安装器只认它自己那套目录名，玩家想要的名字只能装完再搬（[`rename_instance`]）。
#[allow(clippy::too_many_arguments)]
pub fn install_forge<F>(
    game_dir: &Path,
    mc_version: &str,
    forge_version: &str,
    mirror: crate::settings::Mirror,
    asked: Option<&str>,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(LoaderProgress) + Send + Sync),
    mut on_log: F,
) -> Result<InstallOutcome, String>
where
    F: FnMut(&str),
{
    if cancel.is_cancelled() {
        return Ok(InstallOutcome {
            instance: String::new(),
            cancelled: true,
        });
    }

    let dest_dir = game_dir.join("cache");
    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("建缓存目录失败: {}", e))?;
    let installer = dest_dir.join(format!("forge-{}-{}-installer.jar", mc_version, forge_version));

    if !installer.is_file() {
        on_log(&format!("下载 Forge {} 安装器…", forge_version));
        let rest = format!(
            "/net/minecraftforge/forge/{mc}-{fv}/forge-{mc}-{fv}-installer.jar",
            mc = mc_version,
            fv = forge_version
        );
        download_any(&forge_bases(mirror), &rest, &installer)
            .map_err(|e| format!("下载 Forge 安装器失败: {}", e))?;
    }

    // 先把安装器要用的库自己下好。
    //
    // 为什么必须有这一步：Forge 安装器会自己从 `maven.minecraftforge.net` 拉六十多个
    // 构件，而这个域名在本机经常十几秒才回一次、个别直接超时，安装器没有任何超时
    // 上限，就那么干等着（实测挂了三十分钟没动静）。安装器遇到已存在的文件会跳过，
    // 所以提前按同样的路径铺好，它就不联网了。
    let source_label = if mirror.resolved(crate::settings::ProbeKind::Forge) == crate::settings::Mirror::Bmclapi {
        "BMCLAPI 镜像"
    } else {
        "Forge 官方 maven"
    };
    preseed_installer_libraries(
        game_dir,
        &installer,
        mirror,
        source_label,
        forge_library_url,
        cancel,
        on_progress,
        &mut on_log,
    )?;

    ensure_launcher_profiles(game_dir, &mut on_log)?;

    let java = installer_java(game_dir, mc_version)?;
    on_log(&format!("用 {} 运行 Forge 安装器…", java.display()));

    if !run_installer(
        &java,
        &installer,
        game_dir,
        "Forge",
        cancel,
        on_progress,
        &mut on_log,
    )? {
        on_log("安装被取消，Forge 没有装好");
        return Ok(InstallOutcome {
            instance: String::new(),
            cancelled: true,
        });
    }

    let instance = format!("{}-forge-{}", mc_version, forge_version);
    if !crate::install::is_installed(game_dir, &instance) {
        return Err(format!(
            "安装器跑完了，但没找到 versions/{} —— 安装器可能换了命名",
            instance
        ));
    }

    // 安装器只会叫 `1.20.1-forge-47.0.1`，玩家想叫什么得装完再搬。
    let wanted = clean_instance_name(asked, &instance);
    if wanted != instance {
        on_log(&format!("把实例改名成 {}…", wanted));
        crate::install::rename_instance(game_dir, &instance, &wanted)?;
    }

    on_log(&format!("Forge 装好了：versions/{}", wanted));
    Ok(InstallOutcome {
        instance: wanted,
        cancelled: false,
    })
}

// ---------------------------------------------------------------- NeoForge

/// 从 NeoForge 版本号反推它对应的游戏版本。
///
/// NeoForge 的版本号就是「游戏版本的短写」+ 自己的号：
/// `21.1.72` → Minecraft `1.21.1`；`20.4.245` → `1.20.4`。
/// 26.x 之后游戏版本本身不再带前导 `1.`，所以 `26.1.169` → `26.1`。
pub fn neoforge_game_version(neoforge_version: &str) -> Option<String> {
    let mut parts = neoforge_version.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    // 只认纯数字，带 `-beta` 之类后缀的照样能用（后缀在第三段里）
    if !major.chars().all(|c| c.is_ascii_digit()) || !minor.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let major_num: u32 = major.parse().ok()?;
    Some(if major_num >= 26 {
        format!("{}.{}", major, minor)
    } else {
        format!("1.{}.{}", major, minor)
    })
}

/// 列出某个游戏版本可用的 NeoForge 版本（从新到旧）。
///
/// 只走官方 CDN：`maven.neoforged.net` 与 BMCLAPI 的 metadata 都不可靠
/// （前者超时，后者 302 到一个同样不可达的主机）。
pub fn list_neoforge_versions(
    mc_version: &str,
    mirror: crate::settings::Mirror,
) -> Result<Vec<LoaderVersion>, String> {
    // BMCLAPI 没有能用的 maven-metadata（会 302 到一台常挂的机器），
    // 但它自己整理了一份按游戏版本分好的列表。
    if mirror.resolved(crate::settings::ProbeKind::NeoForge) == crate::settings::Mirror::Bmclapi {
        let rest = format!("/list/{}", mc_version);
        match fetch_text_any(&[BMCLAPI_NEOFORGE], &rest) {
            Ok(text) => parse_neoforge_bmclapi_list(&text, mc_version),
            // 列表接口挂了就退回官方 CDN 的全量列表
            Err(e) => {
                crate::logging::warn(format!("BMCLAPI 的 NeoForge 列表拿不到（{}），改走官方", e));
                list_neoforge_versions(mc_version, crate::settings::Mirror::Official)
            }
        }
    } else {
        let rest = "/api/maven/versions/releases/net/neoforged/neoforge";
        let text = fetch_text_any(&[NEOFORGE_CDN], rest)?;
        parse_neoforge_versions(&text, mc_version)
    }
}

/// 解析 BMCLAPI 的 `/neoforge/list/<mc>` 列表。
///
/// 形状是 `[{"rawVersion":"neoforge-21.1.1","version":"21.1.1","mcversion":"1.21.1",...}]`，
/// 已经按游戏版本筛过了，这里只要把字段取出来并按新到旧排一遍。
pub fn parse_neoforge_bmclapi_list(
    text: &str,
    mc_version: &str,
) -> Result<Vec<LoaderVersion>, String> {
    let root = json::parse(text).map_err(|e| format!("解析 NeoForge 镜像列表失败: {}", e))?;
    let Some(items) = Json::as_array(&root) else {
        return Err("NeoForge 镜像列表顶层不是数组".to_string());
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let version = item.get("version").and_then(Json::as_str);
        let Some(version) = version else {
            continue;
        };
        // 镜像偶尔会混进别的游戏版本的条目，按字段再校一遍
        if let Some(gv) = item.get("mcversion").and_then(Json::as_str) {
            if gv != mc_version {
                continue;
            }
        }
        if !seen.insert(version.to_string()) {
            continue;
        }
        out.push(LoaderVersion {
            version: version.to_string(),
            game_version: mc_version.to_string(),
            stable: !version.contains("beta"),
        });
    }
    if out.is_empty() {
        return Err(format!("NeoForge 没有适用于 {} 的版本", mc_version));
    }
    // 列表是文本序的，按版本号从新到旧更稳妥
    out.sort_by(|a, b| compare_versions(&b.version, &a.version));
    Ok(out)
}

/// 粗粒度版本比较：逐段比数字，**数字段不够就不相等**。
///
/// 只用于排序展示，不参与任何「兼容性」判断，所以「21.1.10 > 21.1.9」这种
/// 数字比较对了就够。
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |s: &str| -> Vec<String> {
        s.split(['.', '-', '+'])
            .map(|part| part.to_string())
            .collect()
    };
    let (pa, pb) = (split(a), split(b));
    for (x, y) in pa.iter().zip(pb.iter()) {
        let nx = x.parse::<u64>();
        let ny = y.parse::<u64>();
        let ord = match (nx, ny) {
            (Ok(nx), Ok(ny)) => nx.cmp(&ny),
            _ => x.cmp(y),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    pa.len().cmp(&pb.len())
}

/// 解析 NeoForge 的版本列表。
///
/// 形状是 `{"isSnapshot":false,"versions":["21.1.72", ...]}`，是**全量**列表，
/// 所以这里要按 [`neoforge_game_version`] 过滤出对应当前游戏版本的那些。
pub fn parse_neoforge_versions(
    text: &str,
    mc_version: &str,
) -> Result<Vec<LoaderVersion>, String> {
    let root = json::parse(text).map_err(|e| format!("解析 NeoForge 列表失败: {}", e))?;
    let Some(items) = root.get("versions").and_then(Json::as_array) else {
        return Err("NeoForge 列表缺少 versions".to_string());
    };
    let mut out = Vec::new();
    for item in items {
        let Some(version) = item.as_str() else {
            continue;
        };
        if neoforge_game_version(version).as_deref() != Some(mc_version) {
            continue;
        }
        out.push(LoaderVersion {
            version: version.to_string(),
            game_version: mc_version.to_string(),
            stable: !version.contains("beta"),
        });
    }
    // 官方给的是从旧到新，界面上想先看到新的
    out.reverse();
    if out.is_empty() {
        return Err(format!("NeoForge 没有适用于 {} 的版本", mc_version));
    }
    Ok(out)
}

/// NeoForge 安装器在官方 CDN 上的地址。
pub fn neoforge_installer_url(neoforge_version: &str) -> String {
    format!(
        "{}/releases/net/neoforged/neoforge/{v}/neoforge-{v}-installer.jar",
        NEOFORGE_CDN,
        v = neoforge_version
    )
}

/// 把 NeoForge 安装器里的死链换成能用的地址。
///
/// 安装器里的每个库地址都写着 `maven.neoforged.net`，预置库时要换掉，
/// 否则就会一路 `SocketTimeoutException`。按用户选的源走两条不同的路。
pub fn neoforge_library_url(url: &str, mirror: crate::settings::Mirror) -> String {
    if mirror.resolved(crate::settings::ProbeKind::NeoForge) != crate::settings::Mirror::Bmclapi {
        return rewrite_neoforge(url);
    }
    match url.strip_prefix(NEOFORGE_DEAD_MAVEN) {
        // neoforged 的构件：把前缀换成 BMCLAPI 的 /maven
        Some(rest) => format!("{}/{}", BMCLAPI_MAVEN, rest),
        // 别的主机（libraries.minecraft.net 之类）交给通用的镜像规则
        None => crate::settings::mirror_rewrite(url, crate::settings::Mirror::Bmclapi),
    }
}

/// Forge 的库地址：安装器里写的全是 `maven.minecraftforge.net`，
/// 这台机器上那个域名经常几十秒不回或者干脆超时，安装器会卡在那儿不动。
///
/// 走 BMCLAPI 时换成它的 `/maven` 前缀；走官方时原样返回
/// （官方地址本身是能通的，只是慢）。
pub fn forge_library_url(url: &str, mirror: crate::settings::Mirror) -> String {
    if mirror.resolved(crate::settings::ProbeKind::Forge) != crate::settings::Mirror::Bmclapi {
        return url.to_string();
    }
    match url.strip_prefix(FORGE_MAVEN) {
        // FORGE_MAVEN 不带结尾斜杠，剩下的部分开头还有一条，要去掉再拼
        Some(rest) => format!("{}/{}", BMCLAPI_MAVEN, rest.trim_start_matches('/')),
        None => crate::settings::mirror_rewrite(url, crate::settings::Mirror::Bmclapi),
    }
}

/// 把 NeoForge 的死链换成能用的 CDN。
///
/// 安装器里的每个库地址都写着 `maven.neoforged.net`，预置库时要换成
/// `NEOFORGE_CDN`，否则就会一路 `SocketTimeoutException`。
pub fn rewrite_neoforge(url: &str) -> String {
    url.replace(
        NEOFORGE_DEAD_MAVEN,
        &format!("{}/releases/", NEOFORGE_CDN),
    )
}

/// 装 NeoForge。
///
/// 流程比 Forge 多一步「预置库」：
/// NeoForge 安装器只会去 `maven.neoforged.net` 拿库，而那个主机在不少网络下
/// 直接超时，跑起来就是一片 `SocketTimeoutException`，最后
/// `These libraries failed to download`。
/// 所以先把 `install_profile.json` 与 `version.json` 里列出的库按官方 CDN
/// 落到 `libraries/`，安装器随后只需要校验与 patch，就能跑通。
pub fn install_neoforge<F>(
    game_dir: &Path,
    neoforge_version: &str,
    mirror: crate::settings::Mirror,
    asked: Option<&str>,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(LoaderProgress) + Send + Sync),
    mut on_log: F,
) -> Result<InstallOutcome, String>
where
    F: FnMut(&str),
{
    if cancel.is_cancelled() {
        return Ok(InstallOutcome {
            instance: String::new(),
            cancelled: true,
        });
    }

    let cache = game_dir.join("cache");
    std::fs::create_dir_all(&cache).map_err(|e| format!("建缓存目录失败: {}", e))?;
    let installer = cache.join(format!("neoforge-{}-installer.jar", neoforge_version));

    if !installer.is_file() {
        on_log(&format!("下载 NeoForge {} 安装器…", neoforge_version));
        download_urls(
            &neoforge_installer_candidates(neoforge_version, mirror),
            &installer,
        )
        .map_err(|e| format!("下载 NeoForge 安装器失败: {}", e))?;
    }

    // 安装器得能读出来才算下载完整。下到一半会报 `Invalid or corrupt jarfile`，
    // 所以这里先自检一遍，坏了就删掉重下。
    if crate::natives::read_zip_entries(&installer).is_err() {
        on_log("安装器不完整，重新下载…");
        let _ = std::fs::remove_file(&installer);
        download_urls(
            &neoforge_installer_candidates(neoforge_version, mirror),
            &installer,
        )
        .map_err(|e| format!("重新下载 NeoForge 安装器失败: {}", e))?;
        if crate::natives::read_zip_entries(&installer).is_err() {
            return Err("NeoForge 安装器反复下载都不完整，请稍后重试".to_string());
        }
    }

    let source_label = if mirror.resolved(crate::settings::ProbeKind::NeoForge) == crate::settings::Mirror::Bmclapi {
        "BMCLAPI 镜像"
    } else {
        "NeoForge 官方 CDN"
    };
    preseed_installer_libraries(
        game_dir,
        &installer,
        mirror,
        source_label,
        neoforge_library_url,
        cancel,
        on_progress,
        &mut on_log,
    )?;
    ensure_launcher_profiles(game_dir, &mut on_log)?;

    let mc_version = neoforge_game_version(neoforge_version).unwrap_or_default();
    let java = installer_java(game_dir, &mc_version)?;
    on_log(&format!("用 {} 运行 NeoForge 安装器…", java.display()));

    if !run_installer(
        &java,
        &installer,
        game_dir,
        "NeoForge",
        cancel,
        on_progress,
        &mut on_log,
    )? {
        on_log("安装被取消，NeoForge 没有装好");
        return Ok(InstallOutcome {
            instance: String::new(),
            cancelled: true,
        });
    }

    let instance = format!("neoforge-{}", neoforge_version);
    if !crate::install::is_installed(game_dir, &instance) {
        return Err(format!(
            "安装器跑完了，但没找到 versions/{} —— 安装器可能换了命名",
            instance
        ));
    }

    let wanted = clean_instance_name(asked, &instance);
    if wanted != instance {
        on_log(&format!("把实例改名成 {}…", wanted));
        crate::install::rename_instance(game_dir, &instance, &wanted)?;
    }

    on_log(&format!("NeoForge 装好了：versions/{}", wanted));
    Ok(InstallOutcome {
        instance: wanted,
        cancelled: false,
    })
}

/// 跑 `--installClient` 安装器，等它结束，但每 100 ms 看一眼取消信号。
///
/// 返回 `Ok(false)` 表示被取消（进程已经被杀掉），`Ok(true)` 表示正常跑完。
///
/// 输出重定向到 `cache/` 下的两个文件而不是管道：安装器会刷很多行，
/// 管道写满而没人读会互相等死。跑完再把文件内容逐行转成日志，然后删掉。
fn run_installer<F>(
    java: &Path,
    installer: &Path,
    game_dir: &Path,
    label: &str,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(LoaderProgress) + Send + Sync),
    on_log: &mut F,
) -> Result<bool, String>
where
    F: FnMut(&str),
{
    // 安装器在解包、打补丁，这一步没有任何可靠的进度可报。
    on_progress(LoaderProgress::Working(format!(
        "正在运行 {} 安装器（解包与打补丁，可能几分钟）…",
        label
    )));
    use std::process::Stdio;

    let cache = game_dir.join("cache");
    std::fs::create_dir_all(&cache).map_err(|e| format!("建缓存目录失败: {}", e))?;
    let out_path = cache.join("installer-out.log");
    let err_path = cache.join("installer-err.log");
    let out = std::fs::File::create(&out_path)
        .map_err(|e| format!("创建安装器输出文件失败: {}", e))?;
    let err = std::fs::File::create(&err_path)
        .map_err(|e| format!("创建安装器输出文件失败: {}", e))?;

    // 安装器会解包、打补丁，中间产物全扔在 `java.io.tmpdir` 里。
    // 默认是 `%TEMP%`，本进程被限制得紧的时候（沙箱、低完整性标记、
    // 组策略）那个目录根本建不出子目录，安装器只丢一句
    // `java.nio.file.AccessDeniedException: …\Temp\forge_installer…` 就退出。
    // 游戏目录是我们自己写过的，一定能写，把临时目录指过去最稳。
    let temp_dir = cache.join("installer-tmp");
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("建安装器临时目录失败: {}", e))?;

    let mut child = crate::spawn::silent(java)
        .arg(format!("-Djava.io.tmpdir={}", temp_dir.display()))
        .arg("-jar")
        .arg(installer)
        .arg("--installClient")
        .arg(game_dir)
        .env("TEMP", &temp_dir)
        .env("TMP", &temp_dir)
        .env("TMPDIR", &temp_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .spawn()
        .map_err(|e| format!("启动安装器失败: {}", e))?;

    let status = loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => {
                let _ = child.kill();
                return Err(format!("等待安装器失败: {}", e));
            }
        }
    };

    for path in [&out_path, &err_path] {
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines().map(str::trim) {
                if !line.is_empty() {
                    on_log(&format!("  安装器: {}", line));
                }
            }
        }
        let _ = std::fs::remove_file(path);
    }

    match status {
        None => Ok(false),
        Some(status) if status.success() => Ok(true),
        Some(status) => Err(format!(
            "{} 安装器退出码 {:?}",
            label,
            status.code().unwrap_or(-1)
        )),
    }
}

/// 把 NeoForge 安装器会去死链上找的库，提前落到 `libraries/`。
///
/// 安装器里的 URL 全写着 `maven.neoforged.net`，那个域在不少网络下不通。这里
/// 按用户选的源改写：
/// - 镜像：整体换成 BMCLAPI 的 `/maven`（实测能覆盖安装器列出的全部构件，
///   少数会 302 到别的镜像机）；
/// - 官方：换成 NeoForge 官方 CDN（`neoforged.forgecdn.net`）。
#[allow(clippy::too_many_arguments)]
pub fn preseed_installer_libraries<F>(
    game_dir: &Path,
    installer: &Path,
    mirror: crate::settings::Mirror,
    source_label: &str,
    rewrite: fn(&str, crate::settings::Mirror) -> String,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(LoaderProgress) + Send + Sync),
    on_log: &mut F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    let entries = crate::natives::read_zip_entries(installer)
        .map_err(|e| format!("读安装器失败: {}", e))?;

    let mut wanted: Vec<(String, String)> = Vec::new(); // (相对路径, 下载地址)
    for name in ["install_profile.json", "version.json"] {
        let Some(entry) = entries.iter().find(|e| e.name == name) else {
            continue;
        };
        let data = entry
            .read_data(installer)
            .map_err(|e| format!("读安装器里的 {} 失败: {}", name, e))?;
        let text = String::from_utf8_lossy(&data).to_string();
        collect_artifacts(&text, &mut wanted);
    }

    if wanted.is_empty() {
        return Ok(());
    }

    on_log(&format!(
        "预置 {} 个库（走{}）…",
        wanted.len(),
        source_label
    ));
    let total = wanted.len();
    let mut done = 0usize;
    let mut bytes = 0u64;
    let mut failed = Vec::new();
    on_progress(LoaderProgress::Tick { done, total, bytes });
    for (rel, url) in &wanted {
        if cancel.is_cancelled() {
            break;
        }
        let dest = game_dir
            .join("libraries")
            .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        if dest.is_file() {
            // 已经在了也算「处理过」，否则进度条永远停在 0 会上人以为卡死
            done += 1;
            on_progress(LoaderProgress::Tick { done, total, bytes });
            continue;
        }
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                failed.push(format!("{}: {}", rel, e));
                continue;
            }
        }
        // 镜像偶尔会把连接直接重置（curl 35），一条不通不代表这条构件没有，
        // 所以主源失败后再用官方源补一次。
        let primary = rewrite(url, mirror);
        let secondary = rewrite(url, crate::settings::Mirror::Official);
        let mut outcome = crate::transport::download_to(&primary, &dest, None);
        if outcome.is_err() && secondary != primary {
            let _ = std::fs::remove_file(&dest);
            outcome = crate::transport::download_to(&secondary, &dest, None);
        }
        match outcome {
            Ok(done_bytes) => {
                done += 1;
                bytes += done_bytes.bytes;
                on_progress(LoaderProgress::Tick { done, total, bytes });
            }
            Err(e) => failed.push(format!("{}: {}", rel, e)),
        }
    }

    if !failed.is_empty() {
        on_log(&format!("有 {} 个库没预置成功，先让安装器自己试", failed.len()));
        for item in failed.iter().take(5) {
            on_log(&format!("  {}", item));
        }
    }
    on_log(&format!("预置完成：新下 {} 个", done));
    Ok(())
}

/// 从安装器里的 JSON 文本中收集所有 `downloads.artifact`。
fn collect_artifacts(text: &str, out: &mut Vec<(String, String)>) {
    let Ok(root) = json::parse(text) else {
        return;
    };
    // install_profile.json 的库在顶层；version.json 也是。两个都扫一遍不怕重复，
    // 后面的下载会跳过已存在的文件。
    let groups = [Some(&root), root.get("version")];
    for group in groups.into_iter().flatten() {
        let Some(libs) = group.get("libraries").and_then(Json::as_array) else {
            continue;
        };
        for lib in libs {
            let Some(artifact) = lib.get("downloads").and_then(|d| d.get("artifact")) else {
                continue;
            };
            let Some(url) = artifact.get("url").and_then(Json::as_str) else {
                continue;
            };
            if url.is_empty() {
                continue;
            }
            let rel = artifact
                .get("path")
                .and_then(Json::as_str)
                .map(str::to_string)
                .or_else(|| coord_to_path(lib.get("name").and_then(Json::as_str)));
            let Some(rel) = rel else { continue };
            out.push((rel, url.to_string()));
        }
    }
}

/// `group:artifact:version[:classifier][@ext]` → `group/artifact/version/artifact-version[-classifier].ext`
fn coord_to_path(name: Option<&str>) -> Option<String> {
    let name = name?;
    let (head, ext) = match name.split_once('@') {
        Some((h, e)) => (h, e.to_string()),
        None => (name, "jar".to_string()),
    };
    let parts: Vec<&str> = head.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let classifier = parts.get(3).map(|c| format!("-{}", c)).unwrap_or_default();
    Some(format!(
        "{}/{}/{}/{}-{}{}.{}",
        group, artifact, version, artifact, version, classifier, ext
    ))
}

// ---------------------------------------------------------------- Fabric API

/// 查 Fabric API 对某个游戏版本可用的版本。
///
/// 返回 `(版本号, 下载地址, 文件名)`。
pub fn fabric_api_download(mc_version: &str) -> Result<(String, String, String), String> {
    let url = format!(
        "{}/project/fabric-api/version?game_versions=%5B%22{}%22%5D&loaders=%5B%22fabric%22%5D",
        MODRINTH_API, mc_version
    );
    let text = crate::transport::fetch_text(&url).map_err(|e| format!("查 Fabric API 失败: {}", e))?;
    parse_modrinth_files(&text)
}

/// 从 Modrinth 的版本列表里挑第一个主文件。
pub fn parse_modrinth_files(text: &str) -> Result<(String, String, String), String> {
    let root = json::parse(text).map_err(|e| format!("解析 Modrinth 结果失败: {}", e))?;
    let Some(items) = root.as_array() else {
        return Err("Modrinth 结果不是数组".to_string());
    };
    for item in items {
        let number = item
            .get("version_number")
            .and_then(Json::as_str)
            .unwrap_or("未知版本")
            .to_string();
        let Some(files) = item.get("files").and_then(Json::as_array) else {
            continue;
        };
        // 优先 primary，没有就第一个
        let pick = files
            .iter()
            .find(|f| f.get("primary").and_then(Json::as_bool).unwrap_or(false))
            .or_else(|| files.first());
        let Some(file) = pick else { continue };
        let Some(url) = file.get("url").and_then(Json::as_str) else {
            continue;
        };
        let filename = file
            .get("filename")
            .and_then(Json::as_str)
            .unwrap_or("fabric-api.jar");
        return Ok((number, url.to_string(), filename.to_string()));
    }
    Err("Modrinth 上没有找到可下载的文件".to_string())
}

/// 把 Fabric API 装进某个实例的 `mods/`。
///
/// 隔离实例放 `versions/<实例>/mods/`，共享实例放 `.minecraft/mods/` ——
/// 和游戏自己找模组的路径保持一致，不然放进去也不会被加载。
pub fn install_fabric_api<F>(
    game_dir: &Path,
    instance: &str,
    mc_version: &str,
    mut on_log: F,
) -> Result<PathBuf, String>
where
    F: FnMut(&str),
{
    on_log(&format!("查 Fabric API（游戏 {}）…", mc_version));
    let (number, url, filename) = fabric_api_download(mc_version)?;

    let mods_dir = match crate::instance::detect_mode(game_dir, instance) {
        crate::instance::InstanceMode::Isolated => {
            crate::instance::version_dir(game_dir, instance).join("mods")
        }
        crate::instance::InstanceMode::Shared => game_dir.join("mods"),
    };
    std::fs::create_dir_all(&mods_dir)
        .map_err(|e| format!("建 mods 目录失败（{}）: {}", mods_dir.display(), e))?;

    let dest = mods_dir.join(&filename);
    on_log(&format!("下载 Fabric API {}…", number));
    crate::transport::download_to(&url, &dest, None)
        .map_err(|e| format!("下载 Fabric API 失败: {}", e))?;

    on_log(&format!("Fabric API 放好了：{}", dest.display()));
    Ok(dest)
}

// ---------------------------------------------------------------- 公共小工具

/// 安装器要求游戏目录里先有 `launcher_profiles.json`，缺了就建一个最小的。
///
/// 已有的文件不覆盖 —— 用户可能真在里面有 profile 记录。
fn ensure_launcher_profiles<F>(game_dir: &Path, on_log: &mut F) -> Result<(), String>
where
    F: FnMut(&str),
{
    let path = game_dir.join("launcher_profiles.json");
    if path.is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(game_dir).map_err(|e| format!("建游戏目录失败: {}", e))?;
    std::fs::write(&path, LAUNCHER_PROFILES_MINIMAL)
        .map_err(|e| format!("写 launcher_profiles.json 失败: {}", e))?;
    on_log("补了一份 launcher_profiles.json（安装器要求）");
    Ok(())
}

/// 安装器只要能认出这是启动器目录就行，内容不需要多。
pub const LAUNCHER_PROFILES_MINIMAL: &str = r#"{"profiles":{},"settings":{},"version":3}"#;

/// 挑一个能跑安装器的 java。
///
/// 优先按目标游戏版本声明的 Java 版本挑（安装器要 patch 的是那个版本的字节码，
/// 用错版本会在处理 class 文件时报错），拿不到声明就当 Java 8。
pub fn installer_java(game_dir: &Path, mc_version: &str) -> Result<PathBuf, String> {
    let required = base_java_major(game_dir, mc_version).unwrap_or(8);
    let runtimes = crate::java::detect_all();
    if runtimes.is_empty() {
        return Err("找不到 Java，安装器需要它来打补丁".to_string());
    }
    let (runtime, _) = crate::java::pick_java_choice(&runtimes, required)
        .ok_or_else(|| format!("本机 Java 都低于 {}，安装器跑不起来", required))?;
    Ok(console_java(&runtime.path))
}

/// 读原版版本 JSON 里声明的 Java 主版本。
fn base_java_major(game_dir: &Path, mc_version: &str) -> Option<u32> {
    if mc_version.is_empty() {
        return None;
    }
    let path = game_dir
        .join("versions")
        .join(mc_version)
        .join(format!("{}.json", mc_version));
    let text = std::fs::read_to_string(path).ok()?;
    let raw = parse_version(&text).ok()?;
    raw.java_version.major_version
}

/// `javaw.exe` 收不到输出，跑安装器要换成 `java.exe`。
fn console_java(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if !name.eq_ignore_ascii_case("javaw.exe") {
        return path.to_path_buf();
    }
    let sibling = path.with_file_name("java.exe");
    if sibling.is_file() {
        sibling
    } else {
        path.to_path_buf()
    }
}

/// 版本清单里记加载器的键。
///
/// 为什么不靠 id 猜：实例可以叫任何名字（「26.3-生存」），id 也被改成了实例名，
/// 靠 `fabric-loader-…` 这种前缀只能认出安装器原样写下的目录。装进去之后
/// 是谁的加载器，只有我们自己记下来才准。
pub const LOADER_KEY: &str = "_aicraftLoader";

/// 判断一个已装实例挂了什么加载器，返回（种类, 加载器版本）。
///
/// 先读我们自己写的 `_aicraftLoader`；老实例没有这个键，就退回按 id 猜
/// （`_aicraftOfficialId` 记着上游 id，实例名也可能就是上游 id）。
pub fn detect_installed(game_dir: &Path, instance: &str) -> Option<(LoaderKind, String)> {
    let json_path =
        crate::instance::version_dir(game_dir, instance).join(format!("{}.json", instance));
    let text = std::fs::read_to_string(&json_path).ok()?;
    detect_in_text(&text, instance)
}

/// 同上，但直接吃已经读进内存的清单文本。
///
/// 版本列表页在扫描时本来就把每一份 json 读进来了，再让
/// `detect_installed` 去读一遍等于每帧多几百次磁盘寻道 ——
/// 界面每帧都要画那一枚标签，代价会被放大到看得见。
pub fn detect_in_text(text: &str, instance: &str) -> Option<(LoaderKind, String)> {
    let value = crate::json::parse(text).ok()?;
    if let Some(marker) = value.get(LOADER_KEY) {
        let kind = marker
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(LoaderKind::from_id);
        let version = marker.get("version").and_then(|v| v.as_str());
        if let (Some(kind), Some(version)) = (kind, version) {
            return Some((kind, version.to_string()));
        }
    }
    let id = value
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or(instance)
        .to_string();
    let official = value
        .get(crate::install::OFFICIAL_ID_KEY)
        .and_then(|v| v.as_str())
        .unwrap_or(instance)
        .to_string();
    // 别的启动器（PCL / HMCL / 官方启动器）装的实例没有我们写的标记，
    // 目录名也是 `1.20.1-Fabric`、`Modpacks`、`ATM10 To the Sky` 这种，
    // 按 id 前缀完全猜不出来。但清单里的**库坐标**和 `--fml.*` 参数是
    // 加载器自己在安装时写进去的，改不掉，比名字可信得多。
    if let Some(found) = detect_from_manifest(&value) {
        return Some(found);
    }
    detect_from_ids(&id, &official)
}

/// 从清单本身认出加载器：库坐标 + 启动参数。
///
/// 判定顺序是有讲究的：
///
/// 1. `--fml.neoForgeVersion` / `--fml.forgeVersion` —— 加载器把版本号
///    直接写在参数里，最可靠，且能分清 NeoForge 和 Forge
///    （两家都用 `cpw.mods:bootstraplauncher`，光看 mainClass 分不出来）。
/// 2. `net.neoforged:*` 的库 —— NeoForge 1.20.1 时代借用了 `forge` 这个名字
///    （`net.neoforged:forge:1.20.1-47.1.106`），只看 `net.minecraftforge:forge`
///    会把它错认成 Forge。
/// 3. `net.minecraftforge:forge` / `net.minecraftforge:fmlloader` —— 正统 Forge。
/// 4. `net.fabricmc:fabric-loader` —— Fabric。
fn detect_from_manifest(value: &Json) -> Option<(LoaderKind, String)> {
    let libs = library_names(value);
    let args = game_args(value);

    if let Some(version) = arg_value(&args, "--fml.neoForgeVersion") {
        return Some((LoaderKind::NeoForge, version));
    }
    for name in &libs {
        for prefix in ["net.neoforged:neoforge:", "net.neoforged:forge:"] {
            if let Some(rest) = name.strip_prefix(prefix) {
                let version = strip_game_prefix(rest);
                if !version.is_empty() {
                    return Some((LoaderKind::NeoForge, version));
                }
            }
        }
    }

    if let Some(version) = arg_value(&args, "--fml.forgeVersion") {
        return Some((LoaderKind::Forge, version));
    }
    for name in &libs {
        for prefix in ["net.minecraftforge:forge:", "net.minecraftforge:fmlloader:"] {
            if let Some(rest) = name.strip_prefix(prefix) {
                let version = strip_game_prefix(rest);
                if !version.is_empty() {
                    return Some((LoaderKind::Forge, version));
                }
            }
        }
    }

    for name in &libs {
        if let Some(version) = name.strip_prefix("net.fabricmc:fabric-loader:") {
            let version = version.trim();
            if !version.is_empty() {
                return Some((LoaderKind::Fabric, version.to_string()));
            }
        }
    }

    None
}

/// 从清单里推断这份实例对应的**原版**游戏版本。
///
/// 为什么需要这个：别的启动器写的清单常常没有 `inheritsFrom`
/// （PCL 直接把自己的实例当独立版本写），于是「它挂在哪个原版上」
/// 只能从内容里找。找的顺序：
///
/// 1. `--fml.mcVersion` —— Forge / NeoForge 会明确写出来，最准。
/// 2. `net.fabricmc:intermediary:<游戏版本>` —— Fabric 的中间映射名
///    就是游戏版本。注意有些实例写的是 `0.0.0`，那就不能用。
/// 3. 从 id 里挑一个像版本号的片段（`26.3-Fabric` → `26.3`）。
pub fn detect_game_version(json_text: &str) -> Option<String> {
    let value = json::parse(json_text).ok()?;
    game_version_from_manifest(&value)
}

fn game_version_from_manifest(value: &Json) -> Option<String> {
    let args = game_args(value);
    if let Some(version) = arg_value(&args, "--fml.mcVersion") {
        return Some(version);
    }

    for name in library_names(value) {
        if let Some(version) = name.strip_prefix("net.fabricmc:intermediary:") {
            let version = version.trim();
            if !version.is_empty() && version != "0.0.0" {
                return Some(version.to_string());
            }
        }
    }

    let id = value.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    version_like(id)
}

/// 从一段名字里挑出像游戏版本号的片段。
///
/// 认两种：`1.20.1` / `26.3` 这种纯数字点分，以及 `1.20.1-pre2`
/// 这种带预发布后缀的。挑第一个命中的。
fn version_like(text: &str) -> Option<String> {
    for token in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-')) {
        let token = token.trim_matches('-');
        if token.is_empty() {
            continue;
        }
        let head = token.split('-').next().unwrap_or(token);
        let parts: Vec<&str> = head.split('.').collect();
        if parts.len() < 2 {
            continue;
        }
        if !parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        {
            continue;
        }
        // `0.16.10` 这种加载器版本也长得一样，所以要求第一段不像加载器
        // 常见的小版本号：加载器基本都在 0.x，游戏版本从来不是 0 开头。
        if parts[0].starts_with('0') && parts[0] != "0" {
            continue;
        }
        // `1.20.4-SkyBlock` 里只有 `1.20.4` 是版本号，后半截是整合包的名字；
        // 但 `1.20.1-pre2` 的后缀是版本的一部分。只放行已知的预发布后缀。
        if let Some((_, suffix)) = token.split_once('-') {
            let suffix = suffix.to_ascii_lowercase();
            let keep = ["pre", "rc", "rel", "release", "snapshot"]
                .iter()
                .any(|tag| suffix.starts_with(tag));
            if keep {
                return Some(token.to_string());
            }
        }
        return Some(head.to_string());
    }
    None
}

/// `1.20.1-47.4.8` → `47.4.8`。加载器的坐标里常把游戏版本串在前面。
fn strip_game_prefix(text: &str) -> String {
    match text.split_once('-') {
        Some((head, rest)) if head.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
            rest.trim().to_string()
        }
        _ => text.trim().to_string(),
    }
}

/// 清单里所有库的坐标字符串（含 `patches[*]` 里的那些）。
///
/// 为什么要管 `patches`：NeoForge 的清单把真正的库摊在顶层，
/// 但有些整合包会给上游版本打自己的 patch，库写在那里面。
fn library_names(value: &Json) -> Vec<String> {
    let mut names = Vec::new();
    collect_library_names(value, &mut names);
    names
}

fn collect_library_names(value: &Json, out: &mut Vec<String>) {
    if let Some(libs) = value.get("libraries").and_then(|v| v.as_array()) {
        for lib in libs {
            if let Some(name) = lib.get("name").and_then(|v| v.as_str()) {
                out.push(name.to_string());
            }
        }
    }
    if let Some(patches) = value.get("patches").and_then(|v| v.as_array()) {
        for patch in patches {
            collect_library_names(patch, out);
        }
    }
}

/// 游戏参数摊平成字符串列表（旧格式 `minecraftArguments` 也收进来）。
///
/// 这里**不看 rules**：我们要问的是「这份清单声明了哪个加载器」，
/// 而不是「在这台机器上会不会真的传这个参数」。条件分支里的
/// `--fml.neoForgeVersion` 一样算数。
fn game_args(value: &Json) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(list) = value
        .get("arguments")
        .and_then(|v| v.get("game"))
        .and_then(|v| v.as_array())
    {
        for entry in list {
            push_arg(entry, &mut args);
        }
    }
    if let Some(text) = value.get("minecraftArguments").and_then(|v| v.as_str()) {
        args.extend(text.split_whitespace().map(|s| s.to_string()));
    }
    args
}

fn push_arg(entry: &Json, out: &mut Vec<String>) {
    match entry {
        Json::Str(text) => out.push(text.clone()),
        Json::Array(items) => {
            for item in items {
                push_arg(item, out);
            }
        }
        Json::Object(_) => {
            if let Some(values) = entry.get("value").and_then(|v| v.as_array()) {
                for item in values {
                    push_arg(item, out);
                }
            }
        }
        _ => {}
    }
}

/// 找 `key` 后面跟的那个值，也认 `key=value` 的写法。
fn arg_value(args: &[String], key: &str) -> Option<String> {
    let inline = format!("{}=", key);
    for (index, arg) in args.iter().enumerate() {
        if arg == key {
            if let Some(next) = args.get(index + 1) {
                let next = next.trim();
                if !next.is_empty() {
                    return Some(next.to_string());
                }
            }
        }
        if let Some(rest) = arg.strip_prefix(inline.as_str()) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// 老办法：从 id 里认出加载器。留着给没有 `_aicraftLoader` 的旧实例兜底。
fn detect_from_ids(instance_id: &str, official_id: &str) -> Option<(LoaderKind, String)> {
    for name in [official_id, instance_id] {
        if name.is_empty() {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("fabric-loader-") {
            // fabric-loader-<loader>-<game>：加载器版本在前，游戏版本在后
            let loader = name[name.len() - rest.len()..]
                .split('-')
                .next()
                .unwrap_or_default()
                .to_string();
            if !loader.is_empty() {
                return Some((LoaderKind::Fabric, loader));
            }
        }
        if let Some(rest) = lower.strip_prefix("neoforge-") {
            let version = &name[name.len() - rest.len()..];
            if !version.is_empty() {
                return Some((LoaderKind::NeoForge, version.to_string()));
            }
        }
        if let Some(idx) = lower.find("-forge-") {
            let version = &name[idx + "-forge-".len()..];
            if !version.is_empty() {
                return Some((LoaderKind::Forge, version.to_string()));
            }
        }
    }
    None
}

/// 把已装实例的 Fabric 加载器换一个版本，就地改。
///
/// 只动 `versions/<实例>/<实例>.json` 与 `libraries/` 里的 Fabric 库：
/// 旧的 loader jar 留在 libraries 里不删（别的实例可能还在用，而且删了也不省心），
/// 存档、模组、配置都不碰。
///
/// 为什么只支持 Fabric：Forge / NeoForge 换版本必须重跑它们自己的安装器，
/// 安装器只会往 `versions/<游戏版本>-forge-<版本>/` 写，改不了现成的目录。
#[allow(clippy::too_many_arguments)]
pub fn switch_loader<F>(
    game_dir: &Path,
    instance: &str,
    mc_version: &str,
    kind: LoaderKind,
    loader_version: &str,
    mirror: &crate::settings::Mirror,
    concurrency: usize,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(LoaderProgress) + Send + Sync),
    mut on_log: F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    match kind {
        LoaderKind::Fabric => switch_fabric(
            game_dir,
            instance,
            mc_version,
            loader_version,
            mirror,
            concurrency,
            cancel,
            on_progress,
            on_log,
        ),
        LoaderKind::Forge | LoaderKind::NeoForge => switch_installer_loader(
            game_dir,
            instance,
            mc_version,
            kind,
            loader_version,
            mirror,
            cancel,
            on_progress,
            &mut on_log,
        ),
    }
}

/// Forge / NeoForge 的就地安装：让安装器照常装到它自己的新目录，再把那份
/// 清单改名搬进实例目录，最后删掉空出来的临时目录。
///
/// 为什么必须这样绕：两个安装器都不接受「装进已有目录」，它们的产物目录名
/// 由游戏版本 + 加载器版本拼出来，改不了。但它写出来的其实只有一份清单
/// （jar 来自父版本，库在 `libraries/`），所以搬一个文件就够，实例里的
/// 存档、模组、配置一个字节都不用动。
#[allow(clippy::too_many_arguments)]
fn switch_installer_loader<F>(
    game_dir: &Path,
    instance: &str,
    mc_version: &str,
    kind: LoaderKind,
    loader_version: &str,
    mirror: &crate::settings::Mirror,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(LoaderProgress) + Send + Sync),
    on_log: &mut F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    on_log(&format!(
        "给 {} 装 {} {}（安装器会先写到自己的目录，再搬过来）…",
        instance,
        kind.label(),
        loader_version
    ));

    let outcome = match kind {
        LoaderKind::Forge => install_forge(
            game_dir,
            mc_version,
            loader_version,
            *mirror,
            None,
            cancel,
            on_progress,
            &mut *on_log,
        )?,
        LoaderKind::NeoForge => install_neoforge(
            game_dir,
            loader_version,
            *mirror,
            None,
            cancel,
            on_progress,
            &mut *on_log,
        )?,
        LoaderKind::Fabric => unreachable!("Fabric 走 profile 那条路"),
    };

    if outcome.cancelled {
        return Err("已取消".to_string());
    }

    let official = crate::install::instance_official_id(game_dir, instance)
        .unwrap_or_else(|| mc_version.to_string());
    adopt_loader_dir(
        game_dir,
        &outcome.instance,
        instance,
        &official,
        kind,
        loader_version,
    )?;
    on_log(&format!(
        "装好了：{} 现在用 {} {}（存档与模组没动）",
        instance,
        kind.label(),
        loader_version
    ));
    Ok(())
}

/// 把安装器生成的那份清单改写成实例自己的清单。
fn adopt_loader_dir(
    game_dir: &Path,
    created: &str,
    instance: &str,
    official: &str,
    kind: LoaderKind,
    loader_version: &str,
) -> Result<(), String> {
    if created == instance {
        return Ok(());
    }
    let created_dir = crate::instance::version_dir(game_dir, created);
    let created_json = created_dir.join(format!("{}.json", created));
    let text = std::fs::read_to_string(&created_json)
        .map_err(|e| format!("读安装器写出的清单失败: {}", e))?;
    let marked = retag_for_instance(&text, instance, official, kind, loader_version)?;

    let instance_dir = crate::instance::version_dir(game_dir, instance);
    std::fs::create_dir_all(&instance_dir)
        .map_err(|e| format!("建实例目录失败: {}", e))?;
    std::fs::write(instance_dir.join(format!("{}.json", instance)), marked)
        .map_err(|e| format!("写实例清单失败: {}", e))?;

    // 只删「空壳」：里面除了 json 还有别的东西就留着，不赌。
    let only_json = std::fs::read_dir(&created_dir)
        .map(|entries| {
            entries.flatten().all(|entry| {
                entry
                    .path()
                    .extension()
                    .map(|ext| ext.eq_ignore_ascii_case("json"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if only_json {
        let _ = std::fs::remove_dir_all(&created_dir);
    }
    // 装完顺手压平：Forge / NeoForge 的安装器写出来的清单也是
    // `inheritsFrom` 指向上游的薄清单，留着它就意味着「父版本不能动」。
    // 这一步只是为了把实例做成自包含的，失败不影响启动。
    if let Err(e) = crate::flatten::rewrite_instance(game_dir, instance) {
        crate::logging::debug(format!("{} 的清单保持原样：{}", instance, e));
    }
    Ok(())
}

/// 把清单的 id 换成实例名、记下上游 id，并打上加载器标记。
fn retag_for_instance(
    source: &str,
    instance: &str,
    official: &str,
    kind: LoaderKind,
    loader_version: &str,
) -> Result<String, String> {
    let retagged = crate::install::retag_version_json(source, instance, official)?;
    mark_loader(&retagged, kind, loader_version)
}

/// 往清单里写 `_aicraftLoader`，以后再认加载器就不用猜 id 了。
fn mark_loader(json_text: &str, kind: LoaderKind, loader_version: &str) -> Result<String, String> {
    let mut value = crate::json::parse(json_text).map_err(|e| format!("解析清单失败: {}", e))?;
    let crate::json::Json::Object(entries) = &mut value else {
        return Err("清单不是 JSON 对象".to_string());
    };
    entries.retain(|(key, _)| key != LOADER_KEY);
    entries.push((
        LOADER_KEY.to_string(),
        crate::json::Json::Object(vec![
            (
                "kind".to_string(),
                crate::json::Json::Str(kind.id().to_string()),
            ),
            (
                "version".to_string(),
                crate::json::Json::Str(loader_version.to_string()),
            ),
        ]),
    ));
    Ok(crate::json::write(&value))
}

#[allow(clippy::too_many_arguments)]
fn switch_fabric<F>(
    game_dir: &Path,
    instance: &str,
    mc_version: &str,
    loader_version: &str,
    mirror: &crate::settings::Mirror,
    concurrency: usize,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(LoaderProgress) + Send + Sync),
    mut on_log: F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    on_log(&format!(
        "把 {} 的 Fabric 换成 {}…",
        instance, loader_version
    ));
    let profile_json = fabric_profile(mc_version, loader_version, *mirror)?;

    let parsed =
        parse_version(&profile_json).map_err(|e| format!("解析 Fabric 清单失败: {}", e))?;
    let default_name = if parsed.id.is_empty() {
        format!("fabric-loader-{}-{}", loader_version, mc_version)
    } else {
        parsed.id.clone()
    };
    // 实例名不变，所以要把清单里的 id 换回实例名，并把上游 id 与加载器记下来。
    let profile_json = retag_for_instance(
        &profile_json,
        instance,
        &default_name,
        LoaderKind::Fabric,
        loader_version,
    )?;

    let ctx = crate::rules::RuleContext::current();
    let plan = crate::install::plan_install_as(game_dir, &profile_json, None, &ctx, Some(instance))?;

    on_log(&format!(
        "需要 {} 个文件，约 {:.1} MB",
        plan.items.len(),
        plan.total_bytes as f64 / 1_048_576.0
    ));

    let budget = crate::speed::Budget::unlimited();
    let report = crate::install::execute_install_live(
        &plan,
        concurrency,
        mirror,
        &budget,
        cancel,
        &|ev| {
            if let crate::install::InstallProgress::Tick {
                done,
                total,
                bytes,
            } = ev
            {
                on_progress(LoaderProgress::Tick {
                    done,
                    total,
                    bytes,
                });
            }
        },
    )?;

    if report.cancelled {
        return Err("已取消".to_string());
    }
    if report.failed > 0 {
        let first = report
            .failures
            .first()
            .map(|(p, m)| format!("{}（{}）", p.display(), m))
            .unwrap_or_default();
        return Err(format!(
            "{} 个库下载失败，第一个是 {}",
            report.failed, first
        ));
    }
    // 压平这一步由 `execute_install_live` 在下载全部成功后统一做，
    // 这里不用重复调用。
    on_log(&format!("换好了：{} 现在用 Fabric {}", instance, loader_version));
    Ok(())
}


///
/// `mirror` 只影响 Fabric（它的列表有官方与 BMCLAPI 两套端点）；
/// Forge 与 NeoForge 的列表来源是固定的，见各自的 `list_*`。
pub fn list_versions(
    kind: LoaderKind,
    mc_version: &str,
    mirror: crate::settings::Mirror,
) -> Result<Vec<LoaderVersion>, String> {
    match kind {
        LoaderKind::Fabric => list_fabric_loaders(mc_version, mirror),
        LoaderKind::Forge => list_forge_versions(mc_version, mirror),
        LoaderKind::NeoForge => list_neoforge_versions(mc_version, mirror),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neoforge_versions_encode_the_game_version() {
        assert_eq!(neoforge_game_version("21.1.72").as_deref(), Some("1.21.1"));
        assert_eq!(neoforge_game_version("20.4.245").as_deref(), Some("1.20.4"));
        assert_eq!(neoforge_game_version("20.2.3-beta").as_deref(), Some("1.20.2"));
        // 26.x 之后游戏版本不再带前导 1.
        assert_eq!(neoforge_game_version("26.1.169").as_deref(), Some("26.1"));
        assert_eq!(neoforge_game_version("junk"), None);
        assert_eq!(neoforge_game_version(""), None);
    }

    #[test]
    fn installer_urls_are_what_the_servers_expect() {
        assert_eq!(
            forge_installer_url("1.20.1", "47.0.1"),
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.20.1-47.0.1/forge-1.20.1-47.0.1-installer.jar"
        );
        assert_eq!(
            neoforge_installer_url("21.1.72"),
            "https://neoforged.forgecdn.net/releases/net/neoforged/neoforge/21.1.72/neoforge-21.1.72-installer.jar"
        );
    }

    #[test]
    fn forge_library_urls_follow_the_chosen_source() {
        let official = "https://maven.minecraftforge.net/net/minecraftforge/fmlloader/1.20.1-47.4.8/fmlloader-1.20.1-47.4.8.jar";
        // 镜像：主机换成 BMCLAPI 的 /maven，路径原样留着（注意别多一条斜杠）
        assert_eq!(
            forge_library_url(official, crate::settings::Mirror::Bmclapi),
            "https://bmclapi2.bangbang93.com/maven/net/minecraftforge/fmlloader/1.20.1-47.4.8/fmlloader-1.20.1-47.4.8.jar"
        );
        // 官方：原样返回
        assert_eq!(
            forge_library_url(official, crate::settings::Mirror::Official),
            official
        );
        // 别的主机交给通用镜像规则（libraries.minecraft.net 走 BMCLAPI 的 /maven）
        let mojang = "https://libraries.minecraft.net/com/mojang/brigadier/1.0.18/brigadier-1.0.18.jar";
        assert_eq!(
            forge_library_url(mojang, crate::settings::Mirror::Bmclapi),
            "https://bmclapi2.bangbang93.com/maven/com/mojang/brigadier/1.0.18/brigadier-1.0.18.jar"
        );
    }

    #[test]
    fn neoforge_urls_are_rewritten_to_the_working_cdn() {
        let dead = "https://maven.neoforged.net/releases/net/neoforged/foo/1.0/foo-1.0.jar";
        assert_eq!(
            rewrite_neoforge(dead),
            "https://neoforged.forgecdn.net/releases/net/neoforged/foo/1.0/foo-1.0.jar"
        );
        // 不是那个主机的地址原样留着
        let other = "https://libraries.minecraft.net/a/b.jar";
        assert_eq!(rewrite_neoforge(other), other);
    }

    #[test]
    fn fabric_list_is_parsed() {
        let text = r#"[
            {"loader":{"version":"0.16.10","stable":true}},
            {"loader":{"version":"0.17.0","stable":false}}
        ]"#;
        let list = parse_fabric_loaders(text, "1.20.1").unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].version, "0.16.10");
        assert!(list[0].stable);
        assert!(!list[1].stable);
        assert_eq!(list[0].game_version, "1.20.1");
    }

    #[test]
    fn empty_fabric_list_is_an_error() {
        assert!(parse_fabric_loaders("[]", "1.20.1").is_err());
    }

    #[test]
    fn forge_list_is_sorted_newest_first() {
        let text = r#"[
            {"version":"47.0.1","mcversion":"1.20.1","build":47000001},
            {"version":"47.2.0","mcversion":"1.20.1","build":47000020},
            {"version":"47.1.0","mcversion":"1.20.1","build":47000010}
        ]"#;
        let list = parse_forge_versions(text, "1.20.1").unwrap();
        let versions: Vec<&str> = list.iter().map(|v| v.version.as_str()).collect();
        assert_eq!(versions, ["47.2.0", "47.1.0", "47.0.1"]);
    }

    #[test]
    fn neoforge_list_filters_by_game_version() {
        let text = r#"{"isSnapshot":false,"versions":["20.4.1","21.1.72","21.1.73","21.2.1"]}"#;
        let list = parse_neoforge_versions(text, "1.21.1").unwrap();
        let versions: Vec<&str> = list.iter().map(|v| v.version.as_str()).collect();
        // 只要 21.1.x，并且新的在前
        assert_eq!(versions, ["21.1.73", "21.1.72"]);
    }

    #[test]
    fn neoforge_list_without_matches_is_an_error() {
        let text = r#"{"versions":["20.4.1"]}"#;
        assert!(parse_neoforge_versions(text, "1.21.1").is_err());
    }

    #[test]
    fn modrinth_picks_the_primary_file() {
        let text = r#"[{
            "version_number": "0.92.12+1.20.1",
            "files": [
                {"url":"https://example.com/a.jar","filename":"a.jar","primary":false},
                {"url":"https://example.com/b.jar","filename":"b.jar","primary":true}
            ]
        }]"#;
        let (number, url, filename) = parse_modrinth_files(text).unwrap();
        assert_eq!(number, "0.92.12+1.20.1");
        assert_eq!(url, "https://example.com/b.jar");
        assert_eq!(filename, "b.jar");
    }

    #[test]
    fn modrinth_empty_result_is_an_error() {
        assert!(parse_modrinth_files("[]").is_err());
    }

    #[test]
    fn coordinates_become_library_paths() {
        assert_eq!(
            coord_to_path(Some("org.apache.commons:commons-collections4:4.2")).as_deref(),
            Some("org/apache/commons/commons-collections4/4.2/commons-collections4-4.2.jar")
        );
        assert_eq!(
            coord_to_path(Some("net.fabricmc:sponge-mixin:0.15.0+mixin.0.8.7")).as_deref(),
            Some("net/fabricmc/sponge-mixin/0.15.0+mixin.0.8.7/sponge-mixin-0.15.0+mixin.0.8.7.jar")
        );
        assert_eq!(coord_to_path(Some("bad")).as_deref(), None);
    }

    #[test]
    fn artifacts_are_collected_from_both_shapes() {
        let text = r#"{
            "libraries": [
                {"name":"g:a:1","downloads":{"artifact":{"url":"https://m/x.jar","path":"g/a/1/a-1.jar"}}},
                {"name":"g:b:2","downloads":{"artifact":{"url":"https://m/y.jar"}}}
            ]
        }"#;
        let mut out = Vec::new();
        collect_artifacts(text, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "g/a/1/a-1.jar");
        // 没有 path 的按坐标推
        assert_eq!(out[1].0, "g/b/2/b-2.jar");
    }

    /// 镜像源必须真的把三种加载器都换成 bmclapi 的地址。
    #[test]
    fn bmclapi_bases_point_at_the_mirror() {
        let mirror = crate::settings::Mirror::Bmclapi;
        // Fabric 先试镜像
        assert_eq!(fabric_bases(mirror)[0], FABRIC_META_MIRROR);
        // Forge 安装器走 /maven
        assert_eq!(forge_bases(mirror)[0], BMCLAPI_MAVEN);
        // 官方源先试官方
        assert_eq!(
            fabric_bases(crate::settings::Mirror::Official)[0],
            FABRIC_META
        );
        assert_eq!(
            forge_bases(crate::settings::Mirror::Official)[0],
            FORGE_MAVEN
        );
    }

    #[test]
    fn neoforge_installer_prefers_the_mirror_endpoint() {
        let list = neoforge_installer_candidates("21.1.72", crate::settings::Mirror::Bmclapi);
        assert_eq!(
            list[0],
            "https://bmclapi2.bangbang93.com/neoforge/21.1.72/download/installer"
        );
        assert!(list
            .iter()
            .any(|u| u.contains("/maven/net/neoforged/neoforge/21.1.72/")));

        let official = neoforge_installer_candidates("21.1.72", crate::settings::Mirror::Official);
        assert_eq!(official[0], neoforge_installer_url("21.1.72"));
    }

    #[test]
    fn neoforge_libraries_follow_the_chosen_source() {
        let dead = "https://maven.neoforged.net/releases/net/neoforged/bus/1.0.17/bus-1.0.17.jar";

        assert_eq!(
            neoforge_library_url(dead, crate::settings::Mirror::Bmclapi),
            "https://bmclapi2.bangbang93.com/maven/net/neoforged/bus/1.0.17/bus-1.0.17.jar"
        );
        assert_eq!(
            neoforge_library_url(dead, crate::settings::Mirror::Official),
            "https://neoforged.forgecdn.net/releases/net/neoforged/bus/1.0.17/bus-1.0.17.jar"
        );

        // 不是 neoforged 主机的库（比如原版库）也跟着镜像走
        let vanilla = "https://libraries.minecraft.net/com/mojang/a/1/a-1.jar";
        assert_eq!(
            neoforge_library_url(vanilla, crate::settings::Mirror::Bmclapi),
            "https://bmclapi2.bangbang93.com/maven/com/mojang/a/1/a-1.jar"
        );
        // 官方源下原版库不该被改写
        assert_eq!(
            neoforge_library_url(vanilla, crate::settings::Mirror::Official),
            vanilla
        );
    }

    #[test]
    fn mirror_list_is_parsed_and_sorted() {
        let text = r#"[
            {"rawVersion":"neoforge-21.1.9","version":"21.1.9","mcversion":"1.21.1"},
            {"rawVersion":"neoforge-21.1.72","version":"21.1.72","mcversion":"1.21.1"},
            {"rawVersion":"neoforge-21.1.10","version":"21.1.10","mcversion":"1.21.1"},
            {"rawVersion":"neoforge-20.4.1","version":"20.4.1","mcversion":"1.20.4"}
        ]"#;
        let list = parse_neoforge_bmclapi_list(text, "1.21.1").unwrap();
        let versions: Vec<&str> = list.iter().map(|v| v.version.as_str()).collect();
        // 只要 1.21.1 的，并且按数字从新到旧（21.1.72 > 21.1.10 > 21.1.9）
        assert_eq!(versions, ["21.1.72", "21.1.10", "21.1.9"]);
    }

    #[test]
    fn mirror_list_without_matches_is_an_error() {
        let text = r#"[{"version":"20.4.1","mcversion":"1.20.4"}]"#;
        assert!(parse_neoforge_bmclapi_list(text, "1.21.1").is_err());
    }

    /// 官方 Forge 没有按游戏版本列加载器的接口，只能从整份 metadata 里筛。
    #[test]
    fn forge_metadata_is_filtered_by_game_version() {
        let text = r#"<?xml version="1.0"?>
<metadata>
  <versioning>
    <versions>
      <version>1.20.1-47.0.1</version>
      <version>1.20.1-47.2.0</version>
      <version>1.20.1-47.1.10</version>
      <version>1.20.4-49.0.1</version>
      <version>26.3-66.1.0</version>
    </versions>
  </versioning>
</metadata>"#;
        let list = parse_forge_metadata(text, "1.20.1").unwrap();
        let versions: Vec<&str> = list.iter().map(|v| v.version.as_str()).collect();
        // 只要 1.20.1 的；数字序：47.2.0 > 47.1.10 > 47.0.1
        assert_eq!(versions, ["47.2.0", "47.1.10", "47.0.1"]);
    }

    #[test]
    fn forge_metadata_without_matches_is_an_error() {
        let text = "<version>1.20.4-49.0.1</version>";
        assert!(parse_forge_metadata(text, "1.20.1").is_err());
    }

    #[test]
    fn installed_loader_is_recognised_from_either_name() {
        assert_eq!(
            detect_from_ids("26.3-生存", "fabric-loader-0.16.10-1.20.1"),
            Some((LoaderKind::Fabric, "0.16.10".to_string()))
        );
        assert_eq!(
            detect_from_ids("fabric-loader-0.16.10-1.20.1", ""),
            Some((LoaderKind::Fabric, "0.16.10".to_string()))
        );
        assert_eq!(
            detect_from_ids("我的整合包", "1.20.1-forge-47.0.1"),
            Some((LoaderKind::Forge, "47.0.1".to_string()))
        );
        assert_eq!(
            detect_from_ids("neoforge-21.1.72", ""),
            Some((LoaderKind::NeoForge, "21.1.72".to_string()))
        );
        assert_eq!(detect_from_ids("26.3", "26.3"), None);
    }

    #[test]
    fn the_same_manifest_text_is_read_the_same_way() {
        // detect_in_text 是 detect_installed 的主体：给它一段清单文本就该出结论，
        // 别再去碰磁盘。版本列表页靠它避免每帧读盘。
        let text = r#"{"id":"1.20.1-Fabric","libraries":[{"name":"net.fabricmc:fabric-loader:0.17.2"}],"arguments":{"game":[]}}"#;
        assert_eq!(
            detect_in_text(text, "1.20.1-Fabric"),
            Some((LoaderKind::Fabric, "0.17.2".to_string()))
        );
        // 纯原版：什么都没写
        let vanilla = r#"{"id":"1.20.1","libraries":[{"name":"com.mojang:brigadier:1.0.18"}]}"#;
        assert_eq!(detect_in_text(vanilla, "1.20.1"), None);
        // 不是合法 JSON 就当认不出来，而不是 panic
        assert_eq!(detect_in_text("not json", "x"), None);
    }

    #[test]
    fn loader_marker_survives_a_rename() {
        let dir = crate::testutil::fresh_dir("loader-marker");
        let instance_dir = dir.join("versions").join("我的整合包");
        std::fs::create_dir_all(&instance_dir).unwrap();

        let marked = mark_loader(
            r#"{"id":"1.20.1-forge-47.0.1","inheritsFrom":"1.20.1"}"#,
            LoaderKind::Forge,
            "47.0.1",
        )
        .unwrap();
        std::fs::write(instance_dir.join("我的整合包.json"), marked).unwrap();

        assert_eq!(
            detect_installed(&dir, "我的整合包"),
            Some((LoaderKind::Forge, "47.0.1".to_string()))
        );
    }

    #[test]
    fn adopting_an_installer_directory_keeps_the_instance_files() {
        let dir = crate::testutil::fresh_dir("loader-adopt");
        let instance_dir = dir.join("versions").join("26.3-生存");
        std::fs::create_dir_all(instance_dir.join("saves").join("世界一")).unwrap();
        std::fs::write(instance_dir.join("26.3-生存.jar"), b"jar").unwrap();

        // 安装器只会照自己的命名规则写一个新目录，里面只有清单。
        let created = "26.3-forge-47.0.1";
        let created_dir = dir.join("versions").join(created);
        std::fs::create_dir_all(&created_dir).unwrap();
        std::fs::write(
            created_dir.join(format!("{}.json", created)),
            r#"{"id":"26.3-forge-47.0.1","inheritsFrom":"26.3"}"#,
        )
        .unwrap();

        adopt_loader_dir(&dir, created, "26.3-生存", "26.3", LoaderKind::Forge, "47.0.1").unwrap();

        assert!(!created_dir.exists(), "空壳目录该被清掉");
        assert!(instance_dir.join("saves").join("世界一").is_dir(), "存档不能被碰");
        assert!(instance_dir.join("26.3-生存.jar").is_file());
        assert_eq!(
            detect_installed(&dir, "26.3-生存"),
            Some((LoaderKind::Forge, "47.0.1".to_string()))
        );
        let text = std::fs::read_to_string(instance_dir.join("26.3-生存.json")).unwrap();
        assert!(text.contains("26.3-生存"));
        assert!(text.contains("_aicraftOfficialId"));
    }

    #[test]
    fn a_plain_instance_has_no_loader() {
        let dir = crate::testutil::fresh_dir("loader-plain");
        let instance_dir = dir.join("versions").join("26.3-生存");
        std::fs::create_dir_all(&instance_dir).unwrap();
        std::fs::write(
            instance_dir.join("26.3-生存.json"),
            r#"{"id":"26.3-生存","inheritsFrom":"26.3","_aicraftOfficialId":"26.3"}"#,
        )
        .unwrap();

        assert_eq!(detect_installed(&dir, "26.3-生存"), None);
    }

    #[test]
    fn fabric_from_another_launcher_is_recognised_by_its_library() {
        // PCL 的命名：目录叫 `1.20.1-Fabric`，id 也是，没有 `fabric-loader-` 前缀。
        let dir = crate::testutil::fresh_dir("loader-pcl-fabric");
        let instance_dir = dir.join("versions").join("1.20.1-Fabric");
        std::fs::create_dir_all(&instance_dir).unwrap();
        std::fs::write(
            instance_dir.join("1.20.1-Fabric.json"),
            r#"{
                "id": "1.20.1-Fabric",
                "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
                "libraries": [
                    {"name": "net.fabricmc:intermediary:1.20.1"},
                    {"name": "net.fabricmc:fabric-loader:0.16.10"},
                    {"name": "net.fabricmc:sponge-mixin:0.15.4+mixin.0.8.7"}
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(
            detect_installed(&dir, "1.20.1-Fabric"),
            Some((LoaderKind::Fabric, "0.16.10".to_string()))
        );
    }

    #[test]
    fn forge_from_another_launcher_is_recognised_by_its_library() {
        // HMCL 的整合包，目录名跟加载器毫无关系。
        let dir = crate::testutil::fresh_dir("loader-hmcl-forge");
        let instance_dir = dir.join("versions").join("Modpacks");
        std::fs::create_dir_all(&instance_dir).unwrap();
        std::fs::write(
            instance_dir.join("Modpacks.json"),
            r#"{
                "id": "Modpacks",
                "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
                "libraries": [
                    {"name": "net.minecraftforge:fmlloader:1.20.1-47.4.8"},
                    {"name": "net.minecraftforge:coremods:5.2.4"}
                ],
                "arguments": {"game": ["--fml.forgeVersion", "47.4.8", "--fml.mcVersion", "1.20.1"]}
            }"#,
        )
        .unwrap();

        assert_eq!(
            detect_installed(&dir, "Modpacks"),
            Some((LoaderKind::Forge, "47.4.8".to_string()))
        );
    }

    #[test]
    fn neoforge_wins_over_the_forge_looking_parts() {
        // NeoForge 也带 `cpw.mods` 与 `net.minecraftforge:` 的公共库，
        // 只有参数里的 `--fml.neoForgeVersion` 能把它和 Forge 分开。
        let dir = crate::testutil::fresh_dir("loader-neoforge-pack");
        let instance_dir = dir.join("versions").join("ATM10 To the Sky");
        std::fs::create_dir_all(&instance_dir).unwrap();
        std::fs::write(
            instance_dir.join("ATM10 To the Sky.json"),
            r#"{
                "id": "ATM10 To the Sky",
                "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
                "libraries": [
                    {"name": "cpw.mods:bootstraplauncher:2.0.2"},
                    {"name": "net.minecraftforge:srgutils:0.4.15"},
                    {"name": "net.neoforged.fancymodloader:loader:4.0.42"}
                ],
                "arguments": {"game": ["--fml.neoForgeVersion", "21.1.221", "--fml.fmlVersion", "4.0.42"]}
            }"#,
        )
        .unwrap();

        assert_eq!(
            detect_installed(&dir, "ATM10 To the Sky"),
            Some((LoaderKind::NeoForge, "21.1.221".to_string()))
        );
    }

    #[test]
    fn neoforge_under_the_forge_name_is_still_neoforge() {
        // NeoForge 1.20.1 的坐标是 `net.neoforged:forge:1.20.1-47.1.106`。
        let dir = crate::testutil::fresh_dir("loader-neoforge-1201");
        let instance_dir = dir.join("versions").join("1.20.1-NeoForge");
        std::fs::create_dir_all(&instance_dir).unwrap();
        std::fs::write(
            instance_dir.join("1.20.1-NeoForge.json"),
            r#"{
                "id": "1.20.1-NeoForge",
                "libraries": [{"name": "net.neoforged:forge:1.20.1-47.1.106"}]
            }"#,
        )
        .unwrap();

        assert_eq!(
            detect_installed(&dir, "1.20.1-NeoForge"),
            Some((LoaderKind::NeoForge, "47.1.106".to_string()))
        );
    }

    #[test]
    fn a_vanilla_manifest_from_another_launcher_has_no_loader() {
        let dir = crate::testutil::fresh_dir("loader-vanilla-other");
        let instance_dir = dir.join("versions").join("1.21.7");
        std::fs::create_dir_all(&instance_dir).unwrap();
        std::fs::write(
            instance_dir.join("1.21.7.json"),
            r#"{
                "id": "1.21.7",
                "mainClass": "net.minecraft.client.main.Main",
                "libraries": [{"name": "com.mojang:brigadier:1.3.10"}]
            }"#,
        )
        .unwrap();

        assert_eq!(detect_installed(&dir, "1.21.7"), None);
    }

    #[test]
    fn game_version_comes_from_the_forge_argument() {
        let text = r#"{
            "id": "Modpacks",
            "arguments": {"game": ["--fml.forgeVersion", "47.4.8", "--fml.mcVersion", "1.20.1"]}
        }"#;
        assert_eq!(detect_game_version(text), Some("1.20.1".to_string()));
    }

    #[test]
    fn game_version_comes_from_the_fabric_intermediary() {
        let text = r#"{
            "id": "1.20.1-Fabric",
            "libraries": [{"name": "net.fabricmc:intermediary:1.20.1"}]
        }"#;
        assert_eq!(detect_game_version(text), Some("1.20.1".to_string()));
    }

    #[test]
    fn a_zero_intermediary_is_not_a_game_version() {
        let text = r#"{
            "id": "26.3-Fabric",
            "libraries": [
                {"name": "net.fabricmc:intermediary:0.0.0"},
                {"name": "net.fabricmc:fabric-loader:0.19.5"}
            ]
        }"#;
        assert_eq!(detect_game_version(text), Some("26.3".to_string()));
    }

    #[test]
    fn game_version_falls_back_to_the_id() {
        assert_eq!(
            detect_game_version(r#"{"id":"1.20.4-SkyBlock"}"#),
            Some("1.20.4".to_string())
        );
        assert_eq!(detect_game_version(r#"{"id":"Modpacks"}"#), None);
    }

    #[test]
    fn loader_kind_round_trips() {        for kind in LoaderKind::ALL {
            assert_eq!(LoaderKind::from_id(kind.id()), Some(kind));
        }
        assert_eq!(LoaderKind::from_id(" FABRIC "), Some(LoaderKind::Fabric));
        assert_eq!(LoaderKind::from_id("quilt"), None);
    }
}
