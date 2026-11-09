//! 单个版本的「管理」页：加载器、模组、资源包、存档。
//!
//! 它不是主界面的一页，而是一层盖住整个窗口（含左侧主导航）的独立视图 ——
//! 进来之后视线只在这一份实例上，左上角点「返回」才回到版本列表。
//!
//! 为什么要有这一页：一个版本装好之后，用户真正要反复动的是它自己的那些
//! 东西（加模组、换资源包、备份存档、换加载器版本）。这些都散在磁盘上，
//! 让玩家自己去 `versions/<版本>/` 里翻，等于什么都没做。

use std::path::{Path, PathBuf};

use aicraft_core::instance::{self, Item};
use aicraft_core::loader::{LoaderKind, LoaderVersion};
use aicraft_core::modupdate::Check;

use crate::theme;

/// 左侧二级导航的几个分区。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Loader,
    Mods,
    ResourcePacks,
    Saves,
}

impl Section {
    pub const ALL: [Section; 4] = [
        Section::Loader,
        Section::Mods,
        Section::ResourcePacks,
        Section::Saves,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Loader => "加载器",
            Section::Mods => "模组",
            Section::ResourcePacks => "资源包",
            Section::Saves => "存档",
        }
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
    /// 挨个问 Modrinth：这些模组里哪些有新版本。
    CheckUpdates,
    /// 更新其中某一个（按盘上那个 jar 定位）。
    UpdateMod(PathBuf),
    /// 把所有认得出、又有新版的都更新一遍。
    UpdateAll,
}

impl Action {
    /// 给操作日志用的一句话。
    pub fn describe(&self) -> String {
        match self {
            Action::Close => "管理页：返回版本列表".to_string(),
            Action::Reload => "管理页：重新扫描".to_string(),
            Action::OpenFolder(path) => format!("管理页：打开文件夹 {}", path.display()),
            Action::Delete(path) => format!("管理页：删除 {}", path.display()),
            Action::LoadLoaderVersions(kind) => {
                format!("管理页：查看 {} 可用版本", kind.label())
            }
            Action::SwitchLoader { kind, version } => {
                format!("管理页：把加载器换成 {} {}", kind.label(), version)
            }
            Action::Flatten => "管理页：压平清单".to_string(),
            Action::CheckUpdates => "管理页：检查模组更新".to_string(),
            Action::UpdateMod(path) => format!("管理页：更新 {}", path.display()),
            Action::UpdateAll => "管理页：一键更新全部模组".to_string(),
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
    pub saves: Vec<Item>,
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
            saves: Vec::new(),
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
        if let Ok(text) = std::fs::read_to_string(&json_path) {
            if let Ok(raw) = aicraft_core::version::parse_version(&text) {
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
        self.packs = instance::list_files(&self.root.join("resourcepacks"), &["zip"]);
        self.saves = instance::list_dirs(&self.root.join("saves"));
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
            Section::Mods => self.mods.len(),
            Section::ResourcePacks => self.packs.len(),
            Section::Saves => self.saves.len(),
        }
    }
}

/// 画整页。调用方保证当前处于「管理」状态。
pub fn page(
    ui: &mut egui::Ui,
    state: &mut State,
    game_dir: &Path,
    icons: &mut crate::icons::Cache,
    busy: bool,
) -> Option<Action> {
    let mut action = None;

    egui::Panel::left("manage-nav")
        .exact_size(178.0)
        .show_separator_line(false)
        .frame(
            egui::Frame::NONE
                .fill(theme::PANEL)
                .inner_margin(egui::Margin::symmetric(12, 14)),
        )
        .show(ui, |ui| {
            if ui
                .add(egui::Button::new(
                    egui::RichText::new("‹ 返回")
                        .size(12.5)
                        .color(theme::TEXT_WEAK),
                ))
                .on_hover_text("回到版本列表")
                .clicked()
            {
                action = Some(Action::Close);
            }
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("管理")
                    .size(10.5)
                    .color(theme::TEXT_FAINT),
            );
            ui.label(egui::RichText::new(&state.version).size(15.0).strong());
            ui.label(
                egui::RichText::new(if state.shared {
                    "共享目录".to_string()
                } else {
                    "隔离实例".to_string()
                })
                .size(10.5)
                .color(theme::TEXT_WEAK),
            );
            ui.add_space(12.0);

            for section in Section::ALL {
                let selected = state.section == section;
                let count = state.count(section);
                let text = if section == Section::Loader {
                    section.label().to_string()
                } else {
                    format!("{}  {}", section.label(), count)
                };
                if theme::nav_item(ui, &text, selected).clicked() {
                    state.section = section;
                }
                ui.add_space(2.0);
            }
        });

    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(theme::BG_BOTTOM)
                .inner_margin(egui::Margin::symmetric(22, 18)),
        )
        .show(ui, |ui| {
            theme::paint_background(ui);
            let head = header_row(ui, state, busy);
            if let Some(next) = head {
                action = Some(next);
            }
            ui.add_space(10.0);

            match state.section {
                Section::Loader => {
                    if let Some(next) = loader_section(ui, state, busy) {
                        action = Some(next);
                    }
                }
                Section::Mods => {
                    if let Some(next) = item_list(
                        ui,
                        state,
                        &state.mods.clone(),
                        "还没有模组。把 .jar 丢进这个文件夹就会出现在这里。",
                        icons,
                        true,
                        busy,
                    ) {
                        action = Some(next);
                    }
                }
                Section::ResourcePacks => {
                    if let Some(next) = item_list(
                        ui,
                        state,
                        &state.packs.clone(),
                        "还没有资源包。把 .zip 丢进这个文件夹就会出现在这里。",
                        icons,
                        false,
                        busy,
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
                        false,
                        busy,
                    ) {
                        action = Some(next);
                    }
                }
            }
        });

