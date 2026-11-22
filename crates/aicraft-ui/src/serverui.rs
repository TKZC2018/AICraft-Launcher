//! 「拓展功能 → 服务端管理」页。
//!
//! 三个视图：**列表**（一个服务端一张卡片）、**新建流程**（分三步）和
//! **配置**（改 `server.properties`）。
//!
//! 新建流程照着「下载版本」那套走：先挑原版版本 → 起名字 → 选核心与加载器版本。
//! 每步都是整页，不是弹窗 —— 和客户端装版本的节奏一致，玩家不用在两套交互里
//! 来回切换肌肉记忆。
//!
//! 这一层不碰网络也不写盘：所有动作收成 [`Action`]，由 `main.rs` 执行。

use std::path::PathBuf;

use aicraft_core::server::{self, CoreKind, FieldKind, Properties, Server, FIELDS};

use crate::{human_size, theme};

/// 新建流程走到哪一步。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Step {
    /// 从官方清单里挑一个原版版本
    #[default]
    Version,
    /// 起名字、定内存
    Name,
    /// 选核心（原版 / Fabric / Forge / NeoForge）与它的版本
    Core,
}

/// 「新建服务端」的流程状态。
#[derive(Default)]
pub struct Form {
    pub open: bool,
    pub step: Step,
    /// 版本列表的搜索词
    pub query: String,
    pub name: String,
    pub core: CoreKind,
    pub mc_version: String,
    /// 加载器版本；原版忽略。
    pub core_version: String,
    pub memory_mb: u32,
    /// 当前核心 + 游戏版本的可用加载器版本。
    pub loader_versions: Vec<String>,
    pub loading_loaders: bool,
    pub loader_error: Option<String>,
    /// 这份列表属于哪个 (核心, 游戏版本)。
    pub(crate) loaded_for: Option<(CoreKind, String)>,
    /// 玩家挑好的目标文件夹。None 表示装进数据目录的 `servers/<名字>`。
    pub target: Option<PathBuf>,
    /// 这是从管理页「安装核心」进来的：目录里已经有别的东西（可能只是
    /// 玩家自己放过文件，也可能已经有一份核心）。
    ///
    /// 那种情况下名字由目录决定、改不了，收尾也走 `server::reinstall`
    /// （允许非空、失败不删目录——那是玩家的存档）。
    pub existing: bool,
    /// 要不要在外面套一层 MCDReforged。
    ///
    /// 勾上之后核心装进 `server/` 子目录，外层生成 MCDR 骨架，启动脚本
    /// 拉的是 MCDR 而不是 Java。
    pub mcdr: bool,
    /// 本机 MCDR 环境探测结果。`None` = 还没探过。
    pub env: Option<aicraft_core::mcdr::Env>,
    /// 正在探测环境。
    pub probing: bool,
}

impl Form {
    fn matches(&self) -> bool {
        self.loaded_for
            .as_ref()
            .is_some_and(|(core, mc)| *core == self.core && mc == &self.mc_version)
    }

    /// 顶上那行小字：第几步、到哪了。
    pub fn step_label(&self) -> String {
        match self.step {
            Step::Version => "第 1 步 / 共 3 步 · 选原版版本".to_string(),
            Step::Name => "第 2 步 / 共 3 步 · 起名字".to_string(),
            Step::Core => "第 3 步 / 共 3 步 · 选核心".to_string(),
        }
    }

    /// 当前这一步能不能往下走。
    pub fn can_advance(&self) -> bool {
        match self.step {
            Step::Version => !self.mc_version.trim().is_empty(),
            // 从管理页进来时名字由目录决定，玩家改不了——目录名里带 `[` `]`
            // 这类字符完全正常（`[30011]lobby`），不能拿新建那套规则拦他。
            Step::Name if self.existing => true,
            Step::Name => !self.name.trim().is_empty() && server::is_valid_name(&self.name),
            Step::Core => self.can_create(),
        }
    }

    /// 下一步按钮能不能点。
    pub fn can_create(&self) -> bool {
        if !self.existing && (self.name.trim().is_empty() || !server::is_valid_name(&self.name)) {
            return false;
        }
        if self.mc_version.trim().is_empty() {
            return false;
        }
        if self.core.needs_loader_version() && self.core_version.trim().is_empty() {
            return false;
        }
        if self.loading_loaders {
            return false;
        }
        // 勾了 MCDR 就得先有能用的环境：没有 Python、或者没装 MCDR，
        // 装到一半才发现起不来，比现在拦住更难受。
        if self.mcdr && !self.env.as_ref().is_some_and(|env| env.ready()) {
            return false;
        }
        true
    }

    /// 名字这一栏的提示。
    pub fn name_hint(&self) -> Option<String> {
        let name = self.name.trim();
        if name.is_empty() {
            return None;
        }
        if !server::is_valid_name(name) {
            return Some("不能含 \\ / : * ? \" < > | 这些字符".to_string());
        }
        if server::name_taken(name) {
            return Some(format!("servers/{} 已经有了，换一个名字", name));
        }
        None
    }
}

/// 「管理」视图的状态。
///
/// 一个服务端的所有操作都收在这一页里：装核心、改 `server.properties`、
/// 同意 EULA。照设置页的样子排版——一张卡片一节，节间一条细线——
/// 玩家在设置里已经熟悉了这套节奏，不用再学第二遍。
pub struct Managing {
    /// 认这一页说的是哪个目录。用路径不用名字：玩家可能同时有好几个
    /// 名叫 `survival` 的服摆在不同盘上。
    pub dir: PathBuf,
    pub name: String,
    pub core: CoreKind,
    /// 「Fabric 0.20.1 · 26.3」这种一行摘要，直接来自卡片。
    pub summary: String,
    pub eula: bool,
    /// 目录里到底认出核心没有。空目录也能进来装，所以这里要记着。
    pub recognized: bool,
    /// 这一份套了 MCDR 吗。是的话 `server.properties` 在 `server/` 里。
    pub mcdr: bool,
    /// Bukkit 系分支名（`Paper` 之类），不是分支是空串。
    pub flavor: &'static str,
    pub props: Properties,
    /// 这个配置实际存在哪个文件里。
    ///
    /// 套了 MCDR 的服，`server.properties` 在 `server/` 那一层；代理端是
    /// `velocity.toml`。界面上的「打开源文件」要打开的就是它。
    pub props_path: PathBuf,
    /// 那个文件到底在不在。
    ///
    /// 不在的时候不能拿 `FIELDS` 的默认值凑一份出来给玩家改 —— 那会显示
    /// 一份「看起来是配置、其实磁盘上什么都没有」的表单，改完保存还会凭空
    /// 造出一个服务端从没读过的文件。所以不存在就只提示、不给表单。
    pub props_exists: bool,
    /// 保存过之后提示一下，别让玩家以为没生效。
    pub saved: bool,
    pub error: Option<String>,
    /// 本机 Python / MCDR 环境。管理页那张 MCDR 卡片自己探自己的，
    /// 和新建流程里的 `Form.env` 各存一份 —— 两边可能同时开着，
    /// 共用一个会让「在流程里探到的」错误地显示到管理页上。
    pub env: Option<aicraft_core::mcdr::Env>,
    pub probing: bool,
}

impl Managing {
    /// 管理页 MCDR 卡片上那个开关能不能拨。
    ///
    /// 只有一个条件：环境已经探齐。没 Python 就放行的话，目录搬完了才
    /// 发现起不来——那时候已经搬完了，退不回去。
    pub fn can_toggle_mcdr(&self) -> bool {
        self.env.as_ref().is_some_and(|env| env.ready())
    }
}

/// 安装进度：一句说明，外加一个有数的比例（有个数才画进度条）。
///
/// 比例和显示文字分开存：下载那几处是字节数，预置库那处是文件个数，
/// 同一个比例值在两处的单位完全不一样——让渲染侧去猜单位迟早出错。
pub struct StepProgress {
    /// 「下载 server.jar」「运行 Forge 安装器」这种一句话。
    pub text: String,
    /// `Some(0.0..=1.0)`：知道进度，画实心进度条。
    /// `None`：说不清进度（跑安装器）或只知道已下多少（对方没给长度），
    /// 画来回滚动的动画条。
    pub ratio: Option<f32>,
    /// 进度条上要写的那行字，例如「12.3 MB / 40.1 MB（31%）」
    /// 或「预置依赖库：42 / 135 个」。
    pub detail: Option<String>,
    /// 下载速度（字节/秒）。0 表示还没测出来，或这一步不是在下载。
    pub speed_bps: f64,
    /// 上一次采样：`(时刻, 已下字节)`。只在字节数上报时更新。
    last_sample: Option<(std::time::Instant, u64)>,
}

impl StepProgress {
    pub fn text(text: impl Into<String>) -> Self {
        StepProgress {
            text: text.into(),
            ratio: None,
            detail: None,
            speed_bps: 0.0,
            last_sample: None,
        }
    }

