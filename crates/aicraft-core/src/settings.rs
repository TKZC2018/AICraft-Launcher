//! 启动器设置：版本下载源、并发、下载限速、内存、JVM 参数、Java 路径。
//!
//! 存 `<数据目录>/settings.json`（默认在启动器 exe 旁边的 `AICraftData` 里，
//! 写不进去才退到 `%APPDATA%\aicraft`）。core 不引 serde，所以这里手写
//! 序列化；字段少、结构平，反序列化时按 key 取值，缺字段用默认值补齐——
//! 老版本配置文件里没有的新字段不会让启动失败。
//!
//! 设置只影响「怎么下载 / 怎么起游戏」，不碰版本清单和安装计划本身：
//! 镜像是在真正发请求前改写 URL，安装计划里存的仍是官方地址。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::auth::json_escape;
use crate::json::{self, Json};

/// 配置文件版本号。字段语义变了才需要加。
pub const SETTINGS_VERSION: u32 = 1;

/// 下载限速的可设下限（KB/s）。
///
/// 再低就低于 [`crate::speed`] 里每路的下限了，设了也没有意义。
pub const MIN_LIMIT_KBPS: u32 = 16;

/// 下载限速的可设上限（KB/s，= 100 MB/s）。
///
/// 家用网络远达不到这个数，留着只是防止手滑输入天文数字。
pub const MAX_LIMIT_KBPS: u32 = 102_400;

/// 版本下载源：下载游戏版本文件（清单、版本 JSON、库、资源）时用哪个。
///
/// 只管版本文件；账号、日志这些跟 Mojang 无关的请求不经过它。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mirror {
    /// 开场探一次延迟，谁快用谁。探测结果缓存在进程里。
    Auto,
    /// Mojang 官方。
    Official,
    /// BMCLAPI 国内镜像。
    Bmclapi,
}

/// 要探哪一类源。四类端点和镜像覆盖度都不一样，得分开探。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeKind {
    Version,
    Fabric,
    Forge,
    NeoForge,
}

impl ProbeKind {
    pub const ALL: [ProbeKind; 4] = [
        ProbeKind::Version,
        ProbeKind::Fabric,
        ProbeKind::Forge,
        ProbeKind::NeoForge,
    ];

    /// 这一类源上「官方」和「镜像」各拿哪个地址去测。
    ///
    /// 都取真正会最先请求的那一条：清单就是清单、列表就是列表。
    /// 测一条小文件不代表后面下库的速度，但「连得上、回得快」这件事
    /// 恰恰是选址要的。
    fn probe_urls(self) -> (&'static str, &'static str) {
        match self {
            ProbeKind::Version => (
                "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json",
                "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json",
            ),
            ProbeKind::Fabric => (
                "https://meta.fabricmc.net/v2/versions/loader",
                "https://bmclapi2.bangbang93.com/fabric-meta/v2/versions/loader",
            ),
            ProbeKind::Forge => (
                "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml",
                "https://bmclapi2.bangbang93.com/forge/minecraft/1.20.1",
            ),
            ProbeKind::NeoForge => (
                "https://neoforged.forgecdn.net/releases/net/neoforged/neoforge/maven-metadata.xml",
                "https://bmclapi2.bangbang93.com/neoforge/list/1.21.1",
            ),
        }
    }

    fn slot(self) -> usize {
        match self {
            ProbeKind::Version => 0,
            ProbeKind::Fabric => 1,
            ProbeKind::Forge => 2,
            ProbeKind::NeoForge => 3,
        }
    }
}

/// 探测结果：0 = 还没探过，1 = 官方快，2 = 镜像快。
static AUTO_CHOICE: [std::sync::atomic::AtomicU8; 4] = [
    std::sync::atomic::AtomicU8::new(0),
    std::sync::atomic::AtomicU8::new(0),
    std::sync::atomic::AtomicU8::new(0),
    std::sync::atomic::AtomicU8::new(0),
];

/// 探测前先试的地址最多等多久（秒）。
///
/// 官方那台回得慢的时候能拖到十秒，探到一半就放弃没意义；
/// 这个数只是防止某台完全不回时把后台线程挂死。
const PROBE_TIMEOUT_SECS: u32 = 8;

impl Mirror {
    /// 全部可选值，界面按这个顺序排。
    pub const ALL: [Mirror; 3] = [Mirror::Auto, Mirror::Official, Mirror::Bmclapi];

    /// 存盘用的标识。
    pub fn id(&self) -> &str {
        match self {
            Mirror::Auto => "auto",
            Mirror::Official => "official",
            Mirror::Bmclapi => "bmclapi",
        }
    }

    /// 界面上显示的名字。
    pub fn label(&self) -> &'static str {
        match self {
            Mirror::Auto => "自动",
            Mirror::Official => "官方",
            Mirror::Bmclapi => "BMCLAPI",
        }
    }

    /// 从标识还原；认不出的标识按官方处理。
    pub fn from_id(id: &str) -> Mirror {
        match id {
            "auto" => Mirror::Auto,
            "bmclapi" => Mirror::Bmclapi,
            _ => Mirror::Official,
        }
    }

    /// 已经探到的那一层（`Auto` 会摊平成具体的一层）。
    ///
    /// 还没探过就先按官方算——探测在后台线程里跑，第一屏不该等它。
    pub fn resolved(self, kind: ProbeKind) -> Mirror {
        match self {
            Mirror::Auto => match AUTO_CHOICE[kind.slot()].load(std::sync::atomic::Ordering::Relaxed)
            {
                2 => Mirror::Bmclapi,
                _ => Mirror::Official,
            },
            other => other,
        }
    }

    /// 探一次四类源，把结果记下来。
    ///
    /// 每一类里两条地址并行测，取先回来的那个。两条都失败就保持原样
    /// （继续按官方走，至少不会把能用的源换成一个连不上的）。
    pub fn probe_all() {
        let mut handles = Vec::new();
        for kind in ProbeKind::ALL {
            handles.push(std::thread::spawn(move || {
                let (official_url, mirror_url) = kind.probe_urls();
                let (official, mirror) = probe_pair(official_url, mirror_url);
                let choice = match (official, mirror) {
                    // 只有镜像一种能回来，就用镜像
                    (None, Some(_)) => 2,
                    // 官方能回、镜像不回或更慢
                    (Some(o), Some(m)) if m < o => 2,
                    (Some(_), _) => 1,
                    (None, None) => return,
                };
                AUTO_CHOICE[kind.slot()].store(choice, std::sync::atomic::Ordering::Relaxed);
                crate::logging::debug(format!(
                    "下载源探测：{:?} 官方 {} 秒 / 镜像 {} 秒",
                    kind,
                    official.map(|d| format!("{:.2}", d)).unwrap_or_else(|| "失败".into()),
                    mirror.map(|d| format!("{:.2}", d)).unwrap_or_else(|| "失败".into()),
                ));
            }));
        }
        for handle in handles {
            let _ = handle.join();
        }
    }

    /// 改写一个下载地址。识别不了的主机原样返回。
    pub fn rewrite(&self, url: &str) -> String {
        match self.resolved(ProbeKind::Version) {
            Mirror::Bmclapi => bmclapi_rewrite(url),
            _ => url.to_string(),
        }
    }
}

