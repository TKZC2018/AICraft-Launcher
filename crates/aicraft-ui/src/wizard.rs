//! 装版本的向导：选好原版 → 起名字 → 决定要不要加载器 → 开始下载。
//!
//! 两页都是模态弹窗，一页一件事：
//!
//! 1. **起名**：默认填官方版本号，**不替用户错开**。撞上 `versions/` 里已有的
//!    目录就把「下一步」按住，提示换一个——名字是玩家的东西，自动加后缀会让
//!    他以为自己装的是另一份。
//! 2. **加载器**：可以什么都不装（原样装原版），也可以顺便装 Fabric /
//!    Forge / NeoForge。选了加载器才会去拉它的版本列表。
//!
//! 这一层不碰网络也不碰磁盘：它把意图收成 [`Action`]，交给 `main.rs` 去执行。
//! 唯一的例外是起名时查重，那需要读一眼 `versions/` 目录。

use std::path::Path;

use aicraft_core::install::{is_installed, is_valid_instance_name};
use aicraft_core::loader::{LoaderKind, LoaderVersion};

use crate::theme;

/// 向导走到哪一步了。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// 给这一份起名字
    Name,
    /// 要不要装加载器
    Loader,
}

/// 向导里选中的加载器。
pub struct Choice {
    pub kind: LoaderKind,
    pub version: String,
    pub with_fabric_api: bool,
}

/// 向导最终产出的动作。
pub enum Action {
    /// 去拉某个加载器的版本列表
    Load {
        game_version: String,
        kind: LoaderKind,
    },
    /// 全都定了，可以开装
    Start {
        official_id: String,
        instance: String,
        loader: Option<Choice>,
    },
}

/// 每帧的结果。
pub enum Outcome {
    /// 什么都不用做（向导还开着）
    Idle,
    /// 用户取消、点了遮罩或按了 Esc —— 调用方把向导关掉
    Cancel,
    /// 有动作要执行
    Act(Action),
}

/// 向导状态。
pub struct Wizard {
    /// 官方版本号
    pub official_id: String,
    /// 玩家起的实例名
    pub instance: String,
    pub step: Step,
    /// 选中的加载器；`None` = 「不装加载器」
    pub kind: Option<LoaderKind>,
    /// 当前加载器的版本列表
    pub versions: Vec<LoaderVersion>,
    /// 这份列表属于哪个 (游戏版本, 加载器)
    loaded_for: Option<(String, LoaderKind)>,
    pub loading: bool,
    pub load_error: Option<String>,
    /// 挑中的加载器版本
    pub selected: Option<String>,
    pub with_fabric_api: bool,
}

impl Wizard {
    /// 从「下载版本」页点下载开始。
    ///
    /// 名字默认就填官方版本号，不查也不改——撞名留给 [`Wizard::name_hint`]
    /// 在弹窗里提示，玩家自己换。
    pub fn new(official_id: &str) -> Self {
        Wizard {
            official_id: official_id.to_string(),
            instance: official_id.to_string(),
            step: Step::Name,
            kind: None,
            versions: Vec::new(),
            loaded_for: None,
            loading: false,
            load_error: None,
            selected: None,
            with_fabric_api: true,
        }
    }

    /// 加载器列表到了。
    pub fn set_versions(
        &mut self,
        game_version: String,
        kind: LoaderKind,
        list: Vec<LoaderVersion>,
    ) {
        self.loading = false;
        self.load_error = None;
        self.loaded_for = Some((game_version, kind));
        // 默认挑第一个：列表已经是从新到旧排好的
        self.selected = list.first().map(|v| v.version.clone());
        self.versions = list;
    }

    /// 拉列表失败。
    pub fn fail_loading(&mut self, message: String) {
        self.loading = false;
        self.load_error = Some(message);
        self.versions.clear();
        self.selected = None;
    }

    /// 列表是不是对得上当前选择。
    fn is_fresh(&self, game_version: &str, kind: LoaderKind) -> bool {
        self.loaded_for
            .as_ref()
            .map(|(gv, k)| gv == game_version && *k == kind)
            .unwrap_or(false)
    }