    /// 收一次「已下 `got` / 一共 `total`」，顺手把速度也估出来。
    ///
    /// 速度用两次采样之间的增量算，再做指数平滑（新值占四成）。直接拿
    /// 「总量 ÷ 已用时间」会一路被开头的慢启动拖低，看起来永远比实际慢。
    ///
    /// 采样间隔不到 0.25 秒时跳过：上报是 200ms 一次，挨得太近的几个点
    /// 只差几毫秒，除出来的速度能飙到几百 MB/s。
    pub fn absorb(&mut self, got: u64, total: u64) {
        self.ratio = (total > 0).then(|| (got as f32 / total as f32).min(1.0));
        self.detail = Some(if total > 0 {
            format!(
                "{} / {}（{:.0}%）",
                human_size(got as f64),
                human_size(total as f64),
                (got as f32 / total as f32).min(1.0) * 100.0
            )
        } else {
            format!("已下 {}", human_size(got as f64))
        });

        let now = std::time::Instant::now();
        match self.last_sample {
            Some((at, before)) => {
                let secs = now.duration_since(at).as_secs_f64();
                let delta = got.saturating_sub(before);
                if secs >= 0.25 {
                    if delta > 0 {
                        let instant = delta as f64 / secs;
                        self.speed_bps = if self.speed_bps > 0.0 {
                            self.speed_bps * 0.6 + instant * 0.4
                        } else {
                            instant
                        };
                    }
                    self.last_sample = Some((now, got));
                }
            }
            None => self.last_sample = Some((now, got)),
        }
    }

    /// 换阶段时清掉速度：上一个阶段的速率对这个阶段没有意义。
    pub fn restart(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.ratio = None;
        self.detail = None;
        self.speed_bps = 0.0;
        self.last_sample = None;
    }
}

/// 速度那一行：「2.3 MB/s」。测不出来就空着，不写「0 B/s」。
pub fn speed_text(speed_bps: f64) -> String {
    if speed_bps >= 1.0 {
        format!("{}/s", human_size(speed_bps))
    } else {
        String::new()
    }
}

/// 页面状态。
pub struct State {
    pub servers: Vec<Server>,
    pub loaded: bool,
    pub form: Form,
    /// 正在「管理」的那个服务端；`None` 表示在看列表。
    pub managing: Option<Managing>,
    /// 顶部一条提示：`(文案, 是不是错误)`。
    pub notice: Option<(String, bool)>,
    /// 正在装的实例名（界面据此显示进度并锁住按钮）。
    pub installing: Option<String>,
    /// 装到哪一步了。
    pub progress: Option<StepProgress>,
    /// 玩家自己加进来的目录（来自 `Settings.server_dirs`）。
    ///
    /// 存在这里而不是每次问设置，是因为 `reload()` 会被装完、删完、改配置
    /// 这些地方反复调用，不该每次都让调用方把列表递一遍。
    pub extra: Vec<PathBuf>,
    /// 玩家从列表里移除掉的目录（来自 `Settings::hidden_server_dirs`）。
    pub hidden: Vec<String>,
    /// 「移除目录」的二次确认：待移除的目录。
    ///
    /// 移除只是让它从列表上消失，磁盘一个字节都不动——但因为结果看起来像
    /// 「删掉了」，还是要问一句，顺便把「不会删文件」写清楚。
    pub confirm_hide: Option<PathBuf>,
    /// 「启用 MCDReforged」的二次确认：待套壳的目录。
    ///
    /// 这一步不可逆（整个目录会被搬进 `server/`），所以不能像勾选框那样
    /// 点一下就算数——先弹框把「搬什么、怎么拆回去」写清楚。
    pub confirm_mcdr: Option<PathBuf>,
}

impl Default for State {
    fn default() -> Self {
        State {
            servers: Vec::new(),
            loaded: false,
            form: Form {
                memory_mb: 4096,
                ..Form::default()
            },
            managing: None,
            notice: None,
            installing: None,
            progress: None,
            extra: Vec::new(),
            hidden: Vec::new(),
            confirm_mcdr: None,
            confirm_hide: None,
        }
    }
}

impl State {
    pub fn reload(&mut self) {
        self.servers = server::list_with(&self.extra, &self.hidden);
        self.loaded = true;
    }

    /// 换一批外部目录（和隐藏名单）并重扫。
    pub fn set_extra(&mut self, dirs: &[String], hidden: &[String]) {
        self.extra = dirs.iter().map(PathBuf::from).collect();
        self.hidden = hidden.to_vec();
        self.reload();
    }

    pub fn set_loader_versions(&mut self, core: CoreKind, mc: String, list: Vec<String>) {
        self.form.loading_loaders = false;
        self.form.loader_error = None;
        self.form.loaded_for = Some((core, mc));
        self.form.core_version = list.first().cloned().unwrap_or_default();
        self.form.loader_versions = list;
    }

    pub fn fail_loader_versions(&mut self, message: String) {
        self.form.loading_loaders = false;
        self.form.loader_error = Some(message);
    }

    /// 装完了。
    pub fn created(&mut self, name: &str) {
        self.installing = None;
        self.progress = None;
        self.form.open = false;
        self.form.step = Step::Version;
        self.form.name.clear();
        self.form.target = None;
        self.reload();
        self.notice = Some((format!("servers/{} 装好了", name), false));
    }

    pub fn failed(&mut self, message: String) {
        self.installing = None;
        self.progress = None;
        self.notice = Some((message, true));
    }

    /// 打开某个服务端的管理页。
    pub fn open_manage(&mut self, dir: &std::path::Path) {
        let Some(found) = self.servers.iter().find(|s| s.dir == dir) else {
            return;
        };
        // 配置一律从磁盘现读：玩家可能在启动器外面用编辑器改过，
        // 用缓存里的旧值去覆盖就是丢配置。
        //
        let props_path = found.config_path();
        let props_exists = props_path.is_file();
        let props = if props_exists {
            Properties::read(&props_path)
        } else {
            Properties::default()
        };
        self.managing = Some(Managing {
            dir: found.dir.clone(),
            name: found.name.clone(),
            core: found.core,
            summary: found.summary(),
            eula: found.eula,
            recognized: found.recognized,
            mcdr: found.mcdr,
            flavor: found.flavor,
            props,
            props_path,
            props_exists,
            saved: false,
            error: None,
            env: None,
            probing: false,
        });
    }
}

/// 页面交给主界面的动作。
pub enum Action {
    /// 重新扫一遍磁盘。
    Reload,
    /// 弹文件夹选择框，挑一个服务端目录（已有的就认，空的就当场装）。
    PickFolder,
    /// 打开某个服务端的管理页。
    OpenManage(PathBuf),
    /// 退出管理页，回列表。
    CloseManage,
    /// 在管理页点「安装核心」——进三步流程，目标预设成这个目录。
    ///
    /// 只有还没装核心的目录会走到这里（`!managing.recognized`）；换核心
    /// 那个入口已经去掉了。
    InstallCore(PathBuf),
    /// 玩家点了「移除目录」，先弹确认框（`list_view` 自己消费，不往上传）。
    AskHide(PathBuf),
    /// 把某个目录从列表里移掉（不碰磁盘，已经过二次确认）。
    ///
    /// 我们自己装的、和从别处接进来的，走的是同一条：都只加进隐藏名单。
    /// 玩家不想看见它，不代表想删掉他的存档。
    Hide(PathBuf),
    /// 去拉某个 (核心, 游戏版本) 的加载器版本列表。
    LoadLoaders {
        core: CoreKind,
        mc_version: String,
    },
    /// 开始安装核心。
    Create(Box<CreateRequest>),
    /// 打开目录。
    OpenFolder(PathBuf),
    /// 用系统默认程序打开一个配置文件（`server.properties` / `velocity.toml`）。
    OpenConfig(PathBuf),
    /// 管理页里点「检测 MCDR 环境」。
    ///
    /// 和新建流程里的 `ProbeEnv` 分开：流程那份写 `Form.env`，这份写
    /// `Managing.env`，两边可能同时开着，共用一个会让结果串味。
    ProbeManageEnv,
    /// 管理页里点「检查 MCDR 更新」。
    CheckManageUpdate,
    /// 空文件夹里直接建一份带 MCDR 的：进三步流程，且第 3 步的勾选框
    /// 预先打上。
    InstallMcdr(PathBuf),
    /// 已经有核心的目录要点「启用 MCDReforged」，先弹确认框
    /// （`manage_view` 自己消费，不往上传）。
    AskMcdr(PathBuf),
    /// 确认过了：把整个目录搬进 `server/` 并生成 MCDR 骨架。不可逆。
    AdoptMcdr(PathBuf),
    /// 同意 / 撤回 EULA（在管理页里点）。
    SetEula { dir: PathBuf, accepted: bool },
    /// 保存配置。
    ///
    /// 路径与「这是哪个服」分开传：套了 MCDR 的，`server.properties` 在
    /// `server/` 子目录里，而界面上要说的是外层那个目录名。
    SaveConfig {
        path: PathBuf,
        label: PathBuf,
        props: Properties,
    },
    /// 探一下本机的 Python / MCDR。
    ProbeEnv,
    /// 问 PyPI 上 MCDR 的最新版本。
    CheckMcdrUpdate,
    /// 没有 Python 时，把下载页打开。
    OpenPythonDownload,
}