/// 同时测两条地址，返回各自的耗时（秒）。测不出来的给 `None`。
fn probe_pair(official: &str, mirror: &str) -> (Option<f64>, Option<f64>) {
    let a = std::thread::spawn({
        let url = official.to_string();
        move || crate::transport::probe_latency(&url, PROBE_TIMEOUT_SECS)
    });
    let b = std::thread::spawn({
        let url = mirror.to_string();
        move || crate::transport::probe_latency(&url, PROBE_TIMEOUT_SECS)
    });
    let a = a.join().unwrap_or(None);
    let b = b.join().unwrap_or(None);
    (a, b)
}

/// 按指定源改写地址（给不方便持有 `Mirror` 值的地方用）。
pub fn mirror_rewrite(url: &str, mirror: Mirror) -> String {
    mirror.rewrite(url)
}

/// 官方主机 → BMCLAPI 的路径前缀。
///
/// BMCLAPI 不是简单的「换主机」：库文件要加 `/maven`、资源对象要加
/// `/assets`，而 launchermeta 那几台是直接镜像根路径的。
const BMCLAPI_BASE: &str = "https://bmclapi2.bangbang93.com";

const BMCLAPI_RULES: &[(&str, &str)] = &[
    (
        "https://resources.download.minecraft.net",
        "https://bmclapi2.bangbang93.com/assets",
    ),
    (
        "https://libraries.minecraft.net",
        "https://bmclapi2.bangbang93.com/maven",
    ),
    (
        "https://files.minecraftforge.net/maven",
        "https://bmclapi2.bangbang93.com/maven",
    ),
    // Fabric 的库（fabric-loader、sponge-mixin 等）在 BMCLAPI 的 /maven 下也有。
    // 不映射的话，选了镜像源时 Fabric 实例的库还是会去官方 maven 拉。
    (
        "https://maven.fabricmc.net",
        "https://bmclapi2.bangbang93.com/maven",
    ),
    // NeoForge 的库默认指向 maven.neoforged.net，那个域在不少网络下不通；
    // BMCLAPI 的 /maven 能覆盖（少数据件会 302 到别的镜像，curl 会跟过去）。
    (
        "https://maven.neoforged.net/releases",
        "https://bmclapi2.bangbang93.com/maven",
    ),
    ("https://launcher.mojang.com", BMCLAPI_BASE),
    ("https://launchermeta.mojang.com", BMCLAPI_BASE),
    ("https://piston-meta.mojang.com", BMCLAPI_BASE),
    ("https://piston-data.mojang.com", BMCLAPI_BASE),
];

fn bmclapi_rewrite(url: &str) -> String {
    for (from, to) in BMCLAPI_RULES {
        if let Some(rest) = url.strip_prefix(from) {
            return format!("{}{}", to, rest);
        }
    }
    url.to_string()
}

/// 游戏跑起来之后，启动器自己怎么办。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LauncherMode {
    /// 关掉自己，游戏结束后再把自己拉起来（默认）。
    ///
    /// 这是最省内存的一种：游戏自己就要吃一两 G，启动器在旁边占着没意义。
    /// 但玩家多半还想再开一局，所以玩完了要自己回来。
    #[default]
    Reopen,
    /// 关掉自己，不再回来。
    Close,
    /// 留在后台。
    KeepOpen,
}

impl LauncherMode {
    pub const ALL: [LauncherMode; 3] = [Self::Reopen, Self::Close, Self::KeepOpen];

    /// 存进设置文件的取值。改这些字符串等于让老配置失效，别随手改。
    pub fn id(self) -> &'static str {
        match self {
            LauncherMode::Reopen => "reopen",
            LauncherMode::Close => "close",
            LauncherMode::KeepOpen => "keep",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            // 这三句就是需求里那三句，别改写成「关掉 / 保持打开」那种简称：
            // 简称在设置页里看不出「关掉之后还会不会回来」这个关键差别。
            LauncherMode::Reopen => "关闭启动器在游戏结束后重新打开（默认）",
            LauncherMode::Close => "关闭启动器",
            LauncherMode::KeepOpen => "保持启动器运行",
        }
    }

    pub fn from_id(id: &str) -> Option<LauncherMode> {
        match id {
            "reopen" => Some(LauncherMode::Reopen),
            "close" => Some(LauncherMode::Close),
            "keep" => Some(LauncherMode::KeepOpen),
            _ => None,
        }
    }
}