    /// 起名那页的实时提示（None = 可以下一步）。
    ///
    /// 重名只提示、不改名：自动加后缀等于替玩家做了决定，
    /// 而且他一回头看到 `26.3-2` 会以为自己下错了版本。
    fn name_hint(&self, game_dir: &Path) -> Option<String> {
        let name = self.instance.trim();
        if name.is_empty() {
            return Some("名字不能是空的".to_string());
        }
        if !is_valid_instance_name(name) {
            return Some("不能含 \\ / : * ? \" < > |，也不能是 . 或 ..".to_string());
        }
        if is_installed(game_dir, name) {
            return Some(format!(
                "当前目录里已经有 versions/{} 了，换一个名字吧",
                name
            ));
        }
        None
    }
}

/// 画向导。返回这一帧要不要做点什么。
pub fn show(_ctx: &egui::Context, wizard: &mut Wizard, game_dir: &Path, _busy: bool) -> Outcome {
    match wizard.step {
        Step::Name => name_modal(_ctx, wizard, game_dir),
        // 「加载器」那一页不进弹窗了——它是嵌在「下载版本」页里的一张卡片，
        // 由 `show_loader_card` 画。这里只是把控制权交回去。
        Step::Loader => Outcome::Idle,
    }
}

/// 向导是不是应该以「页面里的卡片」形式出现。
///
/// 只有第二步（要不要装加载器）是这样：它要给玩家看版本列表、要能来回切换，
/// 弹窗里挤着不自在，而且挡住底下的下载页没道理。起名那一步仍走弹窗——
/// 那是个短问题，弹出来问完就没了。
pub fn wants_inline(wizard: &Wizard) -> bool {
    wizard.step == Step::Loader
}

/// 在正文里画「要不要装加载器」这张卡片。
pub fn show_loader_card(ui: &mut egui::Ui, wizard: &mut Wizard, busy: bool) -> Outcome {
    loader_body(ui, wizard, busy)
}

/// 第一步：起名字。
fn name_modal(ctx: &egui::Context, wizard: &mut Wizard, game_dir: &Path) -> Outcome {
    let mut go_next = false;
    let hint = wizard.name_hint(game_dir);

    let response = egui::Modal::new(egui::Id::unique("aicraft-wizard-name"))
        .frame(theme::dialog_frame())
        .show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(
                egui::RichText::new("给这一份起个名字")
                    .size(16.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(format!("官方版本 {}", wizard.official_id))
                    .size(11.5)
                    .color(theme::TEXT_WEAK),
            );
            ui.add_space(12.0);

            ui.label(
                egui::RichText::new("实例名")
                    .size(11.5)
                    .color(theme::TEXT_WEAK),
            );
            let edit = ui.add(
                egui::TextEdit::singleline(&mut wizard.instance)
                    .hint_text("比如 26.3-生存")
                    .desired_width(f32::INFINITY),
            );

            ui.add_space(6.0);
            match &hint {
                Some(text) => {
                    ui.label(egui::RichText::new(text).size(11.0).color(theme::WARN));
                }
                None => {
                    ui.label(
                        egui::RichText::new(
                            "它会成为 versions/<名字>/ 的目录名，存档与模组都跟着它走。",
                        )
                        .size(11.0)
                        .color(theme::TEXT_WEAK),
                    );
                }
            }

            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::primary_button(ui, "下一步", hint.is_none()).clicked() {
                    go_next = true;
                }
                ui.add_space(8.0);
                if ui.button("取消").clicked() {
                    ui.close();
                }
            });

            // Enter 等于「下一步」
            if edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                go_next = true;
            }
        });

    if go_next && hint.is_none() {
        wizard.instance = wizard.instance.trim().to_string();
        wizard.step = Step::Loader;
        wizard.versions.clear();
        wizard.loaded_for = None;
        wizard.selected = None;
        wizard.kind = None;
        wizard.loading = false;
        wizard.load_error = None;
        return Outcome::Idle;
    }
    if response.should_close() {
        return Outcome::Cancel;
    }
    Outcome::Idle
}

/// 可选版本的一整行：整行铺满、选中绿底、行间一条淡线。
///
/// 早先用 `selectable_label` + `chip` 拼，两个控件各自按内容收窄，排出来
/// 只有左边一条——看着像没对齐。画法现在收在 [`crate::flow`]，服务端那边共用。
fn version_row(ui: &mut egui::Ui, v: &LoaderVersion, picked: bool, last: bool) -> bool {
    crate::flow::row(ui, &v.version, picked, last)
}

