//! 单个版本的「管理」页：加载器、模组、资源包、光影包、存档。
//!
//! 它不是主界面的一页，而是一层盖住整个窗口（含左侧主导航）的独立视图 ——
//! 进来之后视线只在这一份实例上，左上角点「返回」才回到版本列表。
//!
//! 为什么要有这一页：一个版本装好之后，用户真正要反复动的是它自己的那些
//! 东西（加模组、换资源包、备份存档、换加载器版本）。这些都散在磁盘上，
//! 让玩家自己去 `versions/<版本>/` 里翻，等于什么都没做。

use std::path::{Path, PathBuf};
use std::time::Instant;

use aicraft_core::instance::{self, Item};
use aicraft_core::loader::{LoaderKind, LoaderVersion};
use aicraft_core::modupdate::Check;
use aicraft_core::packexport::ExportFormat;

use crate::theme;

/// 「自动选择」到底会选中哪一个 Java。
///
/// 启动时是 `java::pick_java_choice` 按「版本清单声明的 Java 主版本」挑的，
/// 而那个主版本常常只写在继承链的父版本那一层（Fabric / Forge 的实例 json
/// 只写差异）。所以这里读完整条链再合并，跟启动那边走同一套判断——
/// 界面上写的是结论，不是「大概会是 Java 21 吧」。
///
/// 它只出现在这一页：自动会挑谁取决于**这一份版本**的清单，全局设置页
/// 没有「当前这一份」，写在那儿只会让人以为全局有个固定答案。
pub struct AutoJava {
    /// 下拉框里「自动选择」后面接的那半句，比如「自动选择 · Java 21」。
    pub text: String,
}

/// 算出「自动选择」的结果。见 [`AutoJava`]。
fn auto_java(
    game_dir: &Path,
    instance: &str,
    runtimes: Option<&[aicraft_core::java::JavaRuntime]>,
) -> AutoJava {
    let plain = || AutoJava {
        text: "自动选择".to_string(),
    };

    if instance.is_empty() {
        return plain();
    }
    // 清单读不出来（父版本没装之类）就不猜了：那种情况启动本来会先报另一个错。
    let Ok(chain) = aicraft_core::version::load_chain_from_dir(game_dir, instance) else {
        return plain();
    };
    let Ok(resolved) = aicraft_core::resolve::resolve(&chain) else {
        return plain();
    };
    // 启动那边也缺省 17（`do_launch` 里的 `unwrap_or(17)`），这里跟着来，
    // 免得同一份版本在两处写着不同的「要求」。
    let needed = resolved.java_version.major_version.unwrap_or(17);

    let Some(list) = runtimes else {
        return plain();
    };

    match aicraft_core::java::pick_java_choice(list, needed) {
        // 精确命中也好、拿更高版本顶替也好，界面上都只回答一件事：
        // 实际会启动哪一个 Java。玩家不需要在这里读一份兼容性说明。
        Some((rt, _)) => AutoJava {
            text: format!("自动选择 · Java {}", rt.major_version),
        },
        // 一个能用的都没有时没有「实际用哪个」可说，只能报缺几。
        None => AutoJava {
            text: format!("自动选择 · 缺 Java {}", needed),
        },
    }
}

/// 左侧二级导航的几个分区。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Loader,
    /// 这一份实例自己的游戏设置（Java / 内存 / JVM 参数）。
    Game,
    Mods,
    ResourcePacks,
    /// 光影包：`shaderpacks/` 里那些 zip（或解压开的文件夹）。
    ///
    /// 判据跟「备份」不是一个：这里看的是**目录在不在**，不是里面有没有东西。
    /// 原版游戏不建这个目录，装了 Iris / Oculus 之后才会被建出来 ——
    /// 连目录都没有，说明玩家压根没碰过光影，这一栏就不该出现。
    ShaderPacks,
    Saves,
    /// 世界备份：versions/<实例>/backups/ 里那些 zip。
    ///
    /// 它不在游戏目录里，而在实例自己的目录下；没有备份的时候整个分区都不出现。
    Backups,
}

impl Section {
    pub const ALL: [Section; 7] = [
        Section::Loader,
        Section::Game,
        Section::Mods,
        Section::ResourcePacks,
        Section::ShaderPacks,
        Section::Saves,
        Section::Backups,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Loader => "概览",
            Section::Game => "游戏设置",
            Section::Mods => "模组",
            Section::ResourcePacks => "资源包",
            Section::ShaderPacks => "光影包",
            Section::Saves => "存档",
            Section::Backups => "备份",
        }
    }

    /// 这一栏是不是「一堆文件」。
    ///
    /// 只有文件栏才谈得上「共 N 个」「打开文件夹」「刷新」；加载器和游戏
    /// 设置没有这回事，导航后面也不该跟一个 0。
    pub fn has_files(self) -> bool {
        matches!(
            self,
            Section::Mods
                | Section::ResourcePacks
                | Section::ShaderPacks
                | Section::Saves
                | Section::Backups
        )
    }
}

/// 这一页要交给主界面的动作。
pub enum Action {
    /// 退出管理页，回到版本列表。
    Close,
    /// 重新扫一遍磁盘。
    Reload,
    /// 用资源管理器打开某个目录。
    OpenFolder(PathBuf),
    /// 删掉一个条目（路径已由本页的确认框点头过）。
    Delete(PathBuf),
    /// 展开某个加载器，去要它的可用版本列表。
    LoadLoaderVersions(LoaderKind),
    /// 把指定加载器装进这个实例（或者把已装的换成这个版本）。
    SwitchLoader { kind: LoaderKind, version: String },
    /// 把清单压平成自包含的一份（HMCL 那种写法，不再依赖父版本）。
    Flatten,
    /// 把这一份实例的资源挨个对一遍清单，缺什么列出来。
    VerifyResources,
    /// 删掉打开着的这一份实例（还要主界面再确认一次）。
    DeleteInstance,
    /// 挨个问 Modrinth：这些模组里哪些有新版本。
    CheckUpdates,
    /// 更新其中某一个（按盘上那个 jar 定位）。
    UpdateMod(PathBuf),
    /// 把所有认得出、又有新版的都更新一遍。
    UpdateAll,
    /// 把这一份实例的游戏设置写回它自己的目录。
    ///
    /// 整份一起传：设置页里几个字段互相牵制（自动/手动、勾选框与下面的值），
    /// 分开传反而要给每个字段编一套动作。
    SaveGameConfig(Box<aicraft_core::instancecfg::InstanceConfig>),
    /// 把这一份实例换到另一个原版底座上（加载器种类不变）。
    ChangeGameVersion(String),
    /// 把这一份实例打成一个能分享的整合包。
    ExportPack {
        format: ExportFormat,
        name: String,
        version: String,
        /// 玩家勾中的顶层条目（`mods`、`config`、`saves` …）。
        include: Vec<String>,
    },
}

impl Action {
    /// 给操作日志用的一句话。
    pub fn describe(&self) -> String {
        match self {
            Action::Close => "管理页：返回上一页".to_string(),
            Action::Reload => "管理页：重新扫描".to_string(),
            Action::OpenFolder(path) => format!("管理页：打开文件夹 {}", path.display()),
            Action::Delete(path) => format!("管理页：删除 {}", path.display()),
            Action::LoadLoaderVersions(kind) => {
                format!("管理页：查看 {} 可用版本", kind.label())
            }
            Action::SwitchLoader { kind, version } => {
                format!("管理页：把加载器换成 {} {}", kind.label(), version)
            }
            Action::ChangeGameVersion(version) => {
                format!("管理页：把游戏版本换成 {}", version)
            }
            Action::Flatten => "管理页：压平清单".to_string(),
            Action::VerifyResources => "管理页：校验资源".to_string(),
            Action::DeleteInstance => "管理页：删除这一份实例（等确认）".to_string(),
            Action::CheckUpdates => "管理页：检查模组更新".to_string(),
            Action::UpdateMod(path) => format!("管理页：更新 {}", path.display()),
            Action::UpdateAll => "管理页：一键更新全部模组".to_string(),
            Action::ExportPack {
                format,
                name,
                version,
                include,
            } => format!(
                "管理页：导出 {}（{} {}，带上 {}）",
                format.label(),
                name,
                version,
                include.join("、")
            ),
            Action::SaveGameConfig(config) => {
                if config.follow_global {
                    "管理页：游戏设置改为沿用全局".to_string()
                } else {
                    format!(
                        "管理页：游戏设置改为单独一套（内存 {} MiB，Java {}）",
                        config.memory_mb(),
                        match &config.java_path {
                            Some(path) => path.display().to_string(),
                            None => "自动".to_string(),
                        }
                    )
                }
            }
        }
    }
}

pub struct State {
    /// 实例名（也就是 `versions/<名字>/`）。
    pub version: String,
    /// 它挂在哪个原版版本上。
    pub mc_version: String,
    /// 上游 id（`_aicraftOfficialId`），用来认出加载器。
    pub official_id: String,
    pub section: Section,
    /// 这一份实例自己的目录（`versions/<实例名>`），跟共享/隔离无关。
    pub version_path: PathBuf,
    /// 游戏实际使用的目录：隔离实例是版本目录，共享模式是 `.minecraft` 根。
    pub root: PathBuf,
    pub shared: bool,
    pub mods: Vec<Item>,
    pub packs: Vec<Item>,
    /// `shaderpacks/` 里的光影包。
    pub shaderpacks: Vec<Item>,
    /// 盘上有没有 `shaderpacks/` 这个目录。
    ///
    /// 单独记一个布尔值而不是每帧去问磁盘：这一栏出不出现就靠它，
    /// 而「有没有东西」不是判据 —— 空目录照样该让人点进去放东西。
    pub has_shaderpacks: bool,

    pub saves: Vec<Item>,
    /// 实例自己目录下 backups/ 里的世界备份（zip）。
    pub backups: Vec<Item>,
    pub error: Option<String>,
    pub loader: Option<(LoaderKind, String)>,
    /// 当前展开着哪个加载器（同一时刻只展开一个）。
    pub loader_open: Option<LoaderKind>,
    pub loader_versions: Vec<LoaderVersion>,
    pub loader_loading: bool,
    pub loader_error: Option<String>,
    /// 已经拿过「哪个加载器 + 哪个原版」的列表，避免每帧重发请求。
    pub loader_for: Option<(LoaderKind, String)>,
    /// 等用户确认删除的条目。
    pub pending_delete: Option<PathBuf>,
    /// 清单声明了父版本、而那个父版本**不在盘上**时，记下它的名字。
    ///
    /// 只有这种情况才真的启动不了，也才需要指引玩家去压平；
    /// 父版本好好装着的实例一切正常，界面上不该为它多占一行。
    pub missing_parent: Option<String>,
    /// 上次「检查更新」的结果，与 `mods` 一一对应（按路径认）。
    pub updates: Vec<Check>,
    /// 检查 / 更新正在跑时的一句话，界面据此禁用按钮。
    pub update_busy: Option<String>,
    /// 更新完留一句结果在标题旁边，不用去翻日志。
    pub update_notice: Option<(String, bool)>,
    /// 这一份实例自己的游戏设置（Java / 内存 / JVM 参数）。
    ///
    /// 存在 `versions/<实例>/aicraft-instance.json`，跟实例目录待在一起；
    /// 默认是「沿用全局」，玩家取消勾选才开始有自己的一套。
    pub game: aicraft_core::instancecfg::InstanceConfig,
    /// 导出整合包那张对话框的草稿；None 表示没开着。
    pub export: Option<ExportDraft>,
    /// 导出正在跑时的一句话（按钮据此变灰）。
    pub exporting: Option<String>,
    /// 上一次导出的结果，留在页面上让人看得见。
    pub export_notice: Option<(String, bool)>,
    /// 「更改游戏版本」那张对话框的草稿；None 表示没开着。
    pub change_game: Option<ChangeGameDraft>,
    /// 页头开始横移的时刻；None = 早就落位了。
    ///
    /// 进这一页、或者在左栏换了分区，都该重新播一次 —— 换了一栏等于换了
    /// 一页内容，标题和列表一起重来才读得出「这是一份新的东西」。
    pub head_at: Option<Instant>,
    /// 列表逐行进场的起点；None = 这一批早就进场完了。
    pub reveal_at: Option<Instant>,
    /// 是不是从首页那颗齿轮进来的。
    ///
    /// 只影响一件事：左上角「返回」该回到主页还是版本列表。管理页盖住
    /// 整个窗口时侧栏是藏着的，看不出页号，来路得自己记着。
    pub from_home: bool,
    /// 左栏最底下那组「浏览更多」展开了没有。
    ///
    /// 它不是一个分区，是一组「打开磁盘上某处」的快捷方式：点它不会换页，
    /// 只会弹出资源管理器。所以状态跟 `section` 分开记。
    pub more_open: bool,
}

