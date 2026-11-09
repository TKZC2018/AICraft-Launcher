//! 「更多功能 → 服务端管理」页。
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

use crate::theme;

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
            Step::Name => !self.name.trim().is_empty() && server::is_valid_name(&self.name),
            Step::Core => self.can_create(),
        }
    }

    /// 下一步按钮能不能点。
    pub fn can_create(&self) -> bool {
        if self.name.trim().is_empty() || !server::is_valid_name(&self.name) {
            return false;
        }
        if self.mc_version.trim().is_empty() {
            return false;
        }
        if self.core.needs_loader_version() && self.core_version.trim().is_empty() {
            return false;
        }
        !self.loading_loaders
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

/// 配置编辑视图的状态。
pub struct Editing {
    pub name: String,
    pub props: Properties,
    /// 保存过之后提示一下，别让玩家以为没生效。
    pub saved: bool,
    pub error: Option<String>,
}

/// 页面状态。
pub struct State {
    pub servers: Vec<Server>,
    pub loaded: bool,
    pub form: Form,
    pub editing: Option<Editing>,
    /// 顶部一条提示：`(文案, 是不是错误)`。
    pub notice: Option<(String, bool)>,
    /// 正在装的实例名（界面据此显示进度并锁住按钮）。
    pub installing: Option<String>,
    pub progress: Option<String>,
    /// 玩家自己加进来的目录（来自 `Settings.server_dirs`）。
    ///
    /// 存在这里而不是每次问设置，是因为 `reload()` 会被装完、删完、改配置
    /// 这些地方反复调用，不该每次都让调用方把列表递一遍。
    pub extra: Vec<PathBuf>,
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
            editing: None,
            notice: None,
            installing: None,
            progress: None,
            extra: Vec::new(),
        }
    }
}

impl State {
    pub fn reload(&mut self) {
        self.servers = server::list_with(&self.extra);
        self.loaded = true;
    }

