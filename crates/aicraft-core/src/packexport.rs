//! 导出整合包：把一份实例打成 HMCL / Modrinth 认得的压缩包。
//!
//! 和 [crate::modpack] 是一对：那边读、这边写。两个格式的目录约定必须与
//! 导入侧完全对齐（见 [crate::modpack::PackFormat::overrides]），否则自己
//! 导出来的包自己都装不回去。
//!
//! core 整仓零第三方依赖，所以这里自带一个最小的 ZIP 写入器：只做 stored
//! （不压缩），写本地头、拷数据、最后补中央目录与 EOCD。包会比压过的大
//! 一些，换来的是不用引 flate2 那种重库。

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::cancel::CancelToken;
use crate::json::{self, Json};
use crate::loader::{self, LoaderKind};

/// 导出成哪种格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// .mrpack：清单里列下载地址，只有认不出的文件才进 overrides/。
    Modrinth,
    /// .zip：文件全部打在包里，清单是 modpack.json + minecraft/pack.json。
    Hmcl,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 2] = [ExportFormat::Modrinth, ExportFormat::Hmcl];

    /// 存进配置 / 日志用的短名。
    pub fn id(self) -> &'static str {
        match self {
            ExportFormat::Modrinth => "modrinth",
            ExportFormat::Hmcl => "hmcl",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Modrinth => "Modrinth 整合包",
            ExportFormat::Hmcl => "HMCL 整合包",
        }
    }

    /// 保存对话框用的扩展名（不带点）。
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Modrinth => "mrpack",
            ExportFormat::Hmcl => "zip",
        }
    }

    /// 一句话说明这个格式长什么样，界面上摆在选项旁边。
    pub fn hint(self) -> &'static str {
        match self {
            ExportFormat::Modrinth => "文件列下载地址，包体小；别人装的时候要联网",
            ExportFormat::Hmcl => "文件全打进包里，包体大；断网也能装",
        }
    }

    pub fn from_id(id: &str) -> Option<ExportFormat> {
        ExportFormat::ALL
            .into_iter()
            .find(|format| format.id().eq_ignore_ascii_case(id.trim()))
    }
}

/// 导出过程中的进度。
#[derive(Debug, Clone)]
pub enum ExportProgress {
    /// 正在做哪一步。
    Stage(String),
    /// 已经写进包里的文件数。
    File { done: usize, total: usize },
}

/// 一次导出要用的全部参数。
pub struct ExportConfig<'a> {
    pub game_dir: &'a Path,
    pub instance: &'a str,
    pub format: ExportFormat,
    pub name: String,
    pub version: String,
    /// 写到哪个文件。调用方已经让用户挑过位置了。
    pub dest: &'a Path,
    /// 要不要问 Modrinth 这些文件的下载地址。
    ///
    /// 关掉就把模组全塞进 overrides/——断网、或者不想让导出过程依赖网络
    /// 时用得上。测试里也靠它避开真实请求。
    pub resolve_downloads: bool,
    /// 玩家在导出对话框里勾上的顶层条目（[`catalog`] 给出的 `name`）。
    ///
    /// 只认顶层：真正决定「带不带存档、带不带日志」的就是这一层，再往下
    /// 逐文件勾选只会让人点到手酸。空表等于什么都不导，导出会被拒掉。
    pub include: &'a [String],
    pub cancel: &'a CancelToken,
}

/// 导出结果。
#[derive(Debug, Clone)]
pub struct ExportReport {
    pub path: PathBuf,
    /// 写进包里的文件总数（含清单）。
    pub files: usize,
    /// 其中靠下载地址进清单的（Modrinth 才有）。
    pub resolved: usize,
    /// 最终文件大小。
    pub bytes: u64,
    /// 打包过程中忍下来的问题（不致命）。
    pub warnings: Vec<String>,
    pub cancelled: bool,
}

impl ExportReport {
    fn cancelled(path: &Path) -> Self {
        ExportReport {
            path: path.to_path_buf(),
            files: 0,
            resolved: 0,
            bytes: 0,
            warnings: Vec::new(),
            cancelled: true,
        }
    }
}

/// 启动器自己铺、或者写进去只会把包弄坏的东西：无论玩家勾了什么，都不进包。
///
/// natives/ 与 libraries/ 是启动器自己铺的，别人装的时候会重新铺一遍；
/// 实例清单与客户端 jar 由导入方按游戏版本重新生成（见 [`skip_top`]）。
/// 这几样在导出对话框里干脆不列出来 —— 列出来却勾不动，比不列更让人犯嘀咕。
const NEVER_EXPORT: [&str; 2] = ["natives", "libraries"];

/// 默认勾上的目录。
///
/// 「装上就该和导出者看到的一样」，靠的就是这几样：模组、配置，加上模组
/// 加载器约定俗成的几个脚本目录。存档、日志、截图属于导出者自己，默认
/// 不勾，但仍然列在对话框里 —— 想连存档一起给朋友，自己打勾就行。
const DEFAULT_DIRS: [&str; 7] = [
    "mods",
    "config",
    "defaultconfigs",
    "resourcepacks",
    "shaderpacks",
    "kubejs",
    "scripts",
];

/// 默认勾上的顶层文件。
const DEFAULT_FILES: [&str; 1] = ["options.txt"];