    if let Some(next) = confirm_delete(ui.ctx(), state, busy) {
        action = Some(next);
    }
    let _ = game_dir;
    action
}

/// 每页顶上那一条：标题 + 目录 + 刷新 / 打开文件夹。
fn header_row(ui: &mut egui::Ui, state: &mut State, busy: bool) -> Option<Action> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(match state.section {
                Section::Loader => "加载器",
                Section::Mods => "模组列表",
                Section::ResourcePacks => "资源包列表",
                Section::Saves => "存档列表",
            })
            .size(17.0)
            .strong(),
        );
        if state.section != Section::Loader {
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
                    .color(theme::ACCENT_STRONG),
            );
        } else if let Some((text, ok)) = &state.update_notice {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(text).size(11.0).color(if *ok {
                theme::ACCENT_STRONG
            } else {
                theme::DANGER
            }));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if state.section != Section::Loader
                && ui
                    .add_enabled(!busy, egui::Button::new("打开文件夹"))
                    .on_hover_text("在资源管理器里打开这个目录")
                    .clicked()
            {
                action = Some(Action::OpenFolder(
                    state.root.join(sub_dir_name(state.section)),
                ));
            }
        });
    });

    if state.section != Section::Loader {
        let dir = state.root.join(sub_dir_name(state.section));
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
        Section::Loader => "",
        Section::Mods => "mods",
        Section::ResourcePacks => "resourcepacks",
        Section::Saves => "saves",
    }
}

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

/// 一个条目列表（模组 / 资源包 / 存档共用）。
///
/// `icons` 只在模组那一栏给 `true`：图标是从 jar 里挖出来的，资源包和
/// 存档没有这回事，别为它们白跑一遍 ZIP 解析。
fn item_list(
    ui: &mut egui::Ui,
    state: &mut State,
    items: &[Item],
    empty_hint: &str,
    icons: &mut crate::icons::Cache,
    with_icon: bool,
    busy: bool,
) -> Option<Action> {
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
            for item in items {
                theme::card().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if with_icon {
                            // 键用整个路径（不只是文件名）：不同版本目录里
                            // 同名的模组是两件东西，共用一张脸会串。
                            let key = format!("mod:{}", item.path.display());
                            let ready = icons.want_jar(&key, &item.path);
                            icon_slot(ui, icons, &key, ready);
                        }
                        let update = if with_icon {
                            state.pending_update(&item.path).cloned()
                        } else {
                            None
                        };
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&item.name).size(12.5).strong());
                            // 有新版本时这一行换成「0.6.9 → 0.6.13」：
                            // 玩家要知道自己从哪儿升到哪儿，光一个「更新」不敢点。
                            match &update {
                                Some(check) => ui.label(
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
                                    .color(theme::ACCENT_STRONG),
                                ),
                                None => ui.label(
                                    egui::RichText::new(crate::human_size(item.bytes as f64))
                                        .size(10.5)
                                        .color(theme::TEXT_WEAK),
                                ),
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
                ui.add_space(6.0);
            }
        });
    let _ = dir;
    action
}

