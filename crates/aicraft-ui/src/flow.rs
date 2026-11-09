//! 安装流程里那些「一步步走」的界面零件。
//!
//! 客户端装版本（[`crate::wizard`]）与服务端建服务端（[`crate::serverui`]）走的是
//! 同一套节奏：顶部一行「返回 + 标题 + 第 N 步」，中间一块可滚动的选择列表，
//! 底部按钮钉在底边。两边各画一套的话，改一处就得记着改另一处——都放这里。
//!
//! 这里只画界面，不认业务：谁的点被选中、下一步该不该亮，都由调用方决定。

use crate::theme;

/// 列表一行的固定高度。
///
/// 34 是「12.5px 字 + 上下各 10px 呼吸」量出来的：再矮行与行之间的细线就会
/// 贴着字顶，再高则一屏少看清楚两个版本。
pub const ROW_H: f32 = 34.0;

/// 列表该占多高：底部按钮 + 窗口底边的呼吸先扣掉，剩下的按整行算，
/// 最后多留半行——露出半行，人才知道下面还有。
///
/// 窗口矮的时候至少给三行半，不然只剩一条缝，一个版本都读不全。
pub fn list_height(available: f32) -> f32 {
    // 34 是底部那排按钮，28 是卡片底边留的呼吸。
    const RESERVED: f32 = 34.0 + 28.0;
    let room = (available - RESERVED).max(ROW_H * 3.0);
    let rows = (room / ROW_H).floor().max(3.0);
    ROW_H * (rows + 0.5)
}

/// 一整行可选的行：整行铺满可用宽度，不是只占文字那么一截。
///
/// 早先用 `selectable_label` + `chip` 拼，两个控件各自按内容收窄，排出来
/// 只有左边一条，右边的滚动条孤零零挂在卡片最右边——看着像没对齐。
/// 自己量一整行，选中底色和悬停底色就都能铺满。
pub fn row(ui: &mut egui::Ui, text: &str, picked: bool, last: bool) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, ROW_H), egui::Sense::click());

    // 选中 / 悬停的底色上下各让出 3px：行与行之间有线，色块顶到线会显得糊。
    let boxed = rect.shrink2(egui::vec2(0.0, 3.0));
    let fill = if picked {
        theme::ACCENT_SOFT
    } else if response.hovered() {
        theme::CARD_HOVER
    } else {
        egui::Color32::TRANSPARENT
    };
    if fill != egui::Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(boxed, egui::CornerRadius::same(7), fill);
    }
    if picked {
        ui.painter().rect_stroke(
            boxed,
            egui::CornerRadius::same(7),
            egui::Stroke::new(1.0, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
    }

    let inner = rect.shrink2(egui::vec2(10.0, 0.0));

    // 行与行之间的细线。两端对齐到文字起止，不顶到卡片边上。
    if !last {
        ui.painter().hline(
            egui::Rangef::new(inner.left(), inner.right()),
            rect.bottom(),
            egui::Stroke::new(1.0, egui::Color32::from_rgb(0xE8, 0xED, 0xF4)),
        );
    }
    let color = if picked {
        theme::ACCENT_STRONG
    } else {
        theme::TEXT
    };
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        egui::FontId::new(12.5, egui::FontFamily::Proportional),
        color,
    );
    let text_y = inner.center().y - galley.size().y / 2.0;
    ui.painter()
        .galley(egui::Pos2::new(inner.left(), text_y), galley.clone(), color);

    theme::hand(response.clone());
    response.clicked()
}

/// 顶部一行：返回 + 标题 + 步骤说明。返回是否点了那个返回。
pub fn header(ui: &mut egui::Ui, back: &str, title: &str, step: &str) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        if theme::text_button(ui, back, false).clicked() {
            clicked = true;
        }
        ui.add_space(8.0);
        ui.label(egui::RichText::new(title).size(17.0).strong());
        ui.label(
            egui::RichText::new(step)
                .size(11.0)
                .color(theme::TEXT_FAINT),
        );
    });
    clicked
}