/// 这些目录里的文件可以靠 Modrinth 的下载地址进清单。
const LOOKUP_DIRS: [&str; 3] = ["mods/", "resourcepacks/", "shaderpacks/"];

/// 一次反查最多带多少个指纹（和 modupdate 一样按 100 分批）。
const BATCH: usize = 100;

/// 导出整合包。
pub fn export_pack(
    config: &ExportConfig,
    on_progress: &(dyn Fn(ExportProgress) + Send + Sync),
    on_log: &mut dyn FnMut(&str),
) -> Result<ExportReport, String> {
    if config.cancel.is_cancelled() {
        return Ok(ExportReport::cancelled(config.dest));
    }

    on_progress(ExportProgress::Stage("读取实例清单".to_string()));
    let game_version = crate::install::instance_game_version(config.game_dir, config.instance)
        .ok_or_else(|| {
            format!(
                "认不出 {} 是哪个 Minecraft 版本，这样的包别人装不起来",
                config.instance
            )
        })?;
    let loader = loader::detect_installed(config.game_dir, config.instance);
    on_log(&format!(
        "实例 {}：Minecraft {}{}",
        config.instance,
        game_version,
        match &loader {
            Some((kind, version)) => format!("，{} {}", kind.label(), version),
            None => String::new(),
        }
    ));

    // 压平清单。压不了不是死路：HMCL 包没有 pack.json 也读得出来，
    // 只是别人装完是原版，所以这里降级成一条警告。
    on_progress(ExportProgress::Stage("整理版本清单".to_string()));
    let mut warnings = Vec::new();
    let pack_json = match crate::flatten::flatten_instance(config.game_dir, config.instance) {
        Ok(text) => match pack_json_text(&text, &game_version) {
            Ok(text) => Some(text),
            Err(e) => {
                warnings.push(format!("版本清单没法压平：{}", e));
                None
            }
        },
        Err(e) => {
            warnings.push(format!("版本清单没法压平：{}", e));
            None
        }
    };
    for line in &warnings {
        on_log(line);
    }

    on_progress(ExportProgress::Stage("清点要打包的文件".to_string()));
    let instance_dir = crate::instance::version_dir(config.game_dir, config.instance);
    if !instance_dir.is_dir() {
        return Err(format!("{} 不在盘上", instance_dir.display()));
    }
    let picked = collect(&instance_dir, config.instance, config.include);
    if picked.is_empty() {
        return Err("一个文件都没勾，这样的包别人装上什么也没有".to_string());
    }
    let total_bytes: u64 = picked.iter().map(|p| p.size).sum();
    on_log(&format!(
        "{} 个文件，{:.1} MB",
        picked.len(),
        total_bytes as f64 / 1_048_576.0
    ));

    // Modrinth 格式先问一遍下载地址；问不到的一律退回 overrides。
    let mut hits: HashMap<String, Hit> = HashMap::new();
    if config.format == ExportFormat::Modrinth && config.resolve_downloads {
        let wanted: Vec<&Picked> = picked
            .iter()
            .filter(|p| is_lookup_candidate(&p.rel))
            .collect();
        if !wanted.is_empty() {
            on_progress(ExportProgress::Stage(
                "问 Modrinth 这些文件在哪下载".to_string(),
            ));
            hits = resolve_downloads(&wanted, on_log);
            on_log(&format!(
                "{} / {} 个文件找到了下载地址",
                hits.len(),
                wanted.len()
            ));
        }
    }

    let index = match config.format {
        ExportFormat::Modrinth => Some(modrinth_index(config, &game_version, loader, &picked, &hits)),
        ExportFormat::Hmcl => None,
    };
    let manifest = match config.format {
        ExportFormat::Hmcl => Some(hmcl_manifest(config, &game_version)),
        ExportFormat::Modrinth => None,
    };

    on_progress(ExportProgress::Stage("打包".to_string()));
    let mut written = 0usize;
    let result = write_archive(
        config,
        &picked,
        &hits,
        index.as_deref(),
        manifest.as_deref(),
        pack_json.as_deref(),
        on_progress,
        &mut written,
    );
    if let Err(e) = result {
        // 写了一半的文件留着只会让人以为导成功了，删掉。
        let _ = std::fs::remove_file(config.dest);
        return Err(e);
    }

    let bytes = std::fs::metadata(config.dest).map(|m| m.len()).unwrap_or(0);
    Ok(ExportReport {
        path: config.dest.to_path_buf(),
        files: written,
        resolved: hits.len(),
        bytes,
        warnings,
        cancelled: false,
    })
}

/// 导出对话框里的一行：实例根目录下的一个条目。
///
/// 只到顶层为止。再往下逐文件列，几百个模组几百行配置，勾选就变成体力活；
/// 而「要不要带存档、带不带日志」这种决定本来就发生在顶层。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportEntry {
    /// 条目名（目录名或文件名），也是 [`ExportConfig::include`] 里认的那个值。
    pub name: String,
    /// 里面一共有多少个文件；单个文件就是 1。
    pub files: usize,
    /// 一共多少字节。
    pub bytes: u64,
    pub is_dir: bool,
    /// 默认勾不勾：模组、配置这类「装上就该一样」的默认勾上，
    /// 存档、日志、截图这类导出者私人的默认不勾。
    pub default_on: bool,
}

