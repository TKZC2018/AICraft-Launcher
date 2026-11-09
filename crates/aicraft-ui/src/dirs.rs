//! 目录列表页：管理「游戏目录」。
//!
//! 官方目录（`%APPDATA%\.minecraft`，现算不存）默认就有一行，但**可以移除**——
//! 移除只是把「不要显示它」记进设置（`hide_official_dir`），磁盘和它里面的东西一概不动。
//! 用户自己加过的目录各占一行；当前用的目录如果哪一行都没覆盖到（旧配置手填的路径），
//! 再单独补一行。每行都能切过去（已经是当前的那行只标「当前」）。
//!
//! 换目录的入口原来在设置页，搬到这里是因为它管的是「在哪批版本里工作」，
//! 和版本列表是一件事，放在版本管理组下面比藏在设置里顺。

use std::path::Path;

use egui::RichText;

use aicraft_core::settings::official_game_dir;

use crate::theme;

/// 页面上表达的动作，出页面后由 `main.rs` 执行。
pub enum Action {
    /// 把某一项设为当前游戏目录
    Switch(String),
    /// 弹出系统文件夹选择框，添加一个目录
    Add,
    /// 从列表里移除某一项（不影响磁盘上的文件）
    Remove(String),
    /// 把"官方目录"那一行从列表里去掉（只动显示，不碰磁盘）
    HideOfficial,
    /// 把"官方目录"那一行重新显示出来
    ShowOfficial,
    /// 重新扫描当前目录下的 versions/
    Rescan,
}

impl Action {
    /// 给操作日志用的一句话。措辞在这里决定一次，调用点不再各写各的。
    pub fn describe(&self) -> String {
        match self {
            Action::Switch(path) => format!("目录列表：切换当前目录为 {}", path),
            Action::Add => "目录列表：添加目录".to_string(),
            Action::Remove(path) => format!("目录列表：移除 {}", path),
            Action::HideOfficial => "目录列表：隐藏官方目录".to_string(),
            Action::ShowOfficial => "目录列表：显示官方目录".to_string(),
            Action::Rescan => "目录列表：重新扫描".to_string(),
        }
    }
}

/// 一行里发生的动作。
enum RowAction {
    None,
    Switch,
    Remove,
}

/// 页面上要画哪几行，由 [`Layout::compute`] 一次算定。
///
/// 抽成纯函数是为了能单测——这块逻辑出过一次「用户加的目录凭空消失」，
/// 而它错了在界面上只会表现成「少了一行」，很难靠肉眼发现。
struct Layout<'a> {
    /// 是否画"官方目录"那一行（用户可以把它移掉）
    show_official: bool,
    /// 官方目录那一行要显示的文字（找不到官方目录时是一句说明）
    official_text: String,
    official_exists: bool,
    official_is_current: bool,
    /// 用户自己加过的目录，一个都不藏（与正在显示的官方目录重复的除外）
    others: Vec<&'a String>,
    /// 当前目录没有被任何一行覆盖，需要单独补一行
    orphan_current: bool,
    row_count: usize,
}

impl<'a> Layout<'a> {
    fn compute(
        current: &str,
        extra: &'a [String],
        official: Option<&Path>,
        show_official: bool,
    ) -> Self {
        let official_text = official
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "没有找到官方目录".to_string());
        let official_is_current = official.map(|p| same_path(p, current)).unwrap_or(false);
        // 官方那一行正在显示时，与它重复的项不另画——同一个路径占两行只会让人怀疑
        // 哪个是真的。官方那一行被移掉时不滤，用户自己加的就该照常出现。
        let others: Vec<&String> = extra
            .iter()
            .filter(|d| {
                !show_official || !official.map(|p| same_path(p, d.as_str())).unwrap_or(false)
            })
            .collect();
        // 当前目录没有被上面任何一行覆盖时（比如旧配置手填的路径，或者官方那行被移掉了
        // 而当前用的正是官方目录），单独补一行，否则页面上看不出「现在用的是哪个」。
        let covered = (show_official && official_is_current)
            || others
                .iter()
                .any(|d| same_path(Path::new(d.as_str()), current));
        let orphan_current = !covered;
        let row_count = usize::from(show_official) + others.len() + usize::from(orphan_current);
        Self {
            show_official,
            official_text,
            official_exists: official.is_some(),
            official_is_current,
            others,
            orphan_current,
            row_count,
        }
    }
}

