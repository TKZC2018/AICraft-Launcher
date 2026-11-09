//! 服务端管理：下载服务端核心、生成启动脚本、编辑 `server.properties`。
//!
//! 启动器自己**不**常驻服务端进程（那要接管 stdout、处理 Ctrl+C、做终端渲染，
//! 完全是另一套东西）。这一层只负责把一份能跑的服务端摆到磁盘上：
//! 核心 jar、`eula.txt`、`server.properties`、以及一个 `start.bat`。
//! 玩家双击脚本就能开服，控制台归他自己的终端窗口。
//!
//! 四种核心的落地方式差别很大：
//!
//! - **原版**：Mojang 的版本 JSON 里 `downloads.server` 直接给 jar，一步到位。
//! - **Fabric**：跑官方安装器的 `server` 子命令（`-downloadMinecraft` 会连原版
//!   `server.jar` 一起下），产出 `fabric-server-launch.jar`。
//! - **Forge / NeoForge**：跑各自的 `--installServer`，产出 `run.bat` 与
//!   `libraries/`；启动脚本转手调它。

use std::path::{Path, PathBuf};

use crate::json::{self, Json};

/// 服务端都放在数据目录下的这个子目录里，一个服务端一个文件夹。
pub const ROOT_DIR: &str = "servers";

/// 我们自己写的元数据文件名（原版安装器不会产这个）。
pub const MARKER: &str = "aicraft-server.json";

// ---------------------------------------------------------------- 核心种类

/// 服务端核心。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CoreKind {
    /// Mojang 官方 `server.jar`。
    #[default]
    Vanilla,
    /// Fabric 服务端（安装器产出 `fabric-server-launch.jar`）。
    Fabric,
    /// Forge 服务端。
    Forge,
    /// NeoForge 服务端。
    NeoForge,
    /// Velocity 代理端。
    ///
    /// 它不跑 Minecraft 世界，只把玩家转给后面的子服，所以没有「游戏版本」这一说。
    /// 我们也**不装**它（代理端的配置体系完全是另一套），只负责把它认出来，
    /// 让玩家能在列表里看到、能改 `velocity.toml`。
    Velocity,
}

impl CoreKind {
    /// 我们能**装**的四种。新建服务端时只列这些。
    pub const ALL: [CoreKind; 4] = [
        CoreKind::Vanilla,
        CoreKind::Fabric,
        CoreKind::Forge,
        CoreKind::NeoForge,
    ];

    /// 我们能**认出来**的五种。比 [`Self::ALL`] 多一个 Velocity——代理端只识别、
    /// 不安装，所以不进新建流程的选项里。
    pub const DETECTABLE: [CoreKind; 5] = [
        CoreKind::Vanilla,
        CoreKind::Fabric,
        CoreKind::Forge,
        CoreKind::NeoForge,
        CoreKind::Velocity,
    ];

    pub fn id(self) -> &'static str {
        match self {
            CoreKind::Vanilla => "vanilla",
            CoreKind::Fabric => "fabric",
            CoreKind::Forge => "forge",
            CoreKind::NeoForge => "neoforge",
            CoreKind::Velocity => "velocity",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CoreKind::Vanilla => "原版",
            CoreKind::Fabric => "Fabric",
            CoreKind::Forge => "Forge",
            CoreKind::NeoForge => "NeoForge",
            CoreKind::Velocity => "Velocity",
        }
    }

    pub fn from_id(id: &str) -> Option<CoreKind> {
        CoreKind::DETECTABLE
            .into_iter()
            .find(|kind| kind.id() == id)
    }

    /// 这一种是不是还要再选一个加载器版本号。
    ///
    /// 原版没有加载器这一层，版本号就是游戏版本本身。
    pub fn needs_loader_version(self) -> bool {
        // 原版没有加载器这一层；Velocity 的版本号就是它自己（`3.4.0`），
        // 也没有「游戏版本」可以选。
        matches!(self, CoreKind::Fabric | CoreKind::Forge | CoreKind::NeoForge)
    }

    /// 这一种核心对应哪个加载器（原版没有）。
    pub fn loader_kind(self) -> crate::loader::LoaderKind {
        match self {
            CoreKind::Vanilla | CoreKind::Fabric => crate::loader::LoaderKind::Fabric,
            CoreKind::Forge => crate::loader::LoaderKind::Forge,
            CoreKind::NeoForge => crate::loader::LoaderKind::NeoForge,
            // Velocity 没有对应的加载器；它不会被问到（`needs_loader_version`
            // 已经是 false），这里只是把 match 补全。
            CoreKind::Velocity => crate::loader::LoaderKind::Fabric,
        }
    }

    /// 装完之后由它来认领这个目录——判断「装没装成」的唯一依据之一。
    pub fn entry_jar(self) -> &'static str {
        match self {
            CoreKind::Vanilla => "server.jar",
            CoreKind::Fabric => "fabric-server-launch.jar",
            CoreKind::Forge => "run.bat",
            CoreKind::NeoForge => "run.bat",
            // 我们不会去装 Velocity，这个值只在「认领目录」时被问到。
            CoreKind::Velocity => "velocity.jar",
        }
    }
}

// ---------------------------------------------------------------- 一个服务端

/// 磁盘上的一个服务端。
#[derive(Debug, Clone)]
pub struct Server {
    pub name: String,
    pub dir: PathBuf,
    pub core: CoreKind,
    pub mc_version: String,
    /// 加载器版本；原版为空串。
    pub core_version: String,
    /// 玩家是否已经点了同意 EULA（`eula.txt` 里写的是不是 true）。
    pub eula: bool,
    /// 是不是我们装出来的（有 [`MARKER`]）。从别处搬来的目录是 false——
    /// 那种目录只能「移出列表」，不能由我们删。
    pub managed: bool,
}

impl Server {
    /// 「原版 26.3」/「Fabric 0.20.1 · 26.3」这种一行摘要。
    pub fn summary(&self) -> String {
        // 从别处搬来的目录可能读不出版本号（Purpur / Paper 这类分支的 jar
        // 里没有 version.json），那就老实说不知道，别把空字符串拼出来。
        let game = if self.mc_version.is_empty() {
            "版本未知".to_string()
        } else {
            self.mc_version.clone()
        };
        if self.core == CoreKind::Velocity {
            // 代理端没有游戏版本，别拼出「Velocity 版本未知」这种话
            return if self.core_version.is_empty() {
                self.core.label().to_string()
            } else {
                format!("{} {}", self.core.label(), self.core_version)
            };
        }
        if self.core.needs_loader_version() {
            if self.core_version.is_empty() {
                format!("{} · {}", self.core.label(), game)
            } else {
                format!("{} {} · {}", self.core.label(), self.core_version, game)
            }
        } else {
            format!("{} {}", self.core.label(), game)
        }
    }

    pub fn properties_path(&self) -> PathBuf {
        self.dir.join("server.properties")
    }

    pub fn eula_path(&self) -> PathBuf {
        self.dir.join("eula.txt")
    }

    pub fn start_script(&self) -> PathBuf {
        self.dir.join("start.bat")
    }
}

/// 服务端根目录（`<数据目录>/servers`）。
///
/// 和游戏目录分开：服务端动辄几十上百 MB，而且是「一台服」而不是「一个实例」，
/// 塞进 `.minecraft/versions` 里既不合语义，也会被版本列表当成一个游戏版本。
pub fn root() -> PathBuf {
    crate::settings::data_dir().join(ROOT_DIR)
}

/// 扫出所有服务端，按名字排序。
///
/// 只认带 [`MARKER`] 的目录：`servers/` 下可能有玩家自己放的东西，
/// 或者装到一半失败留下的半成品，那些不该出现在列表里。
pub fn list() -> Vec<Server> {
    list_with(&[])
}

/// 同上，另外把玩家自己加进来的目录也算上。
///
/// 那些目录不归我们管：可能是在别处开了一年多的老服，整个文件夹搬过来
/// 接着用。它们没有 [`MARKER`]，靠 [`probe`] 现场认核心。
pub fn list_with(extra: &[PathBuf]) -> Vec<Server> {
    let mut out: Vec<Server> = std::fs::read_dir(root())
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .filter_map(|entry| read(&entry.path()))
                .collect()
        })
        .unwrap_or_default();
    for dir in extra {
        let Some(server) = probe(dir) else {
            continue;
        };
        // `servers/` 下那份已经算过了，别重复列一遍
        if out.iter().any(|other| other.dir == server.dir) {
            continue;
        }
        out.push(server);
    }
    out.sort_by_key(|server| server.name.to_lowercase());
    out
}