/// 列出这个实例顶层可以导出的东西。
///
/// 目录排在前面、文件排在后面，各自按名字排；启动器自己的东西
/// （实例清单、客户端 jar、natives、libraries）不出现。
pub fn catalog(instance_dir: &Path, instance: &str) -> Vec<ExportEntry> {
    let Ok(read) = std::fs::read_dir(instance_dir) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());

    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || skip_top(&name, instance) {
            continue;
        }
        let path = entry.path();
        let default_on = is_default(&name);
        if path.is_dir() {
            let (count, bytes) = measure(&path);
            dirs.push(ExportEntry {
                name,
                files: count,
                bytes,
                is_dir: true,
                default_on,
            });
        } else if path.is_file() {
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            files.push(ExportEntry {
                name,
                files: 1,
                bytes,
                is_dir: false,
                default_on,
            });
        }
    }
    dirs.extend(files);
    // 排一下：默认勾中的先按清单里的顺序站在最上面（打开对话框第一眼
    // 看到的就是要带走的那几样），其余目录按名字跟在后面，零散文件垫底。
    // 不排的话，实例里随便一个 `PCL/`、`backups/` 都会挤到模组前面去。
    dirs.sort_by_key(|entry| {
        let rank = if entry.default_on {
            default_rank(&entry.name).unwrap_or(usize::MAX)
        } else {
            usize::MAX
        };
        (
            usize::from(!entry.default_on),
            usize::from(!entry.is_dir),
            rank,
            entry.name.to_lowercase(),
        )
    });
    dirs
}

/// 这个名字在默认清单里排第几。
fn default_rank(name: &str) -> Option<usize> {
    DEFAULT_DIRS
        .iter()
        .chain(DEFAULT_FILES.iter())
        .position(|known| known.eq_ignore_ascii_case(name))
}

/// 一个目录里有多少个文件、一共多少字节。
///
/// 只数不读：导出对话框打开时就要把每个目录的大小摆出来，这时候去读
/// 文件内容只是白费时间。
fn measure(dir: &Path) -> (usize, u64) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut count = 0usize;
    let mut bytes = 0u64;
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            let (inner_count, inner_bytes) = measure(&path);
            count += inner_count;
            bytes += inner_bytes;
        } else if path.is_file() {
            count += 1;
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    (count, bytes)
}

/// 这个名字默认勾不勾。
fn is_default(name: &str) -> bool {
    DEFAULT_DIRS.iter().any(|d| d.eq_ignore_ascii_case(name))
        || DEFAULT_FILES.iter().any(|f| f.eq_ignore_ascii_case(name))
}

/// 把一个实例里**玩家勾中的**那些文件清出来。
///
/// `include` 是顶层条目名；大小写按 Windows 的习惯不敏感。里面没有的名字
/// 一律当没勾 —— 包括 [`NEVER_EXPORT`] 里的东西，勾了也不导。
fn collect(instance_dir: &Path, instance: &str, include: &[String]) -> Vec<Picked> {
    let mut out = Vec::new();
    walk(instance_dir, Path::new(""), instance, include, true, &mut out);
    out
}

fn walk(
    root: &Path,
    rel: &Path,
    instance: &str,
    include: &[String],
    at_top: bool,
    out: &mut Vec<Picked>,
) {
    let Ok(read) = std::fs::read_dir(root.join(rel)) else {
        return;
    };
    let mut entries: Vec<_> = read.flatten().collect();
    // 排一下序，同一个实例每次导出来的包内容顺序一致，便于比对。
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if at_top {
            if skip_top(&name, instance) {
                continue;
            }
            // 顶层只认勾中的那些；下面几层既然能走到这儿，就已经是勾中的了。
            if !include.iter().any(|picked| picked.eq_ignore_ascii_case(&name)) {
                continue;
            }
        }
        let child = rel.join(&name);
        let path = entry.path();
        if path.is_dir() {
            walk(root, &child, instance, include, false, out);
        } else if path.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(Picked {
                rel: child.to_string_lossy().replace('\\', "/"),
                path,
                size,
            });
        }
    }
}

fn skip_top(name: &str, instance: &str) -> bool {
    NEVER_EXPORT.iter().any(|s| s.eq_ignore_ascii_case(name))
        || name.eq_ignore_ascii_case("aicraft-instance.json")
        || name.eq_ignore_ascii_case(&format!("{}.json", instance))
        || name.eq_ignore_ascii_case(&format!("{}.jar", instance))
}

fn is_lookup_candidate(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    LOOKUP_DIRS.iter().any(|d| lower.starts_with(d))
        && (lower.ends_with(".jar") || lower.ends_with(".zip"))
}

/// 一个待打包的文件。
struct Picked {
    /// 相对实例目录的路径，一律用 / 分隔。
    rel: String,
    path: PathBuf,
    size: u64,
}

/// Modrinth 反查到的下载信息。
struct Hit {
    url: String,
    sha512: Option<String>,
    size: u64,
}