/// 加载器分区：说着现在挂了什么，Fabric 还能就地换版本。
fn loader_section(ui: &mut egui::Ui, state: &mut State, busy: bool) -> Option<Action> {
    let mut action = None;

    theme::card().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.label(
            egui::RichText::new(format!("Minecraft {}", state.mc_version))
                .size(12.5)
                .color(theme::TEXT),
        );
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
        let has_this = matches!(&installed, Some((k, _)) if *k == kind);
        let blocked = matches!(&installed, Some((k, _)) if *k != kind);
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
                            egui::RichText::new(format!(
                                "换成这个会替掉 {} {}",
                                k.label(),
                                version
                            ))
                            .size(11.0)
                            .color(theme::WARN),
                        );
                    }
                    None => {
                        ui.label(
                            egui::RichText::new("可安装")
                                .size(11.0)
                                .color(theme::ACCENT_STRONG),
                        );
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::chevron_button(ui, open, !busy)
                        .on_hover_text(if has_this {
                            "换一个加载器版本"
                        } else if blocked {
                            "换到这一家（会替掉现在那个加载器，存档和模组不动）"
                        } else {
                            "挑一个版本装上"
                        })
                        .clicked()
                    {
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
                                    theme::ACCENT_STRONG
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

/// 删除确认：删的可能是存档，一律再问一次。
fn confirm_delete(ctx: &egui::Context, state: &mut State, busy: bool) -> Option<Action> {
    let path = state.pending_delete.clone()?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("这个")
        .to_string();
    let is_dir = path.is_dir();
    let mut confirmed = false;
    let mut dismissed = false;

    let response = egui::Modal::new(egui::Id::unique("aicraft-manage-delete"))
        .frame(theme::dialog_frame())
        .show(ctx, |ui| {
            ui.set_width(370.0);
            ui.label(egui::RichText::new("删掉这个？").size(15.0).strong());
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(&name)
                    .size(12.5)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(if is_dir {
                    "整个文件夹会一起删掉，删了找不回来。"
                } else {
                    "这个文件会被删掉，删了找不回来。"
                })
                .size(11.0)
                .color(theme::WARN),
            );
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if theme::dialog_button(ui, "取消", !busy, 92.0).clicked() {
                    dismissed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::dialog_danger_button(ui, "删除", !busy, 92.0).clicked() {
                        confirmed = true;
                    }
                });
            });
        });

    if response.should_close() {
        dismissed = true;
    }
    if confirmed {
        state.pending_delete = None;
        Some(Action::Delete(path))
    } else if dismissed {
        state.pending_delete = None;
        None
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_has_a_label() {
        let labels: Vec<&str> = Section::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(labels, vec!["加载器", "模组", "资源包", "存档"]);
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
            saves: Vec::new(),
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
    fn only_the_mods_column_asks_for_icons() {
        // 图标是从 jar 里挖出来的，资源包/存档没有这回事：别为它们白跑一遍
        // ZIP 解析。这条盯的是三个调用点传的那个 bool。
        let source = include_str!("manage.rs");
        let mods = source.find("Section::Mods => {").expect("得有模组那一栏");
        let packs = source
            .find("Section::ResourcePacks => {")
            .expect("得有资源包那一栏");
        let saves = source.find("Section::Saves => {").expect("得有存档那一栏");
        let mods_block = &source[mods..packs];
        assert!(mods_block.contains("true,"), "模组那一栏要带图标");
        assert!(!&source[packs..saves].contains("true,\n                    ) {"));
        assert!(!&source[saves..saves + 600].contains("true,\n                    ) {"));
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