/// 读单个目录的服务端元数据。
pub fn read(dir: &Path) -> Option<Server> {
    let text = std::fs::read_to_string(dir.join(MARKER)).ok()?;
    let root = json::parse(&text).ok()?;
    let name = root
        .get("name")
        .and_then(Json::as_str)
        .map(str::to_string)
        .or_else(|| dir.file_name()?.to_str().map(str::to_string))?;
    let core = root
        .get("core")
        .and_then(Json::as_str)
        .and_then(CoreKind::from_id)?;
    Some(Server {
        eula: eula_accepted(dir),
        managed: true,
        name,
        dir: dir.to_path_buf(),
        core,
        mc_version: root
            .get("mc_version")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string(),
        core_version: root
            .get("core_version")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

// ---------------------------------------------------------------- 认核心

/// 从任意目录里认出这是个什么服务端。
///
/// 玩家可能把一个开了很久的老服整个文件夹搬过来，那种目录里没有我们的
/// [`MARKER`]，只能照 Minecraft 自己的布局来认：
///
/// | 看到什么 | 认成 |
/// |---|---|
/// | `libraries/net/neoforged/` | NeoForge |
/// | `libraries/net/minecraftforge/` | Forge |
/// | `libraries/net/fabricmc/fabric-loader/` 或 `fabric-server-launch.jar` | Fabric |
/// | 能读出 `version.json` 的 `server.jar` | 原版 |
///
/// 顺序有讲究：Forge 和 NeoForge 的目录里**也**有一份 `server.jar`（它们
/// 自己下下来喂给安装器的），先看加载器目录才不会把 Forge 服认成原版。
pub fn probe(dir: &Path) -> Option<Server> {
    if !dir.is_dir() {
        return None;
    }
    // 我们装的那些直接读元数据，省得再猜一遍
    if let Some(mut server) = read(dir) {
        server.managed = true;
        return Some(server);
    }

    // 代理端先认：它的 jar 里没有 `version.json`，落到下面会被当成
    // 「原版 · 版本未知」，玩家看到「原版」会以为这是个能直接开的服。
    if let Some(proxy) = probe_proxy(dir) {
        return Some(proxy);
    }

    let jar = pick_server_jar(dir);
    let mc_version = jar
        .as_deref()
        .and_then(jar_version)
        .unwrap_or_default();

    let (core, core_version) = if let Some(version) = library_version(dir, "net/neoforged/neoforge") {
        (CoreKind::NeoForge, version)
    } else if let Some(version) = library_version(dir, "net/minecraftforge/forge") {
        // Forge 的目录名是 `<mc>-<forge>`，我们只要后半截
        let forge = version
            .split_once('-')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or(version);
        (CoreKind::Forge, forge)
    } else if let Some(version) = library_version(dir, "net/fabricmc/fabric-loader") {
        (CoreKind::Fabric, version)
    } else if dir.join("fabric-server-launch.jar").exists() {
        (CoreKind::Fabric, String::new())
    } else if jar.is_some() && !mc_version.is_empty() {
        (CoreKind::Vanilla, String::new())
    } else {
        // 有 jar 但读不出版本号：可能是 Purpur / Paper 这类分支，或者是
        // 我们完全不认识的东西。认成原版但留个空版本号，界面上会显示
        // 「原版 · 版本未知」，玩家仍然能配置和开服。
        let any_jar = jar.is_some() || has_any_jar(dir);
        if !any_jar {
            return None;
        }
        (CoreKind::Vanilla, String::new())
    };

    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("server")
        .to_string();
    Some(Server {
        eula: eula_accepted(dir),
        managed: false,
        name,
        dir: dir.to_path_buf(),
        core,
        mc_version,
        core_version,
    })
}

/// 认代理端（目前只有 Velocity）。
///
/// 判据是 jar 里 `META-INF/MANIFEST.MF` 的 `Implementation-Title`——Velocity 的
/// 清单里写着 `Implementation-Title: Velocity`、`Main-Class: com.velocitypowered.proxy.Velocity`，
/// 而它**没有** `version.json`，所以光看 jar 内容分不出来。
fn probe_proxy(dir: &Path) -> Option<Server> {
    let mut jars: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
        })
        .collect();
    jars.sort();
    // 名字里带 velocity 的先试，省得每个 jar 都解一遍 MANIFEST。
    jars.sort_by_key(|path| {
        let named = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_ascii_lowercase().contains("velocity"));
        !named
    });
    let (jar, version) = jars
        .into_iter()
        .find_map(|jar| proxy_version(&jar).map(|version| (jar, version)))?;
    let _ = jar;
    Some(Server {
        eula: eula_accepted(dir),
        managed: false,
        name: dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("proxy")
            .to_string(),
        dir: dir.to_path_buf(),
        core: CoreKind::Velocity,
        // 代理端不绑游戏版本，这一栏留空（摘要里也不会显示）。
        mc_version: String::new(),
        core_version: version,
    })
}

/// 从 jar 的 `META-INF/MANIFEST.MF` 里认代理端，认得出来就返回它的版本号。
///
/// 只认 `Implementation-Title: Velocity`（大小写不敏感、允许行尾空格），
/// 别的代理端（BungeeCord / Waterfall）将来要支持就加在这里。
fn proxy_version(jar: &Path) -> Option<String> {
    let text = first_local_entry(jar, "META-INF/MANIFEST.MF")
        .or_else(|| {
            // 清单通常不在第一个位置，正规读一遍
            let entries = crate::natives::read_zip_entries(jar).ok()?;
            let entry = entries
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case("META-INF/MANIFEST.MF"))?;
            entry.read_data(jar).ok()
        })?;
    let text = String::from_utf8_lossy(&text);
    let mut title = "";
    let mut version = "";
    for line in text.lines() {
        // 清单是 `Key: Value`，续行以单个空格开头（Velocity 没用到续行）
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "implementation-title" => title = value,
            "implementation-version" => version = value,
            _ => {}
        }
    }
    if !title.eq_ignore_ascii_case("velocity") {
        return None;
    }
    // `3.4.0-SNAPSHOT (git-e9940713-b522)` → `3.4.0-SNAPSHOT`
    let version = version
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    Some(version)
}

/// 这个目录里像「服务端核心」的那个 jar。
///
/// 先认标准名字，再退到「顶层随便哪个 jar」——Purpur、Paper、Leaves 这些
/// 分支的 jar 都带着自己的名字（`purpur-1.20.4-2176.jar`），但它们里面
/// 一样有 `version.json`，认得出来。
fn pick_server_jar(dir: &Path) -> Option<PathBuf> {
    for name in ["server.jar", "minecraft_server.jar", "fabric-server-launch.jar"] {
        let path = dir.join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    let mut jars: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
        })
        .collect();
    jars.sort();
    jars.into_iter().next()
}

/// 顶层有没有任何 jar（用于判断「这是不是个服务端目录」）。
fn has_any_jar(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry.path().is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
            })
        })
        .unwrap_or(false)
}

/// 从 `libraries/<group>/<artifact>/<version>/` 里读加载器版本。
fn library_version(dir: &Path, group: &str) -> Option<String> {
    let base = dir.join("libraries").join(group.replace('/', std::path::MAIN_SEPARATOR_STR));
    let mut versions: Vec<String> = std::fs::read_dir(base)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect();
    // 同一个加载器常常留着好几个版本，挑最新的那个（按数字段比，别按字典序，
    // 否则 0.9 会排在 0.16 后面）
    versions.sort_by(|a, b| compare_versions(a, b));
    versions.pop()
}

