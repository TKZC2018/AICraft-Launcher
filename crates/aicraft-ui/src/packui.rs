//! 整合包拖拽与导入确认。
//!
//! 这个模块只管「界面状态」和「画什么」，真正解包、下载、装加载器都在
//! `aicraft_core::modpack` 里，主线程只负责把结果摆出来。
//!
//! 三条路径进来的东西最后都汇到同一处：
//! - 文件拖到窗口任意位置（`hovered_files` 判悬停，`dropped_files` 判松手）；
//! - 「下载版本」页标题旁那个「导入整合包」按钮（走系统文件对话框）；
//! - 命令行 / 调试钩子（暂未开放）。
//!
//! 拖拽的坑：egui 的 `raw.dropped_files` 在松手那一帧才有值，且只来一次。
//! 所以要在这里立刻把它抄成自己的状态，不能指望它下一帧还在。

use std::path::{Path, PathBuf};

use egui::RichText;

use crate::theme;
use aicraft_core::modpack::{self, PackManifest};

/// 悬停提示的遮罩透明度：够看清「底下的界面还在」，又不至于让人以为卡住。
const DROP_ALPHA: u8 = 208;

/// 导入流程走到了哪一步。
pub enum Stage {
    /// 正在读压缩包里的清单（读 zip 要几百毫秒，不能卡在绘制里）
    Reading(PathBuf),
    /// 清单读出来了，等玩家确认
    Ready(Box<Ready>),
    /// 读失败
    Failed { path: PathBuf, error: String },
}

/// 读出来、等确认的一份整合包。
pub struct Ready {
    pub archive: PathBuf,
    pub manifest: PackManifest,
    /// 玩家正在改的实例名（默认取包名，改到合法才允许开始）
    pub instance: String,
}

impl Ready {
    /// 默认实例名：包名去掉路径分隔符和 Windows 不认的字符。
    ///
    /// 不直接用 `manifest.name`——作者起名很自由，`Fabulously Optimized` 里的
    /// 空格没事，但 `1.20.1-生存/创造` 这种带斜杠的会建出一个套一层的目录。
    pub fn default_instance(manifest: &PackManifest) -> String {
        let cleaned: String = manifest
            .name
            .chars()
            .map(|c| match c {
                '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
                c if c.is_control() => '-',
                c => c,
            })
            .collect();
        let trimmed = cleaned.trim().trim_matches('.').trim();
        if trimmed.is_empty() {
            manifest.game_version.clone()
        } else {
            trimmed.to_string()
        }
    }
}

/// 拖拽与导入的界面状态。
#[derive(Default)]
pub struct State {
    pub stage: Option<Stage>,
    /// 已经在排队等读清单的文件；用来去重，避免同一个文件重复起线程
    pub reading: Option<PathBuf>,
}

impl State {
    /// 开始读一个文件：记下路径，交给主线起线程。
    ///
    /// 返回 true 表示「这个文件确实是新来的」，调用方据此起线程——同一个
    /// 文件在一次拖拽里只读一遍。
    pub fn begin(&mut self, path: PathBuf) -> bool {
        if self.reading.as_ref().is_some_and(|p| p == &path) {
            return false;
        }
        self.reading = Some(path.clone());
        self.stage = Some(Stage::Reading(path));
        true
    }

    /// 清单读到了。
    pub fn ready(&mut self, archive: PathBuf, manifest: PackManifest) {
        let instance = Ready::default_instance(&manifest);
        self.reading = None;
        self.stage = Some(Stage::Ready(Box::new(Ready {
            archive,
            manifest,
            instance,
        })));
    }

    /// 读失败了。
    pub fn fail(&mut self, path: PathBuf, error: String) {
        self.reading = None;
        self.stage = Some(Stage::Failed { path, error });
    }

    pub fn close(&mut self) {
        self.reading = None;
        self.stage = None;
    }
}

/// 文件名是不是像整合包。
///
/// 只看扩展名：`.zip` 什么都可能是，但玩家拖进来一个 zip 时，先按整合包
/// 试着读一遍、读不出来再说「认不出」，比直接拒收体验好。
pub fn looks_like_pack(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("mrpack") | Some("zip")
    )
}

/// 从一帧的输入里挑出「刚拖进来」的文件。
///
/// `dropped_files` 松手那帧才有值，所以调用方拿到的必须当场处理。
pub fn dropped(ctx: &egui::Context) -> Vec<PathBuf> {
    ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .filter_map(|f| f.path().to_path_buf().into())
            .collect()
    })
}

/// 有没有文件正悬在窗口上（拖拽还没松手）。
pub fn hovering(ctx: &egui::Context) -> bool {
    ctx.input(|i| !i.raw.hovered_files.is_empty())
}