/// 「要不要装加载器」的正文。
///
/// 第二步是页面里的一张卡片（见 [`show_loader_card`]），不走弹窗——
/// 原来的 `egui::Modal` 版本已经删了，别因为「向导有两步」就以为两步都是弹窗。
fn loader_body(ui: &mut egui::Ui, wizard: &mut Wizard, busy: bool) -> Outcome {
    let mut action = None;
    let mut back = false;
    let mut start = false;
    let mut cancel = false;

    let game_version = wizard.official_id.clone();
    let ready = wizard.kind.is_none() || wizard.selected.is_some();

    ui.set_width(ui.available_width().min(560.0));
    ui.label(
        egui::RichText::new("安装加载器")
            .size(15.0)
            .strong()
            .color(theme::TEXT),
    );
    ui.add_space(12.0);

    ui.horizontal_wrapped(|ui| {
        if theme::loader_pick(ui, None, wizard.kind.is_none()).clicked() {
            wizard.kind = None;
            wizard.versions.clear();
            wizard.loaded_for = None;
            wizard.selected = None;
            wizard.load_error = None;
            wizard.loading = false;
        }
        for kind in LoaderKind::ALL {
            let picked = wizard.kind == Some(kind);
            if theme::loader_pick(ui, Some(kind), picked).clicked() && !picked {
                wizard.kind = Some(kind);
                wizard.versions.clear();
                wizard.loaded_for = None;
                wizard.selected = None;
                wizard.load_error = None;
                wizard.loading = false;
            }
        }
    });

    match wizard.kind {
        None => {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(format!("Minecraft {}", wizard.official_id))
                    .size(11.0)
                    .color(theme::TEXT_WEAK),
            );
        }
        Some(kind) => {
            ui.add_space(10.0);
            if !wizard.is_fresh(&game_version, kind) && !wizard.loading {
                action = Some(Action::Load {
                    game_version: game_version.clone(),
                    kind,
                });
            }
            if wizard.loading || (wizard.versions.is_empty() && wizard.load_error.is_none()) {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(format!("正在问官方要 {} 的版本…", kind.label()))
                            .size(11.0)
                            .color(theme::TEXT_WEAK),
                    );
                });
            } else if let Some(err) = &wizard.load_error {
                ui.label(
                    egui::RichText::new(err.clone())
                        .size(11.0)
                        .color(theme::DANGER),
                );
            } else {
                if kind == LoaderKind::Fabric {
                    ui.checkbox(&mut wizard.with_fabric_api, "同时安装Fabric API");
                    ui.add_space(4.0);
                }
                // 卡片顶掉的是整个版本清单，玩家要在这几十个版本号里挑一个，
                // 一屏能看见几个算几个——把剩下的高度都给它。
                let picked = crate::flow::list(ui, "wizard-loader-versions", |ui| {
                    let mut picked = None;
                    let count = wizard.versions.len();
                    for (i, v) in wizard.versions.iter().enumerate() {
                        let selected = wizard.selected.as_deref() == Some(v.version.as_str());
                        if version_row(ui, v, selected, i + 1 == count) {
                            picked = Some(v.version.clone());
                        }
                    }
                    picked
                });
                if let Some(version) = picked {
                    wizard.selected = Some(version);
                }
            }
        }
    }

    // 这张卡片现在顶掉的是整个版本清单，身子比内容高得多；按钮紧跟内容
    // 会悬在半空，把它压到底边，和别的页面「按钮在最下面」的手感一致。
    // 卡片底下没有遮罩可点，所以「取消安装」必须留着——那是唯一的退路。
    let out = crate::flow::footer(
        ui,
        crate::flow::Buttons {
            primary: "开始安装",
            enabled: ready && !busy,
            back: Some("上一步"),
            cancel: Some("取消安装"),
            right_aligned: true,
            note: None,
        },
    );
    start |= out.next;
    back |= out.back;
    cancel |= out.cancel;

    if back {
        wizard.step = Step::Name;
        return Outcome::Idle;
    }
    if cancel {
        return Outcome::Cancel;
    }
    // 「开始安装」也走这里：内容里按了按钮就当作要装。
    if start && ready && !busy {
        return Outcome::Act(starting_action(wizard));
    }
    match action {
        Some(act) => Outcome::Act(act),
        None => Outcome::Idle,
    }
}