/// 按数字段比版本号；比不出来就退回字典序。
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |text: &str| -> Vec<u64> {
        text.split(|c: char| !c.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .filter_map(|part| part.parse::<u64>().ok())
            .collect()
    };
    let (left, right) = (split(a), split(b));
    for (x, y) in left.iter().zip(right.iter()) {
        match x.cmp(y) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    left.len().cmp(&right.len()).then_with(|| a.cmp(b))
}

/// 从 jar 里读 `version.json` 的 `id`——Minecraft 服务端 jar 自己就写着版本号。
///
/// 两个来源都试：正常的 ZIP（有中央目录）走 [`crate::natives::read_zip_entries`]，
/// 而 Mojang 现在的服务端 jar 是个「打包器」，可能读不到中央目录，这时直接读
/// 头一个本地文件头——`version.json` 恰好总是第一个。
fn jar_version(jar: &Path) -> Option<String> {
    if let Ok(entries) = crate::natives::read_zip_entries(jar) {
        if let Some(entry) = entries.iter().find(|entry| entry.name == "version.json") {
            if let Ok(data) = entry.read_data(jar) {
                if let Some(id) = version_id(&data) {
                    return Some(id);
                }
            }
        }
    }
    first_local_entry(jar, "version.json").and_then(|data| version_id(&data))
}

fn version_id(data: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(data).ok()?;
    let root = json::parse(text).ok()?;
    root.get("id").and_then(Json::as_str).map(str::to_string)
}

/// 直接读第一个本地文件头；名字对得上才解压。
fn first_local_entry(jar: &Path, want: &str) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(jar).ok()?;
    let mut header = [0u8; 30];
    file.read_exact(&mut header).ok()?;
    if &header[0..4] != b"PK\x03\x04" {
        return None;
    }
    let name_len = u16::from_le_bytes([header[26], header[27]]) as u64;
    let extra_len = u16::from_le_bytes([header[28], header[29]]) as u64;
    let method = u16::from_le_bytes([header[8], header[9]]);
    let compressed_size = u32::from_le_bytes([header[18], header[19], header[20], header[21]]) as u64;
    let uncompressed_size = u32::from_le_bytes([header[22], header[23], header[24], header[25]]) as u64;

    let mut name = vec![0u8; name_len as usize];
    file.read_exact(&mut name).ok()?;
    if std::str::from_utf8(&name).ok()? != want {
        return None;
    }
    // 大小写 0 且标志位带 bit3 时，真实长度在数据描述符里——那种情况直接放弃，
    // 正常的服务端 jar 不会这么写。
    if compressed_size == 0 && uncompressed_size == 0 {
        return None;
    }
    file.seek(SeekFrom::Current(extra_len as i64)).ok()?;
    let mut compressed = vec![0u8; compressed_size as usize];
    file.read_exact(&mut compressed).ok()?;
    match method {
        0 => Some(compressed),
        8 => crate::natives::deflate::inflate(&compressed, uncompressed_size as usize).ok(),
        _ => None,
    }
}

/// `eula.txt` 里写的是不是 `eula=true`。
pub fn eula_accepted(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("eula.txt")) else {
        return false;
    };
    text.lines().any(|line| {
        let line = line.trim();
        !line.starts_with('#')
            && line
                .split_once('=')
                .is_some_and(|(k, v)| k.trim() == "eula" && v.trim().eq_ignore_ascii_case("true"))
    })
}

/// 写 `eula.txt`。
///
/// 这个文件只能由玩家自己点同意来生成——我们不替他按下那个按钮，
/// 界面上必须原样说明「同意的是 Mojang 的 EULA」。
pub fn set_eula(dir: &Path, accepted: bool) -> Result<(), String> {
    let path = dir.join("eula.txt");
    let text = if accepted {
        "# 由玩家在 AICraft 启动器里点了「同意」\n# https://aka.ms/MinecraftEULA\neula=true\n"
    } else {
        "# 未同意\neula=false\n"
    };
    std::fs::write(&path, text).map_err(|e| format!("写 {} 失败: {}", path.display(), e))
}

// ---------------------------------------------------------------- 服务器属性

/// `server.properties` 里的一行。
///
/// 顺序要保住：Minecraft 自己是按键名重新排的，但玩家看的是我们自己写的这份，
/// 保持原顺序比按字母排更像「他刚才那份文件」。
#[derive(Debug, Clone)]
pub struct PropLine {
    pub key: String,
    pub value: String,
}

/// `server.properties` 的解析结果。
#[derive(Debug, Clone, Default)]
pub struct Properties {
    pub lines: Vec<PropLine>,
}

impl Properties {
    /// 解析。注释行与空行**丢弃**——保存时重写，Minecraft 自己也会重新生成注释。
    pub fn parse(text: &str) -> Properties {
        let mut lines = Vec::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            lines.push(PropLine {
                key: key.trim().to_string(),
                value: value.trim().to_string(),
            });
        }
        Properties { lines }
    }

    pub fn read(path: &Path) -> Properties {
        std::fs::read_to_string(path)
            .map(|text| Properties::parse(&text))
            .unwrap_or_default()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines
            .iter()
            .find(|line| line.key == key)
            .map(|line| line.value.as_str())
    }

    /// 改一个键；没有就追加。返回是否真的变了（界面据此决定要不要落盘）。
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        if let Some(line) = self.lines.iter_mut().find(|line| line.key == key) {
            if line.value == value {
                return false;
            }
            line.value = value.to_string();
            return true;
        }
        self.lines.push(PropLine {
            key: key.to_string(),
            value: value.to_string(),
        });
        true
    }

    /// 渲染成文本。Minecraft 读的时候不在意顺序与注释。
    pub fn render(&self) -> String {
        let mut out = String::from("# Minecraft server properties\n# 由 AICraft 启动器编辑\n");
        for line in &self.lines {
            out.push_str(&line.key);
            out.push('=');
            out.push_str(&line.value);
            out.push('\n');
        }
        out
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        std::fs::write(path, self.render())
            .map_err(|e| format!("写 {} 失败: {}", path.display(), e))
    }
}

/// 一个配置项怎么编辑。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// 开关，值是 `true` / `false`。
    Switch,
    /// 整数。
    Number,
    /// 从固定几个值里挑。
    Choice(&'static [&'static str]),
    /// 自由文本。
    Text,
}

/// 界面上要画的一个配置项。
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub kind: FieldKind,
    /// 文件里没有这个键时用的默认值（和 Minecraft 自己的默认一致）。
    pub default: &'static str,
}

/// 界面上列出的常用项。
///
/// **不是全部**：`server.properties` 有六十来个键，全列出来等于让玩家翻说明书。
/// 这里只挑开服第一小时一定会碰的；剩下的他要改，用「打开文件夹」直接编辑文件。
pub const FIELDS: [Field; 16] = [
    Field {
        key: "motd",
        label: "服务器描述",
        hint: "多人列表里显示的那一行字。",
        kind: FieldKind::Text,
        default: "A Minecraft Server",
    },
    Field {
        key: "server-port",
        label: "端口",
        hint: "默认 25565。同一台机器开两个服要改成不同的号。",
        kind: FieldKind::Number,
        default: "25565",
    },
    Field {
        key: "max-players",
        label: "最大人数",
        hint: "同时在线的玩家上限。",
        kind: FieldKind::Number,
        default: "20",
    },
    Field {
        key: "gamemode",
        label: "默认模式",
        hint: "新玩家第一次进来时的模式。",
        kind: FieldKind::Choice(&["survival", "creative", "adventure", "spectator"]),
        default: "survival",
    },
    Field {
        key: "difficulty",
        label: "难度",
        hint: "和平模式下怪物不会生成。",
        kind: FieldKind::Choice(&["peaceful", "easy", "normal", "hard"]),
        default: "easy",
    },
    Field {
        key: "online-mode",
        label: "正版验证",
        hint: "关掉之后离线账号也能进；不玩正版服的自己人局通常关掉。",
        kind: FieldKind::Switch,
        default: "true",
    },
    Field {
        key: "pvp",
        label: "允许 PVP",
        hint: "玩家之间能不能互相伤害。",
        kind: FieldKind::Switch,
        default: "true",
    },
    Field {
        key: "level-name",
        label: "存档名",
        hint: "服务器存档所在目录的名字。",
        kind: FieldKind::Text,
        default: "world",
    },
    Field {
        key: "level-seed",
        label: "地图种子",
        hint: "留空就是随机；只有存档还没生成时才有意义。",
        kind: FieldKind::Text,
        default: "",
    },
    Field {
        key: "view-distance",
        label: "视距",
        hint: "每个玩家能看多远（区块）。开大很吃内存。",
        kind: FieldKind::Number,
        default: "10",
    },
    Field {
        key: "simulation-distance",
        label: "模拟距离",
        hint: "多远的生物和方块还会动（区块）。",
        kind: FieldKind::Number,
        default: "10",
    },
    Field {
        key: "white-list",
        label: "白名单",
        hint: "只允许名单里的玩家进来。名单本身在 white-list.txt。",
        kind: FieldKind::Switch,
        default: "false",
    },
    Field {
        key: "enable-command-block",
        label: "命令方块",
        hint: "允许使用命令方块。",
        kind: FieldKind::Switch,
        default: "false",
    },
    Field {
        key: "allow-flight",
        label: "允许飞行",
        hint: "关着时，飞起来的玩家会被判定为作弊踢掉。",
        kind: FieldKind::Switch,
        default: "false",
    },
    Field {
        key: "spawn-protection",
        label: "出生点保护",
        hint: "出生点周围多少格内只有管理员能改方块，0 表示关掉。",
        kind: FieldKind::Number,
        default: "16",
    },
    Field {
        key: "player-idle-timeout",
        label: "挂机踢出",
        hint: "多少分钟不动就踢下线，0 表示不踢。",
        kind: FieldKind::Number,
        default: "0",
    },
];

