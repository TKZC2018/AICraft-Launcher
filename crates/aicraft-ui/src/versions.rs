//! 「下载版本」页：从官方清单里挑一个下载。
//!
//! 它是一个**页面**，和「版本列表」平级地画在右边那块正文里，不再弹独立窗口：
//! 两页共用同一片区域，来回切换时视线不用跟着窗口跑。
//!
//! 这一页刻意**不检查本机装了什么**：每行就是一个「下载」按钮，装没装过都长一样。
//! 想管已经装好的东西，去「版本列表」；想同一版本再来一份，直接再点一次下载，
//! 名字由接下来的向导让玩家自己起（撞名会被拦下，不会偷偷加后缀）。
//!
//! 这里不碰网络和磁盘：它只把用户意图收集成 [`Action`]，
//! 真正的抓清单 / 下载由 `main.rs` 拿到动作后执行。

use egui::{RichText, Stroke};

use crate::theme;

/// 清单筛选。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Release,
    Snapshot,
    /// 远古版本：`old_beta` 与 `old_alpha`。
    ///
    /// 官方清单里这两类一共六十来个（最早能到 2009 年的 `rd-132211`），
    /// 和快照混在一起时很难翻，所以单开一个入口。
    Old,
    All,
}

impl Filter {
    fn label(self) -> &'static str {
        match self {
            Filter::Release => "正式版",
            Filter::Snapshot => "快照",
            Filter::Old => "远古版本",
            Filter::All => "全部",
        }
    }

    fn matches(self, kind: &str) -> bool {
        match self {
            Filter::Release => kind == "release",
            Filter::Snapshot => kind == "snapshot",
            Filter::Old => kind == "old_beta" || kind == "old_alpha",
            Filter::All => true,
        }
    }
}

/// 官方清单里的一个可安装版本。
#[derive(Clone)]
pub struct Entry {
    pub id: String,
    /// release / snapshot / old_beta / old_alpha
    pub kind: String,
    pub time: String,
}

impl Entry {
    fn type_label(&self) -> &'static str {
        match self.kind.as_str() {
            "release" => "正式版",
            "snapshot" => "快照",
            "old_beta" => "远古 beta",
            "old_alpha" => "远古 alpha",
            _ => "其他",
        }
    }
}

/// 页面状态。
pub struct State {
    /// 是否已经进入过这一页（第一次进入时才自动拉清单）
    pub open: bool,
    pub query: String,
    pub filter: Filter,
    pub entries: Vec<Entry>,
    /// 官方清单是否已经取到过一次
    pub loaded: bool,
    /// 是否正在拉清单
    pub fetching: bool,
    /// 正在安装的版本名字（实例名）
    pub installing: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            open: false,
            query: String::new(),
            filter: Filter::Release,
            entries: Vec::new(),
            loaded: false,
            fetching: false,
            installing: None,
        }
    }
}

impl State {
    /// 需要拉清单吗（进过这一页、还没拉过、也没在拉）。
    pub fn needs_fetch(&self) -> bool {
        self.open && !self.loaded && !self.fetching
    }

    /// 清空清单（刷新前调用，界面立刻显示「获取中」）。
    pub fn begin_fetch(&mut self) {
        self.fetching = true;
    }

    /// 清单到达。
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.loaded = true;
        self.fetching = false;
    }

    /// 拉取失败。
    pub fn fail_fetch(&mut self) {
        self.fetching = false;
    }

    /// 符合当前搜索与筛选的条目。
    fn visible(&self) -> Vec<Entry> {
        let query = self.query.trim().to_lowercase();
        self.entries
            .iter()
            .filter(|e| self.filter.matches(&e.kind))
            .filter(|e| query.is_empty() || e.id.to_lowercase().contains(&query))
            .cloned()
            .collect()
    }
}

/// 用户在这一页里表达的动作，出页面后由 `main.rs` 执行。
pub enum Action {
    /// 重新拉取官方清单
    Refresh,
    /// 下载某个官方版本。
    ///
    /// 只带官方版本号：落盘目录名由主线程现算（已有同名实例就往后排），
    /// 这一页不关心本机装过什么。
    Install(String),
    /// 从磁盘挑一个整合包（.mrpack / .zip）导入
    ImportPack,
}