/// 新建请求（界面填得出来的部分；java / 下载源由 `main.rs` 补）。
pub struct CreateRequest {
    pub plan: server::Plan,
    /// 装进哪个目录；None 表示 `servers/<名字>`。
    pub target: Option<PathBuf>,
    /// 这个目录本来就有东西（从管理页进来的），走 `server::reinstall`：
    /// 允许非空、失败不删目录。新建的话是 false，允许那种「删掉半成品」的收尾。
    pub existing: bool,
    /// 套一层 MCDReforged（核心装进 `server/`，外层生成骨架）。
    pub mcdr: bool,
    /// 套 MCDR 时要用的 Python。
    pub python: Option<PathBuf>,
}

/// 这一页现在该画哪一层。
///
/// 三层是叠着往上盖的：流程盖住管理页，管理页盖住列表。顺序要紧——
/// 从管理页点「安装核心」进来的那份流程，`managing` 是刻意留着的
/// （取消之后要退回那一页），要是先看 `managing`，流程永远画不出来，
/// 玩家得先点一次左上角返回才看得见，看着就像「点了没反应」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    Flow,
    Manage,
    List,
}

fn layer_of(form_open: bool, managing: bool) -> Layer {
    if form_open {
        Layer::Flow
    } else if managing {
        Layer::Manage
    } else {
        Layer::List
    }
}

/// 画整页。
pub fn page(
    ui: &mut egui::Ui,
    state: &mut State,
    game_versions: &[String],
    busy: bool,
) -> Option<Action> {
    match layer_of(state.form.open, state.managing.is_some()) {
        Layer::Flow => new_flow(ui, state, game_versions, busy),
        Layer::Manage => manage_view(ui, state, busy),
        Layer::List => list_view(ui, state, busy),
    }
}

// ---------------------------------------------------------------- 列表

fn list_view(ui: &mut egui::Ui, state: &mut State, busy: bool) -> Option<Action> {
    let mut action = None;

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("服务端管理").size(17.0).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::refresh_button(ui, !busy).clicked() {
                action = Some(Action::Reload);
            }
            ui.add_space(6.0);
            // 唯一的入口：先加目录，再看里面有没有东西。
            //
            // 不设「新建服务端」按钮，是因为那两条路本来就是一回事——
            // 玩家想装的服也得先有一个目录。把它拆成两个按钮，等于让他
            // 在点之前就得知道「这个文件夹现在空不空」，而他往往不知道。
            if theme::primary_button(ui, "添加服务端目录", !busy).clicked() {
                action = Some(Action::PickFolder);
            }
        });
    });
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "点「添加服务端目录」挑一个文件夹：里面已经有服务端，就把它的核心和版本认出来；\n\
             里面是空的，就在原地装一个。目录下的文件我们只读，不会覆盖。",
        )
        .size(11.0)
        .color(theme::TEXT_FAINT),
    );

    if let Some((text, is_error)) = &state.notice {
        ui.add_space(8.0);
        theme::card().show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .size(11.5)
                    .color(if *is_error { theme::DANGER } else { theme::accent_strong() }),
            );
        });
    }

    if let Some(name) = &state.installing {
        ui.add_space(10.0);
        theme::card().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!("正在装 servers/{}", name))
                        .size(12.0)
                        .color(theme::TEXT),
                );
                let speed = state
                    .progress
                    .as_ref()
                    .map(|step| speed_text(step.speed_bps))
                    .unwrap_or_default();
                if !speed.is_empty() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(speed)
                                .size(12.0)
                                .color(theme::accent_strong()),
                        );
                    });
                }
            });
            if let Some(step) = &state.progress {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(step.text.clone())
                        .size(11.0)
                        .color(theme::TEXT_WEAK),
                );
                ui.add_space(6.0);
                match step.ratio {
                    // 知道进度：画真的进度条。
                    Some(frac) => {
                        ui.add(
                            egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                                .desired_width(ui.available_width())
                                .text(step.detail.clone().unwrap_or_default()),
                        );
                    }
                    // 说不清进度，或只知道已下多少：动画条 + 一行补充说明。
                    None => {
                        crate::indeterminate_bar(ui, 14.0);
                        if let Some(detail) = &step.detail {
                            ui.add_space(3.0);
                            ui.label(
                                egui::RichText::new(detail.clone())
                                    .size(10.5)
                                    .color(theme::TEXT_FAINT),
                            );
                        }
                    }
                }
            }
        });
    }

    ui.add_space(10.0);
    if state.servers.is_empty() {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(if state.loaded {
                    "还没有服务端。点右上「添加服务端目录」，\n挑一个已有的服，或者一个空文件夹让它就地装一个。"
                } else {
                    "正在读 servers/ …"
                })
                .size(12.0)
                .color(theme::TEXT_FAINT),
            );
        });
        return action;
    }

    let mut pending: Option<Action> = None;
    egui::ScrollArea::vertical()
        .id_salt("server-list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            for entry in &state.servers {
                if let Some(next) = server_card(ui, entry, busy) {
                    pending = Some(next);
                }
            }
        });
    // `AskHide` 就地换成「弹不弹框」的状态，不再往上走。
    if let Some(next) = pending {
        match next {
            Action::AskHide(dir) => state.confirm_hide = Some(dir),
            Action::AskMcdr(dir) => state.confirm_mcdr = Some(dir),
            other => action = Some(other),
        }
    }

    // 「移除目录」的二次确认。
    //
    // 只说「不删文件」还不够——玩家真正担心的是「我的存档还在吗」，
    // 所以小字里单独点一句，还告诉他能加回来：这是个可撤销的动作，
    // 一旦让他误以为会删掉世界，以后连「移除目录」都不敢碰了。
    if let Some(dir) = state.confirm_hide.clone() {
        let mut dismissed = false;
        let mut confirmed = false;
        egui::Modal::new(egui::Id::unique("aicraft-server-hide"))
            .frame(theme::dialog_frame())
            .show(ui.ctx(), |ui| {
                ui.set_width(400.0);
                ui.label(
                    egui::RichText::new("把这个目录从列表里移除？")
                        .size(15.0)
                        .strong(),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(short_path(&dir))
                        .size(11.0)
                        .color(theme::TEXT_WEAK),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("仅移除目录卡片，不会删除实际文件")
                        .size(11.0)
                        .color(theme::TEXT_FAINT),
                );
                ui.add_space(14.0);
                // 两枚按钮贴右边，主操作（移除）在最右——和界面上其他对话框
                // 一致。`right_to_left` 里先 add 的落在最右，所以顺序是反的。
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::dialog_danger_button(ui, "移除", true, 96.0).clicked() {
                        confirmed = true;
                    }
                    ui.add_space(6.0);
                    if theme::dialog_button(ui, "取消", true, 88.0).clicked() {
                        dismissed = true;
                    }
                });
            });
        if confirmed {
            state.confirm_hide = None;
            action = Some(Action::Hide(dir));
        } else if dismissed {
            state.confirm_hide = None;
        }
    }

    action
}

/// 一张服务端卡片。
fn server_card(ui: &mut egui::Ui, entry: &Server, busy: bool) -> Option<Action> {
    let mut action = None;
    // 绿框原本的意思是「这个服现在起不来，等你点一下」。它唯一的来源是
    // 「还没同意 EULA」，而列表已经不显示 EULA 状态了——留着一个不解释
    // 自己的高亮，比不亮更让人犯嘀咕。所以现在一律白卡。
    let frame = theme::card();
    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            let icon = egui::vec2(34.0, 34.0);
            let (rect, _) = ui.allocate_exact_size(icon, egui::Sense::hover());
            if entry.core == CoreKind::Velocity {
                theme::proxy_glyph(ui, rect);
            } else {
                theme::loader_glyph(ui, rect, loader_of(entry.core));
            }
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&entry.name).size(13.5).strong());
                    if !entry.managed {
                        theme::chip(ui, "外部");
                    }
                    // 代理端和「外部」「空目录」一样，是这一份是什么的标签，
                    // 所以并排放在名字后面，而不是单独占一行说明文字。
                    if entry.core == CoreKind::Velocity {
                        theme::chip(ui, "代理端");
                    }
                    if !entry.recognized {
                        theme::chip(ui, "空目录");
                    }
                    if entry.mcdr {
                        theme::chip(ui, "MCDR");
                    }
                    // Paper / Purpur 这些在核心种类上就是「原版」，光看摘要
                    // 玩家以为开的是原版服。这里补一枚，说明 jar 是谁家的。
                    if !entry.flavor.is_empty() {
                        theme::chip(ui, entry.flavor);
                    }
                });
                // 每一行都带上目录位置：玩家可能同时有好几个同名的服
                // （`survival` 摆在不同盘上），只写名字分不清是哪一个。
                //
                // 认不出核心的目录没有版本可写，就不占那一行——名字旁边
                // 已经有「空目录」这个标签了，再补一句「没认出服务端核心」
                // 是把同一件事说两遍，而且它长得像警告，其实只是个空文件夹。
                if entry.recognized {
                    ui.label(
                        egui::RichText::new(entry.summary())
                            .size(11.0)
                            .color(theme::TEXT_WEAK),
                    );
                }
                ui.label(
                    egui::RichText::new(short_path(&entry.dir))
                        .size(10.5)
                        .color(theme::TEXT_FAINT),
                );
            });
            // 每张卡片右侧都是同样三枚：移除目录 / 打开目录 / 管理。
            //
            // 认没认出核心、是不是我们装的，都不改变这三枚——具体做什么
            // （装核心、同意 EULA、改服务器配置）全在「管理」里面。
            // 卡片上按情况增减按钮，玩家每次都得先读一遍才敢点。
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::dialog_primary_button(ui, "管理", !busy, 66.0).clicked() {
                    action = Some(Action::OpenManage(entry.dir.clone()));
                }
                ui.add_space(6.0);
                if theme::dialog_button(ui, "打开目录", !busy, 82.0).clicked() {
                    action = Some(Action::OpenFolder(entry.dir.clone()));
                }
                ui.add_space(6.0);
                if theme::dialog_danger_button(ui, "移除目录", !busy, 82.0).clicked() {
                    // 不当场移除，先问一句——这个动作虽然不碰磁盘，
                    // 但玩家按下去的时候并不知道这一点。
                    action = Some(Action::AskHide(entry.dir.clone()));
                }
            });
        });

        // 卡片底部不再追加任何说明文字。
        //
        // EULA 状态在「管理」里看；代理端已经在名字旁边有标签；认不出核心
        // 的目录有「空目录」标签。每一句补丁都让列表更像待办清单，而玩家
        // 想知道的其实只是「这一行是谁、在哪、要点哪个按钮」。
    });
    action
}