/// 「更改游戏版本」对话框里选的东西。
///
/// 候选是**官方清单里的 Minecraft 版本**（跟「下载版本」页同一份数据），
/// 不是盘上的实例：这里要换的是这一份实例踩着的那个原版底座，跟
/// 「装过哪些实例」没有关系。没装过的版本也能选——点下去会先把它装上。
pub struct ChangeGameDraft {
    /// 搜索框里的字，用来筛版本号。
    pub query: String,
    /// 想换到的那个游戏版本。
    pub target: String,
}

impl ChangeGameDraft {
    /// 开一张新草稿：选中项先落在现在这一版上，玩家得自己挑个别的。
    pub fn new(current: &str) -> Self {
        ChangeGameDraft {
            query: String::new(),
            target: current.to_string(),
        }
    }

    /// 这个版本号要不要出现在列表里。
    pub fn matches(&self, id: &str) -> bool {
        let query = self.query.trim().to_lowercase();
        query.is_empty() || id.to_lowercase().contains(&query)
    }

    /// 有没有真的换了一个版本。
    pub fn changed(&self, current: &str) -> bool {
        !self.target.trim().is_empty() && self.target != current
    }
}

/// 导出对话框里填的东西。
pub struct ExportDraft {
    pub format: ExportFormat,
    pub name: String,
    pub version: String,
    /// 实例根目录下可以带走的东西，目录在前、文件在后。
    pub rows: Vec<aicraft_core::packexport::ExportEntry>,
    /// 勾中的那些（按名字，和 `rows` 一一对应地查）。
    ///
    /// 用名字而不是下标：重新扫一遍实例之后 `rows` 会变，下标对不上就
    /// 会静默地把勾选挪到别的目录上。
    pub ticked: Vec<String>,
}

impl ExportDraft {
    /// 开一张新草稿：名字默认跟着实例走，版本先给个 1.0.0。
    ///
    /// `rows` 是 [`aicraft_core::packexport::catalog`] 列出来的东西，勾选
    /// 先落在 `default_on` 的那几样上（模组、配置、资源包、光影包、
    /// options.txt），存档和日志留着让玩家自己决定。
    pub fn new(instance: &str, rows: Vec<aicraft_core::packexport::ExportEntry>) -> Self {
        let ticked = rows
            .iter()
            .filter(|row| row.default_on)
            .map(|row| row.name.clone())
            .collect();
        ExportDraft {
            format: ExportFormat::Modrinth,
            name: instance.to_string(),
            version: "1.0.0".to_string(),
            rows,
            ticked,
        }
    }

    /// 名字不能是空的：整合包里没名字，别人装完根本不知道这是什么。
    pub fn usable(&self) -> bool {
        !self.name.trim().is_empty()
    }

    /// 这一项勾了没有。
    pub fn is_ticked(&self, name: &str) -> bool {
        self.ticked.iter().any(|t| t == name)
    }

    /// 勾上或取消勾选。
    pub fn toggle(&mut self, name: &str) {
        match self.ticked.iter().position(|t| t == name) {
            Some(index) => {
                self.ticked.remove(index);
            }
            None => self.ticked.push(name.to_string()),
        }
    }

    /// 勾中的一共有多少个文件、多少字节。
    pub fn picked(&self) -> (usize, u64) {
        self.rows
            .iter()
            .filter(|row| self.is_ticked(&row.name))
            .fold((0usize, 0u64), |(files, bytes), row| {
                (files + row.files, bytes + row.bytes)
            })
    }

    /// 能不能点「选择位置并导出」。
    ///
    /// 除了名字，还要求至少勾了一样：一个空包别人装上去什么都没有，
    /// 与其让他等半天再看到一句报错，不如按钮现在就灰着。
    pub fn can_export(&self) -> bool {
        self.usable() && !self.ticked.is_empty()
    }
}

impl State {
    pub fn new(game_dir: &Path, version: &str) -> Self {
        let mut state = State {
            version: version.to_string(),
            mc_version: version.to_string(),
            official_id: version.to_string(),
            section: Section::Loader,
            version_path: aicraft_core::instance::version_dir(game_dir, version),
            root: game_dir.to_path_buf(),
            shared: true,
            mods: Vec::new(),
            packs: Vec::new(),
            shaderpacks: Vec::new(),
            has_shaderpacks: false,
            saves: Vec::new(),
            backups: Vec::new(),
            error: None,
            loader: None,
            loader_open: None,
            loader_versions: Vec::new(),
            loader_loading: false,
            loader_error: None,
            loader_for: None,
            pending_delete: None,
            missing_parent: None,
            updates: Vec::new(),
            update_busy: None,
            update_notice: None,
            game: aicraft_core::instancecfg::load(game_dir, version),
            export: None,
            exporting: None,
            export_notice: None,
            change_game: None,
            head_at: Some(Instant::now()),
            reveal_at: Some(Instant::now()),
            from_home: false,
            more_open: false,
        };
        state.refresh_identity(game_dir);
        state.reload(game_dir);
        state
    }

    /// 认出「挂在哪个原版上」与「装了什么加载器」。只读清单，不动磁盘。
    pub fn refresh_identity(&mut self, game_dir: &Path) {
        // 只读这一份自己的清单，不去追整条继承链：父版本可能压根没装，
        // 那时链会解析失败，但我们仍然需要知道「它挂在哪个原版上」。
        let json_path = aicraft_core::instance::version_dir(game_dir, &self.version)
            .join(format!("{}.json", self.version));
        if let Ok(text) = std::fs::read_to_string(&json_path)
            && let Ok(raw) = aicraft_core::version::parse_version(&text) {
            // 优先信 `inheritsFrom`：那是我自己装的实例，父版本号写得明明白白。
            // 别的启动器常常不写它（PCL 把实例当独立版本），这时只能从
            // 清单内容里推——`--fml.mcVersion`、Fabric 的 intermediary、
            // 再不行才从 id 里挑版本号（`1.20.1-Fabric` → `1.20.1`）。
            self.mc_version = match raw.inherits_from.clone() {
                Some(parent) => parent,
                None => aicraft_core::loader::detect_game_version(&text)
                    .unwrap_or_else(|| raw.id.clone()),
            };
        }
        self.official_id = aicraft_core::install::instance_official_id(game_dir, &self.version)
            .unwrap_or_else(|| self.version.clone());
        self.loader = aicraft_core::loader::detect_installed(game_dir, &self.version);
        // 只有「声明了父版本、父版本又不在盘上」才真的启动不了。
        // 父版本好好装着的实例一切正常，别在界面上为它多开一张卡片。
        self.missing_parent = aicraft_core::version::missing_parent(game_dir, &self.version);
    }

    /// 重新扫磁盘上的模组、资源包、存档。
    pub fn reload(&mut self, game_dir: &Path) {
        let mode = aicraft_core::instance::detect_mode(game_dir, &self.version);
        self.shared = mode == aicraft_core::instance::InstanceMode::Shared;
        // 共享模式下东西都在 `.minecraft` 根目录里，这里是只读视角：
        // 列出来是为了让玩家知道「你加的模组在别处」，删的时候会提醒。
        self.root = aicraft_core::instance::game_dir_for(game_dir, &self.version, mode);

        self.mods = instance::list_files(&self.root.join("mods"), &["jar", "zip", "disabled"]);
        self.packs = instance::list_packs(&self.root.join("resourcepacks"));

        // 光影包先看目录在不在：原版不建它，玩家装了 Iris / Oculus 之后才会有。
        // 判据是**目录在不在**，不是里头有没有包 —— 空目录也该让人点进去放东西。
        let shader_dir = self.root.join("shaderpacks");
        self.has_shaderpacks = shader_dir.is_dir();
        self.shaderpacks = if self.has_shaderpacks {
            instance::list_shaderpacks(&shader_dir)
        } else {
            Vec::new()
        };

        self.saves = instance::list_dirs(&self.root.join("saves"));
        // 备份跟着实例走（versions/<实例>/backups），跟共享/隔离无关：
        // 共享模式下这个目录照样长在实例自己身上。
        self.backups = instance::list_files(&self.version_path.join("backups"), &["zip"]);
        // 备份被删光了、或者本来就没有：这一栏不该继续留在界面上，
        // 否则玩家会盯着一份空列表发愣。
        if self.section == Section::Backups && self.backups.is_empty() {
            self.section = Section::Loader;
        }
        // 光影包目录没了（换了客户端、把 shaderpacks 删了）同理：
        // 页面上不该留一个已经点不进去的空壳。
        if self.section == Section::ShaderPacks && !self.has_shaderpacks {
            self.section = Section::Loader;
        }
        // 重扫之后列表是另一批东西了，逐行进场从头再来一遍；标题没换，
        // 页头就不必重播。
        self.reveal_at = Some(Instant::now());
        self.error = None;
    }

    pub fn set_loader_versions(
        &mut self,
        kind: LoaderKind,
        mc_version: String,
        list: Vec<LoaderVersion>,
    ) {
        if self.loader_open == Some(kind) && mc_version == self.mc_version {
            self.loader_for = Some((kind, mc_version));
            self.loader_versions = list;
        }
        self.loader_loading = false;
        self.loader_error = None;
    }

    /// 这个加载器的列表拿过了没有（没有就该去要一次）。
    pub fn loader_is_fresh(&self, kind: LoaderKind) -> bool {
        matches!(&self.loader_for, Some((cached, mc)) if *cached == kind && *mc == self.mc_version)
    }

    pub fn fail_loader_versions(&mut self, message: String) {
        self.loader_loading = false;
        self.loader_error = Some(message);
    }

    /// 收下「检查更新」的结果。
    pub fn set_updates(&mut self, list: Vec<Check>) {
        self.update_busy = None;
        // 认得出且有新版的算「可以更新」，其余不进结果，免得界面上多一批
        // 说「你不用动」的行。
        self.updates = list.into_iter().filter(|c| c.is_update()).collect();
        self.update_notice = Some(if self.updates.is_empty() {
            ("检查完了：模组都是最新的".to_string(), true)
        } else {
            (format!("{} 个模组有新版本", self.updates.len()), true)
        });
    }

    /// 这个 jar 有没有待更新版本。
    pub fn pending_update(&self, path: &Path) -> Option<&Check> {
        self.updates.iter().find(|c| c.path == path)
    }

    /// 这个分区里有多少条，用于左侧导航后面的计数。
    pub fn count(&self, section: Section) -> usize {
        match section {
            Section::Loader => usize::from(self.loader.is_some()),
            Section::Game => 0,
            Section::Mods => self.mods.len(),
            Section::ResourcePacks => self.packs.len(),
            Section::ShaderPacks => self.shaderpacks.len(),
            Section::Saves => self.saves.len(),
            Section::Backups => self.backups.len(),
        }
    }

    /// 这一栏现在该不该出现在左边。
    ///
    /// 两栏是有条件的：备份看里头有没有东西，光影包看那个目录在不在。
    pub fn shows(&self, section: Section) -> bool {
        match section {
            // 空着的一格只会让人去点一下看看是什么。
            Section::Backups => !self.backups.is_empty(),
            // 目录在就说明玩家装了光影，哪怕里头还空着也该让他点进去放东西。
            Section::ShaderPacks => self.has_shaderpacks,
            _ => true,
        }
    }
}