/// 把向导当前的选择收成「开始安装」动作。
fn starting_action(wizard: &Wizard) -> Action {
    let loader = wizard.kind.map(|kind| Choice {
        kind,
        version: wizard.selected.clone().unwrap_or_default(),
        with_fabric_api: wizard.with_fabric_api,
    });
    Action::Start {
        official_id: wizard.official_id.clone(),
        instance: wizard.instance.clone(),
        loader,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> aicraft_core::testutil::TempDir {
        aicraft_core::testutil::fresh_dir(tag)
    }

    fn put(game_dir: &Path, name: &str) {
        let dir = game_dir.join("versions").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{}.json", name)), "{}").unwrap();
    }

    fn list(items: &[&str]) -> Vec<LoaderVersion> {
        items
            .iter()
            .map(|v| LoaderVersion {
                version: (*v).to_string(),
                game_version: "1.20.1".to_string(),
                stable: true,
            })
            .collect()
    }

    #[test]
    fn fresh_name_defaults_to_the_official_id() {
        let tmp = temp("wizard-fresh");
        let w = Wizard::new("26.3");
        assert_eq!(w.instance, "26.3");
        assert!(w.name_hint(&tmp).is_none());
    }

    #[test]
    fn taken_name_blocks_the_next_step_and_is_never_renamed() {
        let tmp = temp("wizard-taken");
        put(&tmp, "26.3");
        // 默认就是官方名，撞上了也不替用户改名，只按住「下一步」
        let mut w = Wizard::new("26.3");
        assert_eq!(w.instance, "26.3");
        let hint = w.name_hint(&tmp).unwrap();
        assert!(hint.contains("已经有"), "{}", hint);

        // 自己改一个没被占的就能继续
        w.instance = "26.3-生存".to_string();
        assert!(w.name_hint(&tmp).is_none());
    }

    #[test]
    fn illegal_names_are_rejected() {
        let tmp = temp("wizard-illegal");
        let mut w = Wizard::new("26.3");
        for bad in ["", "  ", "a/b", "a:b", ".."] {
            w.instance = bad.to_string();
            assert!(w.name_hint(&tmp).is_some(), "{:?}", bad);
        }
        // 中文和空格都放行
        w.instance = "26.3 生存".to_string();
        assert!(w.name_hint(&tmp).is_none());
    }

    #[test]
    fn loader_list_picks_the_newest_and_knows_when_it_is_stale() {
        let mut w = Wizard::new("1.20.1");
        w.loading = true;
        w.set_versions(
            "1.20.1".to_string(),
            LoaderKind::Fabric,
            list(&["0.16.10", "0.16.9"]),
        );
        assert!(!w.loading);
        assert_eq!(w.selected.as_deref(), Some("0.16.10"));
        assert!(w.is_fresh("1.20.1", LoaderKind::Fabric));
        // 换游戏版本或换加载器都算不新鲜，得重拉
        assert!(!w.is_fresh("1.20.1", LoaderKind::Forge));
        assert!(!w.is_fresh("1.21.1", LoaderKind::Fabric));
    }

    #[test]
    fn failure_clears_the_selection() {
        let mut w = Wizard::new("1.20.1");
        w.selected = Some("0.16.10".to_string());
        w.loading = true;
        w.fail_loading("网络不通".to_string());
        assert!(w.selected.is_none());
        assert!(w.versions.is_empty());
        assert!(!w.loading);
        assert_eq!(w.load_error.as_deref(), Some("网络不通"));
    }

    #[test]
    fn starting_action_carries_the_loader_choice() {
        let mut w = Wizard::new("1.20.1");
        w.instance = "生存整合".to_string();
        w.selected = Some("0.16.10".to_string());
        w.kind = Some(LoaderKind::Fabric);
        w.with_fabric_api = true;
        let Action::Start {
            official_id,
            instance,
            loader,
        } = starting_action(&w)
        else {
            panic!("应该产出开始动作");
        };
        assert_eq!(official_id, "1.20.1");
        assert_eq!(instance, "生存整合");
        let loader = loader.expect("选了 Fabric");
        assert_eq!(loader.kind, LoaderKind::Fabric);
        assert_eq!(loader.version, "0.16.10");
        assert!(loader.with_fabric_api);
    }
}