impl Action {
    /// 给操作日志用的一句话。
    pub fn describe(&self) -> String {
        match self {
            Action::Refresh => "下载版本：刷新官方清单".to_string(),
            Action::Install(id) => format!("下载版本：选择安装 {}", id),
            Action::ImportPack => "下载版本：导入整合包".to_string(),
        }
    }
}

/// 标题行 + 工具栏（搜索、类型筛选）。
///
/// 和清单分开画，是因为选加载器那一步要把清单那一整块换掉：标题和筛选
/// 仍然该留着，玩家得能看见自己挑的是哪个版本。
pub fn header(ui: &mut egui::Ui, state: &mut State, busy: Option<&str>) -> Option<Action> {
    let mut action = None;

    // ---- 标题行 ----
    ui.horizontal(|ui| {
        ui.label(RichText::new("下载版本").size(17.0).strong());
        ui.add_space(4.0);
        if theme::refresh_button(ui, busy.is_none() && !state.fetching)
            .on_hover_text("点击刷新列表")
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            action = Some(Action::Refresh);
        }
        ui.add_space(6.0);
        if state.fetching {
            ui.spinner();
            ui.label(
                RichText::new("正在获取官方版本清单…")
                    .size(11.0)
                    .color(theme::TEXT_WEAK),
            );
        } else if let Some(name) = &state.installing {
            ui.spinner();
            ui.label(
                RichText::new(format!("正在安装 {}", name))
                    .size(11.0)
                    .color(theme::ACCENT_STRONG),
            );
        } else if state.loaded {
            ui.label(
                RichText::new(format!("官方清单共 {} 个版本", state.entries.len()))
                    .size(11.0)
                    .color(theme::TEXT_WEAK),
            );
        } else {
            ui.label(
                RichText::new("还没有获取清单")
                    .size(11.0)
                    .color(theme::TEXT_WEAK),
            );
        }

        // 导入整合包放在标题行右侧：它和「下载版本」是同一件事的两种来源
        // （一个是官方清单，一个是别人打包好的），放同一行读着顺。
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let enabled = busy.is_none();
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::new(RichText::new("导入整合包").size(12.0).color(if enabled {
                        theme::TEXT
                    } else {
                        theme::TEXT_FAINT
                    }))
                    .fill(theme::CARD)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER))
                    .corner_radius(egui::CornerRadius::same(8))
                    .min_size(egui::vec2(88.0, 26.0)),
                )
                .on_hover_text("从磁盘挑一个 .mrpack 或 .zip；也可以直接把文件拖进窗口")
                .clicked()
            {
                action = Some(Action::ImportPack);
            }
        });
    });
    ui.add_space(10.0);

    // ---- 工具栏：搜索 + 类型筛选 ----
    ui.horizontal(|ui| {
        // 从右往左排：先排四个筛选按钮，搜索框最后吃掉剩下的宽度。
        //
        // 为什么不用「available_width() - 固定值」：按钮要占多宽取决于字号、
        // 按钮内边距和 DPI，写死的保留值一旦估少了，最后一个按钮就会被挤出
        // 卡片右边缘（125% 缩放时「全部」正好压到边框上）。让 egui 自己排，
        // 它知道每个按钮的真实尺寸。
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // 让最右边那个按钮和下面卡片里的「下载」右边缘对齐：
            // 卡片自己有一圈 16px 内边距，工具栏不在卡片里，得自己让出来。
            ui.add_space(16.0);
            let mut clicked = None;
            for filter in [Filter::All, Filter::Old, Filter::Snapshot, Filter::Release] {
                let selected = state.filter == filter;
                if ui.selectable_label(selected, filter.label()).clicked() {
                    clicked = Some(filter);
                }
            }
            ui.add(
                egui::TextEdit::singleline(&mut state.query)
                    .hint_text("搜索版本号…")
                    .desired_width(ui.available_width().max(120.0)),
            );
            if let Some(filter) = clicked {
                state.filter = filter;
            }
        });
    });
    ui.add_space(10.0);

    action
}