/// 左栏最底下那组「浏览更多」：一个可以展开的头，底下两项直接开资源管理器。
///
/// 它不是分区 —— 点这两项不换页，页号还停在原来那一栏上，所以状态跟
/// `section` 分开记（`State::more_open`）。展开 / 收起交给 `CollapsingState`：
/// 它自带 openness 补间，也会记住上一次完全展开时的高度，逐帧裁出正在长
/// 出来的那一截，不用自己算。
///
/// 返回「这一帧是不是正在展开或收起」——调用方据此请求重绘，把动画推完。
fn more_group(ui: &mut egui::Ui, state: &mut State, action: &mut Option<Action>) -> bool {
    let id = egui::Id::unique(egui::IdSalt::new("manage-more"));
    let mut collapsing =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
    if collapsing.is_open() != state.more_open {
        // 点击与状态谁先谁后都可能：这里统一让状态说话，动画自己跟上。
        collapsing.set_open(state.more_open);
    }
    let openness = collapsing.openness(ui.ctx());

    // 头像一行：箭头 + 「浏览更多」。整行都能点，不用非得戳中那颗小箭头。
    let (rect, response) =
        ui.allocate_exact_size(egui::Vec2::new(ui.available_width(), 34.0), egui::Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(8), theme::CARD_HOVER);
    }
    let color = if response.hovered() {
        theme::accent_strong()
    } else {
        theme::TEXT_WEAK
    };
    // 箭头和字当成一整块摆到中间：这一栏所有行都围着中轴排，
    // 只有它左边多一颗箭头，仍按顶格放就会比别的行更靠左。
    let label = ui.painter().layout_no_wrap(
        "浏览更多".to_owned(),
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
        color,
    );
    const ARROW_REACH: f32 = 3.6;
    const ARROW_GAP: f32 = 8.0;
    let block = ARROW_REACH * 2.0 + ARROW_GAP + label.size().x;
    let arrow_x = rect.center().x - block * 0.5 + ARROW_REACH;
    theme::chevron_glyph(
        ui.painter(),
        egui::Pos2::new(arrow_x, rect.center().y),
        openness,
        color,
    );
    ui.painter().galley(
        egui::Pos2::new(
            arrow_x + ARROW_REACH + ARROW_GAP,
            rect.center().y - label.size().y * 0.5,
        ),
        label,
        color,
    );
    if response.clicked() {
        state.more_open = !state.more_open;
    }

    // 两处目录都按「游戏真正会写进去的地方」算，跟版本列表那张卡片同一套判断。
    let run_dir = state.root.clone();
    let log_dir = crate::game_log_dir(&run_dir);
    let crash_dir = crate::game_crash_dir(&run_dir);

    let running = openness > 0.0;
    if running {
        collapsing.show_body_unindented(ui, |ui| {
            ui.add_space(4.0);
            if theme::nav_action_sub(ui, "游戏日志", log_dir.is_some(), true).clicked()
                && let Some(dir) = &log_dir
            {
                aicraft_core::audit::detail(format!("管理页：打开日志目录 {}", dir.display()));
                *action = Some(Action::OpenFolder(dir.clone()));
            }
            ui.add_space(2.0);
            if theme::nav_action_sub(ui, "崩溃日志", crash_dir.is_some(), true).clicked()
                && let Some(dir) = &crash_dir
            {
                aicraft_core::audit::detail(format!("管理页：打开崩溃日志目录 {}", dir.display()));
                *action = Some(Action::OpenFolder(dir.clone()));
            }
        });
    } else {
        collapsing.store(ui.ctx());
    }

    (state.more_open && openness < 1.0) || (!state.more_open && openness > 0.0)
}

/// 画整页。调用方保证当前处于「管理」状态。
#[allow(clippy::too_many_arguments)]
pub fn page(
    ui: &mut egui::Ui,
    state: &mut State,
    game_dir: &Path,
    settings: &aicraft_core::settings::Settings,
    java: &mut crate::settings::JavaScan,
    icons: &mut crate::icons::Cache,
    official: &[crate::versions::Entry],
    busy: bool,
) -> Option<Action> {
    let mut action = None;

    egui::Panel::left("manage-nav")
        .exact_size(178.0)
        // 固定宽度，别让右边缘出现左右调整光标（布局本来就只按 178 算）。
        .resizable(false)
        .show_separator_line(false)
        .frame(
            egui::Frame::NONE
                .fill(theme::panel())
                .inner_margin(egui::Margin::symmetric(12, 14)),
        )
        .show(ui, |ui| {
            // 顶上一行：返回贴左，标题压在中轴上 —— 下面每一行都围着
            // 中轴排，标题要是不在，整列看着就缺了个头。
            //
            // 不用 `horizontal`：那样标题只能排在返回右边，到不了正中间。
            // 先占下整行，再把按钮放到左端、标题画在行心。
            let (row, _) = ui.allocate_exact_size(
                egui::Vec2::new(ui.available_width(), 26.0),
                egui::Sense::hover(),
            );
            // 返回只留一颗箭头、外面带框：这一栏只有 154 宽，
            // 写全「‹ 返回」会一路顶到中轴，跟居中的标题撞在一起。
            // 意思交给悬停提示补上。
            //
            // 整块放进「就是那一行」的子 Ui 里，按钮才不会另起一行 ——
            // 直接在父 Ui 上 allocate，光标已经走过这一行了。
            ui.scope_builder(egui::UiBuilder::new().max_rect(row), |ui| {
                let back = ui
                    .scope_builder(
                        egui::UiBuilder::new().max_rect(egui::Rect::from_center_size(
                            egui::Pos2::new(row.left() + 15.0, row.center().y),
                            egui::Vec2::new(30.0, 24.0),
                        )),
                        theme::back_button,
                    )
                    .inner
                    .on_hover_text("返回上一页");
                if back.clicked() {
                    action = Some(Action::Close);
                }
            });
            // 字号比页签大一号，颜色仍用那支淡灰：它是这一栏的名字，
            // 不需要靠深色去抢注意。
            let title = egui::WidgetText::from(
                egui::RichText::new("管理")
                    .size(16.0)
                    .strong()
                    .color(theme::TEXT_WEAK),
            )
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Body,
            );
            ui.painter().galley(
                egui::Pos2::new(
                    row.center().x - title.size().x * 0.5,
                    row.center().y - title.size().y * 0.5,
                ),
                title,
                theme::TEXT_WEAK,
            );
            // 整栏能滚：窗口矮的时候分区多了也不至于把最底下那组
            // 「浏览更多」挤出可视区。间距收得比别处紧一点，正常窗口高度下
            // 一屏刚好放得下，不用滚。
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // 侧栏本来就窄，行距比正文紧一点：这样 620 高的窗口里
                    // 展开「浏览更多」也能把两项都露出来。
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.add_space(6.0);
                    // 这一栏从上到下都围着中轴排：版本号和下面那些页签
                    // 要是各用一套对齐，整列看着就像被谁从左边推歪了。
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new(&state.version).size(15.0).strong());
                    });
                    ui.add_space(10.0);

                    let highlight = theme::NavHighlight::begin(ui);
                    for section in Section::ALL {
                        // 没有备份就不摆这一栏：空着的一格只会让人去点一下看看是什么。
                        if !state.shows(section) {
                            continue;
                        }
                        let selected = state.section == section;
                        let count = state.count(section);
                        let text = if section.has_files() {
                            format!("{}  {}", section.label(), count)
                        } else {
                            section.label().to_string()
                        };
                        let response = theme::nav_item(ui, &text, selected, true);
                        highlight.row(response.rect, selected);
                        if response.clicked() && state.section != section {
                            state.section = section;
                            // 换了一栏等于换了一页：标题重新从右边进来，列表再一条条
                            // 浮上来。跟「资源下载」切类别时重播页头是同一个道理。
                            state.head_at = Some(Instant::now());
                            state.reveal_at = Some(Instant::now());
                        }
                        ui.add_space(2.0);
                    }

                    // 最底下一组：不换页，只把磁盘上两处地方摆出来。
                    ui.add_space(6.0);
                    if more_group(ui, state, &mut action) {
                        // 展开了就重排一次：底下那两行是刚长出来的，
                        // 不给它机会量高度，收起时动画会从 0 跳到正确值。
                        ui.ctx().request_repaint();
                    }

                    highlight.finish(
                        ui,
                        egui::Id::unique(egui::IdSalt::new("manage-nav-highlight")),
                        settings.animate_ui,
                    );
                });
        });

    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(theme::bg_bottom())
                .inner_margin(egui::Margin::symmetric(22, 18)),
        )
        .show(ui, |ui| {
            // 页头横移的进度与列表逐行进场各算各的：标题先落位，行再一条条
            // 从下面浮上来，跟「下载版本」那一页是同一个节奏。
            let head = theme::head_elapsed_with(
                &mut state.head_at,
                settings.animate_ui,
                ui.ctx(),
                theme::HEAD_TOTAL_SECS,
            );
            // 标题与目录整块从右侧滑进来，跟「下载版本」那一页同一个起手式。
            let slide = theme::slide_progress(head, 0.0, theme::TITLE_SLIDE_SECS);
            let head_action = theme::title_slide(ui, slide, |ui| header_row(ui, state, busy));
            if let Some(next) = head_action {
                action = Some(next);
            }
            ui.add_space(10.0);
            let reveal = if state.section.has_files() {
                theme::row_reveal_elapsed(
                    &mut state.reveal_at,
                    settings.animate_ui,
                    ui.ctx(),
                    theme::ROW_REVEAL_DELAY,
                )
            } else {
                None
            };

            match state.section {
                Section::Loader => {
                    if let Some(next) = loader_section(ui, state, busy) {
                        action = Some(next);
                    }
                }
                Section::Game => {
                    // 窗口只有 620 高，这一栏的控件叠起来比它长：套一层滚动，
                    // 否则「保存」会掉到看不见的地方（还点不到）。
                    egui::ScrollArea::vertical()
                        .id_salt("manage-game")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if let Some(next) =
                                game_section(ui, state, game_dir, settings, java, busy)
                            {
                                action = Some(next);
                            }
                        });
                }
                Section::Mods => {
                    if let Some(next) = item_list(
                        ui,
                        state,
                        &state.mods.clone(),
                        "还没有模组。把 .jar 丢进这个文件夹就会出现在这里。",
                        icons,
                        Rows {
                            look: Look::Mod,
                            reveal,
                            busy,
                        },
                    ) {
                        action = Some(next);
                    }
                }
                Section::ResourcePacks => {
                    if let Some(next) = item_list(
                        ui,
                        state,
                        &state.packs.clone(),
                        "还没有资源包。把 .zip 或解压开的文件夹丢进这个文件夹就会出现在这里。",
                        icons,
                        Rows {
                            look: Look::Pack,
                            reveal,
                            busy,
                        },
                    ) {
                        action = Some(next);
                    }
                }
                Section::ShaderPacks => {
                    if let Some(next) = item_list(
                        ui,
                        state,
                        &state.shaderpacks.clone(),
                        "还没有光影包。把 .zip 或解压开的文件夹丢进这个文件夹，进游戏后在「视频设置 → 光影」里选它。",
                        icons,
                        Rows {
                            look: Look::Shader,
                            reveal,
                            busy,
                        },
                    ) {
                        action = Some(next);
                    }
                }
                Section::Saves => {
                    if let Some(next) = item_list(
                        ui,
                        state,
                        &state.saves.clone(),
                        "还没有存档。进游戏存一次，或者把别人的存档文件夹放进来。",
                        icons,
                        Rows {
                            look: Look::Save,
                            reveal,
                            busy,
                        },
                    ) {
                        action = Some(next);
                    }
                }
                Section::Backups => {
                    if let Some(next) = item_list(
                        ui,
                        state,
                        &state.backups.clone(),
                        "还没有备份。游戏里存过一次世界之后，备份会出现在这里。",
                        icons,
                        Rows {
                            look: Look::Save,
                            reveal,
                            busy,
                        },
                    ) {
                        action = Some(next);
                    }
                }
            }
        });

    if let Some(next) = confirm_delete(ui.ctx(), state, busy) {
        action = Some(next);
    }
    if let Some(next) = export_dialog(ui.ctx(), state, busy) {
        action = Some(next);
    }
    if let Some(next) = change_game_dialog(ui.ctx(), state, official, busy) {
        action = Some(next);
    }
    action
}