/// 把完整路径压成一行，太长的前面省略。
///
/// 卡片上要显示目录位置，但 `E:\Minecraft\server\du\[30011]lobby` 这种
/// 一撑就把右边的按钮挤出可视区。留尾部：盘符和顶层目录名字玩家本来就记得，
/// 认不出来的是最后那一两层。
///
/// 从后往前保留完整的路径段，凑够了就加个 `…\` 开头——**不从字符中间切**，
/// 半截目录名比省略号更让人困惑。
fn short_path(path: &std::path::Path) -> String {
    const LIMIT: usize = 58;
    let text = path.display().to_string();
    if text.chars().count() <= LIMIT {
        return text;
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0usize;
    for part in text.split('\\').rev() {
        // +1 是那根分隔符
        if used + part.chars().count() + 1 > LIMIT {
            break;
        }
        used += part.chars().count() + 1;
        kept.push(part);
    }
    kept.reverse();
    if kept.is_empty() {
        // 单段就超长（比如一个超长文件夹名），只能硬截尾巴
        return format!("…{}", text.chars().skip(text.chars().count() - LIMIT).collect::<String>());
    }
    format!("…\\{}", kept.join("\\"))
}

/// 核心种类 → 图标用的加载器种类（原版用草方块）。
fn loader_of(core: CoreKind) -> Option<aicraft_core::loader::LoaderKind> {
    use aicraft_core::loader::LoaderKind;
    match core {
        CoreKind::Vanilla => None,
        CoreKind::Fabric => Some(LoaderKind::Fabric),
        CoreKind::Forge => Some(LoaderKind::Forge),
        CoreKind::NeoForge => Some(LoaderKind::NeoForge),
        // 代理端没有对应的加载器图标，先借草方块——总比空白好认。
        CoreKind::Velocity => None,
    }
}

// ---------------------------------------------------------------- 新建流程

/// 新建服务端的三步流程。
///
/// 和「下载版本」的向导一个节奏：一步一件事，底下永远是「上一步 / 下一步」，
/// 最后一步才变成「开始安装」。做成整页而不是弹窗，是因为这三步里有两步要
/// 在几十个版本号里挑——弹窗那么点地方，一屏只看得到三四个。
fn new_flow(
    ui: &mut egui::Ui,
    state: &mut State,
    game_versions: &[String],
    busy: bool,
) -> Option<Action> {
    let mut action = None;

    // 标题行：返回 + 标题 + 步骤指示
    let cancel = crate::flow::header(
        ui,
        if state.form.existing {
            "‹ 管理"
        } else {
            "‹ 服务端列表"
        },
        if state.form.existing {
            "安装核心"
        } else {
            "新建服务端"
        },
        &state.form.step_label(),
    );
    ui.add_space(10.0);

    match state.form.step {
        Step::Version => version_step(ui, state, game_versions),
        Step::Name => name_step(ui, state),
        Step::Core => {
            if let Some(next_action) = core_step(ui, state) {
                action = Some(next_action);
            }
        }
    }

    // 按钮钉在底下：三步的内容高矮差得多，按钮跟着内容跑会一跳一跳。
    let note = (state.form.step == Step::Version && !state.form.mc_version.is_empty())
        .then(|| format!("已选 {}", state.form.mc_version));
    let out = crate::flow::footer(
        ui,
        crate::flow::Buttons {
            primary: if state.form.step == Step::Core {
                "开始安装"
            } else {
                "下一步"
            },
            enabled: state.form.can_advance() && !busy && state.installing.is_none(),
            back: (state.form.step != Step::Version).then_some("上一步"),
            cancel: Some("取消"),
            right_aligned: false,
            note: note.as_deref(),
        },
    );

    if cancel || out.cancel {
        state.form.open = false;
        state.notice = None;
        // 从管理页点进来的，退回管理页；从列表点进来的，回列表。
        if state.managing.is_none() {
            state.form.existing = false;
        }
        return None;
    }
    if out.back {
        state.form.step = state.form.step.prev();
        return None;
    }
    if out.next && state.form.can_advance() {
        if state.form.step == Step::Core {
            let plan = server::Plan {
                name: state.form.name.trim().to_string(),
                core: state.form.core,
                mc_version: state.form.mc_version.clone(),
                core_version: if state.form.core.needs_loader_version() {
                    state.form.core_version.clone()
                } else {
                    String::new()
                },
                memory_mb: state.form.memory_mb,
                // java / 下载源 / 限速由 main 拿设置补。
                java: None,
                mirror: aicraft_core::settings::Mirror::Auto,
                limit_kbps: None,
                mcdr: state.form.mcdr,
                python: state
                    .form
                    .env
                    .as_ref()
                    .and_then(|env| env.python.as_ref())
                    .map(|python| python.path.clone()),
            };
            action = Some(Action::Create(Box::new(CreateRequest {
                plan,
                target: state.form.target.clone(),
                existing: state.form.existing,
                mcdr: state.form.mcdr,
                python: state
                    .form
                    .env
                    .as_ref()
                    .and_then(|env| env.python.as_ref())
                    .map(|python| python.path.clone()),
            })));
        } else {
            state.form.step = state.form.step.next();
        }
    }
    action
}

impl Step {
    /// 下一步。最后一步没有更后的地方，停在原地——`next().prev()` 才是可逆的。
    fn next(self) -> Step {
        match self {
            Step::Version => Step::Name,
            Step::Name => Step::Core,
            Step::Core => Step::Core,
        }
    }

    fn prev(self) -> Step {
        match self {
            Step::Version | Step::Name => Step::Version,
            Step::Core => Step::Name,
        }
    }
}

/// 第一步：从官方清单里挑一个版本。
fn version_step(ui: &mut egui::Ui, state: &mut State, game_versions: &[String]) {
    ui.horizontal(|ui| {
        let mut resp = ui.add(
            egui::TextEdit::singleline(&mut state.form.query)
                .hint_text("搜索版本号…")
                .desired_width(200.0),
        );
        theme::input_menu(&mut resp, &mut state.form.query, false);
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!("共 {} 个正式版", game_versions.len()))
                .size(11.0)
                .color(theme::TEXT_FAINT),
        );
    });
    ui.add_space(8.0);

    if game_versions.is_empty() {
        crate::flow::loading(ui, "正在拉官方版本清单，稍等一下…");
        return;
    }

    let query = state.form.query.trim().to_lowercase();
    let visible: Vec<&String> = game_versions
        .iter()
        .filter(|id| query.is_empty() || id.to_lowercase().contains(&query))
        .collect();

    if visible.is_empty() {
        crate::flow::empty(ui, "没有匹配的版本号");
        return;
    }

    ui.label(
        egui::RichText::new("服务端只装正式版——快照与远古版本的 server.jar 通常没有发布。")
            .size(10.5)
            .color(theme::TEXT_FAINT),
    );
    ui.add_space(6.0);

    let picked = crate::flow::list(ui, "server-version-list", |ui| {
        let mut picked = None;
        let count = visible.len();
        for (i, id) in visible.iter().enumerate() {
            let selected = state.form.mc_version == **id;
            if crate::flow::row(ui, id, selected, i + 1 == count) {
                picked = Some((*id).clone());
            }
        }
        picked
    });
    if let Some(id) = picked {
        state.form.mc_version = id;
        state.form.core_version.clear();
        state.form.loader_versions.clear();
        state.form.loaded_for = None;
    }
}