/// 版本清单本体：一列可滚动的版本卡片，底下跟一句说明。
///
/// 选加载器时它会被整块顶掉——见 [`header`] 的说明。
pub fn list(ui: &mut egui::Ui, state: &mut State, busy: Option<&str>) -> Option<Action> {
    let mut action = None;
    let visible = state.visible();
    if state.fetching && !state.loaded {
        crate::flow::loading(ui, "正在拉官方版本清单，稍等一下…");
        return action;
    }

    if state.loaded && visible.is_empty() {
        crate::flow::notice_card(ui, "没有匹配的版本", "换个关键词，或者把类型切到「全部」。");
        return action;
    }

    if !state.loaded && !state.fetching {
        crate::flow::notice_card(ui, "还没拿到官方清单", "点标题旁那个小箭头拉一次。");
        return action;
    }

    // 底部留一行说明的空间，其余全给列表。列表是这一页的正主；选加载器时
    // 它被整块换掉，所以这里不用再为谁让位。
    let list_height = (ui.available_height() - 34.0).max(160.0);

    egui::ScrollArea::vertical()
        .id_salt("download-page-list")
        .max_height(list_height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for entry in &visible {
                egui::Frame::NONE
                    .fill(theme::CARD)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(12, 7))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            // 左：版本号 + 类型 + 时间
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    // 每个版本前面一枚草方块：这一页列的全是原版，
                                    // 用「草方块 + 版本号」比单放一串数字好认。
                                    const ICON: f32 = 26.0;
                                    let (icon_rect, _) = ui.allocate_exact_size(
                                        egui::vec2(ICON, ICON),
                                        egui::Sense::hover(),
                                    );
                                    theme::loader_glyph(ui, icon_rect, None);
                                    ui.add_space(4.0);
                                    ui.label(RichText::new(&entry.id).size(13.5).strong());
                                    theme::chip(ui, entry.type_label());
                                });
                                ui.label(
                                    RichText::new(&entry.time)
                                        .size(10.5)
                                        .color(theme::TEXT_WEAK),
                                );
                            });

                            // 右：就一个下载按钮。装没装过都长一样，
                            // 这一页不认识「已安装」这个概念。
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_enabled(busy.is_none(), egui::Button::new("下载"))
                                        .clicked()
                                    {
                                        action = Some(Action::Install(entry.id.clone()));
                                    }
                                },
                            );
                        });
                    });
                ui.add_space(4.0);
            }
        });

    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: &str) -> Entry {
        Entry {
            id: id.to_string(),
            kind: kind.to_string(),
            time: "2024-01-01T00:00:00+00:00".to_string(),
        }
    }

    #[test]
    fn the_old_filter_picks_both_ancient_kinds() {
        let all = vec![
            entry("26.3", "release"),
            entry("26.4-snapshot-1", "snapshot"),
            entry("b1.7.3", "old_beta"),
            entry("a1.2.6", "old_alpha"),
        ];
        let mut state = State {
            entries: all,
            ..State::default()
        };

        state.filter = Filter::Old;
        let visible = state.visible();
        let picked: Vec<&str> = visible.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(picked, vec!["b1.7.3", "a1.2.6"]);

        // 「全部」本来也含远古，加了新分类不能把它弄丢
        state.filter = Filter::All;
        assert_eq!(state.visible().len(), 4);

        state.filter = Filter::Release;
        assert_eq!(state.visible().len(), 1);
    }

    #[test]
    fn ancient_versions_carry_a_readable_type_label() {
        assert_eq!(entry("b1.7.3", "old_beta").type_label(), "远古 beta");
        assert_eq!(entry("a1.2.6", "old_alpha").type_label(), "远古 alpha");
    }

    #[test]
    fn every_filter_has_its_own_label() {
        let labels: Vec<&str> = [Filter::Release, Filter::Snapshot, Filter::Old, Filter::All]
            .iter()
            .map(|f| f.label())
            .collect();
        assert_eq!(labels, vec!["正式版", "快照", "远古版本", "全部"]);
    }
}