/// 每页顶上那一条：标题 + 目录 + 刷新 / 打开文件夹。
fn header_row(ui: &mut egui::Ui, state: &mut State, busy: bool) -> Option<Action> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(match state.section {
                Section::Loader => "概览",
                Section::Game => "游戏设置",
                Section::Mods => "模组列表",
                Section::ResourcePacks => "资源包列表",
                Section::ShaderPacks => "光影包列表",
                Section::Saves => "存档列表",
                Section::Backups => "备份列表",
            })
            .size(17.0)
            .strong(),
        );
        if state.section.has_files() {
            if theme::refresh_button(ui, !busy).clicked() {
                action = Some(Action::Reload);
            }
            ui.label(
                egui::RichText::new(format!("共 {} 个", state.count(state.section)))
                    .size(11.0)
                    .color(theme::TEXT_FAINT),
            );
        }
        // 只有模组能更新：资源包和存档在 Modrinth 上没有「同一个东西的下一版」
        // 这回事，把它们也挂上去只会让人点了个没反应。
        if state.section == Section::Mods {
            ui.add_space(8.0);
            let can_check = !busy && state.update_busy.is_none() && !state.mods.is_empty();
            if ui
                .add_enabled(can_check, egui::Button::new("检查更新").small())
                .on_hover_text("按文件指纹去 Modrinth 问一遍有没有新版本")
                .clicked()
            {
                action = Some(Action::CheckUpdates);
            }
            if !state.updates.is_empty() {
                ui.add_space(6.0);
                let label = format!("全部更新（{}）", state.updates.len());
                if ui
                    .add_enabled(can_check, egui::Button::new(label).small())
                    .on_hover_text("把上面这些一个个换成新版本")
                    .clicked()
                {
                    action = Some(Action::UpdateAll);
                }
            }
        }
        if let Some(text) = &state.update_busy {
            ui.add_space(8.0);
            ui.spinner();
            ui.label(
                egui::RichText::new(text)
                    .size(11.0)
                    .color(theme::accent_strong()),
            );
        } else if let Some((text, ok)) = &state.update_notice {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(text).size(11.0).color(if *ok {
                theme::accent_strong()
            } else {
                theme::DANGER
            }));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if state.section.has_files()
                && ui
                    .add_enabled(!busy, egui::Button::new("打开文件夹"))
                    .on_hover_text("在资源管理器里打开这个目录")
                    .clicked()
            {
                action = Some(Action::OpenFolder(section_dir(state, state.section)));
            }
        });
    });

    if state.section.has_files() {
        let dir = section_dir(state, state.section);
        ui.label(
            egui::RichText::new(dir.display().to_string())
                .size(10.5)
                .color(theme::TEXT_WEAK),
        );
        if state.shared {
            ui.label(
                egui::RichText::new("这个版本是共享目录：这里的文件由 .minecraft 根目录共用，改动会影响所有共享版本。")
                    .size(10.5)
                    .color(theme::WARN),
            );
        }
    }
    action
}

fn sub_dir_name(section: Section) -> &'static str {
    match section {
        Section::Loader | Section::Game => "",
        Section::Mods => "mods",
        Section::ResourcePacks => "resourcepacks",
        Section::ShaderPacks => "shaderpacks",
        Section::Saves => "saves",
        Section::Backups => "backups",
    }
}

/// 这一栏对应的磁盘目录。
///
/// 备份是唯一不跟着游戏目录走的：它长在实例自己身上，共享模式下也一样。
fn section_dir(state: &State, section: Section) -> PathBuf {
    match section {
        Section::Backups => state.version_path.join("backups"),
        _ => state.root.join(sub_dir_name(section)),
    }
}

/// 图标那一格连同它后面的空隙有多宽。
///
/// 没有图标的行要留出同样的宽度，文字才跟别的行对得齐。
const ICON_SLOT_WIDTH: f32 = 40.0 + 11.0;

/// 列表：一行一个条目，右边是「打开」与「删除」。
/// 模组名前面那一枚图标的位置。
///
/// 还没取到就画一块圆角灰底：先在列表里占住位子，等图像到了直接换上去，
/// 免得整行文字左右横跳。取不到（jar 里没图标）时同样留这块灰底 ——
/// 一列参差不齐的缩进比一片灰块更难看。
fn icon_slot(ui: &mut egui::Ui, icons: &crate::icons::Cache, key: &str, ready: bool) {
    // 40px：模组的图标本身就是这些图标包的名片（REI 的 9 格、architectury 的
    // 铁砧、Sodium 的草方块），26px 时只能看出「这里有个东西」。
    // 图标走 64px 纹理，缩到 40 也不糊。
    const SIZE: f32 = 40.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::hover());
    match (ready, icons.texture(key)) {
        (true, Some(texture)) => {
            egui::Image::from_texture(texture)
                .corner_radius(egui::CornerRadius::same(9))
                .paint_at(ui, rect);
        }
        _ => {
            let painter = ui.painter();
            painter.rect_filled(rect, egui::CornerRadius::same(9), theme::BG_BOTTOM);
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same(9),
                egui::Stroke::new(1.0, theme::BORDER),
                egui::StrokeKind::Inside,
            );
        }
    }
    ui.add_space(11.0);
}

/// 列表里每一条要怎么打扮。
///
/// 模组的脸压在 jar 里、资源包的脸是包里的 `pack.png`、存档是游戏截的那张
/// `icon.png`；光影包没有脸可画。名字的写法也不一样（资源包要按游戏里那样
/// 上色加粗，模组只写文件名）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    /// 模组：从 jar 里挖图标，名字就是文件名。
    Mod,
    /// 资源包：从包里取 `pack.png`，名字用 `pack.mcmeta` 里那句带颜色的话。
    Pack,
    /// 存档与备份：游戏存世界时截的 `icon.png`。
    Save,
    /// 光影包：包里没有图，游戏也不读图，这一栏就不画格子。
    Shader,
}

/// 列表这一批的打扮：怎么取图标、进场进度、按钮能不能点。
///
/// 三个参数单独摆出来会让 `item_list` 顶到 8 个形参（clippy 的上限是 7），
/// 而它们本来就是「这一批怎么画」的同一件事，收成一份更顺。
struct Rows {
    look: Look,
    reveal: Option<f32>,
    busy: bool,
}

/// 一个条目列表（模组 / 资源包 / 存档共用）。
fn item_list(
    ui: &mut egui::Ui,
    state: &mut State,
    items: &[Item],
    empty_hint: &str,
    icons: &mut crate::icons::Cache,
    rows: Rows,
) -> Option<Action> {
    let Rows { look, reveal, busy } = rows;
    let mut action = None;
    if items.is_empty() {
        // 空列表不画框：一句淡字居中说明就行，套卡片反而像有个空盒子悬在那儿。
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(empty_hint)
                    .size(11.5)
                    .color(theme::TEXT_WEAK),
            );
        });
        return None;
    }

    let dir = state.root.join(sub_dir_name(state.section));
    egui::ScrollArea::vertical()
        .id_salt("manage-list")
        .max_height((ui.available_height() - 8.0).max(160.0))
        .show(ui, |ui| {
            for (index, item) in items.iter().enumerate() {
                theme::reveal_row(ui, index, reveal, |ui| {
                theme::card().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        match look {
                            Look::Mod => {
                                // 键用整个路径（不只是文件名）：不同版本目录里
                                // 同名的模组是两件东西，共用一张脸会串。
                                let key = format!("mod:{}", item.path.display());
                                let ready = icons.want_jar(&key, &item.path);
                                icon_slot(ui, icons, &key, ready);
                            }
                            Look::Pack => {
                                let key = format!("pack:{}", item.path.display());
                                // 包里没带 pack.png 的（本机就有一个）：灰块跟「还在加载」
                                // 长得一模一样，会让人以为卡住了。这里只留白，让文字
                                // 仍然跟别的行对齐。
                                if icons.is_failed(&key) {
                                    ui.add_space(ICON_SLOT_WIDTH);
                                } else {
                                    let ready = icons.want_pack(&key, &item.path);
                                    icon_slot(ui, icons, &key, ready);
                                }
                            }
                            // 光影包这一栏不留格子：包里没有图，游戏那边也不读
                            // 缩略图，硬凑一张只会让人以为那是作者画的。
                            Look::Shader => {}
                            Look::Save => {
                                let key = format!("save:{}", item.path.display());
                                if icons.is_failed(&key) {
                                    ui.add_space(ICON_SLOT_WIDTH);
                                } else {
                                    let ready = icons.want_save(&key, &item.path);
                                    icon_slot(ui, icons, &key, ready);
                                }
                            }
                        }
                        let update = if look == Look::Mod {
                            state.pending_update(&item.path).cloned()
                        } else {
                            None
                        };
                        ui.vertical(|ui| {
                            let shown = match look {
                                Look::Pack => pack_label(&item.name),
                                _ => item.name.clone(),
                            };
                            ui.label(egui::RichText::new(shown).size(12.5).strong());
                            // 有新版本时这一行换成「0.6.9 → 0.6.13」：
                            // 玩家要知道自己从哪儿升到哪儿，光一个「更新」不敢点。
                            match &update {
                                Some(check) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} → {}",
                                            check.current.as_deref().unwrap_or("现在这版"),
                                            check
                                                .latest
                                                .as_ref()
                                                .map(|l| l.version_number.as_str())
                                                .unwrap_or("新版")
                                        ))
                                        .size(10.5)
                                        .color(theme::accent_strong()),
                                    );
                                }
                                None => {
                                    ui.label(
                                        egui::RichText::new(crate::human_size(item.bytes as f64))
                                            .size(10.5)
                                            .color(theme::TEXT_WEAK),
                                    );
                                }
                            };
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add_enabled(!busy, egui::Button::new("删除")).clicked() {
                                state.pending_delete = Some(item.path.clone());
                            }
                            if ui.add_enabled(!busy, egui::Button::new("打开")).clicked() {
                                action = Some(Action::OpenFolder(item.path.clone()));
                            }

                            if update.is_some()
                                && ui
                                    .add_enabled(!busy, egui::Button::new("更新"))
                                    .on_hover_text("换成这个项目适合当前实例的最新版")
                                    .clicked()
                            {
                                action = Some(Action::UpdateMod(item.path.clone()));
                            }
                        });
                    });
                });
                });
                ui.add_space(6.0);
            }
        });
    let _ = dir;
    action
}

/// 资源包的文件名里常被作者塞进 `§6`、`§l` 这类游戏格式码 —— 他是指望游戏
/// 渲染成彩色名字，可到了资源管理器和这里就是一串 `§r§0`。列表里把代码去掉，
/// 只留文字；去掉之后什么都不剩（比如名字只由格式码组成）就退回原名。
fn pack_label(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut chars = name.chars();
    while let Some(ch) = chars.next() {
        if ch == '§' {
            chars.next();
            continue;
        }
        out.push(ch);
    }
    // 代码两边常留着空格，剥掉之后就变成两个挨在一起，顺手压成一个。
    let mut squeezed = String::with_capacity(out.len());
    for ch in out.chars() {
        if ch == ' ' && squeezed.ends_with(' ') {
            continue;
        }
        squeezed.push(ch);
    }
    let trimmed = squeezed.trim();
    if trimmed.is_empty() {
        name.to_string()
    } else {
        trimmed.to_string()
    }
}