/// 启动器设置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// 版本下载源。
    pub mirror: Mirror,
    /// Fabric 的下载源（它与原版分开选，因为走的端点完全不同）。
    pub fabric_mirror: Mirror,
    /// Forge 的下载源。
    pub forge_mirror: Mirror,
    /// NeoForge 的下载源。
    pub neoforge_mirror: Mirror,
    /// 并发下载数，1..=16。
    pub concurrency: usize,
    /// 下载总限速（KB/s）；None 表示不限速。
    ///
    /// 这是**所有下载加起来**的上限，不是单车道的上限：
    /// 并发 8 路、限 2048 KB/s 时，每路大约 256 KB/s（分配见 [`crate::speed`]）。
    pub download_limit_kbps: Option<u32>,
    /// 让启动器按本机内存自动决定堆大小。
    ///
    /// 开着时 [`Settings::memory_mb`] 用 [`crate::meminfo::recommended_mb`]
    /// 现算；`max_memory_mb` 原样留着，关掉自动就回到玩家上次手动定的那个数。
    pub auto_memory: bool,
    /// 手动指定的最大内存（MB）。自动模式下只是关掉自动之后的回落值。
    pub max_memory_mb: u32,
    /// 最小内存（MB）；None 表示不传 `-Xms`。
    pub min_memory_mb: Option<u32>,
    /// 额外 JVM 参数原文，按空白/换行切成参数列表。
    pub jvm_args: String,
    /// 手动指定的 java.exe；None 表示自动挑选。
    pub java_path: Option<PathBuf>,
    /// 游戏目录（.minecraft 的路径）。
    ///
    /// 空串表示「还没选过」，由界面用默认位置兜底。存成 String 而不是
    /// `PathBuf`：设置页里它是个可编辑的文本框，逐帧和 PathBuf 互转又慢又难看。
    ///
    /// 这一项历史上有过坑：它原本只是界面上的一个字段，没进设置文件，
    /// 于是每次重启都退回默认目录。偏好类的东西都得落到这里。
    pub game_dir: String,
    /// 用户自己添加过的其他游戏目录（不含官方默认目录）。
    ///
    /// 官方目录 `%APPDATA%\.minecraft` 不进这个列表：它由 [`official_game_dir`]
    /// 现算，写进去反而会在换机器、换账户之后变成一个死路径。
    pub game_dirs: Vec<String>,
    /// 是否在目录列表里隐藏那行"官方目录"。
    ///
    /// 官方目录默认显示，但它只是一个**默认项**，不是不可移除的东西——用户把它
    /// 从列表里移掉时就是这个字段变 true。它只影响显示，不碰磁盘上任何东西，
    /// 也不会改变当前正在用的目录。
    pub hide_official_dir: bool,
    /// 是否让每个版本各自隔离（存档 / 模组 / 配置放在 `versions/<id>/` 下）。
    ///
    /// 默认开启：新装的版本开箱就是独立实例，装两套模组不会互相打架。
    /// 关掉之后退回「共享」布局（和 `.minecraft` 根目录共用），但已经带上
    /// 实例标记的版本仍会按隔离方式启动——它们的存档已经在那个目录里了。
    pub isolate_instances: bool,
    /// 游戏跑起来之后启动器怎么处置（关掉再回来 / 只关掉 / 留在后台）。
    pub launcher_mode: LauncherMode,
    /// 离线用户名。空串表示没设过 —— 界面会显示「未配置」，不替玩家编默认值。
    pub offline_name: String,
    /// 上次选中的游戏版本名。空串表示没选过，启动时挑列表第一个。
    ///
    /// 和 `game_dir` 一样属于「偏好类状态」：只活在 `App` 内存里的话，
    /// 每次开启动器都会退回第一个版本，玩家每次都得重新点一遍。
    /// 存名字而不是下标：`versions/` 是玩家能随手增删的目录，下标第二天就漂了。
    pub selected_version: String,
    /// 玩家自己加进来的服务端目录（不含数据目录下的 `servers/`）。
    ///
    /// 有人把开了一年多的老服整个文件夹搬过来接着用，那种目录不在
    /// `servers/` 下面，也不归我们管——这里只记路径，删列表项不动磁盘。
    pub server_dirs: Vec<String>,
    /// 左侧导航是否收起成图标条（只是个界面偏好，但跟着设置一起存）。
    pub nav_collapsed: bool,
    pub allow_offline: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            mirror: Mirror::Auto,
            fabric_mirror: Mirror::Auto,
            forge_mirror: Mirror::Auto,
            neoforge_mirror: Mirror::Auto,
            concurrency: 8,
            download_limit_kbps: None,
            auto_memory: true,
            max_memory_mb: 4096,
            min_memory_mb: None,
            jvm_args: String::new(),
            java_path: None,
            game_dir: String::new(),
            game_dirs: Vec::new(),
            hide_official_dir: false,
            isolate_instances: true,
            launcher_mode: LauncherMode::Reopen,
            offline_name: String::new(),
            selected_version: String::new(),
            server_dirs: Vec::new(),
            nav_collapsed: false,
            allow_offline: false,
        }
    }
}