/// 画目录列表页。
///
/// `current` 是当前生效的游戏目录，`extra` 是用户额外加的那些（不含官方目录），
/// `show_official` 为 false 表示用户把"官方目录"那一行移掉了。
pub fn page(
    ui: &mut egui::Ui,
    current: &str,
    extra: &[String],
    show_official: bool,
    busy: bool,
) -> Option<Action> {
    let mut action = None;

    // 先把要画哪几行算清楚：官方目录（可隐藏）+ 用户自己加过的（一个都不藏）
    // + 必要时单独一行当前目录。
    //
    // 这里刻意**不**把「正好是当前目录」的那一项从 extra 里滤掉：以前为了不让同一路径
    // 占两行滤过，结果用户加的目录刚好就是当前目录时，整条从页面上消失，看着像没保存。
    // 现在改成「留在原地、把行尾按钮换成『当前』标签」，路径在列表里始终看得见。
    let layout = Layout::compute(
        current,
        extra,
        official_game_dir().as_deref(),
        show_official,
    );
    let others = &layout.others;
    let row_count = layout.row_count;

    ui.horizontal(|ui| {
        ui.label(RichText::new("目录列表").size(17.0).strong());
        ui.add_space(4.0);
        if theme::refresh_button(ui, !busy)
            .on_hover_text("点击刷新列表")
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            action = Some(Action::Rescan);
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!("{} 个目录", row_count))
                .size(11.0)
                .color(theme::TEXT_WEAK),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("添加目录…"))
                .clicked()
            {
                action = Some(Action::Add);
            }
            // 官方目录被移掉之后，这里给一条把它找回来的路——没有这个，
            // 「移除」就成了单向操作，只能去改配置文件。
            if !show_official {
                ui.add_space(6.0);
                if ui
                    .add_enabled(!busy, egui::Button::new("恢复官方目录"))
                    .clicked()
                {
                    action = Some(Action::ShowOfficial);
                }
            }
        });
    });
    ui.add_space(10.0);

    // 官方目录：默认就有一行，也可以移掉——它只是一个默认项，不是长住的。
    // 即使没装过官方启动器也画出来，标一句「不存在」，免得用户以为启动器把它吞了。
    if layout.show_official {
        match row(
            ui,
            "官方目录",
            &layout.official_text,
            layout.official_exists,
            layout.official_is_current,
            true,
            busy,
        ) {
            RowAction::Switch => action = Some(Action::Switch(layout.official_text.clone())),
            // 官方那一行的「移除」翻译成「隐藏」：只动显示，
            // 跟用户自己加的目录从 game_dirs 里真删一条不是一回事。
            RowAction::Remove => action = Some(Action::HideOfficial),
            RowAction::None => {}
        }
        ui.add_space(6.0);
    }

    // 当前目录：没有被上面任何一行覆盖时才单独补一行（旧配置手填的路径，
    // 或者官方那行被移掉而当前用的正是官方目录）。
    if layout.orphan_current {
        let current_ok = !current.trim().is_empty() && Path::new(current).is_dir();
        row(ui, "当前目录", current, current_ok, true, false, busy);
        ui.add_space(6.0);
    }

    // 用户自己加的，一个都不藏。其中可能有「正好是当前目录」的那一项——
    // 它照样显示，只是行尾不画「设为当前」、改画「当前」标签。
    if others.is_empty() {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let (title, hint) = if show_official {
                (
                    "还没有添加过别的目录",
                    "点右上角「添加目录…」把另一个 .minecraft 加进来；这里只记路径，不动里面的文件。",
                )
            } else {
                (
                    "列表是空的",
                    "点右上角「添加目录…」加一个，或者用「恢复官方目录」把默认的那个找回来。",
                )
            };
            ui.label(RichText::new(title).size(12.5).color(theme::TEXT_WEAK));
            ui.add_space(3.0);
            ui.label(RichText::new(hint).size(11.0).color(theme::TEXT_WEAK));
        });
    } else {
        for dir in others.iter().copied() {
            let exists = Path::new(dir).is_dir();
            let is_current = same_path(Path::new(dir), current);
            match row(ui, "其他目录", dir, exists, is_current, true, busy) {
                RowAction::Switch => action = Some(Action::Switch(dir.clone())),
                RowAction::Remove => action = Some(Action::Remove(dir.clone())),
                RowAction::None => {}
            }
            ui.add_space(6.0);
        }
    }

    action
}