/// 概览分区：这一份实例长什么样——游戏版本、加载器、以及导出的入口。
///
/// 叫「概览」而不是「加载器」是因为它现在管的不止加载器：底座版本能换、
/// 加载器能换、整份实例还能导出。从整合包导入的实例会把前两项锁住
/// （见 `aicraft_core::instancecfg::InstanceConfig::from_pack`）。
fn loader_section(ui: &mut egui::Ui, state: &mut State, busy: bool) -> Option<Action> {
    let mut action = None;

    // 从整合包导入的实例：游戏版本与加载器种类都锁住。
    // 包里那一套组合是打包的人验过的，换掉之后模组大概率当场报错，
    // 与其让人换完再回来问「为什么崩了」，不如一开始就不给这个按钮。
    // 加载器版本仍然可以换（同一种加载器内的版本更替不影响包的兼容性）。
    let from_pack = state.game.from_pack;

    theme::card().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("Minecraft {}", state.mc_version))
                    .size(12.5)
                    .color(theme::TEXT),
            );
            // 加载器是谁：版本列表那一行只画了一枚形状，这儿补上名字。
            if let Some((kind, version)) = state.loader.clone() {
                ui.add_space(6.0);
                theme::chip_loader(ui, Some(kind), &format!("{} {}", kind.label(), version));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if from_pack {
                    theme::chip(ui, "整合包锁定");
                } else if ui
                    .add_enabled(!busy, egui::Button::new("更换").small())
                    .clicked()
                {
                    state.change_game = Some(ChangeGameDraft::new(&state.mc_version));
                }
                ui.add_space(6.0);
                // 版本列表那一行原来挂着「校验资源」，详情卡片撤掉之后
                // 这个动作归到「概览」：它管的正是这一份实例整体完不完整。
                if ui
                    .add_enabled(!busy, egui::Button::new("校验资源").small())
                    .on_hover_text("挨个核对库、资源与客户端 jar，缺什么会列出来")
                    .clicked()
                {
                    action = Some(Action::VerifyResources);
                }
            });
        });
        ui.add_space(2.0);
        // 副标题给的是这一份实例的目录：玩家想加模组、找存档、改配置，
        // 最终都要落到这个路径上，写出来比「现在用 Fabric x.y」有用。
        // 加载器是谁，下面三张卡片已经说得更清楚了。
        let path_text = state.version_path.display().to_string();
        ui.add(
            egui::Label::new(
                egui::RichText::new(&path_text)
                    .size(11.0)
                    .color(theme::TEXT_WEAK),
            )
            .truncate(),
        )
        .on_hover_text(path_text);
    });
    ui.add_space(10.0);

    // 只在真的缺底座时出现：清单里写着父版本，盘上却没有那个目录。
    // 这种实例现在一启动就只剩一句「系统找不到指定的路径」，
    // 压平之后它自己就是完整的，连底座都不用装。
    if let Some(parent) = state.missing_parent.clone() {
        let mut flatten = false;
        theme::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            // 按钮就贴着标题放，不用右对齐：内容区一旦比窗口宽，
            // 右对齐会把按钮顶到可视范围之外（这一页的卡片本来就容易溢出）。
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("缺底座：{}", parent))
                        .size(12.5)
                        .color(theme::WARN),
                );
                ui.add_space(10.0);
                if ui
                    .add_enabled(!busy, egui::Button::new("压平"))
                    .on_hover_text("把继承链合并进这一份清单，实例里的东西不动")
                    .clicked()
                {
                    flatten = true;
                }
            });
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "这份清单说它建立在 {} 上，但 versions/{} 不在盘上，所以现在启动不了。",
                        parent, parent
                    ))
                    .size(10.5)
                    .color(theme::TEXT_WEAK),
                )
                .wrap(),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "压平会把 {} 的内容合并进来，之后不用装它也能启动。",
                        parent
                    ))
                    .size(10.5)
                    .color(theme::TEXT_WEAK),
                )
                .wrap(),
            );
        });
        ui.add_space(10.0);
        if flatten {
            // 立刻交出去：下面的加载器列表还可能再发一次「取版本列表」，
            // 那个动作会把这个覆盖掉，压平就点了没反应。
            return Some(Action::Flatten);
        }
    }

    let installed = state.loader.clone();

    for kind in LoaderKind::ALL {
        let blocked = matches!(&installed, Some((k, _)) if *k != kind);
        // 整合包导入的实例：换到别家加载器会把包里那套组合打散，锁住。
        // 同一家内部的版本更替仍然放行——那是「换版本」，不是「换种类」。
        let locked = from_pack && blocked;
        let open = state.loader_open == Some(kind);
        let mut toggled = false;
        let mut chosen: Option<String> = None;

        theme::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                // 图标排在名字前面：三家的名字头两个字母一模一样，
                // 一列排下来光读字很难分，有个形状先落进眼睛里。
                let (icon_rect, _) =
                    ui.allocate_exact_size(egui::Vec2::splat(16.0), egui::Sense::hover());
                theme::loader_glyph(ui, icon_rect, Some(kind));
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(kind.label())
                        .size(13.5)
                        .strong()
                        .color(if blocked {
                            theme::TEXT_FAINT
                        } else {
                            theme::TEXT
                        }),
                );
                ui.add_space(8.0);
                match &installed {
                    Some((k, version)) if *k == kind => {
                        theme::chip(ui, "已安装");
                        ui.label(
                            egui::RichText::new(version)
                                .size(11.5)
                                .color(theme::TEXT_WEAK),
                        );
                    }
                    Some((k, version)) => {
                        ui.label(
                            egui::RichText::new(if locked {
                                // 锁住时换个说法：不是「换了会怎样」，而是「不给换」。
                                format!("整合包锁住了 {}，只能换它的版本", k.label())
                            } else {
                                format!("换成这个会替掉 {} {}", k.label(), version)
                            })
                            .size(11.0)
                            .color(if locked { theme::TEXT_FAINT } else { theme::WARN }),
                        );
                    }
                    None => {
                        ui.label(
                            egui::RichText::new("可安装")
                                .size(11.0)
                                .color(theme::accent_strong()),
                        );
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let chevron = theme::chevron_button(ui, open, !busy && !locked);
                    // 锁定这件事不说清，玩家只会以为箭头坏了。
                    let chevron = if locked {
                        chevron.on_hover_text("这份实例是从整合包导入的，加载器种类锁住了")
                    } else {
                        chevron
                    };
                    if chevron.clicked() {
                        toggled = true;
                    }
                });
            });

            if !open {
                return;
            }

            ui.add_space(8.0);
            if state.loader_loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(format!("正在要 {} 的版本列表…", kind.label()))
                            .size(11.0)
                            .color(theme::TEXT_WEAK),
                    );
                });
                return;
            }
            if let Some(err) = &state.loader_error {
                ui.label(
                    egui::RichText::new(format!("没拿到列表：{}", err))
                        .size(11.0)
                        .color(theme::DANGER),
                );
                return;
            }
            if state.loader_versions.is_empty() {
                return;
            }
            let current = installed
                .as_ref()
                .filter(|(k, _)| *k == kind)
                .map(|(_, version)| version.clone());
            let versions = state.loader_versions.clone();
            egui::ScrollArea::vertical()
                .id_salt("manage-loader-list")
                .max_height(168.0)
                .show(ui, |ui| {
                    for lv in versions {
                        let is_current = current.as_deref() == Some(lv.version.as_str());
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&lv.version).size(12.0).color(
                                if is_current {
                                    theme::accent_strong()
                                } else {
                                    theme::TEXT
                                },
                            ));
                            if is_current {
                                theme::chip(ui, "当前");
                            } else if !lv.stable {
                                theme::chip(ui, "预览版");
                            }
                            if is_current {
                                return;
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    // 已经有一个加载器时（不管是同一家换版本，还是换另一家），
                                    // 这一行的动作都是「切换」；纯原版装第一个才叫「安装」。
                                    let label = if current.is_some() || blocked {
                                        "切换"
                                    } else {
                                        "安装"
                                    };
                                    if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                                        chosen = Some(lv.version.clone());
                                    }
                                },
                            );
                        });
                        ui.add_space(4.0);
                    }
                });
        });

        if toggled {
            if open {
                state.loader_open = None;
            } else {
                state.loader_open = Some(kind);
                state.loader_versions.clear();
                state.loader_error = None;
                state.loader_for = None;
            }
        }

        if let Some(version) = chosen {
            return Some(Action::SwitchLoader { kind, version });
        }

        if state.loader_open == Some(kind)
            && !state.loader_loading
            && state.loader_error.is_none()
            && !state.loader_is_fresh(kind)
        {
            state.loader_loading = true;
            action = Some(Action::LoadLoaderVersions(kind));
        }

        ui.add_space(8.0);
    }

    // 导出整合包。放在加载器这一栏的末尾：它读的是整份实例（模组、配置、
    // 清单），不属于下面任何一个「按文件列」的分区，而加载器页正好是
    // 「这一份实例整体长什么样」的地方。
    theme::card().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("导出整合包")
                    .size(12.5)
                    .color(theme::TEXT),
            );
            ui.add_space(10.0);
            let can = !busy && state.exporting.is_none();
            if ui
                .add_enabled(can, egui::Button::new("导出").small())
                .clicked()
            {
                let rows = aicraft_core::packexport::catalog(&state.version_path, &state.version);
                state.export = Some(ExportDraft::new(&state.version, rows));
                state.export_notice = None;
            }
            if let Some(text) = &state.exporting {
                ui.add_space(6.0);
                ui.spinner();
                ui.label(
                    egui::RichText::new(text)
                        .size(11.0)
                        .color(theme::accent_strong()),
                );
            } else if let Some((text, ok)) = &state.export_notice {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(text).size(11.0).color(if *ok {
                    theme::accent_strong()
                } else {
                    theme::DANGER
                }));
            }
        });
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    "把 mods、config、资源包和这一份的版本清单收进一个文件，别人导入后就能玩到同一套。",
                )
                .size(11.0)
                .color(theme::TEXT_WEAK),
            )
            .truncate(),
        )
        .on_hover_text("日志、截图、存档不进包：那是这台机器的痕迹，不是整合包的一部分。");
    });

    // 删掉这一份实例。单开一张卡片、用警示色：它不可逆，不该和上面那些
    // 「换版本」的按钮挤在同一行里，让人顺手就点下去。
    theme::card().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(
                        egui::RichText::new("删除这份实例").color(theme::DANGER),
                    ),
                )
                .clicked()
            {
                action = Some(Action::DeleteInstance);
            }
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("versions/ 下的这一层目录会整个删掉")
                    .size(10.5)
                    .color(theme::TEXT_WEAK),
            );
        });
    });
    ui.add_space(8.0);

    // 只在纯原版时留一句说明：装好加载器之后，那几行状态（「已安装」/「会替掉 …」）
    // 已经把该说的都说完了，底下再挂一段小字只会让这一页更啰嗦。
    if action.is_none() && installed.is_none() {
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("这些加载器都建立在同一个原版之上：装好后启动参数会自动带上它们。")
                .size(10.5)
                .color(theme::TEXT_FAINT),
        );
    }

    action
}

