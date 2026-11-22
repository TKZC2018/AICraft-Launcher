//! Java 运行时的自动下载与安装。
//!
//! 玩家装一个 1.20.1，清单里写着 `javaVersion.majorVersion: 17`，
//! 机器上只有 Java 8 —— 启动失败，报出来的是一句
//! `UnsupportedClassVersionError`，看不懂也猜不到解法。
//! 与其让玩家自己去 Oracle 网站挑，不如这里替他下一个。
//!
//! 下的不是 JDK 而是 **JRE**：启动器只需要能跑，不需要 javac。
//! 一个 JRE 解压后约 145 MB，JDK 要 300 MB 出头，差得不多但没必要。
//!
//! 来源是 Eclipse Temurin（Adoptium）：
//! - 官方 API 给「某个主版本的最新一版」的下载地址与 SHA-256，
//!   这是唯一能保证「拿到的包是作者发的那个」的依据。
//! - 官方包托管在 GitHub Releases，本机实测 165 KB/s —— 40 MB 要四分多钟。
//!   清华 TUNA 有同一个文件的镜像，实测 14 MB/s，所以**镜像优先、官方兜底**。
//!   TUNA 没有 API，只能按文件名拼路径，文件名正是 API 给的那个。
//!
//! 装到 `<数据目录>/java/<包名去后缀>/`，由 [`crate::java::candidate_paths`]
//! 一并扫到，所以装完不用改任何设置，下次挑 Java 时就看得见它。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::cancel::CancelToken;

/// Adoptium 的「某个主版本最新一版」查询接口。
pub const ADOPTIUM_API: &str = "https://api.adoptium.net/v3/assets/latest";

/// 清华 TUNA 的 Adoptium 镜像根。
///
/// 只有文件，没有 API —— 路径按 `{major}/jre/{arch}/windows/{文件名}` 拼，
/// 文件名来自 API，所以两者必须配着用。
pub const TUNA_MIRROR: &str = "https://mirrors.tuna.tsinghua.edu.cn/Adoptium";

/// 支持自动下载的主版本。
///
/// 8 是老版本（1.12.2 那一代）用的，25 是最新的 LTS。范围之外的不试——
/// Adoptium 只发这几个主流版本，问了也是空。
pub const SUPPORTED: [u32; 4] = [8, 17, 21, 25];

/// 一次下载的进度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// 正在问 Adoptium 要下载地址。
    Querying,
    /// 正在下 zip。`total` 为 0 表示不知道总量。
    Downloading { got: u64, total: u64 },
    /// 正在把 zip 解到目录里。
    Extracting { done: usize, total: usize },
    /// 正在核对解出来的 java 能不能用。
    Probing,
}

impl Stage {
    /// 给界面用的一句话。
    pub fn label(&self) -> String {
        match self {
            Stage::Querying => "正在查下载地址…".to_string(),
            Stage::Downloading { got, total } => {
                if *total > 0 {
                    format!("正在下载 Java：{} / {}", mb(*got), mb(*total))
                } else {
                    format!("正在下载 Java：{}", mb(*got))
                }
            }
            Stage::Extracting { done, total } => {
                format!("正在解压 Java：{} / {} 个文件", done, total)
            }
            Stage::Probing => "正在检查装好的 Java…".to_string(),
        }
    }
}

/// 人读的体量（MB，一位小数）。
fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
}

/// 一份可下载的 JRE 包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// 文件名，如 `OpenJDK21U-jre_x64_windows_hotspot_21.0.13_9.zip`。
    pub name: String,
    /// 官方下载地址（GitHub Releases）。
    pub url: String,
    /// 字节数，来自 API。
    pub size: u64,
    /// SHA-256，来自 API。下载完必须对得上。
    pub sha256: String,
    /// 完整版本串，如 `21.0.13+9.0.LTS`。
    pub version: String,
}

impl Package {
    /// 落盘用的目录名：去掉 `.zip` 的文件名。
    ///
    /// 不另起编号是因为同版本重装会命中已有的目录，直接跳过下载。
    pub fn dir_name(&self) -> String {
        self.name
            .strip_suffix(".zip")
            .unwrap_or(&self.name)
            .to_string()
    }
}