/// 画一行目录。
///
/// 右侧「设为当前」是主入口；路径本身也可点（两条路都通，点的人自己挑）。
/// `removable` 时「移除」排在最右。
/// 只有目录真实存在、且不是当前目录时，这两个入口才可点。
fn row(
    ui: &mut egui::Ui,
    label: &str,
    path: &str,
    exists: bool,
    is_current: bool,
    removable: bool,
    busy: bool,
) -> RowAction {
    let mut action = RowAction::None;
    // 当前那一行用绿色描边 + 绿色投影，扫一眼就知道启动器认的是哪个目录。
    let frame = if is_current {
        theme::selected_card()
    } else {
        theme::card()
    };
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).size(11.0).color(theme::TEXT_WEAK));
            ui.add_space(4.0);
            // 「当前」不再用标签说明：这一行本身就有绿描边 + 绿色投影
            // （见 `theme::selected_card()`），再说一遍是重复。
            if !is_current && !exists {
                theme::chip(ui, "不存在");
            }
            ui.add_space(6.0);

            let color = if exists {
                theme::TEXT
            } else {
                theme::TEXT_WEAK
            };
            let text = RichText::new(path).size(12.0).color(color);
            if ui
                .add(egui::Label::new(text).sense(egui::Sense::click()))
                .clicked()
                && exists
                && !is_current
            {
                action = RowAction::Switch;
            }

            if removable {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!busy, egui::Button::new("移除")).clicked() {
                        action = RowAction::Remove;
                    }
                    ui.add_space(6.0);
                    select_button(ui, exists, is_current, busy, &mut action);
                });
            } else {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    select_button(ui, exists, is_current, busy, &mut action);
                });
            }
        });
    });
    action
}

/// 行尾的「设为当前」/「不可用」。
///
/// 已经是当前目录时**什么都不画**：左侧那个「当前」小标签已经说明了状态，
/// 右边再放一句「使用中」是重复；放一个禁用的按钮又容易被读成「操作失败」。
fn select_button(
    ui: &mut egui::Ui,
    exists: bool,
    is_current: bool,
    busy: bool,
    action: &mut RowAction,
) {
    if is_current {
        return;
    }
    if !exists {
        ui.label(RichText::new("不可用").size(11.0).color(theme::TEXT_WEAK));
        return;
    }
    let button = egui::Button::new(RichText::new("设为当前").size(12.0)).corner_radius(8);
    if ui.add_enabled(!busy, button).clicked() {
        *action = RowAction::Switch;
    }
}