/// 「游戏设置」那一栏：这一份实例自己的 Java、内存与 JVM 参数。
///
/// 默认整块沿用全局设置——绝大多数人不会有第二个想法，把三个控件直接摊在
/// 这里只会让人以为「每个版本都得单独调」。取消勾选之后才长出下面这一套，
/// 而且改完要按「保存」才落盘：这些值会直接进下一次启动的命令行，边拖边存
/// 反而容易出现「拖到一半关掉，留下一个半截的数字」。
fn game_section(
    ui: &mut egui::Ui,
    state: &mut State,
    game_dir: &Path,
    settings: &aicraft_core::settings::Settings,
    java: &mut crate::settings::JavaScan,
    busy: bool,
) -> Option<Action> {
    let mut action = None;
    // 「自动选择」挑谁，取决于这一份版本的清单，两张卡都要用同一个结论。
    let instance = state.version.clone();
    java.start();
    let runtimes = java.runtimes();
    let auto = auto_java(game_dir, &instance, runtimes);
    let game = &mut state.game;
    let was_following = game.follow_global;

    // 勾选框这一行永远在：取消勾选之后要能勾回去。
    let mut toggled = false;
    ui.horizontal(|ui| {
        let response = ui.add_enabled(
            !busy,
            egui::Checkbox::new(&mut game.follow_global, "沿用全局设置"),
        );
        toggled = theme::hand(response).changed();
        ui.label(
            egui::RichText::new("关掉之后这一份用自己的 Java、内存和 JVM 参数")
                .size(10.5)
                .color(theme::TEXT_WEAK),
        );
    });
    // 勾回去是「不再用自己那一套」这个决定本身，不用再点一次保存：
    // 不落盘的话下次启动还会捡起旧的那份设置，勾选框就成了摆设。
    if toggled && game.follow_global {
        action = Some(Action::SaveGameConfig(Box::new(game.clone())));
    }

    if game.follow_global {
        ui.add_space(10.0);
        theme::card().show(ui, |ui| {
            ui.set_width((ui.available_width() - 32.0 - 24.0).max(320.0));
            label_row(ui, "内存", &format!("{} MiB", settings.memory_mb()));
            let java_value = match &settings.java_path {
                Some(path) => crate::settings::describe_java(path, runtimes),
                // 自动也有确定的答案：这一份版本实际会用哪个 Java。
                None => auto.text.clone(),
            };
            label_row(ui, "Java", &java_value);
            label_row(
                ui,
                "JVM 参数",
                if settings.jvm_args.trim().is_empty() {
                    "（没有额外参数）"
                } else {
                    settings.jvm_args.trim()
                },
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("想单独调这一份，就把上面的勾去掉。")
                    .size(10.5)
                    .color(theme::TEXT_FAINT),
            );
        });
        // 注意别直接 return None：刚勾回「沿用全局」时那一次保存就挂在这里。
        return action;
    }

    // 刚在这一次点击里从「沿用全局」切过来时，下面那几个值还是这份文件里的
    // 旧默认（4096 / 自动 / 空），跟玩家刚才在全局页看到的对不上，先对齐一次。
    // 只在**这一次点击**里抄：文件里本来就存着一套的，按它自己的来。
    if was_following && !game.follow_global {
        seed_from_global(game, settings);
    }

    ui.add_space(10.0);
    theme::card().show(ui, |ui| {
        ui.set_width((ui.available_width() - 32.0 - 24.0).max(320.0));

        // ---- 内存 ----
        ui.label(
            egui::RichText::new("内存")
                .size(12.0)
                .color(theme::TEXT_WEAK),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if theme::radio(ui, game.auto_memory, "自动分配内存").clicked() {
                game.auto_memory = true;
            }
            ui.add_space(18.0);
            if theme::radio(ui, !game.auto_memory, "手动选择内存").clicked() {
                if game.auto_memory
                    && let Some(memory) = aicraft_core::meminfo::cached() {
                    game.max_memory_mb = aicraft_core::meminfo::recommended_mb(memory.total_mb);
                }
                game.auto_memory = false;
            }
        });
        ui.add_space(8.0);
        let manual = !game.auto_memory;
        let shown_mb = game.memory_mb();
        ui.horizontal(|ui| {
            const BOX_W: f32 = 74.0;
            let slider_w = (ui.available_width() - BOX_W - 46.0).max(80.0);
            let ceil = aicraft_core::meminfo::cached()
                .map(|m| crate::settings::slider_ceiling(m.total_mb))
                .unwrap_or(crate::settings::MEMORY_CEIL_MB);
            let mut ratio = (shown_mb.saturating_sub(crate::settings::MEMORY_FLOOR_MB) as f32
                / (ceil - crate::settings::MEMORY_FLOOR_MB) as f32)
                .clamp(0.0, 1.0);
            let drag = ui
                .allocate_ui_with_layout(
                    egui::vec2(slider_w, 24.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| theme::slider(ui, &mut ratio, manual),
                )
                .inner;
            if drag.changed() && manual {
                game.max_memory_mb = crate::settings::snap_mb(
                    crate::settings::MEMORY_FLOOR_MB
                        + (ratio * (ceil - crate::settings::MEMORY_FLOOR_MB) as f32) as u32,
                );
            }
            ui.add_space(8.0);
            let mut typed = shown_mb;
            let box_resp = ui.add_enabled(
                manual,
                egui::DragValue::new(&mut typed)
                    .range(
                        crate::settings::MEMORY_FLOOR_MB..=crate::settings::MEMORY_CEIL_MB,
                    )
                    .speed(64.0)
                    .suffix(""),
            );
            if box_resp.changed() {
                game.max_memory_mb = crate::settings::snap_mb(typed);
            }
            ui.label(egui::RichText::new("MiB").size(11.5).color(theme::TEXT_WEAK));
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(if game.auto_memory {
                format!("按本机内存自动分配，现在是 {} MiB。", game.memory_mb())
            } else {
                format!(
                    "手动指定 {} MiB，会作为 -Xmx 传给这一份游戏。",
                    game.max_memory_mb
                )
            })
            .size(10.5)
            .color(theme::TEXT_WEAK),
        );

        crate::settings::divider(ui);

        // ---- Java ----
        ui.label(
            egui::RichText::new("Java")
                .size(12.0)
                .color(theme::TEXT_WEAK),
        );
        ui.add_space(4.0);
        let current_text = match &game.java_path {
            None => auto.text.clone(),
            Some(path) => crate::settings::describe_java(path, runtimes),
        };
        let combo = theme::combo(
            ui,
            "manage-java-choice",
            egui::RichText::new(current_text).size(12.0),
            ui.available_width(),
            |ui| {
                ui.selectable_value(&mut game.java_path, None, "自动选择");
                match runtimes {
                    None => {
                        ui.label(
                            egui::RichText::new("正在扫描本机的 Java…")
                                .size(11.0)
                                .color(theme::TEXT_WEAK),
                        );
                    }
                    Some([]) => {
                        ui.label(
                            egui::RichText::new("没扫到本机的 Java；去设置页「添加 Java 路径」挑一个")
                                .size(11.0)
                                .color(theme::TEXT_WEAK),
                        );
                    }
                    Some(list) => {
                        for rt in list {
                            let label =
                                format!("Java {} · {}", rt.major_version, rt.home().display());
                            ui.selectable_value(&mut game.java_path, Some(rt.path.clone()), label);
                        }
                    }
                }
            },
        );
        theme::hand(combo);

        crate::settings::divider(ui);

        // ---- JVM 参数 ----
        ui.label(
            egui::RichText::new("JVM 参数")
                .size(12.0)
                .color(theme::TEXT_WEAK),
        );
        ui.add_space(4.0);
        let mut resp = ui.add(
            egui::TextEdit::multiline(&mut game.jvm_args)
                .hint_text("-XX:+UseG1GC")
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        theme::input_menu(&mut resp, &mut game.jvm_args, false);

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if theme::primary_button(ui, "保存", !busy).clicked() {
                action = Some(Action::SaveGameConfig(Box::new(game.clone())));
            }
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("只影响这一份；下次启动生效。")
                    .size(10.5)
                    .color(theme::TEXT_WEAK),
            );
        });
    });

    // 卡片贴着滚动区底边时，「保存」那行会被切掉半截——留一点余量，
    // 滚到底能看见整颗按钮。
    ui.add_space(10.0);
    let _ = game_dir;
    action
}

/// 「跟着全局走」那张卡里的只读一行：左边项目名，右边值。
fn label_row(ui: &mut egui::Ui, name: &str, value: &str) {
    const NAME_W: f32 = 72.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 22.0),
        egui::Sense::hover(),
    );
    ui.painter().text(
        egui::pos2(rect.left(), rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(11.5),
        theme::TEXT_WEAK,
    );
    ui.painter().text(
        egui::pos2(rect.left() + NAME_W, rect.center().y),
        egui::Align2::LEFT_CENTER,
        value,
        egui::FontId::proportional(11.5),
        theme::TEXT,
    );
}

/// 第一次关掉「沿用全局」时，把全局那几个值抄进来当起点。
///
/// 不抄的话，玩家会看到内存从「自动 4.0 GiB」变成一个没见过的 4096，
/// 还得先猜「这是哪儿来的」。
fn seed_from_global(game: &mut aicraft_core::instancecfg::InstanceConfig, settings: &aicraft_core::settings::Settings) {
    game.auto_memory = settings.auto_memory;
    game.max_memory_mb = if settings.auto_memory {
        settings.memory_mb()
    } else {
        settings.max_memory_mb
    };
    game.jvm_args = settings.jvm_args.clone();
    game.java_path = settings.java_path.clone();
}

/// 导出对话框里的勾选表：这个实例里有什么，哪些要带走。
///
/// 只列顶层。玩家真正要决定的是「带不带存档、带不带日志」，而这一层
/// 正好就是这些决定的粒度；再往下铺开几百个模组文件，勾选会变成体力活。
///
/// 默认勾上模组、配置、资源包、光影包和 options.txt —— 别人装上就该
/// 和导出者看到的一样。存档、日志、截图属于导出者自己，列出来但不勾。
fn export_picker(ui: &mut egui::Ui, draft: &mut ExportDraft) {
    let (files, bytes) = draft.picked();
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("要带走哪些")
                .size(11.5)
                .color(theme::TEXT_WEAK),
        );
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!("{} 个文件，{}", files, crate::human_size(bytes as f64)))
                .size(10.5)
                .color(theme::TEXT_FAINT),
        );
    });
    ui.add_space(3.0);

    if draft.rows.is_empty() {
        ui.label(
            egui::RichText::new("这个实例目录里没有可以带走的东西。")
                .size(11.0)
                .color(theme::WARN),
        );
        return;
    }

    // 勾选框本身还是 egui 那个：这里的行太多，自己画一个既没必要，
    // 也会和下面「文件数 / 大小」那两列对不齐。
    let mut changes: Vec<String> = Vec::new();
    egui::ScrollArea::vertical()
        .id_salt("manage-export-picker")
        .max_height(132.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::NONE
                .fill(theme::BG_BOTTOM)
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for row in &draft.rows {
                        let mut on = draft.is_ticked(&row.name);
                        ui.horizontal(|ui| {
                            if theme::hand(ui.checkbox(&mut on, "")).changed() {
                                changes.push(row.name.clone());
                            }
                            ui.add_space(2.0);
                            // 目录后面带一个斜杠，一眼看得出是文件夹还是单个文件。
                            let label = if row.is_dir {
                                format!("{}/", row.name)
                            } else {
                                row.name.clone()
                            };
                            ui.label(egui::RichText::new(label).size(12.0).color(theme::TEXT));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(crate::human_size(row.bytes as f64))
                                            .size(10.5)
                                            .color(theme::TEXT_FAINT),
                                    );
                                    ui.add_space(8.0);
                                    ui.label(
                                        egui::RichText::new(format!("{} 个文件", row.files))
                                            .size(10.5)
                                            .color(theme::TEXT_FAINT),
                                    );
                                },
                            );
                        });
                    }
                });
        });

    for name in changes {
        draft.toggle(&name);
    }

    if draft.ticked.is_empty() {
        ui.add_space(3.0);
        ui.label(
            egui::RichText::new("至少勾一样：一个空包别人装上去什么都没有。")
                .size(11.0)
                .color(theme::WARN),
        );
    }
}

/// 导出整合包的对话框：选格式、填名字和版本。
fn export_dialog(ctx: &egui::Context, state: &mut State, busy: bool) -> Option<Action> {
    let mut draft = state.export.take()?;
    let mut dismissed = false;
    let mut confirmed = None;

    let response = egui::Modal::new(egui::Id::unique("aicraft-manage-export"))
        .frame(theme::dialog_frame())
        .show(ctx, |ui| {
            ui.set_width(430.0);
            ui.label(
                egui::RichText::new("导出整合包")
                    .size(16.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(format!("从实例 {} 打一个包", state.version))
                    .size(11.5)
                    .color(theme::TEXT_WEAK),
            );
            ui.add_space(12.0);

            ui.label(
                egui::RichText::new("格式")
                    .size(11.5)
                    .color(theme::TEXT_WEAK),
            );
            ui.add_space(2.0);
            for format in ExportFormat::ALL {
                ui.horizontal(|ui| {
                    if theme::radio(ui, draft.format == format, format.label()).clicked() {
                        draft.format = format;
                    }
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format.hint())
                            .size(10.5)
                            .color(theme::TEXT_FAINT),
                    );
                });
                ui.add_space(2.0);
            }
            ui.add_space(10.0);

            ui.label(
                egui::RichText::new("名字")
                    .size(11.5)
                    .color(theme::TEXT_WEAK),
            );
            let mut resp = ui.add(
                egui::TextEdit::singleline(&mut draft.name)
                    .hint_text("比如 我的整合包")
                    .desired_width(f32::INFINITY),
            );
            theme::input_menu(&mut resp, &mut draft.name, false);
            ui.add_space(8.0);

            ui.label(
                egui::RichText::new("版本")
                    .size(11.5)
                    .color(theme::TEXT_WEAK),
            );
            let mut resp = ui.add(
                egui::TextEdit::singleline(&mut draft.version)
                    .hint_text("比如 1.0.0")
                    .desired_width(f32::INFINITY),
            );
            theme::input_menu(&mut resp, &mut draft.version, false);
            ui.add_space(8.0);

            export_picker(ui, &mut draft);
            ui.add_space(8.0);

            if draft.name.trim().is_empty() {
                ui.label(
                    egui::RichText::new("名字不能空着：别人装完得知道这是什么。")
                        .size(11.0)
                        .color(theme::WARN),
                );
            } else {
                ui.label(
                    egui::RichText::new(format!(
                        "会存成 .{}，格式是 {}",
                        draft.format.extension(),
                        draft.format.label()
                    ))
                    .size(11.0)
                    .color(theme::TEXT_WEAK),
                );
            }
            ui.add_space(14.0);

            let can_go = draft.can_export() && !busy;
            ui.horizontal(|ui| {
                if theme::dialog_button(ui, "取消", !busy, 92.0).clicked() {
                    dismissed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::dialog_primary_button(ui, "选择位置并导出", can_go, 140.0).clicked() {
                        confirmed = Some(());
                    }
                });
            });
        });

    if response.should_close() {
        dismissed = true;
    }
    if let Some(()) = confirmed {
        Some(Action::ExportPack {
            format: draft.format,
            name: draft.name.trim().to_string(),
            version: draft.version.trim().to_string(),
            include: draft.ticked.clone(),
        })
    } else {
        if !dismissed {
            state.export = Some(draft);
        }
        None
    }
}