/// 悬停时盖住整个窗口的那层提示。
///
/// 画在最前面（`Foreground`），松手后由 `State::begin` 接住。
pub fn drop_overlay(ctx: &egui::Context) {
    if !hovering(ctx) {
        return;
    }
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::unique("aicraft-drop-overlay"),
    ));

    painter.rect_filled(
        screen,
        0.0,
        egui::Color32::from_rgba_unmultiplied(
            theme::BG_BOTTOM.r(),
            theme::BG_BOTTOM.g(),
            theme::BG_BOTTOM.b(),
            DROP_ALPHA,
        ),
    );

    // 虚线框往里收一圈，让人看清边界在哪
    let frame = screen.shrink(22.0);
    painter.rect_stroke(
        frame,
        14.0,
        egui::Stroke::new(2.0, theme::ACCENT),
        egui::StrokeKind::Inside,
    );

    let center = frame.center();
    painter.text(
        center - egui::vec2(0.0, 16.0),
        egui::Align2::CENTER_CENTER,
        "松手就导入",
        egui::FontId::proportional(22.0),
        theme::TEXT,
    );
    painter.text(
        center + egui::vec2(0.0, 18.0),
        egui::Align2::CENTER_CENTER,
        "支持 Modrinth（.mrpack）和 HMCL 导出的整合包",
        egui::FontId::proportional(12.5),
        theme::TEXT_WEAK,
    );
}

/// 起一个后台线程读清单。
///
/// 返回值就是那条 channel：成功给 `Ok((原路径, 清单))`，失败带原因。
pub fn spawn_read(
    path: PathBuf,
) -> std::sync::mpsc::Receiver<(PathBuf, Result<PackManifest, String>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = modpack::inspect(&path);
        let _ = tx.send((path, result));
    });
    rx
}

/// 导入确认卡片。
///
/// 返回 `Some(实例名)` 表示玩家点了「开始导入」，`Some` 里的名字已经过校验。
/// 点取消 / 点外面 / 按 ESC 都返回 `None`，并且把 `state.stage` 清掉。
pub fn modal(
    ctx: &egui::Context,
    state: &mut State,
    game_dir: &Path,
    busy: bool,
) -> Option<String> {
    let stage = state.stage.as_ref()?;
    let mut confirmed = None;
    let mut dismissed = false;

    match stage {
        Stage::Reading(path) => {
            let response = egui::Modal::new(egui::Id::unique("aicraft-pack-reading"))
                .frame(theme::dialog_frame())
                .show(ctx, |ui| {
                    ui.set_width(360.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("正在读整合包…")
                                .size(14.0)
                                .strong()
                                .color(theme::TEXT),
                        );
                    });
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("整合包"),
                        )
                        .size(11.0)
                        .color(theme::TEXT_WEAK),
                    );
                });
            if response.should_close() {
                dismissed = true;
            }
        }
        Stage::Failed { path, error } => {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("整合包")
                .to_string();
            let response = egui::Modal::new(egui::Id::unique("aicraft-pack-failed"))
                .frame(theme::dialog_frame())
                .show(ctx, |ui| {
                    ui.set_width(380.0);
                    ui.label(
                        RichText::new("这个整合包装不了")
                            .size(15.0)
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new(&name).size(12.5).color(theme::TEXT).strong());
                    ui.add_space(6.0);
                    ui.label(RichText::new(error).size(11.5).color(theme::DANGER));
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::dialog_button(ui, "知道了", true, 92.0).clicked() {
                                dismissed = true;
                            }
                        });
                    });
                });
            if response.should_close() {
                dismissed = true;
            }
        }
        Stage::Ready(ready) => {
            // 借用冲突的常规解法：先把要画的只读信息抄出来，
            // 再把 `instance` 的可变引用临时取出来交给输入框。
            let summary = ready.manifest.summary();
            let game = ready.manifest.game_version.clone();
            let loader = ready
                .manifest
                .loader
                .as_ref()
                .map(|(kind, ver)| format!("{} {}", kind.label(), ver));
            let format = ready.manifest.format.label();
            let files = ready.manifest.files.len() + ready.manifest.cf_files.len();

            let mut instance = ready.instance.clone();
            let response = egui::Modal::new(egui::Id::unique("aicraft-pack-ready"))
                .frame(theme::dialog_frame())
                .show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.label(
                        RichText::new("导入整合包")
                            .size(16.0)
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.add_space(2.0);
                    ui.label(RichText::new(&summary).size(11.5).color(theme::TEXT_WEAK));
                    ui.add_space(10.0);

                    // 三行小账目：玩家在点「开始导入」前该知道要下多少东西
                    ui.horizontal(|ui| {
                        meta_chip(ui, &format!(".{}", format_extension(format)));
                        meta_chip(ui, &format!("Minecraft {}", game));
                        if let Some(loader) = &loader {
                            meta_chip(ui, loader);
                        }
                        if files > 0 {
                            meta_chip(ui, &format!("{} 个文件要下载", files));
                        }
                    });
                    ui.add_space(12.0);

                    ui.label(
                        RichText::new("装成哪个实例")
                            .size(11.5)
                            .color(theme::TEXT_WEAK),
                    );
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut instance)
                            .hint_text("比如 26.3-FO")
                            .desired_width(f32::INFINITY),
                    );

                    ui.add_space(6.0);
                    let hint = name_hint(&instance, game_dir);
                    match &hint {
                        Some(text) => {
                            ui.label(RichText::new(text).size(11.0).color(theme::WARN));
                        }
                        None => {
                            ui.label(
                                RichText::new(format!("会装到 versions/{}", instance.trim()))
                                    .size(11.0)
                                    .color(theme::TEXT_WEAK),
                            );
                        }
                    }
                    ui.add_space(14.0);

                    let can_go = hint.is_none() && !busy;
                    ui.horizontal(|ui| {
                        if theme::dialog_button(ui, "取消", !busy, 92.0).clicked() {
                            dismissed = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::dialog_primary_button(ui, "开始导入", can_go, 108.0).clicked()
                            {
                                confirmed = Some(instance.trim().to_string());
                            }
                        });
                    });
                    // 回车 = 开始导入（名字合法时）
                    if can_go && edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        confirmed = Some(instance.trim().to_string());
                    }
                });

            // 名字改动要写回去，否则每帧都从旧值重画，输入框会「打不出字」
            if let Some(Stage::Ready(ready)) = state.stage.as_mut() {
                ready.instance = instance;
            }
            if response.should_close() {
                dismissed = true;
            }
        }
    }

    if confirmed.is_some() {
        // 保留 stage：主线程要靠它拿到 archive 与 manifest，接手后自己清
        return confirmed;
    }
    if dismissed {
        state.close();
    }
    None
}