/// 第二步：起名字、定内存。
fn name_step(ui: &mut egui::Ui, state: &mut State) {
    theme::card().show(ui, |ui| {
        ui.label(
            egui::RichText::new(if state.form.existing {
                "这一份的名字"
            } else {
                "给这个服务端起个名字"
            })
            .size(13.0)
            .strong(),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(if state.form.existing {
                "名字跟着目录走，改不了——想换名字得换个目录重新装。"
            } else {
                "会成为 servers/ 下面的文件夹名，也是列表里显示的名字。"
            })
            .size(11.0)
            .color(theme::TEXT_WEAK),
        );
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            field_label(ui, "名字");
            if state.form.existing {
                ui.label(
                    egui::RichText::new(state.form.name.trim())
                        .size(12.0)
                        .color(theme::TEXT),
                );
            } else {
                let mut resp = ui.add(
                    egui::TextEdit::singleline(&mut state.form.name)
                        .desired_width(200.0)
                        .hint_text("survival"),
                );
                theme::input_menu(&mut resp, &mut state.form.name, false);
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            field_label(ui, "");
            if state.form.existing {
                let where_to = state
                    .form
                    .target
                    .as_ref()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_default();
                ui.label(
                    egui::RichText::new(format!("会装到 {}", where_to))
                        .size(11.0)
                        .color(theme::TEXT_FAINT),
                );
            } else {
                match state.form.name_hint() {
                    Some(hint) => {
                        ui.label(egui::RichText::new(hint).size(11.0).color(theme::DANGER));
                    }
                    None => {
                        let where_to = match &state.form.target {
                            Some(dir) => dir.display().to_string(),
                            None => format!("servers/{}", state.form.name.trim()),
                        };
                        ui.label(
                            egui::RichText::new(format!("会装到 {}", where_to))
                                .size(11.0)
                                .color(theme::TEXT_FAINT),
                        );
                    }
                }
            }
        });

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            field_label(ui, "内存");
            let mut mb = state.form.memory_mb;
            let response = ui.add(
                egui::DragValue::new(&mut mb)
                    .range(512..=65536)
                    .speed(64.0)
                    .suffix(" MiB"),
            );
            if response.changed() {
                state.form.memory_mb = mb;
            }
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("会写进启动脚本的 -Xmx")
                    .size(10.5)
                    .color(theme::TEXT_FAINT),
            );
        });
    });
}

/// 第三步：选核心与它的版本。
fn core_step(ui: &mut egui::Ui, state: &mut State) -> Option<Action> {
    let mut action = None;

    ui.horizontal(|ui| {
        field_label(ui, "核心");
        for core in CoreKind::ALL {
            let picked = state.form.core == core;
            if theme::radio(ui, picked, core.label()).clicked() && !picked {
                state.form.core = core;
                state.form.core_version.clear();
                state.form.loader_versions.clear();
                state.form.loader_error = None;
                state.form.loaded_for = None;
            }
            ui.add_space(12.0);
        }
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        field_label(ui, "");
        ui.label(
            egui::RichText::new(format!("Minecraft {}", state.form.mc_version))
                .size(11.0)
                .color(theme::TEXT_FAINT),
        );
    });

    if state.form.core.needs_loader_version() {
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new(format!("选一个 {} 版本", state.form.core.label()))
                .size(12.0)
                .strong(),
        );
        ui.add_space(6.0);

        if state.form.loading_loaders {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("正在问官方要版本…")
                        .size(11.0)
                        .color(theme::TEXT_WEAK),
                );
            });
            return action;
        }
        if let Some(error) = &state.form.loader_error {
            ui.label(
                egui::RichText::new(error)
                    .size(11.0)
                    .color(theme::DANGER),
            );
            return action;
        }

        let picked = crate::flow::list(ui, "server-loader-list", |ui| {
            let mut picked = None;
            let count = state.form.loader_versions.len();
            for (i, version) in state.form.loader_versions.iter().enumerate() {
                let selected = state.form.core_version == *version;
                if crate::flow::row(ui, version, selected, i + 1 == count) {
                    picked = Some(version.clone());
                }
            }
            picked
        });
        if let Some(version) = picked {
            state.form.core_version = version;
        }

        // 要了列表还没有 —— 顺手问一次。
        // `loader_error.is_none()` 这个条件不能少：拉失败之后列表永远对不上，
        // 少了它每帧都会重发请求，网络断了会变成死循环刷日志。
        if !state.form.matches() && !state.form.mc_version.is_empty() && state.form.loader_error.is_none()
        {
            state.form.loading_loaders = true;
            action = Some(Action::LoadLoaders {
                core: state.form.core,
                mc_version: state.form.mc_version.clone(),
            });
        }
    } else {
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new(format!(
                "原版服务端：Minecraft {} 的官方 server.jar，不带任何加载器。",
                state.form.mc_version
            ))
            .size(11.0)
            .color(theme::TEXT_WEAK),
        );
    }

    // ---- 要不要再套一层 MCDR --------------------------------------
    ui.add_space(14.0);
    let mut mcdr = state.form.mcdr;
    if ui
        .checkbox(&mut mcdr, "同时套一层 MCDReforged（有插件和 !!命令）")
        .changed()
    {
        state.form.mcdr = mcdr;
        // 第一次勾上才去探，省得没打算用的人也要等这一下。
        if mcdr && state.form.env.is_none() && !state.form.probing {
            state.form.probing = true;
            action = Some(Action::ProbeEnv);
        }
    }

    if state.form.mcdr {
        ui.add_space(6.0);
        mcdr_env(ui, state, &mut action);
    }

    action
}

/// 勾了 MCDR 之后那一段：探测结果的三种说法。
///
/// 缺 Python 和「有 Python 但没装 MCDR」给的操作完全不同（一个是去装 Python，
/// 一个是在命令行跑一句 pip），所以分开写，不合并成一句「环境不完整」。
fn mcdr_env(ui: &mut egui::Ui, state: &mut State, action: &mut Option<Action>) {
    if state.form.probing {
        probing_row(ui);
        return;
    }

    let Some(env) = state.form.env.clone() else {
        if ui.button("检测环境").clicked() {
            state.form.probing = true;
            *action = Some(Action::ProbeEnv);
        }
        return;
    };

    match env_body(ui, &env) {
        Some(EnvWant::Probe) => {
            state.form.probing = true;
            *action = Some(Action::ProbeEnv);
        }
        Some(EnvWant::Update) => *action = Some(Action::CheckMcdrUpdate),
        Some(EnvWant::OpenPython) => *action = Some(Action::OpenPythonDownload),
        None => {}
    }
}

/// 环境探测那一行的「转圈 + 一句话」。
///
/// 新建流程和管理页都要画，抽出来免得两处各写一遍以后越走越偏。
fn probing_row(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("正在看本机的 Python 和 MCDR…")
                .size(11.0)
                .color(theme::TEXT_WEAK),
        );
    });
}

/// 环境卡片上玩家可能点到的两件事。
///
/// 只回一个「想干什么」而不是直接回 `Action`：新建流程和管理页各有各的
/// 探测动作，混用会让管理页点一下把结果写进流程表单里。
#[derive(Clone, Copy, PartialEq, Eq)]
enum EnvWant {
    Probe,
    Update,
    OpenPython,
}

/// 已经把环境探回来了：按三档画结论与按钮。
fn env_body(ui: &mut egui::Ui, env: &aicraft_core::mcdr::Env) -> Option<EnvWant> {
    let mut want = None;

    // 两件独立的事并排写：竖着码两行会把卡片右半截甩成一片空白，
    // 横过来一屏就能看完，也不用再往右看齐什么。
    //
    // 不用「环境不完整」这种概括——玩家看一眼就知道是差 Python 还是差包，
    // 这两件事要做的动作完全不同。
    ui.columns(2, |cols| {
        match env.python.as_ref() {
            // 只写版本号：前面「Python：」已经说了是什么，再拼一次名字就重复了。
            Some(python) if python.usable() => {
                env_line(&mut cols[0], "Python：", &python.version, true)
            }
            Some(python) => env_line(
                &mut cols[0],
                "Python：",
                &format!("{}（版本太旧，至少要 3.9）", python.version),
                false,
            ),
            None => env_line(&mut cols[0], "Python：", "缺少Python运行时", false),
        }
        match env.mcdr_version.as_deref() {
            Some(version) => env_line(&mut cols[1], "MCDReforg：", version, true),
            None => env_line(&mut cols[1], "MCDReforg：", "未安装", false),
        }
    });

    // 只有「Python 有了但没装包」才需要多给一句能直接照抄的命令；
    // 缺 Python 时上面那行已经说清楚了，下面有下载页按钮。
    if env.python.as_ref().is_some_and(|python| python.usable()) && env.mcdr_version.is_none()
        && let Some(hint) = env.missing() {
        ui.add_space(2.0);
        ui.label(egui::RichText::new(hint).size(10.5).color(theme::WARN));
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("重新检测").clicked() {
            want = Some(EnvWant::Probe);
        }
        if env.ready() && ui.button("检查 MCDR 更新").clicked() {
            want = Some(EnvWant::Update);
        }
        // 没有 Python 就把下载页直接打开，别让人自己去搜。
        if env.python.is_none() && ui.button("打开 Python 下载页").clicked() {
            want = Some(EnvWant::OpenPython);
        }
    });
    want
}