/// 更改游戏版本的确认框。
///
/// 换底座不是「改一个数字」：加载器要跟着重装一遍，而模组是按游戏版本挑的，
/// 换过去之后它们未必还认得新底座。这一页的东西（存档、配置、模组文件）
/// 一个都不会动，所以这里要讲清楚的是「动的是什么、可能坏的是什么」。
fn change_game_dialog(
    ctx: &egui::Context,
    state: &mut State,
    official: &[crate::versions::Entry],
    busy: bool,
) -> Option<Action> {
    let mut draft = state.change_game.take()?;
    let mut dismissed = false;
    let mut confirmed = false;

    let response = egui::Modal::new(egui::Id::unique("aicraft-manage-change-game"))
        .frame(theme::dialog_frame())
        .show(ctx, |ui| {
            ui.set_width(430.0);
            ui.label(
                egui::RichText::new("更改游戏版本")
                    .size(16.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(format!(
                    "{} 现在挂在 Minecraft {} 上",
                    state.version, state.mc_version
                ))
                .size(11.5)
                .color(theme::TEXT_WEAK),
            );
            ui.add_space(12.0);

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("换成")
                        .size(11.5)
                        .color(theme::TEXT_WEAK),
                );
                ui.add_space(8.0);
                let mut resp = ui.add(
                    egui::TextEdit::singleline(&mut draft.query)
                        .hint_text("找版本，比如 1.20")
                        .desired_width(180.0),
                );
                theme::input_menu(&mut resp, &mut draft.query, false);
            });
            ui.add_space(4.0);
            if official.is_empty() {
                ui.label(
                    egui::RichText::new("还没拿到官方版本清单，等一下再试。")
                        .size(11.0)
                        .color(theme::WARN),
                );
            } else {
                // 只列正式版：换底座是「把这一份搬到另一版上」，玩家要的是
                // 一个能好好玩下去的版本，快照和远古版本摆在这里只会碍事。
                let shown: Vec<&crate::versions::Entry> = official
                    .iter()
                    .filter(|e| e.kind == "release" && draft.matches(&e.id))
                    .collect();
                if shown.is_empty() {
                    ui.label(
                        egui::RichText::new("没有对得上的正式版。")
                            .size(11.0)
                            .color(theme::TEXT_FAINT),
                    );
                }
                egui::ScrollArea::vertical()
                    .id_salt("manage-change-game-list")
                    .max_height(190.0)
                    .show(ui, |ui| {
                        for entry in shown {
                            let current = entry.id == state.mc_version;
                            ui.horizontal(|ui| {
                                // 现在这一版也列出来、也标出来：让它消失反而会
                                // 让人以为「是不是没装」，标一句「当前」最省事。
                                if theme::radio(ui, draft.target == entry.id, &entry.id).clicked() {
                                    draft.target = entry.id.clone();
                                }
                                if current {
                                    theme::chip(ui, "当前");
                                }
                            });
                            ui.add_space(2.0);
                        }
                    });
            }
            ui.add_space(10.0);

            // 危害提示：这是这个弹窗存在的理由，写得具体一点。
            theme::card().show(ui, |ui| {
                ui.set_width((ui.available_width() - 32.0).max(320.0));
                for line in [
                    "换版本会把实例重新挂到新底座上，加载器跟着重装一遍。",
                    "存档、配置、模组文件都不会被删。",
                    "但模组是按游戏版本挑的：新版本上它们可能跑不起来，甚至一启动就崩。",
                    "换完想退回来，重新选回原来那一版就行。",
                ] {
                    ui.label(
                        egui::RichText::new(line)
                            .size(10.5)
                            .color(theme::TEXT_WEAK),
                    );
                }
            });
            ui.add_space(8.0);

            if !draft.changed(&state.mc_version) && !official.is_empty() {
                ui.label(
                    egui::RichText::new("选一个和现在不同的版本。")
                        .size(11.0)
                        .color(theme::TEXT_FAINT),
                );
            }
            ui.add_space(12.0);

            let can_go = draft.changed(&state.mc_version) && !busy;
            ui.horizontal(|ui| {
                if theme::dialog_button(ui, "取消", !busy, 92.0).clicked() {
                    dismissed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::dialog_primary_button(ui, "换过去并重装加载器", can_go, 170.0)
                        .clicked()
                    {
                        confirmed = true;
                    }
                });
            });
        });

    if response.should_close() {
        dismissed = true;
    }
    if confirmed {
        Some(Action::ChangeGameVersion(draft.target.trim().to_string()))
    } else {
        if !dismissed {
            state.change_game = Some(draft);
        }
        None
    }
}