/// 本机架构在 Adoptium 里的写法。
fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        _ => "x64",
    }
}

/// 自动下载的 Java 装在哪。
///
/// 放在数据目录下而不是游戏目录：它是启动器的资产，不属于某一个实例。
/// 换游戏目录、删实例都不该把它带走。
pub fn managed_root() -> PathBuf {
    crate::settings::data_dir().join("java")
}

/// 问 Adoptium 要「`major` 这一支的最新一版 JRE（Windows）」。
///
/// 只给 Windows 的 x64/aarch64 —— 这个启动器本身就只有 Windows 构建。
pub fn find(major: u32) -> Result<Package, String> {
    let url = format!(
        "{}/{}/hotspot?architecture={}&image_type=jre&os=windows&vendor=eclipse",
        ADOPTIUM_API,
        major,
        arch()
    );
    let text = crate::transport::fetch_text(&url)
        .map_err(|e| format!("查 Java {} 的下载地址失败: {}", major, e))?;
    parse_package(&text)
}

/// 从 Adoptium 的响应里取出第一个包的地址与校验值。
pub fn parse_package(text: &str) -> Result<Package, String> {
    let root = crate::json::parse(text).map_err(|e| format!("下载地址不是合法 JSON: {}", e))?;
    let list = root.as_array().ok_or("下载地址不像 Adoptium 的响应")?;
    let first = list.first().ok_or_else(|| {
        "Adoptium 没有给这个版本发包（可能这个主版本的 JRE 已经不再发布）".to_string()
    })?;

    let binary = first.get("binary").ok_or("响应里没有 binary")?;
    let package = binary.get("package").ok_or("响应里没有 package")?;
    let version = first.get("version");

    let name = package
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("响应里没有文件名")?
        .to_string();
    let url = package
        .get("link")
        .and_then(|v| v.as_str())
        .ok_or("响应里没有下载地址")?
        .to_string();
    let sha256 = package
        .get("checksum")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if sha256.is_empty() {
        return Err("响应里没有 SHA-256，不敢下这种没校验的包".to_string());
    }

    Ok(Package {
        name,
        url,
        size: package.get("size").and_then(|v| v.as_i64()).unwrap_or(0) as u64,
        sha256,
        version: version
            .and_then(|v| v.get("semver"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// 拼一份镜像候选地址。
///
/// 清华镜像只托管文件、不提供 API，所以只能拿 API 给的文件名去拼。
/// 拼错了会 404，下一个候选（官方）会接手。
pub fn mirror_url(package: &Package, major: u32) -> String {
    format!(
        "{}/{}/jre/{}/windows/{}",
        TUNA_MIRROR,
        major,
        arch(),
        package.name
    )
}

/// 下载并安装一个 JRE，返回装好的运行时。
///
/// 步骤：查地址 → 下 zip（校验 SHA-256）→ 解压 → 试跑 `java -version`
/// 认版本。任何一步失败都会把半成品清掉，不留一个「看着像装好了」的目录。
///
/// 已经装过同一个包时直接复用（先试跑一遍确认它还能用）。
pub fn install(
    major: u32,
    cancel: &CancelToken,
    on_progress: Arc<dyn Fn(Stage) + Send + Sync>,
    on_log: &(dyn Fn(String) + Send + Sync),
) -> Result<crate::java::JavaRuntime, String> {
    if !SUPPORTED.contains(&major) {
        return Err(format!(
            "Java {} 不在自动下载的范围内（只有 {}）",
            major,
            SUPPORTED
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(" / ")
        ));
    }

    on_progress(Stage::Querying);
    let package = find(major)?;
    on_log(format!(
        "Java {} 最新一版是 {}（{}）",
        major,
        package.version,
        package.name
    ));

    let root = managed_root();
    let target = root.join(package.dir_name());

    // 已经装过：试跑一下，能跑就直接用，不必再下一遍 40 MB。
    if target.is_dir() {
        if let Some(rt) = probe_dir(&target) {
            on_log(format!(
                "{} 里已经有一份能用的 Java {}，直接用",
                target.display(),
                rt.major_version
            ));
            return Ok(rt);
        }
    }

    std::fs::create_dir_all(&root)
        .map_err(|e| format!("创建 {} 失败: {}", root.display(), e))?;

    let cache = root.join(".cache");
    std::fs::create_dir_all(&cache).map_err(|e| format!("创建缓存目录失败: {}", e))?;
    let zip = cache.join(&package.name);

    if zip.is_file() && !sha256_matches(&zip, &package.sha256) {
        on_log("缓存里的包校验对不上，删掉重下".to_string());
        let _ = std::fs::remove_file(&zip);
    }

    if !zip.is_file() {
        download(&package, major, &zip, cancel, &on_progress, on_log)?;
    }

    // 解压到临时目录，验完再改名。中断不会留下半个 JRE。
    let temp = root.join(format!(".{}.tmp", package.dir_name()));
    if temp.exists() {
        let _ = std::fs::remove_dir_all(&temp);
    }
    extract(&zip, &temp, &on_progress, cancel)?;

    on_progress(Stage::Probing);
    let runtime = match probe_dir(&temp) {
        Some(rt) => rt,
        None => {
            let _ = std::fs::remove_dir_all(&temp);
            return Err(format!(
                "解压完了但 {} 里找不到能跑的 java.exe",
                temp.display()
            ));
        }
    };
    if !runtime.satisfies(major) {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(format!(
            "下下来的 Java 是 {}，但需要 {} —— 可能只拿到了旧版本",
            runtime.major_version, major
        ));
    }

    if target.exists() {
        let _ = std::fs::remove_dir_all(&target);
    }
    std::fs::rename(&temp, &target)
        .map_err(|e| format!("把解好的 Java 搬到 {} 失败: {}", target.display(), e))?;

    // 包留着没用：下次要重装还是先问 API 要新地址。
    let _ = std::fs::remove_file(&zip);

    let mut runtime = runtime;
    runtime.path = target.join("bin").join(exe_name());
    on_log(format!(
        "Java {} 装好了：{}",
        runtime.major_version,
        target.display()
    ));
    Ok(runtime)
}

/// 目录里的 `bin/javaw.exe` 探测出来没有？
fn probe_dir(dir: &Path) -> Option<crate::java::JavaRuntime> {
    let exe = dir.join("bin").join(exe_name());
    crate::java::probe_java(&exe)
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "javaw.exe"
    } else {
        "java"
    }
}

fn sha256_matches(path: &Path, want: &str) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut stream = crate::sha256::Sha256Stream::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => stream.update(&buf[..n]),
            Err(_) => return false,
        }
    }
    stream.finish_hex().eq_ignore_ascii_case(want)
}

/// 下 zip：镜像先试，官方兜底，下完校验 SHA-256。
fn download(
    package: &Package,
    major: u32,
    dest: &Path,
    cancel: &CancelToken,
    on_progress: &Arc<dyn Fn(Stage) + Send + Sync>,
    on_log: &(dyn Fn(String) + Send + Sync),
) -> Result<(), String> {
    let candidates = [mirror_url(package, major), package.url.clone()];
    let mut last = String::new();
    for (index, url) in candidates.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err("已取消".to_string());
        }
        let label = if index == 0 { "镜像" } else { "官方" };
        on_log(format!("从{}下载 {}", label, package.name));
        match download_one(url, dest, package.size, cancel, on_progress) {
            Ok(()) => {
                if sha256_matches(dest, &package.sha256) {
                    on_log(format!("{} 下好了，校验通过", package.name));
                    return Ok(());
                }
                let _ = std::fs::remove_file(dest);
                last = format!("{} 的 SHA-256 对不上", label);
                on_log(format!("{}，换下一个地址", last));
            }
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err("已取消".to_string());
                }
                last = format!("{}下载失败: {}", label, e);
                on_log(last.clone());
            }
        }
    }
    Err(format!("Java {} 没下下来（{}）", major, last))
}