/// 一行「名称：值」。值能用的写绿，缺的写橙。
///
/// 名称那一段用定宽，两行的冒号才对得齐——直接 `ui.label` 拼起来会因为
/// 「Python」和「MCDReforg」字宽不同而错开，看着像是两张表硬拼在一起。
fn env_line(ui: &mut egui::Ui, name: &str, value: &str, ok: bool) {
    const NAME_W: f32 = 96.0;
    const ROW_H: f32 = 22.0;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(NAME_W, ROW_H), egui::Sense::hover());
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            name,
            egui::FontId::proportional(11.5),
            theme::TEXT_WEAK,
        );
        let color = if ok { theme::accent_strong() } else { theme::WARN };
        ui.label(egui::RichText::new(value).size(11.5).color(color));
    });
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(74.0, 20.0), egui::Sense::hover());
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(11.5),
        theme::TEXT_WEAK,
    );
}

// ---------------------------------------------------------------- 管理视图

/// 一个服务端的全部操作，照设置页的样子排版：一页几张卡片，节间一条细线。
///
/// 为什么都收在这一页：列表上每张卡片能放三枚按钮就已经很挤，而「装核心」
/// 「改配置」「同意 EULA」三件事玩家是一次性做完的——来回跳反而更累。
fn manage_view(ui: &mut egui::Ui, state: &mut State, busy: bool) -> Option<Action> {
    let mut action = None;
    let mut close = false;

    let managing = state.managing.as_mut()?;

    // 外层是滚动区，浮动滚动条压在内容右侧；卡片按可用宽度铺满会正好被它
    // 盖住一条边。`set_width` 设的是卡片**内层**宽度，`theme::card()` 自己
    // 还有 16px 的左右内边距，所以外层会比它多 32px —— 先扣掉那 32，
    // 再让出 24px 的右边距。和设置页用的是同一个算法。
    let card_width = (ui.available_width() - 32.0 - 24.0).max(320.0);

    ui.horizontal(|ui| {
        if theme::text_button(ui, "‹ 服务端列表", false).clicked() {
            close = true;
        }
        ui.add_space(8.0);
        ui.label(egui::RichText::new(&managing.name).size(16.0).strong());
        if !managing.recognized {
            theme::chip(ui, "空目录");
        }
        if managing.mcdr {
            theme::chip(ui, "MCDR");
        }
        if !managing.flavor.is_empty() {
            theme::chip(ui, managing.flavor);
        }
    });

    if let Some(error) = &managing.error {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(error).size(11.0).color(theme::DANGER));
    }

    ui.add_space(10.0);
    egui::ScrollArea::vertical()
        .id_salt("server-manage")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 9.0;

            // ---- 概况 ----
            crate::settings::section(ui, "概况");
            theme::card().show(ui, |ui| {
                ui.set_width(card_width);
                if core_row(ui, managing) {
                    action = Some(Action::InstallCore(managing.dir.clone()));
                }
                crate::settings::divider(ui);
                row(ui, "位置", &managing.dir.display().to_string());
                if managing.mcdr {
                    crate::settings::divider(ui);
                    // 套了 MCDR 的，玩家改配置时要找的是 `server/` 里那份，
                    // 把位置直接写出来省得他去翻。
                    row(
                        ui,
                        "服务端目录",
                        &aicraft_core::mcdr::inner_dir(&managing.dir)
                            .display()
                            .to_string(),
                    );
                }
            });

            // ---- MCDReforged ----
            //
            // 单独一张卡片：它是「这个服要不要多一层外壳」的决定，和
            // 「装哪个核心」不是同一件事。原来只藏在新建流程的第 3 步里，
            // 已经有服的人翻不到，所以管理页也摆一张。
            crate::settings::section(ui, "MCDReforged");
            theme::card().show(ui, |ui| {
                ui.set_width(card_width);
                let ready = managing.can_toggle_mcdr();

                // 顶行：一句说明靠左，开关靠右。开关是这张卡片唯一的主操作，
                // 贴在这一行玩家一眼就能找到；原先它单独占底部一整行，
                // 右边空出一大片，看着像是没画完。
                ui.horizontal_top(|ui| {
                    const SWITCH_W: f32 = 44.0;
                    let text_w = (ui.available_width() - SWITCH_W - 16.0).max(200.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(text_w, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.label(
                                egui::RichText::new(if managing.mcdr {
                                    "这一份已经套了 MCDReforged：真正的服务端在 server/ 里，\
                                     用 help 面板的 !!命令管它。"
                                } else {
                                    "MCDReforged 是套在服务端外面的一层：装了它就能用插件、\
                                     用 !!命令做热重载，不用重启。"
                                })
                                .size(11.0)
                                .color(theme::TEXT_WEAK),
                            );
                        },
                    );
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Min),
                        |ui| {
                            // 开关本身只画不翻：拨过去不可逆，得先过确认框。
                            let toggle = ui
                                .allocate_ui(egui::vec2(SWITCH_W, 20.0), |ui| {
                                    theme::switch(ui, "", managing.mcdr, ready)
                                })
                                .inner;
                            if toggle.clicked() && !managing.mcdr && ready {
                                action = Some(Action::AskMcdr(managing.dir.clone()));
                            }
                        },
                    );
                });

                ui.add_space(8.0);

                // 环境没探过就先摆一颗按钮：开关点不动的时候玩家得知道该先做什么。
                let want = if managing.probing {
                    probing_row(ui);
                    None
                } else if let Some(env) = managing.env.clone() {
                    env_body(ui, &env)
                } else {
                    if ui.button("检测环境").clicked() {
                        action = Some(Action::ProbeManageEnv);
                    }
                    None
                };
                match want {
                    Some(EnvWant::Probe) => action = Some(Action::ProbeManageEnv),
                    Some(EnvWant::Update) => action = Some(Action::CheckManageUpdate),
                    Some(EnvWant::OpenPython) => {
                        action = Some(Action::OpenPythonDownload);
                    }
                    None => {}
                }

                if managing.mcdr || ready {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(if managing.mcdr {
                            "已经是开着的了，没有关掉的入口——想拆回去只能打开\
                             资源管理器，把 server/ 里的东西搬回外面。"
                        } else if managing.recognized {
                            "打开之后，这个目录里现有的东西会全部搬进 server/，\
                             这一步不可逆。"
                        } else {
                            "打开之后，核心会装在 server/ 里，外层由 MCDReforged 接管。"
                        })
                        .size(11.0)
                        .color(theme::TEXT_FAINT),
                    );
                }

                // 空目录仍然给一条「一步到位」的捷径：勾上开关之后还得选核心、
                // 起名字，而这里的人可能只是想快点开一个带插件的服。
                if !managing.recognized && ready && !managing.mcdr {
                    ui.add_space(8.0);
                    if theme::primary_button(ui, "建一份带 MCDR 的", !busy).clicked() {
                        action = Some(Action::InstallMcdr(managing.dir.clone()));
                    }
                }
            });

            // ---- EULA（代理端没有这个文件）----
            if managing.core != CoreKind::Velocity && managing.recognized {
                crate::settings::section(ui, "EULA");
                theme::card().show(ui, |ui| {
                    ui.set_width(card_width);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(if managing.eula {
                                "已同意 Mojang 的最终用户许可协议。"
                            } else {
                                "还没同意 Mojang 的 EULA，服务端起不来。"
                            })
                            .size(11.5)
                            .color(if managing.eula {
                                theme::TEXT_WEAK
                            } else {
                                theme::WARN
                            }),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if managing.eula {
                                if theme::dialog_button(ui, "撤回", !busy, 76.0).clicked() {
                                    action = Some(Action::SetEula {
                                        dir: managing.dir.clone(),
                                        accepted: false,
                                    });
                                }
                            } else if theme::dialog_primary_button(ui, "同意 EULA", !busy, 88.0)
                                .clicked()
                            {
                                action = Some(Action::SetEula {
                                    dir: managing.dir.clone(),
                                    accepted: true,
                                });
                            }
                        });
                    });
                });
            }

            // ---- 服务器配置 ----
            if managing.core == CoreKind::Velocity {
                crate::settings::section(ui, "服务器配置");
                theme::card().show(ui, |ui| {
                    ui.set_width(card_width);
                    ui.label(
                        egui::RichText::new(
                            "代理端没有 server.properties，配置在 velocity.toml 里改。",
                        )
                        .size(11.0)
                        .color(theme::TEXT_WEAK),
                    );
                    if managing.props_exists {
                        ui.add_space(8.0);
                        if theme::dialog_button(ui, "打开 velocity.toml", !busy, 120.0).clicked() {
                            action = Some(Action::OpenConfig(managing.props_path.clone()));
                        }
                    }
                });
            } else if managing.props_exists {
                // 磁盘上没有 `server.properties` 时整节都不出现：这份文件归
                // Minecraft 自己，第一次启动才生成。拿 `FIELDS` 的默认值凑
                // 一份表单摆出来，玩家改完一保存就凭空多出一份他从没读过的
                // 配置——所以这里不是「画个空状态」，是干脆不摆。
                crate::settings::section(ui, "服务器配置");
                // 先把路径抄出来：闭包里还要借着 `managing` 读别的字段，
                // 直接在里面 clone 会和 `ui` 的可变借用打架。
                let props_path = managing.props_path.clone();
                theme::card().show(ui, |ui| {
                    ui.set_width(card_width);
                    // 和设置页同一个形状：左列是「这一项叫什么」，右列是
                    // 「它现在是什么」。以前是「名字 + 控件」挤在一行左边，
                    // 控件左边缘跟着标签长短一走，扫下来像一排锯齿。
                    for field in FIELDS.iter() {
                        let current = managing
                            .props
                            .get(field.key)
                            .unwrap_or(field.default)
                            .to_string();
                        crate::settings::setting_row(ui, field.label, |ui| match field.kind {
                            FieldKind::Switch => {
                                let mut on = current == "true";
                                if ui.checkbox(&mut on, "").changed() {
                                    managing
                                        .props
                                        .set(field.key, if on { "true" } else { "false" });
                                    managing.saved = false;
                                }
                            }
                            FieldKind::Number => {
                                let mut number = current.parse::<i64>().unwrap_or(0);
                                if ui
                                    .add(egui::DragValue::new(&mut number).speed(1.0))
                                    .changed()
                                {
                                    managing.props.set(field.key, &number.to_string());
                                    managing.saved = false;
                                }
                            }
                            FieldKind::Choice(options) => {
                                let combo = theme::combo(
                                    ui,
                                    field.key,
                                    current.clone(),
                                    150.0,
                                    |ui| {
                                        for option in options {
                                            let picked = current == *option;
                                            if ui.selectable_label(picked, *option).clicked() {
                                                managing.props.set(field.key, option);
                                                managing.saved = false;
                                            }
                                        }
                                    },
                                );
                                theme::hand(combo);
                            }
                            FieldKind::Text => {
                                let mut text = current;
                                let mut resp = ui.add(
                                    egui::TextEdit::singleline(&mut text)
                                        .desired_width(220.0),
                                );
                                theme::input_menu(&mut resp, &mut text, false);
                                if resp.changed() {
                                    managing.props.set(field.key, &text);
                                    managing.saved = false;
                                }
                            }
                        });
                        if !field.hint.is_empty() {
                            ui.label(
                                egui::RichText::new(field.hint)
                                    .size(10.5)
                                    .color(theme::TEXT_FAINT),
                            );
                        }
                    }
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if theme::primary_button(ui, "保存", true).clicked() {
                        // 路径现成算好了：套 MCDR 的服它在 `server/` 那一层。
                        action = Some(Action::SaveConfig {
                            path: props_path.clone(),
                            label: managing.dir.clone(),
                            props: managing.props.clone(),
                        });
                    }
                    ui.add_space(8.0);
                    // 摆一个「用记事本打开」的出口：注释行、`FIELDS` 里没列
                    // 到的键（`level-seed`、`allow-flight`…）都只能在那里改。
                    if theme::dialog_button(ui, "打开源文件", !busy, 100.0).clicked() {
                        action = Some(Action::OpenConfig(props_path.clone()));
                    }
                    ui.add_space(8.0);
                    if managing.saved {
                        ui.label(
                            egui::RichText::new("已保存")
                                .size(11.5)
                                .color(theme::accent_strong()),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new("改完记得点保存；服务端重启后生效。")
                                .size(11.0)
                                .color(theme::TEXT_FAINT),
                        );
                    }
                });
            }
        });

    // 「启用 MCDReforged」的二次确认。
    //
    // 这是整个启动器里唯一一个**不可逆**的按钮，所以文案不能只写「确定吗」：
    // 得说清搬什么、搬到哪、以及万一想反悔该怎么做（打开资源管理器自己搬）。
    if let Some(dir) = state.confirm_mcdr.clone() {
        let mut dismissed = false;
        let mut confirmed = false;
        egui::Modal::new(egui::Id::unique("aicraft-server-mcdr"))
            .frame(theme::dialog_frame())
            .show(ui.ctx(), |ui| {
                ui.set_width(420.0);
                ui.label(
                    egui::RichText::new("启用 MCDReforged？")
                        .size(15.0)
                        .strong(),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "整个目录里现有的东西（核心、库、存档、配置）都会搬进 server/，\
                         外层由 MCDReforged 接管。",
                    )
                    .size(11.5)
                    .color(theme::TEXT_WEAK),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(
                        "搬家是在同一个目录里重命名，几 GB 的存档也是瞬间完成，不会中途损坏。",
                    )
                    .size(11.0)
                    .color(theme::TEXT_FAINT),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("这一步不可逆：启动器没有「关掉」的入口。")
                        .size(11.0)
                        .color(theme::WARN),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "想拆回去只能自己打开资源管理器，把 server/ 里的东西搬回外面。",
                    )
                    .size(11.0)
                    .color(theme::TEXT_FAINT),
                );
                ui.add_space(14.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::dialog_danger_button(ui, "启用", true, 96.0).clicked() {
                        confirmed = true;
                    }
                    ui.add_space(6.0);
                    if theme::dialog_button(ui, "取消", true, 88.0).clicked() {
                        dismissed = true;
                    }
                });
            });
        if confirmed {
            state.confirm_mcdr = None;
            action = Some(Action::AdoptMcdr(dir));
        } else if dismissed {
            state.confirm_mcdr = None;
        }
    }

    if close {
        state.managing = None;
        action = Some(Action::CloseManage);
    }
    action
}