fn resolve_downloads(entries: &[&Picked], on_log: &mut dyn FnMut(&str)) -> HashMap<String, Hit> {
    let mut out = HashMap::new();

    for chunk in entries.chunks(BATCH) {
        let mut hashes = Vec::new();
        let mut by_hash: HashMap<String, &Picked> = HashMap::new();
        for picked in chunk {
            match crate::sha1::sha1_file_hex(&picked.path) {
                Ok(hash) => {
                    by_hash.insert(hash.clone(), picked);
                    hashes.push(hash);
                }
                Err(e) => on_log(&format!("算不出 {} 的指纹：{}", picked.rel, e)),
            }
        }
        if hashes.is_empty() {
            continue;
        }

        let body = json::write(&Json::Object(vec![
            (
                "hashes".to_string(),
                Json::Array(hashes.iter().map(|h| Json::Str(h.clone())).collect()),
            ),
            ("algorithm".to_string(), Json::Str("sha1".to_string())),
        ]));
        let url = format!("{}/version_files", loader::MODRINTH_API);
        let response = match crate::transport::post_json(&url, &body) {
            Ok(response) => response,
            Err(e) => {
                on_log(&format!(
                    "问不到 Modrinth（{}），模组都放进 overrides",
                    e.message
                ));
                continue;
            }
        };
        if !response.is_success() {
            on_log(&format!(
                "Modrinth 回了 {}，模组都放进 overrides",
                response.status
            ));
            continue;
        }
        let Ok(value) = json::parse(&response.body) else {
            on_log("Modrinth 回的东西看不懂，模组都放进 overrides");
            continue;
        };

        for (hash, picked) in by_hash {
            let Some(version) = value.get(&hash) else {
                continue;
            };
            let Some(file) = primary_file(version, &hash) else {
                continue;
            };
            let Some(link) = file.get("url").and_then(Json::as_str) else {
                continue;
            };
            out.insert(
                picked.rel.clone(),
                Hit {
                    url: link.to_string(),
                    sha512: file
                        .get("hashes")
                        .and_then(|h| h.get("sha512"))
                        .and_then(Json::as_str)
                        .map(str::to_string),
                    size: file.get("size").and_then(Json::as_i64).unwrap_or(0).max(0) as u64,
                },
            );
        }
    }

    out
}

/// 从一条版本记录里挑出「就是我们这一份」的那个文件。
fn primary_file<'a>(version: &'a Json, hash: &str) -> Option<&'a Json> {
    let files = version.get("files").and_then(Json::as_array)?;
    files
        .iter()
        .find(|f| f.get("hashes").and_then(|h| h.get("sha1")).and_then(Json::as_str) == Some(hash))
        .or_else(|| files.first())
}

/// Modrinth 的 modrinth.index.json。
fn modrinth_index(
    config: &ExportConfig,
    game_version: &str,
    loader: Option<(LoaderKind, String)>,
    picked: &[Picked],
    hits: &HashMap<String, Hit>,
) -> String {
    let mut files = Vec::new();
    for picked in picked {
        let Some(hit) = hits.get(&picked.rel) else {
            continue;
        };
        let mut hashes = vec![(
            "sha1".to_string(),
            Json::Str(crate::sha1::sha1_file_hex(&picked.path).unwrap_or_default()),
        )];
        if let Some(sha512) = &hit.sha512 {
            hashes.push(("sha512".to_string(), Json::Str(sha512.clone())));
        }
        files.push(Json::Object(vec![
            ("path".to_string(), Json::Str(picked.rel.clone())),
            ("hashes".to_string(), Json::Object(hashes)),
            (
                "downloads".to_string(),
                Json::Array(vec![Json::Str(hit.url.clone())]),
            ),
            (
                "fileSize".to_string(),
                Json::Number(hit.size.max(picked.size) as f64),
            ),
        ]));
    }

    let mut deps = vec![(
        "minecraft".to_string(),
        Json::Str(game_version.to_string()),
    )];
    if let Some((kind, version)) = loader {
        let key = match kind {
            LoaderKind::Fabric => "fabric-loader",
            LoaderKind::Forge => "forge",
            LoaderKind::NeoForge => "neoforge",
        };
        deps.push((key.to_string(), Json::Str(version)));
    }

    json::write(&Json::Object(vec![
        ("formatVersion".to_string(), Json::Number(1.0)),
        ("game".to_string(), Json::Str("minecraft".to_string())),
        ("versionId".to_string(), Json::Str(config.version.clone())),
        ("name".to_string(), Json::Str(config.name.clone())),
        (
            "summary".to_string(),
            Json::Str(format!("由 AICraft Launcher 从实例 {} 导出", config.instance)),
        ),
        ("files".to_string(), Json::Array(files)),
        ("dependencies".to_string(), Json::Object(deps)),
    ]))
}

/// HMCL 的 modpack.json。
fn hmcl_manifest(config: &ExportConfig, game_version: &str) -> String {
    json::write(&Json::Object(vec![
        ("manifestType".to_string(), Json::Str("HMCL".to_string())),
        ("manifestVersion".to_string(), Json::Number(1.0)),
        ("name".to_string(), Json::Str(config.name.clone())),
        ("version".to_string(), Json::Str(config.version.clone())),
        (
            "author".to_string(),
            Json::Str("AICraft Launcher".to_string()),
        ),
        (
            "description".to_string(),
            Json::Str(format!("由 AICraft Launcher 从实例 {} 导出", config.instance)),
        ),
        (
            "gameVersion".to_string(),
            Json::Str(game_version.to_string()),
        ),
    ]))
}