    /// 换一批外部目录并重扫。
    pub fn set_extra(&mut self, dirs: &[String]) {
        self.extra = dirs.iter().map(PathBuf::from).collect();
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

    /// 打开某个服务端的配置。
    pub fn open_config(&mut self, name: &str) {
        let Some(found) = self.servers.iter().find(|s| s.name == name) else {
            return;
        };
        let props = Properties::read(&found.properties_path());
        self.editing = Some(Editing {
            name: name.to_string(),
            props,
            saved: false,
            error: None,
        });
    }
}

/// 页面交给主界面的动作。
pub enum Action {
    /// 重新扫一遍磁盘。
    Reload,
    /// 弹文件夹选择框，挑一个已有的服务端目录。
    PickFolder,
    /// 把某个外部目录从列表里移掉（不碰磁盘）。
    Forget(String),
    /// 去拉某个 (核心, 游戏版本) 的加载器版本列表。
    LoadLoaders {
        core: CoreKind,
        mc_version: String,
    },
    /// 开始安装。
    Create(Box<CreateRequest>),
    /// 打开目录。
    OpenFolder(PathBuf),
    /// 删掉某个服务端（已经过二次确认）。
    Remove(String),
    /// 同意 / 撤回 EULA。
    SetEula { name: String, accepted: bool },
    /// 打开配置视图。
    OpenConfig(String),
    /// 退出配置视图。
    CloseConfig,
    /// 保存配置。
    SaveConfig { name: String, props: Properties },
}

/// 新建请求（界面填得出来的部分；java / 下载源由 `main.rs` 补）。
pub struct CreateRequest {
    pub plan: server::Plan,
    /// 装进哪个目录；None 表示 `servers/<名字>`。
    pub target: Option<PathBuf>,
}

/// 画整页。
pub fn page(
    ui: &mut egui::Ui,
    state: &mut State,
    game_versions: &[String],
    busy: bool,
) -> Option<Action> {
    if state.editing.is_some() {
        config_view(ui, state)
    } else if state.form.open {
        new_flow(ui, state, game_versions, busy)
    } else {
        list_view(ui, state, busy)
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
            // 已经有一个开着的服（哪怕不是我们装的）时，最常做的其实是
            // 「把它加进来」，所以这个按钮和「新建服务端」并排、同一档重量。
            if theme::dialog_button(ui, "添加文件夹", !busy, 88.0).clicked() {
                action = Some(Action::PickFolder);
            }
            ui.add_space(6.0);
            let label = "新建服务端";
            if theme::primary_button(ui, label, !busy && state.installing.is_none()).clicked() {
                state.form.open = true;
                state.form.step = Step::Version;
                state.form.name.clear();
                state.form.mc_version.clear();
                state.form.core_version.clear();
                state.form.loader_versions.clear();
                state.form.loaded_for = None;
                // 手动点「新建服务端」时一定要清掉上一次从文件夹带进来的
                // 目标路径，否则会悄悄装进别人上次选过的目录里。
                state.form.target = None;
                state.notice = None;
            }
        });
    });
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "服务端放在数据目录的 servers/ 下，和游戏实例分开。\n\
             已经在别处开着的服，用「添加文件夹」把它接进来——只读，不会动里面的文件。",
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
                    .color(if *is_error { theme::DANGER } else { theme::ACCENT_STRONG }),
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
            });
            if let Some(step) = &state.progress {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(step)
                        .size(11.0)
                        .color(theme::TEXT_WEAK),
                );
            }
        });
    }

    ui.add_space(10.0);
    if state.servers.is_empty() {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(if state.loaded {
                    "还没有服务端。点右上「新建服务端」装一个，\n或者用「添加文件夹」把已有的服接进来。"
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
    if pending.is_some() {
        action = pending;
    }
    action
}

/// 一张服务端卡片。
fn server_card(ui: &mut egui::Ui, entry: &Server, busy: bool) -> Option<Action> {
    let mut action = None;
    // 代理端没有 eula.txt，别让它一直亮着「还没同意 EULA」的绿框。
    let frame = if entry.eula || entry.core == CoreKind::Velocity {
        theme::card()
    } else {
        theme::selected_card()
    };
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
                });
                let line = if entry.managed {
                    entry.summary()
                } else {
                    format!("{} · {}", entry.summary(), entry.dir.display())
                };
                ui.label(
                    egui::RichText::new(line)
                        .size(11.0)
                        .color(theme::TEXT_WEAK),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // 代理端没有 server.properties，那个表单对它毫无意义。
                if entry.core != CoreKind::Velocity
                    && theme::dialog_button(ui, "配置", !busy, 66.0).clicked()
                {
                    action = Some(Action::OpenConfig(entry.name.clone()));
                }
                ui.add_space(6.0);
                if theme::dialog_button(ui, "文件夹", !busy, 66.0).clicked() {
                    action = Some(Action::OpenFolder(entry.dir.clone()));
                }
                ui.add_space(6.0);
                if entry.managed {
                    if theme::dialog_danger_button(ui, "删除", !busy, 66.0).clicked() {
                        action = Some(Action::Remove(entry.name.clone()));
                    }
                } else if theme::dialog_button(ui, "移出列表", !busy, 76.0).clicked() {
                    // 这个目录不是我们建的，删它等于替玩家删掉一个正在开着的服。
                    action = Some(Action::Forget(entry.dir.display().to_string()));
                }
            });
        });

        if entry.core == CoreKind::Velocity {
            // 代理端没有 eula.txt，也没有 server.properties；提醒一句它该改哪个文件。
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("代理端：它自己不生成世界，配置在 velocity.toml 里改。")
                    .size(11.0)
                    .color(theme::TEXT_WEAK),
            );
        } else if !entry.eula {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("还没同意 Mojang 的 EULA，服务端起不来。")
                        .size(11.0)
                        .color(theme::WARN),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::dialog_primary_button(ui, "同意 EULA", !busy, 88.0).clicked() {
                        action = Some(Action::SetEula {
                            name: entry.name.clone(),
                            accepted: true,
                        });
                    }
                });
            });
        }
    });
    action
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
    let cancel = crate::flow::header(ui, "‹ 服务端列表", "新建服务端", &state.form.step_label());
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
            };
            action = Some(Action::Create(Box::new(CreateRequest {
                plan,
                target: state.form.target.clone(),
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
        ui.add(
            egui::TextEdit::singleline(&mut state.form.query)
                .hint_text("搜索版本号…")
                .desired_width(200.0),
        );
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
            egui::RichText::new("给这个服务端起个名字")
                .size(13.0)
                .strong(),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("会成为 servers/ 下面的文件夹名，也是列表里显示的名字。")
                .size(11.0)
                .color(theme::TEXT_WEAK),
        );
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            field_label(ui, "名字");
            ui.add(
                egui::TextEdit::singleline(&mut state.form.name)
                    .desired_width(200.0)
                    .hint_text("survival"),
            );
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            field_label(ui, "");
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

    action
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

// ---------------------------------------------------------------- 配置视图

fn config_view(ui: &mut egui::Ui, state: &mut State) -> Option<Action> {
    let mut action = None;
    let mut close = false;

    let editing = state.editing.as_mut()?;

    ui.horizontal(|ui| {
        if theme::text_button(ui, "‹ 服务端列表", false).clicked() {
            close = true;
        }
        ui.add_space(8.0);
        ui.label(egui::RichText::new(&editing.name).size(16.0).strong());
        ui.label(
            egui::RichText::new("server.properties")
                .size(11.0)
                .color(theme::TEXT_FAINT),
        );
    });

    if let Some(error) = &editing.error {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(error).size(11.0).color(theme::DANGER));
    }

    ui.add_space(10.0);
    egui::ScrollArea::vertical()
        .id_salt("server-config")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 9.0;
            theme::card().show(ui, |ui| {
                for field in FIELDS.iter() {
                    let current = editing
                        .props
                        .get(field.key)
                        .unwrap_or(field.default)
                        .to_string();
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(120.0, 20.0), egui::Sense::hover());
                        ui.painter().text(
                            rect.left_center(),
                            egui::Align2::LEFT_CENTER,
                            field.label,
                            egui::FontId::proportional(11.5),
                            theme::TEXT_WEAK,
                        );
                        match field.kind {
                            FieldKind::Switch => {
                                let mut on = current == "true";
                                if ui.checkbox(&mut on, "").changed() {
                                    editing
                                        .props
                                        .set(field.key, if on { "true" } else { "false" });
                                    editing.saved = false;
                                }
                            }
                            FieldKind::Number => {
                                let mut number = current.parse::<i64>().unwrap_or(0);
                                if ui
                                    .add(egui::DragValue::new(&mut number).speed(1.0))
                                    .changed()
                                {
                                    editing.props.set(field.key, &number.to_string());
                                    editing.saved = false;
                                }
                            }
                            FieldKind::Choice(options) => {
                                let combo = egui::ComboBox::from_id_salt(field.key)
                                    .selected_text(current.clone())
                                    .width(150.0)
                                    .show_ui(ui, |ui| {
                                        for option in options {
                                            let picked = current == *option;
                                            if ui.selectable_label(picked, *option).clicked() {
                                                editing.props.set(field.key, option);
                                                editing.saved = false;
                                            }
                                        }
                                    });
                                theme::hand(combo.response);
                            }
                            FieldKind::Text => {
                                let mut text = current;
                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut text)
                                            .desired_width(220.0),
                                    )
                                    .changed()
                                {
                                    editing.props.set(field.key, &text);
                                    editing.saved = false;
                                }
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
                    action = Some(Action::SaveConfig {
                        name: editing.name.clone(),
                        props: editing.props.clone(),
                    });
                }
                ui.add_space(8.0);
                if editing.saved {
                    ui.label(
                        egui::RichText::new("已保存")
                            .size(11.5)
                            .color(theme::ACCENT_STRONG),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("改完记得点保存；服务端重启后生效。")
                            .size(11.0)
                            .color(theme::TEXT_FAINT),
                    );
                }
            });
        });

    if close {
        state.editing = None;
        action = Some(Action::CloseConfig);
    }
    action
}

/// 配置保存成功后的收尾（`main.rs` 调用）。
pub fn config_saved(state: &mut State) {
    if let Some(editing) = state.editing.as_mut() {
        editing.saved = true;
        editing.error = None;
    }
}

pub fn config_failed(state: &mut State, message: String) {
    if let Some(editing) = state.editing.as_mut() {
        editing.error = Some(message);
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
        };
        assert!(!outside.managed);
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