/// 一行「标签 + 值」，标签固定宽度好让上下两行对齐。
/// 概况里的「核心」一行。
///
/// 装核心的入口就摆在这儿，不再单独占一张卡片——这一页原来那张
/// 「安装 / 更换核心」既管装也管换，而换核心（把 Forge 换成 Fabric、
/// 或者重装同一个）实际上很容易把玩家的库和启动脚本搅乱，收益远小于风险，
/// 所以只留下「从零装一个」。已经有核心的这份，这一行就只是显示。
///
/// 返回 true 表示玩家点了「安装核心」。
fn core_row(ui: &mut egui::Ui, managing: &Managing) -> bool {
    let mut install = false;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(120.0, 20.0), egui::Sense::hover());
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            "核心",
            egui::FontId::proportional(11.5),
            theme::TEXT_WEAK,
        );
        ui.label(
            egui::RichText::new(&managing.summary)
                .size(11.5)
                .color(theme::TEXT),
        );
        if !managing.recognized {
            ui.add_space(10.0);
            let button = ui.add(
                egui::Button::new(
                    egui::RichText::new("安装核心")
                        .size(11.5)
                        .color(theme::accent_strong()),
                )
                .fill(theme::accent_soft())
                .stroke(egui::Stroke::new(1.0, theme::accent_fill()))
                .corner_radius(egui::CornerRadius::same(6)),
            );
            theme::hand(button.clone());
            if button.clicked() {
                install = true;
            }
        }
    });
    install
}

fn row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(120.0, 20.0), egui::Sense::hover());
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.5),
            theme::TEXT_WEAK,
        );
        ui.label(egui::RichText::new(value).size(11.5).color(theme::TEXT));
    });
}

/// 配置保存成功后的收尾（`main.rs` 调用）。
pub fn config_saved(state: &mut State) {
    if let Some(managing) = state.managing.as_mut() {
        managing.saved = true;
        managing.error = None;
    }
}