// ---------------------------------------------------------------- 名字校验

/// 服务端文件夹名是否可用。
pub fn is_valid_name(name: &str) -> bool {
    crate::install::is_valid_instance_name(name)
}

/// 这个名字是不是已经被占了。
pub fn name_taken(name: &str) -> bool {
    root().join(name).exists()
}

// ---------------------------------------------------------------- 安装

/// 安装进度。
#[derive(Debug, Clone)]
pub enum Progress {
    /// 正在做什么。
    Stage(String),
}

/// 一次安装的全部输入。
#[derive(Debug, Clone)]
pub struct Plan {
    pub name: String,
    pub core: CoreKind,
    pub mc_version: String,
    /// 加载器版本；原版忽略。
    pub core_version: String,
    /// 写进启动脚本的 `-Xmx`。
    pub memory_mb: u32,
    /// 跑安装器用的 java；None 表示自己挑。
    pub java: Option<PathBuf>,
    /// 下载源（Forge / NeoForge 的 maven 走它）。
    pub mirror: crate::settings::Mirror,
    /// 下载限速。
    pub limit_kbps: Option<u32>,
}

/// 服务器里用哪一份 Java。
///
/// 服务端不需要图形界面，但安装器要跑，所以按游戏版本推一个大版本要求。
/// 推不准也不要紧：只要挑到的 Java 不低于它，jar 都能跑起来。
pub fn required_java(mc_version: &str) -> u32 {
    // 26.x 之后 Mojang 改了版本号编排（不再带前导 1.），那些版本都要 Java 21+。
    let mut parts = mc_version.split('.');
    let first = parts.next().unwrap_or("").parse::<u32>().unwrap_or(0);
    let second = parts.next().unwrap_or("").parse::<u32>().unwrap_or(0);
    let third = parts.next().unwrap_or("").parse::<u32>().unwrap_or(0);
    if first >= 26 {
        return 21;
    }
    match (first, second) {
        (1, minor) if minor >= 20 => {
            // 1.20.5 起要求 Java 21，之后一直没降；1.20.4 及以前还是 17。
            if minor > 20 || third >= 5 {
                21
            } else {
                17
            }
        }
        (1, 18) | (1, 19) => 17,
        (1, 17) => 16,
        _ => 8,
    }
}