/// 两个路径是否指同一个地方。Windows 上大小写不敏感，比较时统一转小写。
fn same_path(a: &Path, b: &str) -> bool {
    let b = b.trim();
    if b.is_empty() {
        return false;
    }
    a.display().to_string().to_lowercase() == b.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{same_path, Layout};
    use std::path::Path;

    /// 大小写与首尾空白都不该影响判断——Windows 路径本来就不区分大小写，
    /// 用户手打/从别处粘贴过来的路径常常带空格。
    #[test]
    fn same_path_ignores_case_and_padding() {
        let a = Path::new(r"C:\Users\Administrator\AppData\Roaming\.minecraft");
        assert!(same_path(
            a,
            r"c:\users\administrator\appdata\roaming\.minecraft"
        ));
        assert!(same_path(
            a,
            "  C:\\Users\\Administrator\\AppData\\Roaming\\.minecraft  "
        ));
        assert!(!same_path(a, r"D:\Games\.minecraft"));
    }

    /// 空串永远不算同一个地方，否则「还没选过目录」会被当成命中。
    #[test]
    fn empty_never_matches() {
        let a = Path::new(r"C:\Games\.minecraft");
        assert!(!same_path(a, ""));
        assert!(!same_path(a, "   "));
    }

    /// 用户加的目录正好就是当前目录时**必须照原样显示出来**。
    ///
    /// 这是真踩过的坑：早先按「与当前目录相同就不画」过滤，用户把官方目录设为当前之后，
    /// 自己加的那条（当时恰好也是当前目录）从页面上消失，看着像根本没保存。
    #[test]
    fn added_dir_stays_visible_when_it_is_the_current_one() {
        let official = Path::new(r"C:\Users\Administrator\AppData\Roaming\.minecraft");
        let extra = vec![r"E:\Minecraft\.minecraft".to_string()];
        // 当前目录 == 用户加的那条
        let layout = Layout::compute(r"E:\Minecraft\.minecraft", &extra, Some(official), true);
        assert_eq!(layout.others.len(), 1, "加的目录不能被藏起来");
        assert!(!layout.orphan_current, "它已经在 others 里，不用再补一行");
        // 官方目录那一行 + 用户加的那一行
        assert_eq!(layout.row_count, 2);
    }

    /// 切到官方目录之后，用户加的那条仍在列表里，而且这时它不再等于当前目录，
    /// 所以还要能「设为当前」切回去。
    #[test]
    fn added_dir_survives_switching_to_the_official_one() {
        let official = Path::new(r"C:\Users\Administrator\AppData\Roaming\.minecraft");
        let extra = vec![r"E:\Minecraft\.minecraft".to_string()];
        let layout = Layout::compute(
            r"C:\Users\Administrator\AppData\Roaming\.minecraft",
            &extra,
            Some(official),
            true,
        );
        assert_eq!(layout.others.len(), 1);
        assert!(layout.official_is_current);
        assert!(!layout.orphan_current);
        assert_eq!(layout.row_count, 2);
        assert!(!same_path(
            Path::new(layout.others[0]),
            r"C:\Users\Administrator\AppData\Roaming\.minecraft"
        ));
    }

    /// 旧配置里手填的当前目录既不在官方、也不在自己那批里时，要单独补一行，
    /// 否则页面上看不出「现在用的到底是哪个」。
    #[test]
    fn stray_current_dir_gets_its_own_row() {
        let official = Path::new(r"C:\Users\Administrator\AppData\Roaming\.minecraft");
        let extra = vec![r"E:\Minecraft\.minecraft".to_string()];
        let layout = Layout::compute(r"D:\Games\.minecraft", &extra, Some(official), true);
        assert!(layout.orphan_current);
        assert_eq!(layout.row_count, 3, "官方 + 自己加的 + 补出来的当前");
    }

    /// 与官方目录重复的项不另画（官方那一行已经在显示同一个路径了）。
    #[test]
    fn duplicates_of_the_official_dir_are_not_drawn_twice() {
        let official = Path::new(r"C:\Users\Administrator\AppData\Roaming\.minecraft");
        let extra = vec![r"c:\users\administrator\appdata\roaming\.minecraft".to_string()];
        let layout = Layout::compute(r"E:\Minecraft\.minecraft", &extra, Some(official), true);
        assert!(layout.others.is_empty());
        assert!(layout.orphan_current, "当前目录不在列表里，要单独一行");
        assert_eq!(layout.row_count, 2);
    }

    /// 找不到官方目录时也占一行（写「没有找到官方目录」），免得用户以为被吞了。
    #[test]
    fn missing_official_dir_still_gets_a_row() {
        let extra = vec![r"E:\Minecraft\.minecraft".to_string()];
        let layout = Layout::compute(r"E:\Minecraft\.minecraft", &extra, None, true);
        assert!(!layout.official_exists);
        assert_eq!(layout.official_text, "没有找到官方目录");
        assert!(!layout.orphan_current);
        assert_eq!(layout.row_count, 2);
    }

    /// 官方目录只是一个默认项，用户把它移掉之后就不该再画那一行。
    #[test]
    fn hiding_the_official_dir_removes_its_row() {
        let official = Path::new(r"C:\Users\Administrator\AppData\Roaming\.minecraft");
        let extra = vec![r"E:\Minecraft\.minecraft".to_string()];
        let layout = Layout::compute(r"E:\Minecraft\.minecraft", &extra, Some(official), false);
        assert!(!layout.show_official);
        assert_eq!(layout.others.len(), 1, "自己加的还在");
        assert!(!layout.orphan_current);
        assert_eq!(layout.row_count, 1, "只剩自己加的那一行");
    }

    /// 官方那行被移掉、而当前用的正好是官方目录时，必须补一行「当前目录」，
    /// 否则页面上看不到自己在用什么。
    #[test]
    fn hiding_official_keeps_the_current_row_when_it_was_official() {
        let official = Path::new(r"C:\Users\Administrator\AppData\Roaming\.minecraft");
        let extra: Vec<String> = Vec::new();
        let layout = Layout::compute(
            r"C:\Users\Administrator\AppData\Roaming\.minecraft",
            &extra,
            Some(official),
            false,
        );
        assert!(layout.orphan_current);
        assert_eq!(layout.row_count, 1);
    }

    /// 官方那行被移掉之后，与自己加的那条重复时不再被滤掉——那条得照常显示。
    #[test]
    fn hiding_official_unfilters_a_duplicate_entry() {
        let official = Path::new(r"C:\Users\Administrator\AppData\Roaming\.minecraft");
        let extra = vec![r"C:\Users\Administrator\AppData\Roaming\.minecraft".to_string()];
        let layout = Layout::compute(
            r"C:\Users\Administrator\AppData\Roaming\.minecraft",
            &extra,
            Some(official),
            false,
        );
        assert_eq!(layout.others.len(), 1, "官方那行没了，这条就该出现");
        assert!(!layout.orphan_current, "已经被 others 覆盖，不用补行");
        assert_eq!(layout.row_count, 1);
    }
}