/// 删除确认：删的可能是存档，一律再问一次。
fn confirm_delete(ctx: &egui::Context, state: &mut State, busy: bool) -> Option<Action> {
    let path = state.pending_delete.clone()?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("这个")
        .to_string();
    let is_dir = path.is_dir();
    let lines: &[&str] = if is_dir {
        &["整个文件夹将一并删除，且无法恢复。"]
    } else {
        &["此文件将被删除，且无法恢复。"]
    };
    let ask = theme::Confirm {
        id: "aicraft-manage-delete",
        title: "删除此项？",
        subject: Some(&name),
        lines,
        danger: true,
        warn: true,
        confirm: "删除",
        width: 370.0,
    };
    match theme::confirm_dialog(ctx, busy, ask) {
        theme::Answer::Confirmed => {
            state.pending_delete = None;
            Some(Action::Delete(path))
        }
        theme::Answer::Dismissed => {
            state.pending_delete = None;
            None
        }
        theme::Answer::Pending => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_has_a_label() {
        let labels: Vec<&str> = Section::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(
            labels,
            vec!["概览", "游戏设置", "模组", "资源包", "光影包", "存档", "备份"]
        );
    }

    #[test]
    fn a_change_game_draft_starts_on_the_version_it_is_on() {
        let draft = ChangeGameDraft::new("1.20.1");
        // 一进来就选中现在这一版：默认值不能是「随便换一个」，那是玩家自己
        // 要按下去的决定。
        assert_eq!(draft.target, "1.20.1");
        assert!(!draft.changed("1.20.1"));
        // 只认「换了没有」，不认「换得对不对」——那是玩家自己挑的。
        assert!(draft.changed("1.21.1"));
    }

    #[test]
    fn a_change_game_draft_filters_by_what_is_typed() {
        let mut draft = ChangeGameDraft::new("1.20.1");
        // 空搜索框什么都不筛。
        assert!(draft.matches("1.21.5"));
        draft.query = " 1.20 ".to_string();
        assert!(draft.matches("1.20.1"));
        assert!(!draft.matches("1.21.5"));
        // 搜索框只筛列表，不动选中项：输入到一半不该把玩家已经挑好的版本换掉。
        assert_eq!(draft.target, "1.20.1");
        assert!(!draft.changed("1.20.1"));
    }

    fn entry(name: &str, files: usize, bytes: u64, default_on: bool) -> aicraft_core::packexport::ExportEntry {
        aicraft_core::packexport::ExportEntry {
            name: name.to_string(),
            files,
            bytes,
            is_dir: true,
            default_on,
        }
    }

    #[test]
    fn an_export_draft_starts_from_the_instance_name() {
        let rows = vec![entry("mods", 3, 300, true), entry("saves", 1, 10, false)];
        let draft = ExportDraft::new("26.3-Fabric", rows);
        assert_eq!(draft.name, "26.3-Fabric");
        assert_eq!(draft.version, "1.0.0");
        assert_eq!(draft.format, ExportFormat::Modrinth);
        assert!(draft.usable());
    }

    #[test]
    fn an_export_draft_needs_a_name() {
        let mut draft = ExportDraft::new("x", vec![entry("mods", 1, 1, true)]);
        draft.name = "   ".to_string();
        assert!(!draft.usable(), "只有空格也算没填");
    }

    #[test]
    fn only_the_default_entries_start_ticked() {
        let rows = vec![
            entry("mods", 3, 300, true),
            entry("config", 2, 20, true),
            entry("saves", 1, 10, false),
            entry("logs", 4, 40, false),
        ];
        let draft = ExportDraft::new("x", rows);
        assert!(draft.is_ticked("mods"));
        assert!(draft.is_ticked("config"));
        assert!(!draft.is_ticked("saves"), "存档是导出者自己的，默认不带走");
        assert!(!draft.is_ticked("logs"));
        // 统计只算勾中的那些。
        assert_eq!(draft.picked(), (5, 320));
    }

    #[test]
    fn an_export_needs_at_least_one_tick() {
        let rows = vec![entry("mods", 1, 1, false)];
        let mut draft = ExportDraft::new("x", rows);
        assert!(draft.ticked.is_empty());
        assert!(!draft.can_export(), "一个文件都没勾，导出来是个空包");
        draft.toggle("mods");
        assert!(draft.can_export());
        draft.toggle("mods");
        assert!(!draft.can_export(), "再点一下又回到空包");
    }

    #[test]
    fn exporting_is_described_for_the_log() {
        let action = Action::ExportPack {
            format: ExportFormat::Hmcl,
            name: "我的整合包".to_string(),
            version: "1.2.0".to_string(),
            include: vec!["mods".to_string(), "config".to_string()],
        };
        assert_eq!(
            action.describe(),
            "管理页：导出 HMCL 整合包（我的整合包 1.2.0，带上 mods、config）"
        );
    }

    #[test]
    fn the_game_section_is_not_a_file_list() {
        // 它没有「共 N 个」「打开文件夹」「刷新」这些概念，导航后面也不该跟计数。
        assert!(!Section::Game.has_files());
        assert_eq!(Section::Game.label(), "游戏设置");
    }

    fn runtime(major: u32, version: &str) -> aicraft_core::java::JavaRuntime {
        aicraft_core::java::JavaRuntime {
            path: PathBuf::from(format!("C:\\Java\\jdk-{}\\bin\\javaw.exe", major)),
            major_version: major,
            version_string: version.to_string(),
            is_64bit: true,
            source: "手动添加".to_string(),
        }
    }

    /// 造一份只有一个版本的目录，清单里声明需要的 Java 主版本。
    fn instance_with_java(required: u32) -> aicraft_core::testutil::TempDir {
        let dir = aicraft_core::testutil::fresh_dir("manage-auto-java");
        let version = dir.join("versions").join("test");
        std::fs::create_dir_all(&version).unwrap();
        let text = format!(
            r#"{{"id":"test","mainClass":"a.B","javaVersion":{{"majorVersion":{}}}}}"#,
            required
        );
        std::fs::write(version.join("test.json"), text).unwrap();
        dir
    }

    #[test]
    fn no_instance_means_no_answer_about_java() {
        // 一个版本都没有时，连清单都没有，别硬猜一个数字出来。
        let lists = [runtime(21, "21.0.4")];
        let auto = auto_java(Path::new("C:\\none"), "", Some(&lists));
        assert_eq!(auto.text, "自动选择");
    }

    #[test]
    fn an_exact_match_is_named_in_the_dropdown() {
        let dir = instance_with_java(21);
        let lists = [runtime(25, "25.0.4"), runtime(21, "21.0.4")];
        let auto = auto_java(&dir, "test", Some(&lists));
        // 清单要 21，本机正好有 21：挑的是 21，不是版本号更大的 25。
        assert_eq!(auto.text, "自动选择 · Java 21");
    }

    #[test]
    fn a_missing_version_is_announced_with_the_stand_in() {
        // 要 17、本机只有 21：界面上只说实际会用的那个（21），不提「顶替」——
        // 这句话写在设置里只会让人以为启动器出了什么问题。
        let dir = instance_with_java(17);
        let lists = [runtime(21, "21.0.4")];
        let auto = auto_java(&dir, "test", Some(&lists));
        assert_eq!(auto.text, "自动选择 · Java 21");
    }

    #[test]
    fn nothing_usable_is_said_out_loud() {
        let dir = instance_with_java(21);
        let lists = [runtime(8, "1.8.0_451")];
        let auto = auto_java(&dir, "test", Some(&lists));
        assert_eq!(auto.text, "自动选择 · 缺 Java 21");
    }

    #[test]
    fn a_version_still_being_scanned_says_nothing_it_cannot_prove() {
        let dir = instance_with_java(17);
        let auto = auto_java(&dir, "test", None);
        // 扫描没回来时还不知道会落到哪一个，就老实写「自动选择」。
        assert_eq!(auto.text, "自动选择");
    }

    #[test]
    fn a_broken_manifest_falls_back_to_the_plain_text() {
        // 父版本没装、清单读不出来时不该猜：那种情况启动会先报别的错。
        let dir = aicraft_core::testutil::fresh_dir("manage-auto-java-broken");
        let lists = [runtime(21, "21.0.4")];
        let auto = auto_java(&dir, "missing", Some(&lists));
        assert_eq!(auto.text, "自动选择");
    }

    #[test]
    fn a_freshly_opened_instance_expects_to_go_back_to_the_version_list() {
        // 来路默认是「版本列表」：只有从首页那颗齿轮进来时，调用方才会
        // 把 `from_home` 改成 true。反过来忘了改，返回按钮也只是回到版本
        // 列表——错得温和，不会把人送去一个没打算去的地方。
        let tmp = aicraft_core::testutil::fresh_dir("manage-from-home");
        std::fs::create_dir_all(tmp.join("versions").join("26.3")).unwrap();
        let state = State::new(&tmp, "26.3");
        assert!(!state.from_home);
    }

    #[test]
    fn the_more_group_starts_folded() {
        // 「浏览更多」默认收着：它底下是日志目录这类偶尔才去一次的东西，
        // 常驻展开会把侧栏撑长，把常用那几栏往下推。
        let tmp = aicraft_core::testutil::fresh_dir("manage-more-folded");
        std::fs::create_dir_all(tmp.join("versions").join("26.3")).unwrap();
        let state = State::new(&tmp, "26.3");
        assert!(!state.more_open);
    }

    #[test]
    fn new_state_picks_up_the_instance_own_game_settings() {
        let tmp = aicraft_core::testutil::fresh_dir("manage-gamecfg");
        let vdir = tmp.join("versions").join("26.3");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(
            vdir.join("26.3.json"),
            r#"{"id":"26.3","mainClass":"a.B","libraries":[]}"#,
        )
        .unwrap();
        let config = aicraft_core::instancecfg::InstanceConfig {
            follow_global: false,
            auto_memory: false,
            max_memory_mb: 6144,
            jvm_args: "-XX:+UseG1GC".to_string(),
            java_path: None,
            from_pack: false,
        };
        aicraft_core::instancecfg::save(&tmp, "26.3", &config).unwrap();

        let state = State::new(&tmp, "26.3");
        assert!(!state.game.follow_global);
        assert_eq!(state.game.memory_mb(), 6144);
    }

    #[test]
    fn new_state_reads_the_instance_layout() {
        let tmp = aicraft_core::testutil::fresh_dir("manage-new");
        let vdir = tmp.join("versions").join("26.3");
        std::fs::create_dir_all(vdir.join("mods")).unwrap();
        std::fs::create_dir_all(vdir.join("saves").join("世界")).unwrap();
        std::fs::write(
            vdir.join("26.3.json"),
            r#"{"id":"26.3","inheritsFrom":"1.20.1"}"#,
        )
        .unwrap();
        std::fs::write(vdir.join("26.3.jar"), b"x").unwrap();
        std::fs::write(vdir.join("mods").join("sodium.jar"), b"123").unwrap();

        let state = State::new(&tmp, "26.3");
        assert_eq!(state.mc_version, "1.20.1");
        assert!(!state.shared);
        assert_eq!(state.mods.len(), 1);
        assert_eq!(state.saves.len(), 1);
        assert!(state.loader.is_none());
        assert_eq!(state.count(Section::Mods), 1);
        assert_eq!(state.count(Section::Loader), 0);
    }

    #[test]
    fn a_missing_parent_is_noticed_and_one_that_is_installed_is_left_alone() {
        let tmp = aicraft_core::testutil::fresh_dir("manage-parent");
        let child = tmp.join("versions").join("26.3-Fabric");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(
            child.join("26.3-Fabric.json"),
            r#"{"id":"26.3-Fabric","inheritsFrom":"26.3"}"#,
        )
        .unwrap();

        // 父版本不在盘上：这时候才该提示「缺底座」，界面上才会出现压平按钮。
        let state = State::new(&tmp, "26.3-Fabric");
        assert_eq!(state.missing_parent.as_deref(), Some("26.3"));

        // 把父版本装上：一切正常，不该再为它占一行。
        let parent = tmp.join("versions").join("26.3");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::write(parent.join("26.3.json"), r#"{"id":"26.3"}"#).unwrap();
        let state = State::new(&tmp, "26.3-Fabric");
        assert!(state.missing_parent.is_none());
    }

    #[test]
    fn only_the_mods_column_offers_updates() {
        // 更新按钮与图标同一个前提：只有模组那一栏才有「下一个版本」这个概念。
        // 资源包和存档在 Modrinth 上没有对应记录，画了也是空的。
        let src = include_str!("manage.rs");
        assert!(src.contains("state.section == Section::Mods"));
        assert!(src.contains("Action::CheckUpdates"));
    }

    #[test]
    fn a_checked_mod_keeps_its_own_update() {
        let mut state = State {
            version: "t".to_string(),
            mc_version: "1.21.1".to_string(),
            official_id: "t".to_string(),
            section: Section::Mods,
            version_path: PathBuf::from("v"),
            root: PathBuf::from("r"),
            shared: false,
            mods: Vec::new(),
            packs: Vec::new(),
            shaderpacks: Vec::new(),
            has_shaderpacks: false,
            saves: Vec::new(),
            backups: Vec::new(),
            error: None,
            loader: None,
            loader_open: None,
            loader_versions: Vec::new(),
            loader_loading: false,
            loader_error: None,
            loader_for: None,
            pending_delete: None,
            missing_parent: None,
            updates: Vec::new(),
            update_busy: None,
            update_notice: None,
            game: aicraft_core::instancecfg::InstanceConfig::default(),
            export: None,
            exporting: None,
            export_notice: None,
            change_game: None,
            head_at: None,
            reveal_at: None,
            from_home: false,
            more_open: false,
        };
        let old = PathBuf::from("r/mods/sodium-0.5.0.jar");
        state.set_updates(vec![
            Check {
                path: old.clone(),
                filename: "sodium-0.5.0.jar".to_string(),
                title: Some("Sodium".to_string()),
                current: Some("0.5.0".to_string()),
                latest: Some(aicraft_core::modupdate::Latest {
                    version_number: "0.6.0".to_string(),
                    file: aicraft_core::modrinth::ModFile {
                        version_number: "0.6.0".to_string(),
                        url: "https://example.invalid/s.jar".to_string(),
                        filename: "sodium-0.6.0.jar".to_string(),
                        sha1: None,
                    },
                }),
            },
            // 没新版的那条不该出现在结果里
            Check {
                path: PathBuf::from("r/mods/other.jar"),
                filename: "other.jar".to_string(),
                title: Some("Other".to_string()),
                current: Some("1.0".to_string()),
                latest: None,
            },
        ]);
        assert_eq!(state.updates.len(), 1);
        assert!(state.update_busy.is_none());
        assert!(state.pending_update(&old).is_some());
        assert!(state
            .pending_update(Path::new("r/mods/other.jar"))
            .is_none());
    }

    #[test]
    fn a_pack_filename_drops_the_game_format_codes() {
        // 本机真有这么个包：作者把格式码写进了文件名，指望游戏渲染成彩色。
        assert_eq!(
            pack_label("!!!§l 立方构想 V4018 256x §r§0 2024-06-27 JAVA版 §r§0.zip"),
            "!!! 立方构想 V4018 256x 2024-06-27 JAVA版 .zip"
        );
        assert_eq!(pack_label("Sodium.zip"), "Sodium.zip", "没有格式码就原样");
        assert_eq!(pack_label("§r§0"), "§r§0", "剥完什么都不剩就退回原名");
    }

    #[test]
    fn every_section_asks_for_its_own_kind_of_icon() {
        // 四栏取图的来路全不一样：模组压在 jar 里、资源包是包里的 pack.png、
        // 光影包得按文件名去认、存档是游戏截的那张图。这条盯的是四个调用点
        // 各自传对了 Look，别一栏借了另一栏的取法。
        let source = include_str!("manage.rs");
        let mods = source.find("Section::Mods => {").expect("得有模组那一栏");
        let packs = source
            .find("Section::ResourcePacks => {")
            .expect("得有资源包那一栏");
        let shaders = source
            .find("Section::ShaderPacks => {")
            .expect("得有光影包那一栏");
        let saves = source.find("Section::Saves => {").expect("得有存档那一栏");
        assert!(&source[mods..packs].contains("Look::Mod"), "模组那一栏要用模组取法");
        assert!(
            &source[packs..shaders].contains("Look::Pack"),
            "资源包那一栏要用资源包取法"
        );
        assert!(
            &source[shaders..saves].contains("Look::Shader"),
            "光影包那一栏用它自己的打扮"
        );
        assert!(
            &source[saves..].contains("Look::Save"),
            "存档那一栏要读那张截图"
        );
    }

    #[test]
    fn the_backup_section_only_shows_up_when_there_are_backups() {
        let tmp = aicraft_core::testutil::fresh_dir("manage-backups");
        let vdir = tmp.join("versions").join("26.3");
        std::fs::create_dir_all(vdir.join("backups")).unwrap();
        std::fs::write(vdir.join("26.3.json"), r#"{"id":"26.3"}"#).unwrap();

        // 目录空着：这一栏不该出现在左边，也不该让人切进去。
        let mut state = State::new(&tmp, "26.3");
        assert!(state.backups.is_empty());
        assert!(!state.shows(Section::Backups));
        assert!(state.shows(Section::Saves), "别的栏一直都在");

        std::fs::write(vdir.join("backups").join("2024-06-06_world.zip"), b"zip").unwrap();
        state.section = Section::Backups;
        state.reload(&tmp);
        assert_eq!(state.backups.len(), 1);
        assert_eq!(state.count(Section::Backups), 1);
        assert!(state.shows(Section::Backups));

        // 备份删光之后，正停在备份栏的页面要退回加载器，而不是留个空壳。
        std::fs::remove_file(vdir.join("backups").join("2024-06-06_world.zip")).unwrap();
        state.reload(&tmp);
        assert_eq!(state.section, Section::Loader);
    }

    #[test]
    fn the_shader_section_waits_for_its_folder() {
        let tmp = aicraft_core::testutil::fresh_dir("manage-shaders");
        let vdir = tmp.join("versions").join("26.3");
        std::fs::create_dir_all(vdir.join("mods")).unwrap();
        std::fs::write(vdir.join("26.3.json"), r#"{"id":"26.3"}"#).unwrap();

        // 原版不建 shaderpacks/：没有这个目录就不该出现这一栏。
        let mut state = State::new(&tmp, "26.3");
        assert!(!state.has_shaderpacks);
        assert!(!state.shows(Section::ShaderPacks));
        assert!(state.shows(Section::ResourcePacks), "别的栏不受牵连");

        // 目录刚建出来、里头还空着：这时候就该出现，好让人点进去放东西。
        std::fs::create_dir_all(vdir.join("shaderpacks")).unwrap();
        state.reload(&tmp);
        assert!(state.shows(Section::ShaderPacks));
        assert_eq!(state.count(Section::ShaderPacks), 0);

        std::fs::write(vdir.join("shaderpacks").join("Bsl.zip"), b"zip").unwrap();
        state.section = Section::ShaderPacks;
        state.reload(&tmp);
        assert_eq!(state.count(Section::ShaderPacks), 1);
        assert_eq!(
            section_dir(&state, Section::ShaderPacks),
            vdir.join("shaderpacks")
        );

        // 目录被整个删掉之后，正停在这一栏的页面要退回概览，不留空壳。
        std::fs::remove_dir_all(vdir.join("shaderpacks")).unwrap();
        state.reload(&tmp);
        assert!(!state.shows(Section::ShaderPacks));
        assert_eq!(state.section, Section::Loader);
    }

    #[test]
    fn backups_live_in_the_instance_folder_even_when_shared() {
        // 共享模式下模组/资源包/存档都在 .minecraft 根目录，备份却跟着实例走：
        // 游戏自己就是把它写在 versions/<实例>/backups 的。
        let tmp = aicraft_core::testutil::fresh_dir("manage-backups-shared");
        let vdir = tmp.join("versions").join("26.3");
        std::fs::create_dir_all(vdir.join("backups")).unwrap();
        std::fs::write(vdir.join("26.3.json"), r#"{"id":"26.3"}"#).unwrap();
        std::fs::write(vdir.join("backups").join("a.zip"), b"zip").unwrap();

        let state = State::new(&tmp, "26.3");
        assert!(state.shared, "没有清单的版本按共享处理");
        assert_eq!(state.backups.len(), 1);
        assert_eq!(
            section_dir(&state, Section::Backups),
            vdir.join("backups")
        );
    }

    #[test]
    fn reload_sees_new_files() {
        let tmp = aicraft_core::testutil::fresh_dir("manage-reload");
        let vdir = tmp.join("versions").join("26.3");
        std::fs::create_dir_all(vdir.join("resourcepacks")).unwrap();
        std::fs::write(vdir.join("26.3.json"), r#"{"id":"26.3"}"#).unwrap();
        std::fs::write(vdir.join("26.3.jar"), b"x").unwrap();

        let mut state = State::new(&tmp, "26.3");
        assert_eq!(state.packs.len(), 0);
        std::fs::write(vdir.join("resourcepacks").join("清爽.zip"), b"zz").unwrap();
        state.reload(&tmp);
        assert_eq!(state.packs.len(), 1);
    }
}