fn meta_chip(ui: &mut egui::Ui, text: &str) {
    theme::chip(ui, text);
}

fn format_extension(format: &str) -> &'static str {
    if format.contains("Modrinth") {
        "mrpack"
    } else {
        // HMCL 和 CurseForge 都是普通 zip
        "zip"
    }
}

/// 实例名合不合法。
///
/// 和向导里那套一样，只是这里不查「官方版本」——整合包本来就不是官方版本。
pub fn name_hint(instance: &str, game_dir: &Path) -> Option<String> {
    let name = instance.trim();
    if name.is_empty() {
        return Some("名字不能是空的".to_string());
    }
    if !aicraft_core::install::is_valid_instance_name(name) {
        return Some("不能含 \\ / : * ? \" < > |，也不能是 . 或 ..".to_string());
    }
    if aicraft_core::install::is_installed(game_dir, name) {
        return Some(format!(
            "当前目录里已经有 versions/{} 了，换一个名字吧",
            name
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicraft_core::loader::LoaderKind;
    use aicraft_core::modpack::PackFormat;

    fn manifest(name: &str, game: &str) -> PackManifest {
        PackManifest {
            format: PackFormat::Modrinth,
            name: name.to_string(),
            version: "1.0".to_string(),
            game_version: game.to_string(),
            loader: Some((LoaderKind::Fabric, "0.20.1".to_string())),
            files: Vec::new(),
            cf_files: Vec::new(),
        }
    }

    #[test]
    fn the_default_name_is_safe_to_use_as_a_folder() {
        let ready = Ready {
            archive: PathBuf::from("x.mrpack"),
            manifest: manifest("Fabulously/Optimized: 1.20.1", "1.20.1"),
            instance: String::new(),
        };
        let name = Ready::default_instance(&ready.manifest);
        assert!(!name.contains('/'), "{}", name);
        assert!(!name.contains(':'), "{}", name);
        assert!(aicraft_core::install::is_valid_instance_name(&name));
    }

    #[test]
    fn a_nameless_pack_falls_back_to_its_game_version() {
        let ready = manifest("   ", "26.3");
        assert_eq!(Ready::default_instance(&ready), "26.3");
    }

    #[test]
    fn only_zip_like_files_are_tried() {
        assert!(looks_like_pack(Path::new("a.mrpack")));
        assert!(looks_like_pack(Path::new("a.ZIP")));
        assert!(!looks_like_pack(Path::new("a.jar")));
        assert!(!looks_like_pack(Path::new("a")));
    }

    #[test]
    fn the_same_file_is_only_queued_once() {
        let mut state = State::default();
        let path = PathBuf::from("C:\\packs\\a.mrpack");
        assert!(state.begin(path.clone()));
        assert!(!state.begin(path));
        assert!(matches!(state.stage, Some(Stage::Reading(_))));
        state.ready(PathBuf::from("C:\\packs\\b.mrpack"), manifest("FO", "26.3"));
        assert!(matches!(state.stage, Some(Stage::Ready(_))));
        assert!(state.reading.is_none());
    }
}