pub fn config_failed(state: &mut State, message: String) {
    if let Some(managing) = state.managing.as_mut() {
        managing.error = Some(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_form_cannot_start() {
        let form = Form::default();
        assert!(!form.can_create(), "名字空着不该能点开始");
    }

    #[test]
    fn a_loader_core_needs_a_loader_version() {
        let form = Form {
            name: "survival".to_string(),
            mc_version: "26.3".to_string(),
            core: CoreKind::Fabric,
            ..Form::default()
        };
        assert!(!form.can_create(), "没选加载器版本不该能点开始");
        let vanilla = Form {
            core: CoreKind::Vanilla,
            ..form
        };
        assert!(vanilla.can_create(), "原版不需要加载器版本");
    }

    #[test]
    fn bad_names_are_reported_before_we_touch_the_disk() {
        let form = Form {
            name: "a/b".to_string(),
            ..Form::default()
        };
        assert!(form.name_hint().is_some_and(|h| h.contains("不能含")));
        assert!(!form.can_create());
    }

    #[test]
    fn a_picked_folder_becomes_the_target_and_keeps_the_default_name() {
        // 玩家选了 E:\srv\survival 这种目录：名字从目录名来，装的时候
        // 直接落进去，不再套一层 servers/<名字>。
        let form = Form {
            open: true,
            target: Some(PathBuf::from(r"E:\srv\survival")),
            ..Form::default()
        };
        assert_eq!(
            form.target.as_deref(),
            Some(std::path::Path::new(r"E:\srv\survival"))
        );
        // 目标目录不影响「能不能点下一步」——名字还没填，照样拦住
        assert!(!form.can_advance());
    }

    #[test]
    fn the_flow_covers_the_manage_page_it_started_from() {
        // 从管理页点「安装核心」时 `managing` 仍然留着，
        // 好让「取消」退回那一页。这时该画流程，不是管理页。
        assert_eq!(layer_of(true, true), Layer::Flow);
        // 管理页 → 列表
        assert_eq!(layer_of(false, true), Layer::Manage);
        // 普通情况
        assert_eq!(layer_of(true, false), Layer::Flow);
        assert_eq!(layer_of(false, false), Layer::List);
    }

    #[test]
    fn mcdr_gate_asks_for_a_working_environment_first() {
        // 勾了 MCDR 但环境没探到 / 不满足：不许往下走。
        // 装到一半发现起不来，比现在拦住难受得多。
        let mut form = Form {
            // 卡的是最后一步那个「开始安装」，所以得站到那一步上
            step: Step::Core,
            mcdr: true,
            mc_version: "1.20.1".to_string(),
            name: "mcdr".to_string(),
            core: CoreKind::Vanilla,
            ..Default::default()
        };
        assert!(!form.can_advance(), "还没探环境就不该放行");

        // 探到了 Python、但没有 MCDR 包
        form.env = Some(aicraft_core::mcdr::Env {
            python: Some(aicraft_core::mcdr::Python {
                path: PathBuf::from(r"D:\python\python.exe"),
                version: "3.10.9".to_string(),
                major: 3,
                minor: 10,
            }),
            mcdr_version: None,
            note: None,
        });
        assert!(!form.can_advance(), "缺 MCDR 包也不该放行");

        // 两样齐了才放行
        form.env = Some(aicraft_core::mcdr::Env {
            python: Some(aicraft_core::mcdr::Python {
                path: PathBuf::from(r"D:\python\python.exe"),
                version: "3.10.9".to_string(),
                major: 3,
                minor: 10,
            }),
            mcdr_version: Some("2.16.2".to_string()),
            note: None,
        });
        assert!(form.can_advance(), "环境齐了就该能装");

        // 不勾 MCDR 时环境是什么样都不影响
        form.mcdr = false;
        form.env = None;
        assert!(form.can_advance());
    }

    #[test]
    fn the_mcdr_switch_only_unlocks_when_the_environment_is_ready() {
        // 开关拨下去就不可逆，所以「能不能拨」只看环境：没探到就没法保证
        // 套完之后起得来，那时候已经搬完了，退不回去。
        let mut managing = Managing {
            dir: PathBuf::from(r"E:\srv\empty"),
            name: "empty".to_string(),
            core: CoreKind::Vanilla,
            summary: "空目录".to_string(),
            eula: false,
            recognized: false,
            mcdr: false,
            flavor: "",
            props: Properties::default(),
            props_path: PathBuf::from(r"E:\srv\empty\server.properties"),
            props_exists: false,
            saved: false,
            error: None,
            env: None,
            probing: false,
        };
        assert!(!managing.can_toggle_mcdr(), "还没探环境就不该放行");

        managing.env = Some(aicraft_core::mcdr::Env {
            python: None,
            mcdr_version: None,
            note: None,
        });
        assert!(!managing.can_toggle_mcdr(), "没 Python 不该放行");

        managing.env = Some(aicraft_core::mcdr::Env {
            python: Some(aicraft_core::mcdr::Python {
                path: PathBuf::from(r"D:\python\python.exe"),
                version: "3.10.9".to_string(),
                major: 3,
                minor: 10,
            }),
            mcdr_version: Some("2.16.2".to_string()),
            note: None,
        });
        assert!(managing.can_toggle_mcdr(), "环境齐了就该能拨");

        // 已经有核心的目录一样能拨——那正是「搬家」那条路。
        managing.recognized = true;
        assert!(managing.can_toggle_mcdr(), "已有核心的服也要能套");
    }

    #[test]
    fn an_external_server_is_not_deletable() {
        // 别处搬来的目录只能「移出列表」，`managed` 是这条规则唯一的依据。
        let outside = Server {
            name: "old".to_string(),
            dir: PathBuf::from(r"E:\srv\old"),
            core: CoreKind::Fabric,
            mc_version: "1.20.1".to_string(),
            core_version: "0.16.10".to_string(),
            eula: false,
            managed: false,
            recognized: true,
            mcdr: false,
            flavor: "Paper",
        };
        assert!(!outside.managed);
        assert_eq!(outside.flavor, "Paper");
    }

    #[test]
    fn a_progress_step_never_reports_speed_before_it_has_two_samples() {
        // 第一次上报只有「已下多少」，没有时间差可除。这时候要是给出
        // 速度，那一定是拿 0 当除数算出来的垃圾值。
        let mut step = StepProgress::text("下载 server.jar");
        step.absorb(1_048_576, 10_485_760);
        assert_eq!(step.speed_bps, 0.0);
        assert!(step.ratio.is_some());
        assert!(step.detail.unwrap().contains("10%"));
    }

    #[test]
    fn a_new_stage_throws_the_old_speed_away() {
        // 「查清单 → 读版本描述 → 下载 server.jar」是连着来的三步。
        // 上一步的速度留着，下载阶段开头就会显示一个跟当前完全无关的数字。
        let mut step = StepProgress::text("查清单");
        step.speed_bps = 8_388_608.0;
        step.ratio = Some(0.5);
        step.detail = Some("旧的".to_string());

        step.restart("下载 server.jar");

        assert_eq!(step.text, "下载 server.jar");
        assert_eq!(step.speed_bps, 0.0);
        assert!(step.ratio.is_none());
        assert!(step.detail.is_none());
    }

    #[test]
    fn speed_is_only_written_when_there_is_something_to_write() {
        assert_eq!(speed_text(0.0), "");
        assert_eq!(speed_text(0.4), "");
        // 1 B/s 起才说话——测不出来时写「0 B/s」比不写更让人心里没底。
        assert!(!speed_text(1.0).is_empty());
        assert!(speed_text(2_097_152.0).contains("/s"));
    }

    #[test]
    fn a_name_we_did_not_choose_is_never_a_blocker() {
        // 换核心时名字是从目录名读出来的，玩家改不了。哪怕它碰巧长得像
        // 非法名字，也不该把他卡在第二步——他没有任何办法往下走。
        // 新建才按规则校验，因为那个名字是他自己填的。
        let odd = "a:b";
        let existing = Form {
            open: true,
            step: Step::Name,
            name: odd.to_string(),
            target: Some(PathBuf::from(r"E:\srv\a:b")),
            existing: true,
            mc_version: "1.21.8".to_string(),
            ..Form::default()
        };
        assert!(existing.can_advance(), "从管理页进来时不该被名字卡住");

        let fresh = Form {
            open: true,
            step: Step::Name,
            name: odd.to_string(),
            mc_version: "1.21.8".to_string(),
            ..Form::default()
        };
        assert!(!fresh.can_advance(), "新建时这种名字还是不行");
    }

    #[test]
    fn a_long_folder_path_keeps_its_tail() {
        // 卡片上那行路径要能塞进一行，太长的从前面省略，但必须留住最后两层
        let short = std::path::Path::new(r"E:\srv\survival");
        assert_eq!(short_path(short), r"E:\srv\survival");

        let long = std::path::Path::new(
            r"E:\Minecraft\server\du\[30011]lobby\something\very\deep\nesting\survival",
        );
        let text = short_path(long);
        assert!(text.chars().count() <= 60, "压完还得够短: {}", text);
        assert!(text.starts_with('…'), "省略号在前面: {}", text);
        assert!(text.ends_with("survival"), "最后一段必须留着: {}", text);
        // 不能从字符中间切开
        assert!(!text.contains("survi\u{2026}"), "别切半截: {}", text);
    }

    #[test]
    fn the_steps_only_advance_when_the_current_one_is_ready() {
        let mut form = Form::default();
        assert_eq!(form.step, Step::Version);
        assert!(!form.can_advance(), "还没选版本不该能下一步");
        form.mc_version = "26.3".to_string();
        assert!(form.can_advance());

        form.step = Step::Name;
        assert!(!form.can_advance(), "名字空着不该能下一步");
        form.name = "survival".to_string();
        assert!(form.can_advance());

        // 最后一步要真正的安装条件：选了 Fabric 就得有加载器版本
        form.step = Step::Core;
        form.core = CoreKind::Fabric;
        assert!(!form.can_advance());
        form.core_version = "0.20.1".to_string();
        assert!(form.can_advance());
    }

    #[test]
    fn stepping_back_and_forth_lands_where_it_started() {
        assert_eq!(Step::Version.next(), Step::Name);
        assert_eq!(Step::Name.next(), Step::Core);
        assert_eq!(Step::Core.prev(), Step::Name);
        assert_eq!(Step::Name.prev(), Step::Version);
        // 两头都夹住：第一步没有更前的，最后一步没有更后的。
        assert_eq!(Step::Version.prev(), Step::Version);
        assert_eq!(Step::Core.next(), Step::Core);
    }

    #[test]
    fn a_loader_list_only_belongs_to_the_core_it_was_fetched_for() {
        let mut state = State::default();
        state.form.core = CoreKind::Fabric;
        state.form.mc_version = "26.3".to_string();
        assert!(!state.form.matches(), "还没拉过列表");
        state.set_loader_versions(
            CoreKind::Fabric,
            "26.3".to_string(),
            vec!["0.20.1".to_string()],
        );
        assert!(state.form.matches());
        assert_eq!(state.form.core_version, "0.20.1");
        state.form.core = CoreKind::Forge;
        assert!(!state.form.matches(), "换了核心，旧列表就不算数了");
    }
}