/// 底部那排按钮要什么。
#[derive(Clone, Copy)]
pub struct Buttons<'a> {
    /// 主按钮的文案（「下一步」/「开始安装」）
    pub primary: &'a str,
    /// 主按钮能不能按
    pub enabled: bool,
    /// 副按钮「上一步」；`None` = 第一步，没有可退的地方
    pub back: Option<&'a str>,
    /// 副按钮「取消」；`None` = 不给退路（目前没用上，留着给「只能装」的场景）
    pub cancel: Option<&'a str>,
    /// 靠右排（卡片里）还是靠左排（整页里）
    pub right_aligned: bool,
    /// 主按钮右边跟一句说明，比如「已选 26.3」
    pub note: Option<&'a str>,
}

/// 底部按钮按出来的结果。
#[derive(Default, Clone, Copy)]
pub struct Footer {
    pub next: bool,
    pub back: bool,
    pub cancel: bool,
}

/// 底部按钮：先把自己钉到底边，再画。
///
/// 各步的内容高矮差得多，按钮跟着内容跑就会一跳一跳；钉住之后，
/// 「下一步」永远在同一个位置，闭着眼都点得到。
pub fn footer(ui: &mut egui::Ui, b: Buttons<'_>) -> Footer {
    let rest = ui.available_height() - 28.0;
    if rest > 0.0 {
        ui.add_space(rest);
    }

    let mut out = Footer::default();
    let emit = |ui: &mut egui::Ui, out: &mut Footer| {
        if theme::primary_button(ui, b.primary, b.enabled).clicked() {
            out.next = true;
        }
        if let Some(label) = b.back {
            ui.add_space(8.0);
            if theme::dialog_button(ui, label, true, 78.0).clicked() {
                out.back = true;
            }
        }
        if let Some(label) = b.cancel {
            ui.add_space(8.0);
            if theme::dialog_button(ui, label, true, 66.0).clicked() {
                out.cancel = true;
            }
        }
        if let Some(note) = b.note {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(note)
                    .size(11.0)
                    .color(theme::TEXT_FAINT),
            );
        }
    };

    if b.right_aligned {
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| emit(ui, &mut out),
        );
    } else {
        ui.horizontal(|ui| emit(ui, &mut out));
    }
    out
}

/// 固定高度的滚动列表，行距归零（间距交给 [`row`] 的细线控制）。
pub fn list<R>(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .max_height(list_height(ui.available_height()))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui)
        })
        .inner
}

/// 转圈 + 一句说明，居中。
pub fn loading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(30.0);
    ui.vertical_centered(|ui| {
        ui.spinner();
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(text)
                .size(12.0)
                .color(theme::TEXT_WEAK),
        );
    });
}

/// 一张说明卡片：一句主文案 + 一句小字。
///
/// 「列表空着」和「清单还没拿到」都走这个——两处原本各写一遍
/// `theme::card()` + 两个 label，字号和内边距还不一样。
pub fn notice_card(ui: &mut egui::Ui, title: &str, sub: &str) {
    theme::card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(
            egui::RichText::new(title)
                .size(13.0)
                .color(theme::TEXT_WEAK),
        );
        if !sub.is_empty() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(sub)
                    .size(11.5)
                    .color(theme::TEXT_WEAK),
            );
        }
    });
}

/// 居中的一句提示（空列表、没匹配等）。
pub fn empty(ui: &mut egui::Ui, text: &str) {
    ui.add_space(30.0);
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(text)
                .size(12.0)
                .color(theme::TEXT_WEAK),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_never_shrinks_below_three_rows() {
        // 窗口矮到什么都放不下时，也得留三行半——否则只剩一条缝。
        assert_eq!(list_height(0.0), ROW_H * 3.5);
        assert_eq!(list_height(120.0), ROW_H * 3.5);
        // 扣掉 62 之后刚好 7.2 行 → 7 行 + 半行，露半行表示「下面还有」。
        assert_eq!(list_height(62.0 + ROW_H * 7.2), ROW_H * 7.5);
        assert_eq!(list_height(62.0 + ROW_H * 12.0), ROW_H * 12.5);
    }
}