impl Settings {
    /// 把额外 JVM 参数文本切成参数列表。
    pub fn extra_jvm_args(&self) -> Vec<String> {
        self.jvm_args
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    /// 把取值收进合法范围，避免界面手输把启动搞崩。
    /// 实际要传给游戏的堆大小（MB）。
    ///
    /// 自动模式下按本机内存现算；本机内存还没探到就先用手动那个值，
    /// 免得在「还没量完」的窗口里给出一个 0。
    pub fn memory_mb(&self) -> u32 {
        if !self.auto_memory {
            return self.max_memory_mb;
        }
        match crate::meminfo::cached() {
            Some(memory) => crate::meminfo::recommended_mb(memory.total_mb),
            None => self.max_memory_mb,
        }
    }

    pub fn clamp(&mut self) {
        self.concurrency = self.concurrency.clamp(1, 16);
        // 0 当作「不限速」保留，其余收进区间
        self.download_limit_kbps = match self.download_limit_kbps {
            Some(0) | None => None,
            Some(k) => Some(k.clamp(MIN_LIMIT_KBPS, MAX_LIMIT_KBPS)),
        };
        self.max_memory_mb = self.max_memory_mb.clamp(512, 65536);
        if let Some(min) = self.min_memory_mb {
            self.min_memory_mb = Some(min.clamp(256, self.max_memory_mb));
        }
        // 目录列表：去掉空白项、重复项；官方目录还在列表里显示时，也去掉和它重名的项
        // （官方那一行就是它，不该再占一行）。官方那行被用户移掉时不剔——那时
        // 用户自己加进来的官方路径就是唯一代表它的那条。
        // 当前目录**不**从列表里剔：用户切走以后还要能切回来。
        // 比较时统一转小写：Windows 上 `E:\MC` 和 `e:\mc` 是同一个地方。
        let official = if self.hide_official_dir {
            String::new()
        } else {
            official_game_dir()
                .map(|p| p.display().to_string().to_lowercase())
                .unwrap_or_default()
        };
        let mut kept: Vec<String> = Vec::new();
        for dir in &self.game_dirs {
            let trimmed = dir.trim();
            if trimmed.is_empty() {
                continue;
            }
            let key = trimmed.to_lowercase();
            if !official.is_empty() && key == official {
                continue;
            }
            if kept.iter().any(|k| k.to_lowercase() == key) {
                continue;
            }
            kept.push(trimmed.to_string());
        }
        self.game_dirs = kept;

        // 服务端目录同理：去空白、去重（同样按小写比）。
        let mut servers: Vec<String> = Vec::new();
        for dir in &self.server_dirs {
            let trimmed = dir.trim();
            if trimmed.is_empty() {
                continue;
            }
            let key = trimmed.to_lowercase();
            if servers.iter().any(|k| k.to_lowercase() == key) {
                continue;
            }
            servers.push(trimmed.to_string());
        }
        self.server_dirs = servers;
    }

    /// 序列化成 JSON。
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\n");
        out.push_str(&format!("  \"version\": {},\n", SETTINGS_VERSION));
        out.push_str(&format!(
            "  \"mirror\": \"{}\",\n",
            json_escape(self.mirror.id())
        ));
        out.push_str(&format!(
            "  \"fabric_mirror\": \"{}\",\n",
            json_escape(self.fabric_mirror.id())
        ));
        out.push_str(&format!(
            "  \"forge_mirror\": \"{}\",\n",
            json_escape(self.forge_mirror.id())
        ));
        out.push_str(&format!(
            "  \"neoforge_mirror\": \"{}\",\n",
            json_escape(self.neoforge_mirror.id())
        ));
        out.push_str(&format!("  \"concurrency\": {},\n", self.concurrency));
        match self.download_limit_kbps {
            Some(k) => out.push_str(&format!("  \"download_limit_kbps\": {},\n", k)),
            None => out.push_str("  \"download_limit_kbps\": null,\n"),
        }
        out.push_str(&format!("  \"auto_memory\": {},\n", self.auto_memory));
        out.push_str(&format!("  \"max_memory_mb\": {},\n", self.max_memory_mb));
        match self.min_memory_mb {
            Some(min) => out.push_str(&format!("  \"min_memory_mb\": {},\n", min)),
            None => out.push_str("  \"min_memory_mb\": null,\n"),
        }
        out.push_str(&format!(
            "  \"jvm_args\": \"{}\",\n",
            json_escape(&self.jvm_args)
        ));
        match &self.java_path {
            Some(path) => out.push_str(&format!(
                "  \"java_path\": \"{}\",\n",
                json_escape(&path.display().to_string())
            )),
            None => out.push_str("  \"java_path\": null,\n"),
        }
        out.push_str(&format!(
            "  \"game_dir\": \"{}\",\n",
            json_escape(&self.game_dir)
        ));
        out.push_str("  \"game_dirs\": [");
        for (i, dir) in self.game_dirs.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("\"{}\"", json_escape(dir)));
        }
        out.push_str("],\n");
        out.push_str(&format!(
            "  \"hide_official_dir\": {},\n",
            self.hide_official_dir
        ));
        out.push_str(&format!(
            "  \"isolate_instances\": {},\n",
            self.isolate_instances
        ));
        out.push_str(&format!(
            "  \"launcher_mode\": \"{}\",\n",
            json_escape(self.launcher_mode.id())
        ));
        out.push_str(&format!(
            "  \"offline_name\": \"{}\",\n",
            json_escape(&self.offline_name)
        ));
        out.push_str(&format!(
            "  \"selected_version\": \"{}\",\n",
            json_escape(&self.selected_version)
        ));
        out.push_str("  \"server_dirs\": [");
        for (i, dir) in self.server_dirs.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("\"{}\"", json_escape(dir)));
        }
        out.push_str("],\n");
        out.push_str(&format!(
            "  \"nav_collapsed\": {},\n",
            self.nav_collapsed
        ));
        out.push_str(&format!(
            "  \"allow_offline\": {}\n",
            self.allow_offline
        ));
        out.push_str("}\n");
        out
    }

    /// 从 JSON 还原；缺字段用默认值补齐。
    pub fn from_json(text: &str) -> Result<Settings, String> {
        let root = json::parse(text).map_err(|e| e.to_string())?;
        if !matches!(root, Json::Object(_)) {
            return Err("设置文件根节点不是对象".to_string());
        }

        let mut s = Settings::default();

        // 旧版本存过 "custom" 源与 "mirror_custom" 前缀；自定义源已撤掉，
        // 读到就当作官方（识别不了的来源一律回到官方，别留一个坏源）。
        if let Some(id) = root.get("mirror").and_then(Json::as_str) {
            s.mirror = Mirror::from_id(id);
        }

        // 三种加载器各有自己的源。旧配置没这几个键时跟随「版本下载源」，
        // 这样已经选了 BMCLAPI 的人不会突然被拉回官方源。
        let fallback = s.mirror;
        for (key, slot) in [
            ("fabric_mirror", &mut s.fabric_mirror),
            ("forge_mirror", &mut s.forge_mirror),
            ("neoforge_mirror", &mut s.neoforge_mirror),
        ] {
            *slot = root
                .get(key)
                .and_then(Json::as_str)
                .map(Mirror::from_id)
                .unwrap_or(fallback);
        }

        if let Some(n) = root.get("concurrency").and_then(Json::as_i64) {
            s.concurrency = n.max(0) as usize;
        }
        if let Some(n) = root.get("download_limit_kbps").and_then(Json::as_i64) {
            // 0 或负数在解析阶段就当「不限速」，不用等到 clamp
            s.download_limit_kbps = if n > 0 { Some(n as u32) } else { None };
        }
        if let Some(flag) = root.get("auto_memory").and_then(Json::as_bool) {
            s.auto_memory = flag;
        }
        if let Some(n) = root.get("max_memory_mb").and_then(Json::as_i64) {
            s.max_memory_mb = n.max(0) as u32;
        }
        if let Some(n) = root.get("min_memory_mb").and_then(Json::as_i64) {
            s.min_memory_mb = Some(n.max(0) as u32);
        }
        if let Some(text) = root.get("jvm_args").and_then(Json::as_str) {
            s.jvm_args = text.to_string();
        }
        if let Some(path) = root.get("java_path").and_then(Json::as_str) {
            if !path.trim().is_empty() {
                s.java_path = Some(PathBuf::from(path));
            }
        }
        if let Some(name) = root.get("selected_version").and_then(Json::as_str) {
            s.selected_version = name.to_string();
        }
        if let Some(flag) = root.get("nav_collapsed").and_then(Json::as_bool) {
            s.nav_collapsed = flag;
        }
        if let Some(items) = root.get("server_dirs").and_then(Json::as_array) {
            s.server_dirs = items
                .iter()
                .filter_map(Json::as_str)
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(str::to_string)
                .collect();
        }
        if let Some(flag) = root.get("hide_official_dir").and_then(Json::as_bool) {
            s.hide_official_dir = flag;
        }
        if let Some(dir) = root.get("game_dir").and_then(Json::as_str) {
            s.game_dir = dir.trim().to_string();
        }
        if let Some(items) = root.get("game_dirs").and_then(Json::as_array) {
            s.game_dirs = items
                .iter()
                .filter_map(Json::as_str)
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(str::to_string)
                .collect();
        }
        if let Some(flag) = root.get("isolate_instances").and_then(Json::as_bool) {
            s.isolate_instances = flag;
        }
        if let Some(id) = root.get("launcher_mode").and_then(Json::as_str) {
            // 认不出来的取值就按默认走：宁可多关一次启动器，也别让它赖在后台
            s.launcher_mode = LauncherMode::from_id(id).unwrap_or_default();
        }
        if let Some(name) = root.get("offline_name").and_then(Json::as_str) {
            s.offline_name = name.trim().to_string();
        }
        if let Some(flag) = root.get("allow_offline").and_then(Json::as_bool) {
            s.allow_offline = flag;
        }
        // 旧版本存过 "log_level"；现在记录不限等级，读到就忽略（保持兼容）。
        // 也存过 "client_id"；客户端 ID 现在写死在代码里，读到时同样忽略。

        s.clamp();
        Ok(s)
    }

    /// 从磁盘读；文件不存在也按默认值返回。
    pub fn load_or_default(path: &Path) -> Settings {
        match std::fs::read_to_string(path) {
            Ok(text) => Settings::from_json(&text).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    /// 写盘（先写临时文件再改名，避免写一半断电留下坏文件）。
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let mut clean = self.clone();
        clean.clamp();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建设置目录失败: {}", e))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, clean.to_json()).map_err(|e| format!("写设置失败: {}", e))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("保存设置失败: {}", e))
    }
}