/// 单条下载 + 进度回报。
///
/// `transport` 只在最后给一次总字节，而解压前那 40 MB 是这整件事里
/// 唯一需要玩家等的部分 —— 所以另起一个线程，每 200 ms 看一眼 `.part`
/// 长到哪了，把数字报给界面。
fn download_one(
    url: &str,
    dest: &Path,
    total_hint: u64,
    cancel: &CancelToken,
    on_progress: &Arc<dyn Fn(Stage) + Send + Sync>,
) -> Result<(), String> {
    let part = part_path(dest);
    let done = Arc::new(AtomicBool::new(false));
    let watcher_done = Arc::clone(&done);
    let part_for_watcher = part.clone();
    let reporter_progress = Arc::clone(on_progress);
    let reporter = std::thread::spawn(move || {
        let mut last = u64::MAX;
        while !watcher_done.load(Ordering::Relaxed) {
            let got = std::fs::metadata(&part_for_watcher)
                .map(|m| m.len())
                .unwrap_or(0);
            if got != last {
                reporter_progress(Stage::Downloading {
                    got,
                    total: total_hint,
                });
                last = got;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });

    let result = crate::transport::download_with_cancel(url, dest, None, None, cancel);
    done.store(true, Ordering::Relaxed);
    let _ = reporter.join();

    result
        .map(|_| ())
        .map_err(|e| format!("下载 {} 失败: {}", url, e))
}

/// `download_with_cancel` 的临时文件名规则，和它保持一致。
fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

/// 解 zip 到目录，去掉那一层顶层文件夹。
///
/// Temurin 的包解开是 `jdk-21.0.13+9-jre/bin/java.exe` 这样的结构，
/// 顶层那个文件夹名带版本号；留着会让路径变成
/// `java/<包名>/jdk-21.0.13+9-jre/bin/javaw.exe`，多一层毫无意义。
fn extract(
    zip: &Path,
    dir: &Path,
    on_progress: &Arc<dyn Fn(Stage) + Send + Sync>,
    cancel: &CancelToken,
) -> Result<(), String> {
    let entries = crate::natives::read_zip_entries(zip)
        .map_err(|e| format!("读 {} 失败: {}", zip.display(), e))?;
    let prefix = common_prefix(&entries);
    let files: Vec<_> = entries.iter().filter(|e| !e.is_dir).collect();
    let total = files.len();
    std::fs::create_dir_all(dir).map_err(|e| format!("创建 {} 失败: {}", dir.display(), e))?;

    for (index, entry) in files.iter().enumerate() {
        if cancel.is_cancelled() {
            let _ = std::fs::remove_dir_all(dir);
            return Err("已取消".to_string());
        }
        let Some(rel) = strip_prefix(&entry.name, &prefix) else {
            continue;
        };
        // 防路径穿越：ZIP 里的名字是外部数据，不能直接当路径用。
        let Some(safe) = crate::modpack::safe_relative(rel) else {
            continue;
        };
        let target = dir.join(&safe);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建 {} 失败: {}", parent.display(), e))?;
        }
        let data = entry
            .read_data(zip)
            .map_err(|e| format!("解压 {} 失败: {}", entry.name, e))?;
        let mut file =
            std::fs::File::create(&target).map_err(|e| format!("写 {} 失败: {}", target.display(), e))?;
        file.write_all(&data)
            .map_err(|e| format!("写 {} 失败: {}", target.display(), e))?;

        // 每 32 个报一次：一千多个小文件，逐条刷界面反而看不清。
        if index % 32 == 0 {
            on_progress(Stage::Extracting {
                done: index + 1,
                total,
            });
        }
    }
    on_progress(Stage::Extracting { done: total, total });
    Ok(())
}

/// 所有条目共有的第一层目录名（没有就返回空串）。
fn common_prefix(entries: &[crate::natives::ZipEntry]) -> String {
    let mut first: Option<String> = None;
    for entry in entries {
        let Some(head) = entry.name.split('/').next() else {
            continue;
        };
        if head.is_empty() {
            continue;
        }
        match &first {
            None => first = Some(head.to_string()),
            Some(known) if known != head => return String::new(),
            Some(_) => {}
        }
    }
    match first {
        // 只有一层文件（`bin/java.exe` 这种）说明没有外层文件夹，
        // 那 `bin` 本身就是内容，不能当壳剥掉。
        Some(head) if entries.iter().all(|e| e.name.starts_with(&format!("{}/", head))) => head,
        _ => String::new(),
    }
}

/// 把 `prefix/` 从条目名前面剥掉。
fn strip_prefix<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return Some(name);
    }
    name.strip_prefix(prefix)?.strip_prefix('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    // 结构照抄真实响应：`binary` 与 `version` 是同一个元素的**兄弟**字段，
    // 不是把 `version` 塞进 `binary` 里。
    const SAMPLE: &str = concat!(
        "[{\"binary\":{\"package\":{",
        "\"checksum\":\"2332c0b4cff6bed722da3567c91d5e47568bcba863fa4945d0a8d6fc76158628\",",
        "\"link\":\"https://github.com/adoptium/temurin21-binaries/releases/download/",
        "jdk-21.0.13%2B9/OpenJDK21U-jre_x64_windows_hotspot_21.0.13_9.zip\",",
        "\"name\":\"OpenJDK21U-jre_x64_windows_hotspot_21.0.13_9.zip\",",
        "\"size\":49106397}},",
        "\"version\":{\"semver\":\"21.0.13+9.0.LTS\",\"major\":21}}]"
    );

    #[test]
    fn a_package_reads_what_the_installer_needs() {
        let package = parse_package(SAMPLE).unwrap();
        assert_eq!(
            package.name,
            "OpenJDK21U-jre_x64_windows_hotspot_21.0.13_9.zip"
        );
        assert_eq!(package.size, 49_106_397);
        assert_eq!(package.version, "21.0.13+9.0.LTS");
        assert!(package.url.starts_with("https://github.com/adoptium/"));
        assert_eq!(package.sha256.len(), 64);
    }

    #[test]
    fn a_package_without_a_checksum_is_refused() {
        // 没有 SHA-256 就没法确认拿到的是不是作者发的那个包。
        let text = SAMPLE.replace(
            "\"checksum\":\"2332c0b4cff6bed722da3567c91d5e47568bcba863fa4945d0a8d6fc76158628\",",
            "",
        );
        let err = parse_package(&text).unwrap_err();
        assert!(err.contains("SHA-256"), "实际得到：{}", err);
    }

    #[test]
    fn an_empty_release_list_says_so() {
        let err = parse_package("[]").unwrap_err();
        assert!(err.contains("没有给这个版本发包"), "实际得到：{}", err);
    }

    #[test]
    fn the_mirror_url_reuses_the_api_file_name() {
        let package = parse_package(SAMPLE).unwrap();
        let url = mirror_url(&package, 21);
        assert!(url.starts_with(TUNA_MIRROR), "实际得到：{}", url);
        assert!(url.contains("/21/jre/"), "实际得到：{}", url);
        assert!(url.ends_with(&package.name), "实际得到：{}", url);
    }

    fn entry(name: &str) -> crate::natives::ZipEntry {
        crate::natives::ZipEntry {
            name: name.to_string(),
            compression: 0,
            compressed_size: 0,
            uncompressed_size: 0,
            local_header_offset: 0,
            is_dir: name.ends_with('/'),
        }
    }

    #[test]
    fn the_wrapping_folder_is_detected() {
        let entries = vec![
            entry("jdk-21.0.13+9-jre/"),
            entry("jdk-21.0.13+9-jre/bin/"),
            entry("jdk-21.0.13+9-jre/bin/javaw.exe"),
            entry("jdk-21.0.13+9-jre/release"),
        ];
        assert_eq!(common_prefix(&entries), "jdk-21.0.13+9-jre");
        assert_eq!(
            strip_prefix("jdk-21.0.13+9-jre/bin/javaw.exe", "jdk-21.0.13+9-jre"),
            Some("bin/javaw.exe")
        );
    }

    #[test]
    fn a_flat_archive_is_left_alone() {
        // 顶层就是 bin/ 与 lib/ 时，那两个是内容不是壳。
        let entries = vec![
            entry("bin/javaw.exe"),
            entry("lib/modules"),
        ];
        assert_eq!(common_prefix(&entries), "");
        assert_eq!(strip_prefix("bin/javaw.exe", ""), Some("bin/javaw.exe"));
    }

    #[test]
    fn a_version_outside_the_supported_range_is_refused_before_any_network() {
        let cancel = CancelToken::new();
        let err = install(11, &cancel, Arc::new(|_| {}), &|_| {}).unwrap_err();
        assert!(err.contains("不在自动下载的范围内"), "实际得到：{}", err);
    }
}