/// 挑一份能跑安装器的 Java。
fn pick_java(plan: &Plan) -> Result<PathBuf, String> {
    if let Some(path) = &plan.java {
        return Ok(path.clone());
    }
    let required = required_java(&plan.mc_version);
    let runtimes = crate::java::detect_all();
    if runtimes.is_empty() {
        return Err("本机没找到 Java，装服务端需要它来跑安装器".to_string());
    }
    if let Some(runtime) = crate::java::pick_java(&runtimes, required) {
        // `javaw.exe` 收不到输出，安装器的错误就看不见了，换成同目录的 java.exe。
        return Ok(console_java(&runtime.path));
    }
    // 都低于要求时退回版本最高的那个：装不上至少报错能读。
    let newest = runtimes
        .iter()
        .max_by_key(|runtime| runtime.major_version)
        .map(|runtime| runtime.path.clone())
        .ok_or_else(|| "本机没找到 Java".to_string())?;
    Ok(console_java(&newest))
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

/// 装一个服务端。
///
/// 失败时**把刚建的目录删掉**：留一个半成品在列表里，玩家下次会以为它装好了。
pub fn create<F>(
    plan: &Plan,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
    mut on_log: F,
) -> Result<Server, String>
where
    F: FnMut(&str),
{
    if !is_valid_name(&plan.name) {
        return Err("这个名字不能当文件夹名".to_string());
    }
    let dir = root().join(&plan.name);
    if dir.exists() {
        return Err(format!("servers/{} 已经存在了", plan.name));
    }
    create_at(plan, &dir, cancel, on_progress, &mut on_log)
}

/// 装进玩家自己挑的目录。
///
/// 和 [`create`] 的区别只有一个：目录由调用方给。玩家在资源管理器里选了一个
/// 空文件夹时走这条——他不一定想把服务端塞进我们的 `servers/` 下面。
///
/// 目录里已经有东西就不动它：宁可报错让他换个文件夹，也不覆盖别人的文件。
pub fn create_at<F>(
    plan: &Plan,
    dir: &Path,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
    mut on_log: F,
) -> Result<Server, String>
where
    F: FnMut(&str),
{
    let existed = dir.exists();
    if existed {
        let mut entries = std::fs::read_dir(dir)
            .map_err(|e| format!("读 {} 失败: {}", dir.display(), e))?;
        if entries.next().is_some() {
            return Err(format!(
                "{} 里已经有文件了，换一个空文件夹吧",
                dir.display()
            ));
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败: {}", e))?;

    let result = install_into(plan, dir, cancel, on_progress, &mut on_log);
    if result.is_err() {
        // 只删我们自己造出来的那个目录；原本就存在的空文件夹留着
        if !existed {
            let _ = std::fs::remove_dir_all(dir);
            on_log("安装失败，已经把这个半成品目录删掉了");
        }
    }
    result
}

fn install_into<F>(
    plan: &Plan,
    dir: &Path,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
    on_log: &mut F,
) -> Result<Server, String>
where
    F: FnMut(&str),
{
    match plan.core {
        CoreKind::Vanilla => install_vanilla(plan, dir, cancel, on_progress, on_log)?,
        CoreKind::Fabric => install_fabric(plan, dir, cancel, on_progress, on_log)?,
        CoreKind::Forge | CoreKind::NeoForge => {
            install_installer_core(plan, dir, cancel, on_progress, on_log)?
        }
        CoreKind::Velocity => {
            return Err("Velocity 是代理端，启动器目前只识别它，不替你安装。".to_string())
        }
    }

    // 装完立刻铺一份默认配置：Minecraft 只在第一次启动时才生成，而玩家多半
    // 想在开服之前就把 motd、端口定好。
    let props_path = dir.join("server.properties");
    if !props_path.exists() {
        let mut props = Properties::default();
        for field in FIELDS {
            props.set(field.key, field.default);
        }
        props.save(&props_path)?;
    }
    if !dir.join("eula.txt").exists() {
        set_eula(dir, false)?;
    }
    write_start_script(plan, dir, on_log)?;
    write_marker(plan, dir)?;

    let server = read(dir).ok_or_else(|| "写完元数据读不回来".to_string())?;
    on_log(&format!("服务端就绪：servers/{}", plan.name));
    Ok(server)
}

/// 原版：Mojang 的版本 JSON 里直接给 `downloads.server`。
fn install_vanilla<F>(
    plan: &Plan,
    dir: &Path,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
    on_log: &mut F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    download_vanilla_jar(plan, dir, cancel, on_progress, on_log)
}

/// 取一份文本：先试镜像地址、再试官方地址。
///
/// BMCLAPI 在负载高的时候会直接 reset 连接（curl 退出码 35），一次失败不说明
/// 这条路不通，所以每条地址都多试一遍再放弃。
fn fetch_text_with_fallback(url: &str, mirror: crate::settings::Mirror) -> Result<String, String> {
    let mirrored = mirror.rewrite(url);
    let mut candidates = Vec::new();
    if mirrored != url {
        candidates.push(mirrored);
    }
    candidates.push(url.to_string());

    let mut errors = Vec::new();
    for candidate in candidates {
        for attempt in 0..2 {
            match crate::transport::fetch_text(&candidate) {
                Ok(text) => return Ok(text),
                Err(e) => {
                    if attempt == 0 {
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        continue;
                    }
                    errors.push(format!("{}: {}", candidate, e));
                }
            }
        }
    }
    Err(errors.join("；"))
}

/// 取版本清单。选了镜像就先走 BMCLAPI，不通再退回官方。
fn fetch_manifest_mirrored(
    mirror: crate::settings::Mirror,
) -> Result<crate::install::VersionManifest, String> {
    let text = fetch_text_with_fallback(crate::install::VERSION_MANIFEST_URL, mirror)
        .map_err(|e| format!("下载版本清单失败: {}", e))?;
    crate::install::parse_manifest(&text)
}

/// 把原版 `server.jar` 放到 `dir` 里（已经有了就跳过）。
///
/// 原版和 Fabric 都要它：Fabric 的安装器自己带 `-downloadMinecraft`，但那条路
/// 只会去官方 piston-data 拉六十多兆。先自己按设置里的源下一份，安装器看到
/// `server.jar` 已经在了就不会再下。
fn download_vanilla_jar<F>(
    plan: &Plan,
    dir: &Path,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
    on_log: &mut F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    let dest = dir.join("server.jar");
    if dest.is_file() {
        return Ok(());
    }
    on_progress(Progress::Stage("查版本清单".to_string()));
    let manifest = fetch_manifest_mirrored(plan.mirror)?;
    let summary = manifest
        .versions
        .iter()
        .find(|version| version.id == plan.mc_version)
        .ok_or_else(|| format!("官方清单里没有 {} 这个版本", plan.mc_version))?;
    on_log(&format!("找到 {}（{}）", summary.id, summary.version_type));

    on_progress(Progress::Stage("读版本描述".to_string()));
    let text = fetch_text_with_fallback(&summary.url, plan.mirror)
        .map_err(|e| format!("取版本描述失败: {}", e))?;
    let raw = crate::version::parse_version(&text).map_err(|e| format!("版本描述解析失败: {}", e))?;
    let server = raw
        .downloads_server
        .ok_or_else(|| format!("{} 没有服务端下载（快照和远古版本经常没有）", plan.mc_version))?;

    on_progress(Progress::Stage("下载 server.jar".to_string()));
    on_log(&format!(
        "下载 server.jar（{} MB）…",
        server.size.unwrap_or(0) / 1_048_576
    ));
    download_with_fallback(
        &server.url,
        &dest,
        server.sha1.as_deref(),
        plan.limit_kbps,
        cancel,
        plan.mirror,
        on_log,
    )?;
    Ok(())
}

/// 下载一个文件：镜像下不动就退回官方地址。
///
/// 六十多兆的 server.jar 走 BMCLAPI 时经常在几千字节处被对端重置
/// （curl 退出码 35）——它自己的重试兜不住大文件，这里换条路再来一次。
fn download_with_fallback<F>(
    url: &str,
    dest: &Path,
    sha1: Option<&str>,
    limit_kbps: Option<u32>,
    cancel: &crate::cancel::CancelToken,
    mirror: crate::settings::Mirror,
    on_log: &mut F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    let mirrored = mirror.rewrite(url);
    let mut candidates = Vec::new();
    if mirrored != url {
        candidates.push(mirrored);
    }
    candidates.push(url.to_string());

    let total = candidates.len();
    let mut last = String::new();
    for (index, candidate) in candidates.into_iter().enumerate() {
        match crate::transport::download_with_cancel(
            &candidate,
            dest,
            sha1,
            limit_kbps,
            cancel,
        ) {
            Ok(_) => return Ok(()),
            Err(e) if e.is_cancelled() => return Err("已取消".to_string()),
            Err(e) => {
                last = e.to_string();
                if index + 1 < total {
                    on_log(&format!("镜像上没下下来（{}），换官方地址再试一次", e));
                }
            }
        }
    }
    Err(format!("下载 {} 失败: {}", dest.display(), last))
}

/// Fabric：官方安装器的 `server` 子命令。
fn install_fabric<F>(
    plan: &Plan,
    dir: &Path,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
    on_log: &mut F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    on_progress(Progress::Stage("下载 Fabric 安装器".to_string()));
    let cache = dir.join(".aicraft");
    std::fs::create_dir_all(&cache).map_err(|e| format!("建缓存目录失败: {}", e))?;
    let installer = cache.join("fabric-installer.jar");
    let url = fabric_installer_url(plan.mirror)?;
    // 安装器本体在 `maven.fabricmc.net` 上，BMCLAPI 的 `/maven` 也有同一份。
    let candidates = if plan.mirror.resolved(crate::settings::ProbeKind::Fabric)
        == crate::settings::Mirror::Bmclapi
    {
        vec![swap_maven(&url, crate::loader::BMCLAPI_MAVEN), url.clone()]
    } else {
        vec![url.clone()]
    };
    let mut last_error = String::new();
    for candidate in candidates {
        on_log(&format!("下载 Fabric 安装器：{}", candidate));
        match crate::transport::download_with_cancel(
            &candidate,
            &installer,
            None,
            plan.limit_kbps,
            cancel,
        ) {
            Ok(_) => {
                last_error.clear();
                break;
            }
            Err(e) => {
                if e.is_cancelled() {
                    return Err("已取消".to_string());
                }
                last_error = format!("{}: {}", candidate, e);
            }
        }
    }
    if !last_error.is_empty() {
        return Err(format!("下载安装器失败：{}", last_error));
    }

    let java = pick_java(plan)?;
    // 原版 `server.jar` 有六十来个兆，交给安装器的 `-downloadMinecraft` 就
    // 只能走官方；先自己按设置里的源下一份，安装器看到它就不再下了。
    download_vanilla_jar(plan, dir, cancel, on_progress, on_log)?;

    on_progress(Progress::Stage("运行 Fabric 安装器".to_string()));
    on_log(&format!("用 {} 跑安装器…", java.display()));
    let mirrored =
        plan.mirror.resolved(crate::settings::ProbeKind::Fabric) == crate::settings::Mirror::Bmclapi;
    let install = |maven: Option<&str>| -> Result<(), String> {
        let mut command = crate::spawn::silent(&java);
        command
            .arg("-jar")
            .arg(&installer)
            .arg("server")
            .arg("-dir")
            .arg(dir)
            .arg("-mcversion")
            .arg(&plan.mc_version)
            .arg("-loader")
            .arg(&plan.core_version);
        if let Some(base) = maven {
            // 安装器是直接字符串拼接基址的，少了结尾那条斜杠就会拼成
            // `.../mavenorg/ow2/...` 然后 404。
            command.arg("-mavenurl").arg(base);
        }
        run_quietly(command, dir, cancel, "Fabric 安装器")
    };

    let attempt = if mirrored {
        install(Some(&format!("{}/", crate::loader::BMCLAPI_MAVEN)))
    } else {
        install(None)
    };
    if let Err(e) = attempt {
        if mirrored {
            // 镜像的 maven 偶尔会给 Connection reset（本项目实测过），
            // 库文件也不全；这条路失败就退回官方再跑一遍。
            on_log(&format!("走镜像装 Fabric 失败（{}），退回官方 maven 再试", e));
            install(None)?;
        } else {
            return Err(e);
        }
    }

    if !dir.join("fabric-server-launch.jar").is_file() {
        return Err("安装器跑完了，但没看到 fabric-server-launch.jar".to_string());
    }
    Ok(())
}

/// Forge / NeoForge：跑各自的 `--installServer`。
fn install_installer_core<F>(
    plan: &Plan,
    dir: &Path,
    cancel: &crate::cancel::CancelToken,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
    on_log: &mut F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    let cache = dir.join(".aicraft");
    std::fs::create_dir_all(&cache).map_err(|e| format!("建缓存目录失败: {}", e))?;
    let installer = cache.join(format!("{}-installer.jar", plan.core.id()));

    on_progress(Progress::Stage(format!("下载 {} 安装器", plan.core.label())));
    let mut last_error = String::new();
    for url in installer_urls(plan) {
        on_log(&format!("下载 {} 安装器：{}", plan.core.label(), url));
        match crate::transport::download_with_cancel(&url, &installer, None, plan.limit_kbps, cancel)
        {
            Ok(_) => {
                last_error.clear();
                break;
            }
            Err(e) => {
                if e.is_cancelled() {
                    return Err("已取消".to_string());
                }
                last_error = format!("{}: {}", url, e);
            }
        }
    }
    if !last_error.is_empty() {
        return Err(format!("下载安装器失败：{}", last_error));
    }
    // 安装器是普通 jar；下到半截的那种，`java -jar` 只会甩一句
    // `Invalid or corrupt jarfile`，不如在这里先看出来。
    if std::fs::metadata(&installer).map(|m| m.len()).unwrap_or(0) < 100_000 {
        return Err("安装器下载不完整（文件太小），换个下载源再试一次".to_string());
    }

    // Forge / NeoForge 安装器只会照着自己 jar 里写死的地址去拿库，
    // 而 NeoForge 的 maven 在不少网络下直接超时。先把这些库按用户选的源
    // 铺进 libraries/，安装器随后只需校验与 patch。
    let rewrite: fn(&str, crate::settings::Mirror) -> String = match plan.core {
        CoreKind::NeoForge => crate::loader::neoforge_library_url,
        _ => crate::loader::forge_library_url,
    };
    let label = if plan.mirror == crate::settings::Mirror::Bmclapi {
        "BMCLAPI 镜像"
    } else {
        "官方源"
    };
    // 预置阶段的进度条走 loader 那套 `LoaderProgress`，这里只把「在忙什么」
    // 转出来；逐条下载的细进度不值得占一行。
    let progress = |_p: crate::loader::LoaderProgress| {};
    crate::loader::preseed_installer_libraries(
        dir,
        &installer,
        plan.mirror,
        label,
        rewrite,
        cancel,
        &progress,
        on_log,
    )?;

    let java = pick_java(plan)?;
    on_progress(Progress::Stage(format!(
        "运行 {} 安装器（要下几十个库，可能好几分钟）",
        plan.core.label()
    )));
    on_log(&format!("用 {} 跑安装器…", java.display()));

    let mut command = crate::spawn::silent(&java);
    command
        .arg(format!("-Djava.io.tmpdir={}", cache.join("tmp").display()))
        .arg("-jar")
        .arg(&installer)
        .arg("--installServer")
        .arg(dir);
    command
        .env("TEMP", cache.join("tmp"))
        .env("TMP", cache.join("tmp"));
    std::fs::create_dir_all(cache.join("tmp")).ok();
    run_quietly(command, dir, cancel, &format!("{} 安装器", plan.core.label()))?;

    if !dir.join("run.bat").is_file() && !dir.join("run.sh").is_file() {
        return Err(format!(
            "{} 安装器跑完了，但没看到 run.bat —— 安装器可能换了产出",
            plan.core.label()
        ));
    }
    // 安装器自己会写 user_jvm_args.txt，我们把内存填进去（默认只有一行注释）。
    let args_file = dir.join("user_jvm_args.txt");
    if !args_file.exists() {
        let _ = std::fs::write(
            &args_file,
            format!("# 由 AICraft 启动器生成\n-Xmx{}M\n", plan.memory_mb),
        );
    } else if let Ok(text) = std::fs::read_to_string(&args_file) {
        if !text.lines().any(|line| line.trim().starts_with("-Xmx")) {
            let _ = std::fs::write(
                &args_file,
                format!("{}-Xmx{}M\n", text, plan.memory_mb),
            );
        }
    }
    Ok(())
}

/// 安装器的候选地址（按设置里的源排，第一条最想用）。
///
/// Forge / NeoForge 的库最后还是要从各自的 maven 拉，安装器本体这段能省
/// 几十兆的先省掉。
pub fn installer_urls(plan: &Plan) -> Vec<String> {
    let mirror = plan.mirror.resolved(match plan.core {
        CoreKind::Forge => crate::settings::ProbeKind::Forge,
        CoreKind::NeoForge => crate::settings::ProbeKind::NeoForge,
        CoreKind::Vanilla | CoreKind::Fabric | CoreKind::Velocity => {
            crate::settings::ProbeKind::Version
        }
    });
    match plan.core {
        CoreKind::Forge => {
            let official =
                crate::loader::forge_installer_url(&plan.mc_version, &plan.core_version);
            let mirrored =
                crate::loader::forge_installer_mirror_url(&plan.mc_version, &plan.core_version);
            if mirror == crate::settings::Mirror::Bmclapi {
                vec![mirrored, official]
            } else {
                vec![official, mirrored]
            }
        }
        CoreKind::NeoForge => {
            let official = crate::loader::neoforge_installer_url(&plan.core_version);
            // `/neoforge/version/<v>/download/installer` 是 BMCLAPI 自己整理的
            // 索引，比它的 `/maven` 稳（后者会 302 到一台常连不上的机器）。
            let endpoint = format!(
                "{}/version/{}/download/installer",
                crate::loader::BMCLAPI_NEOFORGE,
                plan.core_version
            );
            if mirror == crate::settings::Mirror::Bmclapi {
                vec![endpoint, official]
            } else {
                vec![official, endpoint]
            }
        }
        // 原版和 Fabric 不走这里；Velocity 不装。
        CoreKind::Vanilla | CoreKind::Fabric | CoreKind::Velocity => Vec::new(),
    }
}

/// 把 `maven.fabricmc.net` 上的地址换成镜像的 `/maven` 前缀。
fn swap_maven(url: &str, base: &str) -> String {
    match url.strip_prefix("https://maven.fabricmc.net") {
        Some(rest) => format!("{}{}", base.trim_end_matches('/'), rest),
        None => url.to_string(),
    }
}

/// Fabric 安装器（官方 maven 上固定那几个版本，取列表里最新的稳定版）。
fn fabric_installer_url(mirror: crate::settings::Mirror) -> Result<String, String> {
    let bases = if mirror.resolved(crate::settings::ProbeKind::Fabric)
        == crate::settings::Mirror::Bmclapi
    {
        ["https://bmclapi2.bangbang93.com/fabric-meta", crate::loader::FABRIC_META]
    } else {
        [crate::loader::FABRIC_META, "https://bmclapi2.bangbang93.com/fabric-meta"]
    };
    let text = crate::loader::fetch_text_any(&bases, "/v2/versions/installer")
        .map_err(|e| format!("取安装器版本失败: {}", e))?;
    let root = json::parse(&text).map_err(|e| e.to_string())?;
    let list = root.as_array().ok_or_else(|| "安装器列表不是数组".to_string())?;
    // 第一个就是最新的；没标 stable 的也认，Install 这个项目很少发预览版。
    let first = list
        .iter()
        .find(|item| item.get("stable").and_then(Json::as_bool).unwrap_or(true))
        .or_else(|| list.first())
        .ok_or_else(|| "安装器列表是空的".to_string())?;
    first
        .get("url")
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| "安装器条目里没有 url".to_string())
}

/// 跑一个子进程、收它的输出、支持取消。
///
/// 安装器的输出（几百行 maven 下载日志）不往界面上倒，全写进
/// `<dir>/.aicraft/installer.log`；出错时只把最后几行带出来。
fn run_quietly(
    mut command: std::process::Command,
    dir: &Path,
    cancel: &crate::cancel::CancelToken,
    label: &str,
) -> Result<(), String>
{
    use std::process::Stdio;
    let log_path = dir.join(".aicraft").join("installer.log");
    let _ = std::fs::create_dir_all(dir.join(".aicraft"));
    let out = std::fs::File::create(&log_path)
        .map_err(|e| format!("建安装器日志失败: {}", e))?;
    let err = out
        .try_clone()
        .map_err(|e| format!("复制日志句柄失败: {}", e))?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .map_err(|e| format!("启动{}失败: {}", label, e))?;

    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("已取消".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let tail = tail_of(&log_path, 12);
                    return Err(if tail.is_empty() {
                        format!("{}退出码 {}", label, status.code().unwrap_or(-1))
                    } else {
                        format!(
                            "{}失败（退出码 {}）：\n{}",
                            label,
                            status.code().unwrap_or(-1),
                            tail
                        )
                    });
                }
                return Ok(());
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(200)),
            Err(e) => return Err(format!("等{}结束失败: {}", label, e)),
        }
    }
}