/// 官方启动器的游戏目录：`%APPDATA%\.minecraft`。
///
/// 现算而不是存进设置：换台机器、换个 Windows 账户，这个位置就变了，
/// 存下来只会变成一个死路径。目录不存在时返回 `None`（没装过官方启动器）。
pub fn official_game_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    if base.is_empty() {
        return None;
    }
    let dir = PathBuf::from(base).join(".minecraft");
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

/// 数据目录：设置、账号、日志都放在这里。
///
/// 优先级：`AICRAFT_HOME`（显式指定）→ **启动器 exe 同目录下的 `AICraftData`**
/// → `%APPDATA%\aicraft` → `%LOCALAPPDATA%\aicraft` → `$XDG_CONFIG_HOME/aicraft`
/// → 临时目录。
///
/// 把 exe 目录排在 `%APPDATA%` 前面，是为了做成「解压即用」：整个文件夹拷到
/// U 盘或另一台机器，配置和日志跟着走，不留下系统级残留。
///
/// 候选目录会**实地试写**一次再采用：目录存在不等于能写（只读盘、被策略
/// 拦截、权限被改过都会让 `create_dir_all` 成功而写文件失败）。exe 放在
/// `C:\Program Files` 这类只读位置时，试写会失败并自动落回 `%APPDATA%`——
/// 存不了设置是小事，启动器起不来才是大事。
pub fn data_dir() -> PathBuf {
    static RESOLVED: OnceLock<PathBuf> = OnceLock::new();
    RESOLVED.get_or_init(resolve_data_dir).clone()
}

/// 便携模式的目录名（放在 exe 旁边）。
pub const PORTABLE_DIR_NAME: &str = "AICraftData";

fn resolve_data_dir() -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(home) = std::env::var_os("AICRAFT_HOME") {
        if !home.is_empty() {
            candidates.push(PathBuf::from(home));
        }
    }
    candidates.push(portable_dir());
    for var in ["APPDATA", "LOCALAPPDATA", "XDG_CONFIG_HOME"] {
        if let Ok(base) = std::env::var(var) {
            if !base.trim().is_empty() {
                candidates.push(PathBuf::from(base).join("aicraft"));
            }
        }
    }
    candidates.push(std::env::temp_dir().join("aicraft"));

    // 显式指定了 AICRAFT_HOME 就尊重它，不再往下退：用户说放哪就放哪。
    if std::env::var_os("AICRAFT_HOME").is_some() {
        return candidates.remove(0);
    }

    for dir in &candidates {
        if is_writable_dir(dir) {
            return dir.clone();
        }
    }
    // 一个都写不进去也别 panic：起得来比什么都重要，落到临时目录并让界面去提示。
    candidates.pop().unwrap_or_else(std::env::temp_dir)
}

/// 账号相关文件（`accounts.json` + `machine.key` + 头像缓存）放哪。
///
/// 和 [`data_dir`] 故意分开：设置和日志跟着走便携目录没问题，但账号是两个
/// 东西——它体积不随使用变、内容敏感、而且「解压到哪就在哪」会让玩家把
/// 启动器挪个位置就登出一次。放 `%APPDATA%\aicraft` 是 Windows 惯例：
/// 托盘小、删程序目录不会连账号一起删、备份时也知道该找哪。
///
/// 落地顺序：`%APPDATA%\aicraft` → `%LOCALAPPDATA%\aicraft` → 数据目录。
/// 前两个都要实地试写，写不进去才退回便携位置（那种情况多半是漫游配置
/// 被策略锁了，让账号跟着程序走总比起不来好）。
pub fn accounts_dir() -> PathBuf {
    static RESOLVED: OnceLock<PathBuf> = OnceLock::new();
    RESOLVED.get_or_init(resolve_accounts_dir).clone()
}

fn resolve_accounts_dir() -> PathBuf {
    for var in ["APPDATA", "LOCALAPPDATA"] {
        if let Ok(base) = std::env::var(var) {
            if base.trim().is_empty() {
                continue;
            }
            let dir = PathBuf::from(base).join("aicraft");
            if is_writable_dir(&dir) {
                return dir;
            }
        }
    }
    data_dir()
}

/// exe 同目录下的便携数据文件夹：`<exe 所在目录>/AICraftData`。
///
/// 取 exe 路径而不是 `current_dir()`：从资源管理器双击、从开始菜单、从别的
/// 目录 `start` 进来，工作目录可能是任意地方，只有 exe 位置才是「启动器在
/// 哪」的可靠答案。`current_exe()` 在极少数环境（被删除、符号链接失效）会
/// 失败，那时退回工作目录。
pub fn portable_dir() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(std::env::temp_dir);
    base.join(PORTABLE_DIR_NAME)
}