/// 压平出来的清单，改成 HMCL 期望的样子。
///
/// 只动一处：jar。压平时它写的是实例名，而导入侧认游戏版本时第一优先
/// 就是 pack.json 的 jar.id（见 [crate::modpack::parse_hmcl]），留着实例名
/// 会让「要装哪个 Minecraft」变成一个实例名。
fn pack_json_text(text: &str, game_version: &str) -> Result<String, String> {
    let mut value = json::parse(text).map_err(|e| format!("实例清单不是合法 JSON: {}", e))?;
    let Json::Object(entries) = &mut value else {
        return Err("实例清单顶层不是对象".to_string());
    };
    match entries.iter_mut().find(|(key, _)| key == "jar") {
        Some((_, slot)) => *slot = Json::Str(game_version.to_string()),
        None => entries.push(("jar".to_string(), Json::Str(game_version.to_string()))),
    }
    Ok(json::write(&value))
}

#[allow(clippy::too_many_arguments)]
fn write_archive(
    config: &ExportConfig,
    picked: &[Picked],
    hits: &HashMap<String, Hit>,
    index: Option<&str>,
    manifest: Option<&str>,
    pack_json: Option<&str>,
    on_progress: &(dyn Fn(ExportProgress) + Send + Sync),
    written: &mut usize,
) -> Result<(), String> {
    if let Some(parent) = config.dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录失败: {}（{}）", e, parent.display()))?;
    }
    let file = std::fs::File::create(config.dest)
        .map_err(|e| format!("写不了 {}: {}", config.dest.display(), e))?;
    let mut zip = Zip::new(io::BufWriter::new(file));

    match config.format {
        ExportFormat::Modrinth => {
            let text = index.ok_or_else(|| "缺清单".to_string())?;
            zip.add_bytes(crate::modpack::MODRINTH_INDEX, text.as_bytes())
                .map_err(|e| format!("写清单失败: {}", e))?;
            *written += 1;
        }
        ExportFormat::Hmcl => {
            let text = manifest.ok_or_else(|| "缺清单".to_string())?;
            zip.add_bytes(crate::modpack::HMCL_MANIFEST, text.as_bytes())
                .map_err(|e| format!("写清单失败: {}", e))?;
            *written += 1;
            if let Some(text) = pack_json {
                zip.add_bytes(crate::modpack::HMCL_PACK, text.as_bytes())
                    .map_err(|e| format!("写 pack.json 失败: {}", e))?;
                *written += 1;
            }
        }
    }

    let total = picked.len();
    for (done, entry) in picked.iter().enumerate() {
        if config.cancel.is_cancelled() {
            return Err("导出被取消了".to_string());
        }
        let name = match config.format {
            // 找得到下载地址的已经在清单里了，包体里再放一份就重复了。
            ExportFormat::Modrinth => {
                if hits.contains_key(&entry.rel) {
                    continue;
                }
                format!("overrides/{}", entry.rel)
            }
            ExportFormat::Hmcl => format!("minecraft/{}", entry.rel),
        };
        zip.add_file(&name, &entry.path)
            .map_err(|e| format!("打包 {} 失败: {}", entry.rel, e))?;
        *written += 1;
        on_progress(ExportProgress::File { done: done + 1, total });
    }

    zip.finish().map_err(|e| format!("收尾失败: {}", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 最小 ZIP 写入器（stored）
// ---------------------------------------------------------------------------

/// 一个写进包的成员，收尾时要用它拼中央目录。
struct Member {
    name: String,
    crc: u32,
    size: u64,
    offset: u64,
}

/// 只做 stored 的 ZIP 写入器。
///
/// 文件名一律按 UTF-8 写（通用标志位第 11 位），否则中文模组名在别的
/// 启动器里会变成乱码。
struct Zip<W: Write> {
    out: W,
    pos: u64,
    members: Vec<Member>,
}

impl<W: Write> Zip<W> {
    fn new(out: W) -> Self {
        Zip {
            out,
            pos: 0,
            members: Vec::new(),
        }
    }

    fn add_bytes(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let crc = crc32(data);
        // 中央目录里记的是本地头的位置，必须在写头之前取。
        let offset = self.pos;
        self.header(name, crc, data.len() as u64)?;
        self.out.write_all(data)?;
        self.pos += data.len() as u64;
        self.members.push(Member {
            name: name.to_string(),
            crc,
            size: data.len() as u64,
            offset,
        });
        Ok(())
    }

    fn add_file(&mut self, name: &str, path: &Path) -> io::Result<()> {
        let size = std::fs::metadata(path)?.len();
        let crc = crc32_file(path)?;
        let offset = self.pos;
        self.header(name, crc, size)?;
        let copied = io::copy(&mut std::fs::File::open(path)?, &mut self.out)?;
        if copied != size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} 读到一半变了（{} ≠ {} 字节）",
                    path.display(),
                    copied,
                    size
                ),
            ));
        }
        self.pos += copied;
        self.members.push(Member {
            name: name.to_string(),
            crc,
            size,
            offset,
        });
        Ok(())
    }

    fn header(&mut self, name: &str, crc: u32, size: u64) -> io::Result<()> {
        if self.pos > u32::MAX as u64 || size > u32::MAX as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "整合包超过 4 GB，这个写入器不支持 ZIP64",
            ));
        }
        let mut head = Vec::with_capacity(30 + name.len());
        head.extend_from_slice(b"PK\x03\x04");
        head.extend_from_slice(&20u16.to_le_bytes()); // 解压所需版本
        head.extend_from_slice(&0x0800u16.to_le_bytes()); // 名字是 UTF-8
        head.extend_from_slice(&0u16.to_le_bytes()); // stored
        head.extend_from_slice(&0u16.to_le_bytes()); // 时间
        head.extend_from_slice(&0u16.to_le_bytes()); // 日期
        head.extend_from_slice(&crc.to_le_bytes());
        head.extend_from_slice(&(size as u32).to_le_bytes());
        head.extend_from_slice(&(size as u32).to_le_bytes());
        head.extend_from_slice(&(name.len() as u16).to_le_bytes());
        head.extend_from_slice(&0u16.to_le_bytes()); // 扩展字段长度
        head.extend_from_slice(name.as_bytes());
        self.out.write_all(&head)?;
        self.pos += head.len() as u64;
        Ok(())
    }

    fn finish(mut self) -> io::Result<W> {
        let start = self.pos;
        let members = std::mem::take(&mut self.members);
        for member in &members {
            let mut head = Vec::with_capacity(46 + member.name.len());
            head.extend_from_slice(b"PK\x01\x02");
            head.extend_from_slice(&20u16.to_le_bytes()); // 制作版本
            head.extend_from_slice(&20u16.to_le_bytes()); // 解压所需版本
            head.extend_from_slice(&0x0800u16.to_le_bytes());
            head.extend_from_slice(&0u16.to_le_bytes());
            head.extend_from_slice(&0u16.to_le_bytes());
            head.extend_from_slice(&0u16.to_le_bytes());
            head.extend_from_slice(&member.crc.to_le_bytes());
            head.extend_from_slice(&(member.size as u32).to_le_bytes());
            head.extend_from_slice(&(member.size as u32).to_le_bytes());
            head.extend_from_slice(&(member.name.len() as u16).to_le_bytes());
            head.extend_from_slice(&0u16.to_le_bytes()); // 扩展
            head.extend_from_slice(&0u16.to_le_bytes()); // 注释
            head.extend_from_slice(&0u16.to_le_bytes()); // 起始磁盘
            head.extend_from_slice(&0u16.to_le_bytes()); // 内部属性
            head.extend_from_slice(&0u32.to_le_bytes()); // 外部属性
            head.extend_from_slice(&(member.offset as u32).to_le_bytes());
            head.extend_from_slice(member.name.as_bytes());
            self.out.write_all(&head)?;
            self.pos += head.len() as u64;
        }
        let size = self.pos - start;
        if self.pos > u32::MAX as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "整合包超过 4 GB，这个写入器不支持 ZIP64",
            ));
        }

        let mut tail = Vec::with_capacity(22);
        tail.extend_from_slice(b"PK\x05\x06");
        tail.extend_from_slice(&0u16.to_le_bytes());
        tail.extend_from_slice(&0u16.to_le_bytes());
        tail.extend_from_slice(&(members.len() as u16).to_le_bytes());
        tail.extend_from_slice(&(members.len() as u16).to_le_bytes());
        tail.extend_from_slice(&(size as u32).to_le_bytes());
        tail.extend_from_slice(&(start as u32).to_le_bytes());
        tail.extend_from_slice(&0u16.to_le_bytes());
        self.out.write_all(&tail)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