/// 取日志文件最后几行，用来把安装器的报错带出去。
fn tail_of(path: &Path, lines: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// 写元数据。
fn write_marker(plan: &Plan, dir: &Path) -> Result<(), String> {
    let text = format!(
        "{{\n  \"name\": \"{}\",\n  \"core\": \"{}\",\n  \"mc_version\": \"{}\",\n  \"core_version\": \"{}\"\n}}\n",
        crate::auth::json_escape(&plan.name),
        plan.core.id(),
        crate::auth::json_escape(&plan.mc_version),
        crate::auth::json_escape(&plan.core_version),
    );
    std::fs::write(dir.join(MARKER), text).map_err(|e| format!("写元数据失败: {}", e))
}

/// 写 `start.bat`。
///
/// 启动器不接管控制台，玩家靠这个脚本开服。脚本里把 `-Xmx` 与
/// `-Dfile.encoding=UTF-8` 都定下来——后者不给的话，中文 motd 在控制台
/// 会变成一串问号。
fn write_start_script<F>(plan: &Plan, dir: &Path, on_log: &mut F) -> Result<(), String>
where
    F: FnMut(&str),
{
    let java = match pick_java(plan) {
        Ok(path) => path.display().to_string(),
        Err(_) => "java".to_string(),
    };
    let bat = dir.join("start.bat");
    let body = match plan.core {
        // Forge / NeoForge 的安装器会自己写 run.bat（里面带它要的那串 JVM 参数），
        // 我们只负责把工作目录与编码摆好，别覆盖它。
        CoreKind::Forge | CoreKind::NeoForge => "@echo off\r\nchcp 65001 >nul\r\ncd /d \"%~dp0\"\r\ncall run.bat nogui\r\npause\r\n".to_string(),
        CoreKind::Fabric => format!(
            "@echo off\r\nchcp 65001 >nul\r\ncd /d \"%~dp0\"\r\n\"{}\" -Xmx{}M -Dfile.encoding=UTF-8 -jar fabric-server-launch.jar nogui\r\npause\r\n",
            java, plan.memory_mb
        ),
        CoreKind::Vanilla => format!(
            "@echo off\r\nchcp 65001 >nul\r\ncd /d \"%~dp0\"\r\n\"{}\" -Xmx{}M -Dfile.encoding=UTF-8 -jar server.jar nogui\r\npause\r\n",
            java, plan.memory_mb
        ),
        // 我们不装代理端，这一支到不了。
        CoreKind::Velocity => String::new(),
    };
    std::fs::write(&bat, body).map_err(|e| format!("写 start.bat 失败: {}", e))?;
    on_log(&format!("已生成 {}", bat.display()));
    Ok(())
}

/// 删掉一个服务端。删之前界面上必须二次确认。
pub fn remove(server: &Server) -> Result<(), String> {
    std::fs::remove_dir_all(&server.dir)
        .map_err(|e| format!("删 {} 失败: {}", server.dir.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_keep_order_and_drop_comments() {
        let text = "#Minecraft server properties\nmotd=Hello\nserver-port=25565\n\n# 注释\npvp=true\n";
        let props = Properties::parse(text);
        assert_eq!(props.lines.len(), 3);
        assert_eq!(props.lines[0].key, "motd");
        assert_eq!(props.get("server-port"), Some("25565"));
        assert_eq!(props.get("pvp"), Some("true"));
    }

    #[test]
    fn setting_an_existing_key_reports_real_changes() {
        let mut props = Properties::parse("motd=Hello\n");
        assert!(!props.set("motd", "Hello"), "值没变不该报告改动");
        assert!(props.set("motd", "World"));
        assert!(props.set("max-players", "10"), "新键要追加");
        assert_eq!(props.get("max-players"), Some("10"));
    }

    #[test]
    fn values_keep_their_equals_signs() {
        // motd 里写 `=` 是很常见的（「玩法=A+B」），只能按第一个 `=` 切。
        let props = Properties::parse("motd=玩法=A+B\n");
        assert_eq!(props.get("motd"), Some("玩法=A+B"));
    }

    #[test]
    fn rendering_round_trips_through_parse() {
        let mut props = Properties::default();
        props.set("motd", "昔涟的服");
        props.set("online-mode", "false");
        let back = Properties::parse(&props.render());
        assert_eq!(back.get("motd"), Some("昔涟的服"));
        assert_eq!(back.get("online-mode"), Some("false"));
    }

    #[test]
    fn eula_is_only_accepted_when_the_line_says_true() {
        let dir = crate::testutil::fresh_dir("server-eula");
        assert!(!eula_accepted(dir.as_ref()), "没有文件就是没同意");
        set_eula(dir.as_ref(), false).unwrap();
        assert!(!eula_accepted(dir.as_ref()));
        set_eula(dir.as_ref(), true).unwrap();
        assert!(eula_accepted(dir.as_ref()));
    }

    #[test]
    fn the_java_requirement_follows_the_game_version() {
        assert_eq!(required_java("1.16.5"), 8);
        assert_eq!(required_java("1.17.1"), 16);
        assert_eq!(required_java("1.20.1"), 17);
        assert_eq!(required_java("1.20.4"), 17);
        assert_eq!(required_java("1.20.5"), 21);
        assert_eq!(required_java("1.21.1"), 21);
        assert_eq!(required_java("26.3"), 21);
    }

    #[test]
    fn every_core_knows_its_id_and_back() {
        for core in CoreKind::ALL {
            assert_eq!(CoreKind::from_id(core.id()), Some(core));
            assert!(!core.label().is_empty());
            assert!(!core.entry_jar().is_empty());
        }
        assert_eq!(CoreKind::from_id("paper"), None);
    }

    #[test]
    fn only_vanilla_skips_the_loader_version() {
        assert!(!CoreKind::Vanilla.needs_loader_version());
        assert!(CoreKind::Fabric.needs_loader_version());
        assert!(CoreKind::Forge.needs_loader_version());
        assert!(CoreKind::NeoForge.needs_loader_version());
    }

    /// 造一个「只有本地文件头」的 jar，模拟 Mojang 那种没有中央目录的打包器。
    fn fake_packer_jar(path: &Path, entry: &str, body: &str) {
        let name = entry.as_bytes();
        let data = body.as_bytes();
        let mut out = Vec::new();
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&[20, 0]); // version needed
        out.extend_from_slice(&[0, 0]); // flags
        out.extend_from_slice(&[0, 0]); // method 0 = stored
        out.extend_from_slice(&[0, 0, 0, 0]); // time/date
        out.extend_from_slice(&[0, 0, 0, 0]); // crc（我们不校验）
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]); // extra
        out.extend_from_slice(name);
        out.extend_from_slice(data);
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn a_packer_jar_still_gives_up_its_version() {
        // 真实的 Mojang 服务端 jar 没有中央目录，读不出 version.json 就只能认
        // 成「原版 · 版本未知」——所以这条退路必须留着。
        let dir = crate::testutil::fresh_dir("server-probe-vanilla");
        fake_packer_jar(
            &dir.join("server.jar"),
            "version.json",
            r#"{"id": "26.3", "name": "26.3", "world_version": 5023}"#,
        );
        let server = probe(&dir).expect("应该认出来");
        assert_eq!(server.core, CoreKind::Vanilla);
        assert_eq!(server.mc_version, "26.3");
        assert!(!server.managed, "别处搬来的目录不归我们管");
    }

    /// 造一个带 `META-INF/MANIFEST.MF` 的普通 zip（有中央目录）。
    fn fake_manifest_jar(path: &Path, title: &str, version: &str) {
        let manifest = format!(
            "Manifest-Version: 1.0\r\nImplementation-Version: {}\r\nImplementation-Title: {}\r\nMain-Class: com.velocitypowered.proxy.Velocity\r\n\r\n",
            version, title
        );
        let name = b"META-INF/MANIFEST.MF";
        let data = manifest.as_bytes();
        let mut out = Vec::new();
        // 本地文件头：签名 4 + 版本 2 + 标志 2 + 方法 2 + 时间 2 + 日期 2
        // + crc 4 + 压缩长 4 + 原始长 4 + 名字长 2 + 扩展长 2 = 30
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&[20, 0]); // version needed
        out.extend_from_slice(&[0, 0]); // flags
        out.extend_from_slice(&[0, 0]); // method 0 = stored
        out.extend_from_slice(&[0, 0, 0, 0]); // time/date
        out.extend_from_slice(&[0, 0, 0, 0]); // crc（我们不校验）
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name);
        out.extend_from_slice(data);
        // 中央目录：签名 4 + 版本 2 + 需要版本 2 + 标志 2 + 方法 2 + 时间 2
        // + 日期 2 + crc 4 + 压缩长 4 + 原始长 4 + 名字长 2 + 扩展长 2
        // + 注释长 2 + 盘号 2 + 内部属性 2 + 外部属性 4 + 偏移 4 = 46
        let cd_start = out.len() as u32;
        out.extend_from_slice(b"PK\x01\x02");
        out.extend_from_slice(&[20, 0]); // 制作版本
        out.extend_from_slice(&[20, 0]); // 需要版本
        out.extend_from_slice(&[0, 0]); // 标志
        out.extend_from_slice(&[0, 0]); // 方法 0 = stored
        out.extend_from_slice(&[0, 0, 0, 0]); // time/date
        out.extend_from_slice(&[0, 0, 0, 0]); // crc
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]); // 扩展长
        out.extend_from_slice(&[0, 0]); // 注释长
        out.extend_from_slice(&[0, 0]); // 盘号
        out.extend_from_slice(&[0, 0]); // 内部属性
        out.extend_from_slice(&[0, 0, 0, 0]); // 外部属性
        out.extend_from_slice(&0u32.to_le_bytes()); // 本地头偏移
        out.extend_from_slice(name);
        let cd_size = out.len() as u32 - cd_start;
        // EOCD
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_start.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn a_velocity_proxy_is_recognised_by_its_manifest() {
        // Velocity 的 jar 里没有 version.json，光看 jar 内容会被当成
        // 「原版 · 版本未知」——玩家会以为这是个能直接开的服。
        let dir = crate::testutil::fresh_dir("server-probe-velocity");
        fake_manifest_jar(
            &dir.join("velocity-3.4.0-SNAPSHOT-522.jar"),
            "Velocity",
            "3.4.0-SNAPSHOT (git-e9940713-b522)",
        );
        let server = probe(&dir).expect("应该认出来");
        assert_eq!(server.core, CoreKind::Velocity);
        assert_eq!(server.core_version, "3.4.0-SNAPSHOT");
        assert!(server.mc_version.is_empty(), "代理端不绑游戏版本");
        assert!(!server.managed);
        // 摘要里不该出现「版本未知」——代理端本来就没有游戏版本
        assert_eq!(server.summary(), "Velocity 3.4.0-SNAPSHOT");
    }

    #[test]
    fn a_plain_jar_with_another_title_is_not_a_proxy() {
        // 别的服务端 jar 里也有 MANIFEST，不能见到清单就认成代理端。
        let dir = crate::testutil::fresh_dir("server-probe-not-proxy");
        fake_manifest_jar(&dir.join("minecraft_server.jar"), "Minecraft Server", "1.20.1");
        let server = probe(&dir).expect("还是能认出来");
        assert_ne!(server.core, CoreKind::Velocity);
        assert_eq!(server.core, CoreKind::Vanilla);
    }

    #[test]
    fn a_proxy_is_not_installable() {
        // 新建流程只列 ALL（四个），Velocity 只在 DETECTABLE 里。
        assert_eq!(CoreKind::ALL.len(), 4);
        assert!(!CoreKind::ALL.contains(&CoreKind::Velocity));
        assert_eq!(CoreKind::DETECTABLE.len(), 5);
        assert!(CoreKind::DETECTABLE.contains(&CoreKind::Velocity));
        assert_eq!(CoreKind::from_id("velocity"), Some(CoreKind::Velocity));
        // 它没有加载器版本可选
        assert!(!CoreKind::Velocity.needs_loader_version());
    }

    #[test]
    fn a_loader_folder_wins_over_the_server_jar_beside_it() {
        // Forge / NeoForge 的目录里**也**有一份 server.jar，先看 server.jar
        // 就会把 Forge 服认成原版。
        let dir = crate::testutil::fresh_dir("server-probe-forge");
        fake_packer_jar(
            &dir.join("server.jar"),
            "version.json",
            r#"{"id": "1.20.1"}"#,
        );
        let lib = dir.join("libraries/net/minecraftforge/forge/1.20.1-47.2.0");
        std::fs::create_dir_all(&lib).unwrap();
        let server = probe(&dir).expect("应该认出来");
        assert_eq!(server.core, CoreKind::Forge);
        assert_eq!(server.mc_version, "1.20.1");
        // 目录名是 <mc>-<forge>，我们只要后半截
        assert_eq!(server.core_version, "47.2.0");
    }

    #[test]
    fn the_newest_loader_folder_is_the_one_reported() {
        // 换过加载器版本的目录里会留着好几份，字典序会把 0.9 排到 0.16 后面
        let dir = crate::testutil::fresh_dir("server-probe-fabric");
        for version in ["0.9.0", "0.16.10", "0.16.9"] {
            std::fs::create_dir_all(
                dir.join("libraries/net/fabricmc/fabric-loader").join(version),
            )
            .unwrap();
        }
        let server = probe(&dir).expect("有加载器目录就算数");
        assert_eq!(server.core, CoreKind::Fabric);
        assert_eq!(server.core_version, "0.16.10");
        assert_eq!(server.mc_version, "");
    }

    #[test]
    fn an_empty_folder_is_not_a_server() {
        let dir = crate::testutil::fresh_dir("server-probe-empty");
        assert!(probe(&dir).is_none());
        assert!(probe(&dir.join("nope")).is_none(), "目录不存在也不能 panic");
    }

    #[test]
    fn a_summary_says_what_would_run() {
        let server = Server {
            name: "survival".to_string(),
            dir: PathBuf::from("/tmp/survival"),
            core: CoreKind::Fabric,
            mc_version: "26.3".to_string(),
            core_version: "0.20.1".to_string(),
            eula: false,
            managed: true,
        };
        assert_eq!(server.summary(), "Fabric 0.20.1 · 26.3");
        let vanilla = Server {
            core: CoreKind::Vanilla,
            core_version: String::new(),
            ..server
        };
        assert_eq!(vanilla.summary(), "原版 26.3");
        let unknown = Server {
            name: "survival".to_string(),
            dir: PathBuf::from("/tmp/survival"),
            core: CoreKind::Fabric,
            mc_version: String::new(),
            core_version: "0.20.1".to_string(),
            eula: false,
            managed: true,
        };
        assert_eq!(unknown.summary(), "Fabric 0.20.1 · 版本未知");
    }

    #[test]
    fn field_defaults_parse_as_their_own_kind() {
        for field in FIELDS {
            match field.kind {
                FieldKind::Switch => {
                    assert!(
                        field.default == "true" || field.default == "false",
                        "{} 的默认值不是开关值",
                        field.key
                    );
                }
                FieldKind::Number => {
                    assert!(
                        field.default.parse::<i64>().is_ok(),
                        "{} 的默认值不是整数",
                        field.key
                    );
                }
                FieldKind::Choice(options) => {
                    assert!(
                        options.contains(&field.default),
                        "{} 的默认值不在选项里",
                        field.key
                    );
                }
                FieldKind::Text => {}
            }
        }
    }

    fn plan(core: CoreKind, mirror: crate::settings::Mirror) -> Plan {
        Plan {
            name: "probe".to_string(),
            core,
            mc_version: "1.20.1".to_string(),
            core_version: "47.2.0".to_string(),
            memory_mb: 2048,
            java: None,
            mirror,
            limit_kbps: None,
        }
    }

    #[test]
    fn the_installer_list_follows_the_chosen_source() {
        // 选了镜像就把镜像排前面，但官方永远留作退路——镜像少一个文件时
        // 不该整个安装卡死。
        let mirrored = installer_urls(&plan(
            CoreKind::Forge,
            crate::settings::Mirror::Bmclapi,
        ));
        assert_eq!(mirrored.len(), 2);
        assert!(mirrored[0].contains("bmclapi2.bangbang93.com/maven"));
        assert!(mirrored[1].starts_with("https://maven.minecraftforge.net"));

        let official = installer_urls(&plan(
            CoreKind::Forge,
            crate::settings::Mirror::Official,
        ));
        assert!(official[0].starts_with("https://maven.minecraftforge.net"));
        assert!(official[1].contains("bmclapi2.bangbang93.com/maven"));
    }

    #[test]
    fn neoforge_prefers_the_mirrors_own_index() {
        // BMCLAPI 的 `/maven` 会 302 到一台常连不上的机器，`/neoforge/version`
        // 是它自己整理的索引，排前面。
        let urls = installer_urls(&plan(
            CoreKind::NeoForge,
            crate::settings::Mirror::Bmclapi,
        ));
        assert!(urls[0].contains("/neoforge/version/47.2.0/download/installer"));
    }

    #[test]
    fn vanilla_and_fabric_have_no_installer_candidates() {
        for core in [CoreKind::Vanilla, CoreKind::Fabric] {
            assert!(installer_urls(&plan(core, crate::settings::Mirror::Bmclapi)).is_empty());
        }
    }

    #[test]
    fn fabric_installer_urls_get_the_mirror_prefix() {
        let url = "https://maven.fabricmc.net/net/fabricmc/fabric-installer/1.1.2/fabric-installer-1.1.2.jar";
        assert_eq!(
            swap_maven(url, crate::loader::BMCLAPI_MAVEN),
            "https://bmclapi2.bangbang93.com/maven/net/fabricmc/fabric-installer/1.1.2/fabric-installer-1.1.2.jar"
        );
        // 别的主机原样返回，不能瞎换
        let other = "https://example.com/a.jar";
        assert_eq!(swap_maven(other, crate::loader::BMCLAPI_MAVEN), other);
    }
}