/// 目录能不能真的用来写东西：建目录、写文件、建子目录，三步都过得去才算。
///
/// 只试「写一个文件」是不够的：有的环境允许往已有目录里丢文件，却拒绝新建
/// 子目录（只读挂载、安全软件拦、别处拿同名文件占了位置都是这样），而日志和
/// 账号都要往子目录里放。探针名带进程号与纳秒，避免多线程/多进程互踩。
pub fn is_writable_dir(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let tag = format!(
        ".write-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    );

    let probe_file = dir.join(&tag);
    if std::fs::write(&probe_file, b"ok").is_err() {
        return false;
    }
    let _ = std::fs::remove_file(&probe_file);

    let probe_dir = dir.join(&tag);
    if std::fs::create_dir_all(&probe_dir).is_err() {
        return false;
    }
    let _ = std::fs::remove_dir(&probe_dir);
    true
}

/// 设置文件位置：`<data_dir>/settings.json`。
pub fn default_path() -> PathBuf {
    data_dir().join("settings.json")
}
/// 「让用户自己挑保存位置」的对话框该从哪个目录打开。
///
/// 只要求目录存在，不要求能写 —— 用户在里面往上跳一层就行，真写不进去时
/// 系统对话框自己会拦。优先数据目录，再退到桌面 / 文档 / 当前目录。
pub fn suggested_export_dir() -> PathBuf {
    let mut candidates = vec![data_dir()];
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let home = PathBuf::from(home);
        candidates.push(home.join("Desktop"));
        candidates.push(home.join("Documents"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd);
    }
    candidates
        .into_iter()
        .find(|d| d.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_keeps_url() {
        let url = "https://piston-meta.mojang.com/v1/packages/x/1.20.json";
        assert_eq!(Mirror::Official.rewrite(url), url);
    }

    #[test]
    fn bmclapi_maps_each_host() {
        // 版本清单和版本 JSON 也走镜像；镜像慢的时候靠「自动」换回官方
        assert_eq!(
            Mirror::Bmclapi.rewrite("https://piston-meta.mojang.com/v1/packages/x/1.20.json"),
            "https://bmclapi2.bangbang93.com/v1/packages/x/1.20.json"
        );
        assert_eq!(
            Mirror::Bmclapi.rewrite("https://piston-data.mojang.com/v1/objects/ab/client.jar"),
            "https://bmclapi2.bangbang93.com/v1/objects/ab/client.jar"
        );
        assert_eq!(
            Mirror::Bmclapi.rewrite("https://libraries.minecraft.net/com/example/a-1.0.jar"),
            "https://bmclapi2.bangbang93.com/maven/com/example/a-1.0.jar"
        );
        assert_eq!(
            Mirror::Bmclapi.rewrite("https://resources.download.minecraft.net/ab/abcdef"),
            "https://bmclapi2.bangbang93.com/assets/ab/abcdef"
        );
        // 认不出的主机原样返回，不要瞎改
        assert_eq!(
            Mirror::Bmclapi.rewrite("https://example.com/x.jar"),
            "https://example.com/x.jar"
        );
    }

    #[test]
    fn unknown_id_falls_back_to_official() {
        // 旧版存过 "custom" 源（带 mirror_custom 前缀）；这一版起撤掉了自定义前缀，
        // 读到旧配置要回到官方，而不是留一个已经没实现的源。
        let back =
            Settings::from_json("{\"mirror\": \"custom\", \"mirror_custom\": \"https://m.example.com\"}")
                .unwrap();
        assert_eq!(back.mirror, Mirror::Official);
        assert_eq!(Mirror::from_id("official"), Mirror::Official);
        assert_eq!(Mirror::from_id("bmclapi"), Mirror::Bmclapi);
        assert_eq!(Mirror::from_id("custom"), Mirror::Official);
        assert_eq!(Mirror::from_id(""), Mirror::Official);
    }

    #[test]
    fn mirror_roundtrip() {
        for mirror in Mirror::ALL {
            assert_eq!(Mirror::from_id(mirror.id()), mirror);
        }
    }

    #[test]
    fn jvm_args_split_on_whitespace() {
        let s = Settings {
            jvm_args: "  -Dfoo=1\n-XX:+UseG1GC   -Xss1M ".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            s.extra_jvm_args(),
            vec!["-Dfoo=1", "-XX:+UseG1GC", "-Xss1M"]
        );
    }

    #[test]
    fn json_roundtrip() {
        let s = Settings {
            mirror: Mirror::Bmclapi,
            fabric_mirror: Mirror::Bmclapi,
            forge_mirror: Mirror::Official,
            neoforge_mirror: Mirror::Bmclapi,
            concurrency: 12,
            download_limit_kbps: Some(2048),
            auto_memory: false,
            max_memory_mb: 8192,
            min_memory_mb: Some(1024),
            jvm_args: "-Dfoo=\"bar\"\n-XX:+UseG1GC".to_string(),
            java_path: Some(PathBuf::from("C:\\Java\\bin\\java.exe")),
            game_dir: "D:\\Games\\.minecraft".to_string(),
            game_dirs: vec!["E:\\Other\\.minecraft".to_string()],
            hide_official_dir: true,
            isolate_instances: false,
            launcher_mode: LauncherMode::KeepOpen,
            offline_name: "昔涟".to_string(),
            selected_version: "26.3-Fabric".to_string(),
            server_dirs: vec!["D:\\srv".to_string()],
            nav_collapsed: true,
            allow_offline: true,
        };
        let text = s.to_json();
        let back = Settings::from_json(&text).unwrap();
        assert_eq!(back.mirror, Mirror::Bmclapi);
        assert_eq!(back.concurrency, 12);
        assert_eq!(back.download_limit_kbps, Some(2048));
        assert_eq!(back.launcher_mode, LauncherMode::KeepOpen);
        assert!(!back.auto_memory);
        assert_eq!(back.max_memory_mb, 8192);
        assert_eq!(back.min_memory_mb, Some(1024));
        assert_eq!(back.jvm_args, s.jvm_args);
        assert_eq!(back.java_path, s.java_path);
        assert_eq!(back.game_dir, s.game_dir);
        assert_eq!(back.game_dirs, s.game_dirs);
        assert!(back.hide_official_dir);
        assert!(!back.isolate_instances);
        assert_eq!(back.offline_name, s.offline_name);
        assert_eq!(back.selected_version, "26.3-Fabric");
        assert_eq!(back.server_dirs, vec!["D:\\srv".to_string()]);
        assert!(back.nav_collapsed);
        assert!(back.allow_offline);
        assert_eq!(back.fabric_mirror, Mirror::Bmclapi);
        assert_eq!(back.forge_mirror, Mirror::Official);
        assert_eq!(back.neoforge_mirror, Mirror::Bmclapi);
    }

    /// 实例隔离默认是开的：新装的版本各自独立，装两套模组不会互相打架。
    #[test]
    fn the_launcher_reopens_by_default() {
        assert_eq!(Settings::default().launcher_mode, LauncherMode::Reopen);
        assert_eq!(LauncherMode::default(), LauncherMode::Reopen);
        assert_eq!(LauncherMode::ALL.len(), 3);
    }

    #[test]
    fn every_launcher_mode_round_trips() {
        for mode in LauncherMode::ALL {
            let s = Settings {
                launcher_mode: mode,
                ..Settings::default()
            };
            let back = Settings::from_json(&s.to_json()).expect("应当能读回来");
            assert_eq!(back.launcher_mode, mode, "{} 没有往返成功", mode.id());
            assert!(s.to_json().contains(&format!("\"launcher_mode\": \"{}\"", mode.id())));
        }
    }

    #[test]
    fn an_unknown_launcher_mode_falls_back_to_reopening() {
        let s = Settings::from_json("{\"launcher_mode\": \"whatever\"}").expect("应当能读");
        assert_eq!(s.launcher_mode, LauncherMode::Reopen);
        let old = Settings::from_json("{\"concurrency\": 4}").expect("旧文件没有这个键");
        assert_eq!(old.launcher_mode, LauncherMode::Reopen);
    }

    #[test]
    fn instance_isolation_is_on_by_default() {
        let s = Settings::default();
        assert!(s.isolate_instances);
        assert!(s.to_json().contains("\"isolate_instances\": true"));
    }

    /// 老配置文件里没有这个键，读出来也该是「开」。
    #[test]
    fn missing_isolation_key_means_isolated() {
        let back = Settings::from_json("{\"concurrency\": 4}").unwrap();
        assert!(back.isolate_instances);
    }

    /// 显式关掉要能被读回来，否则界面上的勾选等于没存。
    #[test]
    fn isolation_can_be_turned_off() {
        let back = Settings::from_json("{\"isolate_instances\": false}").unwrap();
        assert!(!back.isolate_instances);
        assert!(back.to_json().contains("\"isolate_instances\": false"));
    }

    /// 三种加载器各有自己的源，且旧配置要跟随「版本下载源」，不能突然被拉回官方。
    #[test]
    fn loader_mirrors_are_independent() {
        let mut s = Settings::default();
        assert_eq!(s.fabric_mirror, Mirror::Auto);
        assert_eq!(s.forge_mirror, Mirror::Auto);
        assert_eq!(s.neoforge_mirror, Mirror::Auto);

        s.fabric_mirror = Mirror::Bmclapi;
        s.neoforge_mirror = Mirror::Bmclapi;
        let back = Settings::from_json(&s.to_json()).unwrap();
        assert_eq!(back.fabric_mirror, Mirror::Bmclapi);
        assert_eq!(back.forge_mirror, Mirror::Auto);
        assert_eq!(back.neoforge_mirror, Mirror::Bmclapi);

        // 旧配置只有全局 `mirror`：三个加载器都跟着它走
        let old = Settings::from_json("{\"mirror\": \"bmclapi\"}").unwrap();
        assert_eq!(old.fabric_mirror, Mirror::Bmclapi);
        assert_eq!(old.forge_mirror, Mirror::Bmclapi);
        assert_eq!(old.neoforge_mirror, Mirror::Bmclapi);

        // 存进去的键名要对得上
        let text = s.to_json();
        for key in ["\"fabric_mirror\"", "\"forge_mirror\"", "\"neoforge_mirror\""] {
            assert!(text.contains(key), "{}", text);
        }
    }

    /// 游戏目录与离线用户名曾经只是界面字段，重启就丢；钉住它们真的会存盘。
    #[test]
    fn game_dir_and_name_survive_a_roundtrip() {
        let mut s = Settings::default();
        assert!(s.game_dir.is_empty(), "没选过时应该是空串，交给界面兜底");
        s.game_dir = "E:\\Minecraft\\我的世界".to_string();
        s.offline_name = "Steve2".to_string();
        let back = Settings::from_json(&s.to_json()).unwrap();
        assert_eq!(back.game_dir, "E:\\Minecraft\\我的世界");
        assert_eq!(back.offline_name, "Steve2");

        // 旧配置文件里没有这两个键，读出来是空串而不是报错
        let old = Settings::from_json("{\"concurrency\": 4}").unwrap();
        assert_eq!(old.game_dir, "");
        assert_eq!(old.offline_name, "");

        // 存进去的键名要对得上，别让用户手改文件时白改
        let text = s.to_json();
        assert!(text.contains("\"game_dir\""), "{}", text);
        assert!(text.contains("\"offline_name\""), "{}", text);
    }

    /// 客户端 ID 曾经存进设置文件，现在写死在 `auth::DEFAULT_CLIENT_ID` 里。
    /// 老文件里残留的那个键要能安静地忽略掉，不能让设置解析失败。
    #[test]
    fn old_client_id_key_is_ignored() {
        let old = Settings::from_json(
            "{\"concurrency\": 4, \"client_id\": \"51306758-f4c5-4577-a122-98ea37ffae0b\"}",
        )
        .unwrap();
        assert_eq!(old.concurrency, 4);
        // 写回时不再出现这个键
        assert!(!old.to_json().contains("client_id"), "{}", old.to_json());
    }

    /// 目录列表：存得进读得出，空项/重复项/当前目录/官方目录都在 clamp 里被剔掉。
    #[test]
    fn game_dirs_survive_a_roundtrip_and_get_cleaned() {
        let mut s = Settings::default();
        assert!(s.game_dirs.is_empty(), "默认一个额外目录都没有");

        s.game_dir = "E:\\Minecraft\\.minecraft".to_string();
        s.game_dirs = vec![
            "  ".to_string(),
            "D:\\Games\\.minecraft".to_string(),
            "d:\\games\\.minecraft".to_string(),
            "E:\\Minecraft\\.minecraft".to_string(),
        ];
        let back = Settings::from_json(&s.to_json()).unwrap();
        // 空白项没了、大小写重复并掉了；当前目录**留着**（切走之后还要能切回来）
        assert_eq!(
            back.game_dirs,
            vec![
                "D:\\Games\\.minecraft".to_string(),
                "E:\\Minecraft\\.minecraft".to_string()
            ]
        );

        // 旧配置文件里没有这个键，读成空列表而不是报错
        let old = Settings::from_json("{\"concurrency\": 4}").unwrap();
        assert!(old.game_dirs.is_empty());
        // 键名要对得上，用户手改文件时不会被静默吞掉
        assert!(s.to_json().contains("\"game_dirs\""), "{}", s.to_json());
    }

    #[test]
    fn download_limit_defaults_to_unlimited() {
        let back = Settings::from_json("{\"concurrency\": 2}").unwrap();
        assert_eq!(back.download_limit_kbps, None);

        // 0 与负数都当「不限速」，不写成 Some(0) 那种要处处特判的值
        let zero = Settings::from_json("{\"download_limit_kbps\": 0}").unwrap();
        assert_eq!(zero.download_limit_kbps, None);
        let neg = Settings::from_json("{\"download_limit_kbps\": -5}").unwrap();
        assert_eq!(neg.download_limit_kbps, None);

        // 存盘写的是 null
        let text = Settings::default().to_json();
        assert!(text.contains("\"download_limit_kbps\": null"), "{}", text);
    }

    #[test]
    fn download_limit_is_clamped() {
        let mut too_small = Settings {
            download_limit_kbps: Some(1),
            ..Settings::default()
        };
        too_small.clamp();
        assert_eq!(too_small.download_limit_kbps, Some(MIN_LIMIT_KBPS));

        let mut too_big = Settings {
            download_limit_kbps: Some(u32::MAX),
            ..Settings::default()
        };
        too_big.clamp();
        assert_eq!(too_big.download_limit_kbps, Some(MAX_LIMIT_KBPS));

        let mut zero = Settings {
            download_limit_kbps: Some(0),
            ..Settings::default()
        };
        zero.clamp();
        assert_eq!(zero.download_limit_kbps, None);
    }

    /// 官方目录只是「默认就有的那一项」，用户可以把它从列表里移掉。
    /// 移掉之后，自己加进来的那条官方路径就是唯一代表它的行，不能再被当成重复项剔掉。
    #[test]
    fn hiding_the_official_dir_keeps_user_entries() {
        assert!(!Settings::default().hide_official_dir, "默认是显示官方目录");

        // 读得出来，默认读不出来时是 false
        let shown = Settings::from_json("{\"hide_official_dir\": false}").unwrap();
        assert!(!shown.hide_official_dir);
        let hidden = Settings::from_json("{\"hide_official_dir\": true}").unwrap();
        assert!(hidden.hide_official_dir);
        let old = Settings::from_json("{\"concurrency\": 4}").unwrap();
        assert!(!old.hide_official_dir);

        // 键名要对得上，写回时不会丢
        let mut s = Settings {
            hide_official_dir: true,
            ..Settings::default()
        };
        assert!(s.to_json().contains("\"hide_official_dir\": true"));
        s.hide_official_dir = false;
        assert!(s.to_json().contains("\"hide_official_dir\": false"));
    }

    #[test]
    fn old_log_level_key_is_ignored() {
        let back = Settings::from_json("{\"log_level\": \"verbose\"}").unwrap();
        assert_eq!(back.max_memory_mb, 4096);
        assert_eq!(back.concurrency, 8);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let back = Settings::from_json("{\"concurrency\": 3}").unwrap();
        assert_eq!(back.concurrency, 3);
        assert_eq!(back.max_memory_mb, 4096);
        assert_eq!(back.mirror, Mirror::Auto);
    }

    #[test]
    fn clamp_bounds() {
        let mut s = Settings {
            concurrency: 999,
            max_memory_mb: 1,
            min_memory_mb: Some(99_999),
            ..Settings::default()
        };
        s.clamp();
        assert_eq!(s.concurrency, 16);
        assert_eq!(s.max_memory_mb, 512);
        assert_eq!(s.min_memory_mb, Some(512));
    }

    #[test]
    fn load_or_default_handles_missing_and_broken() {
        let dir = crate::testutil::fresh_dir("settings-load");
        let path = dir.join("settings.json");
        assert_eq!(
            Settings::load_or_default(&path).max_memory_mb,
            Settings::default().max_memory_mb
        );

        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(
            Settings::load_or_default(&path).concurrency,
            Settings::default().concurrency
        );
    }

    #[test]
    fn save_then_load() {
        let dir = crate::testutil::fresh_dir("settings-save");
        let path = dir.join("settings.json");
        let s = Settings {
            mirror: Mirror::Bmclapi,
            concurrency: 4,
            jvm_args: "-XX:+UseZGC".to_string(),
            ..Settings::default()
        };
        s.save(&path).unwrap();
        let back = Settings::load_or_default(&path);
        assert_eq!(back.mirror, Mirror::Bmclapi);
        assert_eq!(back.concurrency, 4);
        assert_eq!(back.extra_jvm_args(), vec!["-XX:+UseZGC"]);
    }

    #[test]
    fn writable_dir_accepts_a_fresh_directory() {
        let dir = crate::testutil::fresh_dir("writable-ok");
        assert!(is_writable_dir(&dir.join("nested")));
        // 探测用的临时文件与目录都该被清掉，不留垃圾。
        assert_eq!(std::fs::read_dir(dir.join("nested")).unwrap().count(), 0);
    }

    #[test]
    fn writable_dir_rejects_a_blocked_path() {
        // 父级是普通文件时连目录都建不出来 —— 这是「建得出来但写不进去」之外
        // 最常见的一种拒绝。子目录那一步没法在单测里单独构造（要真的改 ACL），
        // 所以这里只钉住整条路径失败时返回 false 这一点。
        let dir = crate::testutil::fresh_dir("writable-blocked");
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        assert!(!is_writable_dir(&blocker.join("logs")));
    }

    #[test]
    fn suggested_export_dir_is_an_existing_directory() {
        let dir = suggested_export_dir();
        assert!(dir.is_dir(), "{} 不是目录", dir.display());
    }

    #[test]
    fn portable_dir_sits_next_to_the_executable() {
        // 便携目录必须紧挨着 exe：这是「整个文件夹拷走就能用」的前提。
        let portable = portable_dir();
        assert_eq!(
            portable.file_name().and_then(|n| n.to_str()),
            Some(PORTABLE_DIR_NAME)
        );
        let exe = std::env::current_exe().expect("测试进程总要有个 exe 路径");
        let parent = exe.parent().expect("exe 总有父目录");
        // current_exe 在不同环境下可能是 target/debug/deps/xxx.exe，
        // 这里只要求便携目录与 exe 的父目录一致（同一套解析规则）。
        assert_eq!(portable.parent(), Some(parent));
    }

}