fn crc_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut value = i as u32;
            for _ in 0..8 {
                value = if value & 1 == 1 {
                    0xEDB8_8320 ^ (value >> 1)
                } else {
                    value >> 1
                };
            }
            *slot = value;
        }
        table
    })
}

fn crc32(data: &[u8]) -> u32 {
    let table = crc_table();
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = table[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

fn crc32_file(path: &Path) -> io::Result<u32> {
    let table = crc_table();
    let mut file = std::fs::File::open(path)?;
    let mut crc = 0xFFFF_FFFFu32;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        for &byte in &buf[..read] {
            crc = table[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
    }
    Ok(!crc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::fresh_dir;

    /// 造一个最小实例：versions/<id>/<id>.json + 几个要打包的文件。
    fn make_instance(root: &Path, id: &str) -> PathBuf {
        let dir = crate::instance::version_dir(root, id);
        std::fs::create_dir_all(dir.join("mods")).unwrap();
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::create_dir_all(dir.join("saves")).unwrap();
        let manifest = r#"{
            "id": "1.20.1-Fabric",
            "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
            "assetIndex": { "id": "5" },
            "assets": "5",
            "libraries": [
                { "name": "net.fabricmc:fabric-loader:0.15.11" },
                { "name": "net.fabricmc:intermediary:1.20.1" }
            ],
            "arguments": { "game": [], "jvm": [] }
        }"#;
        std::fs::write(dir.join(format!("{}.json", id)), manifest).unwrap();
        std::fs::write(dir.join(format!("{}.jar", id)), b"jar").unwrap();
        std::fs::write(dir.join("options.txt"), "lang:zh_cn\n").unwrap();
        std::fs::write(dir.join("mods").join("a.jar"), b"mod-a").unwrap();
        std::fs::write(dir.join("mods").join("b.jar"), b"mod-b").unwrap();
        std::fs::write(dir.join("config").join("x.toml"), b"cfg").unwrap();
        std::fs::write(dir.join("logs").join("latest.log"), b"noise").unwrap();
        std::fs::write(dir.join("saves").join("world.dat"), b"my world").unwrap();
        dir
    }

    /// 默认勾选的那几样（模组、配置、资源包、光影包、options.txt）。
    fn defaults(instance_dir: &Path) -> Vec<String> {
        catalog(instance_dir, "1.20.1-Fabric")
            .into_iter()
            .filter(|entry| entry.default_on)
            .map(|entry| entry.name)
            .collect()
    }

    fn run(root: &Path, id: &str, format: ExportFormat, dest: &Path) -> ExportReport {
        let include = defaults(&crate::instance::version_dir(root, id));
        run_with(root, id, format, dest, &include)
    }

    fn run_with(
        root: &Path,
        id: &str,
        format: ExportFormat,
        dest: &Path,
        include: &[String],
    ) -> ExportReport {
        let cancel = CancelToken::new();
        let config = ExportConfig {
            game_dir: root,
            instance: id,
            format,
            name: "测试包".to_string(),
            version: "1.0.0".to_string(),
            dest,
            // 测试里不联网：全部退回 overrides。
            resolve_downloads: false,
            include,
            cancel: &cancel,
        };
        export_pack(&config, &|_| {}, &mut |_| {}).unwrap()
    }

    #[test]
    fn an_hmcl_export_round_trips_through_the_importer() {
        let temp = fresh_dir("packexport-hmcl");
        let root = &*temp;
        make_instance(root, "1.20.1-Fabric");
        let dest = root.join("out.zip");

        let report = run(root, "1.20.1-Fabric", ExportFormat::Hmcl, &dest);
        assert!(!report.cancelled);
        assert!(report.files >= 4, "至少清单 + pack.json + 几个文件");
        assert!(dest.is_file());

        // 自己导出来的包，导入侧必须认得出来。
        let manifest = crate::modpack::inspect(&dest).unwrap();
        assert_eq!(manifest.format, crate::modpack::PackFormat::Hmcl);
        assert_eq!(manifest.name, "测试包");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.game_version, "1.20.1");
        assert_eq!(
            manifest.loader,
            Some((LoaderKind::Fabric, "0.15.11".to_string()))
        );
    }

    #[test]
    fn a_modrinth_export_round_trips_through_the_importer() {
        let temp = fresh_dir("packexport-mrpack");
        let root = &*temp;
        make_instance(root, "1.20.1-Fabric");
        let dest = root.join("out.mrpack");

        let report = run(root, "1.20.1-Fabric", ExportFormat::Modrinth, &dest);
        assert!(!report.cancelled);
        // 没联网，所以一个下载地址都没拿到。
        assert_eq!(report.resolved, 0);

        let manifest = crate::modpack::inspect(&dest).unwrap();
        assert_eq!(manifest.format, crate::modpack::PackFormat::Modrinth);
        assert_eq!(manifest.game_version, "1.20.1");
        assert!(manifest.files.is_empty(), "没反查到地址就不该列文件");
    }

    #[test]
    fn logs_and_saves_never_go_into_the_pack() {
        let temp = fresh_dir("packexport-skip");
        let root = &*temp;
        make_instance(root, "1.20.1-Fabric");
        let dest = root.join("out.zip");
        run(root, "1.20.1-Fabric", ExportFormat::Hmcl, &dest);

        let names: Vec<String> = crate::natives::read_zip_entries(&dest)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.iter().any(|n| n == "minecraft/options.txt"));
        assert!(names.iter().any(|n| n == "minecraft/mods/a.jar"));
        assert!(names.iter().any(|n| n == "minecraft/config/x.toml"));
        assert!(!names.iter().any(|n| n.starts_with("minecraft/logs/")));
        assert!(!names.iter().any(|n| n.starts_with("minecraft/saves/")));
        // 实例清单与客户端 jar 由别人重新生成，不塞进包。
        assert!(!names.iter().any(|n| n.ends_with("1.20.1-Fabric.json")));
        assert!(!names.iter().any(|n| n.ends_with("1.20.1-Fabric.jar")));
    }

    #[test]
    fn the_zip_writer_produces_a_file_the_reader_can_open() {
        let temp = fresh_dir("packexport-zip");
        let path = &*temp.join("a.zip");
        let mut zip = Zip::new(std::fs::File::create(path).unwrap());
        zip.add_bytes("hello.txt", "你好".as_bytes()).unwrap();
        zip.add_bytes("dir/inner.bin", &[0u8, 1, 2, 3]).unwrap();
        zip.finish().unwrap();

        let entries = crate::natives::read_zip_entries(path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "hello.txt");
        assert_eq!(entries[0].uncompressed_size, "你好".len() as u64);
        assert_eq!(
            entries[0].read_data(path).unwrap(),
            "你好".as_bytes().to_vec()
        );
        assert_eq!(entries[1].read_data(path).unwrap(), vec![0u8, 1, 2, 3]);
    }

    #[test]
    fn the_crc_matches_the_known_value() {
        // "123456789" 的 CRC-32 是 0xCBF43926，这是标准校验向量。
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn an_instance_without_a_readable_version_is_refused() {
        let temp = fresh_dir("packexport-noversion");
        let root = &*temp;
        let dir = crate::instance::version_dir(root, "破的");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("破的.json"), "{ not json").unwrap();

        let cancel = CancelToken::new();
        let dest = root.join("out.zip");
        let config = ExportConfig {
            game_dir: root,
            instance: "破的",
            format: ExportFormat::Hmcl,
            name: "x".to_string(),
            version: "1".to_string(),
            dest: &dest,
            resolve_downloads: false,
            include: &["mods".to_string()],
            cancel: &cancel,
        };
        let err = export_pack(&config, &|_| {}, &mut |_| {}).unwrap_err();
        assert!(err.contains("Minecraft 版本"), "实际是：{}", err);
        assert!(!dest.exists(), "失败时不该留下半截文件");
    }

    #[test]
    fn a_cancelled_export_writes_nothing() {
        let temp = fresh_dir("packexport-cancel");
        let root = &*temp;
        make_instance(root, "1.20.1-Fabric");
        let cancel = CancelToken::new();
        cancel.cancel();
        let dest = root.join("out.zip");
        let config = ExportConfig {
            game_dir: root,
            instance: "1.20.1-Fabric",
            format: ExportFormat::Hmcl,
            name: "x".to_string(),
            version: "1".to_string(),
            dest: &dest,
            resolve_downloads: false,
            include: &["mods".to_string()],
            cancel: &cancel,
        };
        let report = export_pack(&config, &|_| {}, &mut |_| {}).unwrap();
        assert!(report.cancelled);
        assert!(!dest.exists());
    }

    #[test]
    fn the_catalog_lists_what_can_be_taken_and_what_is_ticked_by_default() {
        let temp = fresh_dir("packexport-catalog");
        let root = &*temp;
        let dir = make_instance(root, "1.20.1-Fabric");
        let entries = catalog(&dir, "1.20.1-Fabric");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        // 玩家自己的东西要列出来（想带走就自己勾），但默认不勾。
        assert!(names.contains(&"saves"), "实际是：{:?}", names);
        assert!(names.contains(&"logs"));
        // 启动器自己的东西干脆不列。
        assert!(!names.contains(&"libraries"));
        assert!(!names.contains(&"natives"));
        assert!(!names.iter().any(|n| n.ends_with(".json")));
        assert!(!names.iter().any(|n| n.ends_with(".jar")));

        let ticked = |name: &str| {
            entries
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.default_on)
                .unwrap_or(false)
        };
        assert!(ticked("mods"));
        assert!(ticked("config"));
        assert!(ticked("options.txt"));
        assert!(!ticked("saves"));
        assert!(!ticked("logs"));

        // 目录排在文件前面，并且带上自己的大小。
        let mods = entries.iter().find(|e| e.name == "mods").unwrap();
        assert!(mods.is_dir);
        assert_eq!(mods.files, 2);
        assert_eq!(mods.bytes, "mod-a".len() as u64 + "mod-b".len() as u64);
        assert!(entries[0].is_dir);
    }

    #[test]
    fn only_the_ticked_entries_go_into_the_pack() {
        let temp = fresh_dir("packexport-picked");
        let root = &*temp;
        let dir = make_instance(root, "1.20.1-Fabric");
        let dest = root.join("out.zip");
        // 只勾存档：模组和配置不该跟进去。
        run_with(
            root,
            "1.20.1-Fabric",
            ExportFormat::Hmcl,
            &dest,
            &["saves".to_string()],
        );
        let _ = dir;

        let names: Vec<String> = crate::natives::read_zip_entries(&dest)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.iter().any(|n| n == "minecraft/saves/world.dat"));
        assert!(!names.iter().any(|n| n.starts_with("minecraft/mods/")));
        assert!(!names.iter().any(|n| n == "minecraft/config/x.toml"));
        assert!(!names.iter().any(|n| n == "minecraft/options.txt"));
    }

    #[test]
    fn an_export_with_nothing_ticked_is_refused() {
        let temp = fresh_dir("packexport-empty-pick");
        let root = &*temp;
        make_instance(root, "1.20.1-Fabric");
        let cancel = CancelToken::new();
        let dest = root.join("out.zip");
        let config = ExportConfig {
            game_dir: root,
            instance: "1.20.1-Fabric",
            format: ExportFormat::Hmcl,
            name: "x".to_string(),
            version: "1".to_string(),
            dest: &dest,
            resolve_downloads: false,
            include: &[],
            cancel: &cancel,
        };
        let err = export_pack(&config, &|_| {}, &mut |_| {}).unwrap_err();
        assert!(err.contains("没勾"), "实际是：{}", err);
        assert!(!dest.exists());
    }

    #[test]
    fn pack_json_takes_the_game_version_as_its_jar_id() {
        let text = r#"{"id":"我的实例","jar":"我的实例","libraries":[]}"#;
        let out = pack_json_text(text, "1.20.1").unwrap();
        let value = json::parse(&out).unwrap();
        assert_eq!(value.get("jar").and_then(Json::as_str), Some("1.20.1"));
        // 别的字段原样留着
        assert_eq!(value.get("id").and_then(Json::as_str), Some("我的实例"));
    }
}
