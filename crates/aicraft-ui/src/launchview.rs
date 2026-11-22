//! 启动游戏时那张「节点进度」弹窗。
//!
//! 启动那几秒，首页上别的东西都不该再被勾走注意力：整屏先磨成一片糊的底，
//! 中间摆一张卡片，一排节点按步骤一个个点亮，下面一行写现在走到哪一步。
//! 原来「启动游戏」那颗按钮的位置留一颗红的「取消启动」——它是唯一还能按的东西。

use std::sync::{Arc, Mutex};

use eframe::egui;
use egui::{RichText, Vec2};

use crate::blur;
use crate::theme;

/// 节点之间隔多远。6 个点 × 52 正好 260，420 宽的卡片两边各余 80。
const NODE_GAP: f32 = 52.0;
const NODE_RADIUS: f32 = 6.5;
const RAIL_HEIGHT: f32 = 26.0;

/// 这一步走到轨道上的哪个位置（0..=1）。
///
/// 第 1 步停在最左那颗，最后一步才走到最右 —— 进度是「已经走过的路」，
/// 不是「第几步」，所以起点是 0 而不是 1/total。
fn target_progress(step: usize, total: usize) -> f32 {
    if total < 2 {
        return 1.0;
    }
    let walked = step.clamp(1, total) - 1;
    walked as f32 / (total - 1) as f32
}

/// 让 `current` 朝 `target` 挪一点，挪完不越过它。
///
/// 步骤是跳着来的（后台报第 3 步就是第 3 步），但画面不该跟着跳：
/// 按时间追，快的时候一帧到位、慢的时候分成十几帧，看着才是「滑过去」的。
fn ease_toward(current: f32, target: f32, dt: f32) -> f32 {
    let gap = target - current;
    if gap.abs() < 0.0005 {
        return target;
    }
    // 一秒钟走完四分之三的差距：够快，又不至于像瞬移。
    let step = 1.0 - (-dt * 9.0).exp();
    let next = current + gap * step;
    // 只许朝目标走，不许冲过头（否则会来回抖一下）。
    if gap > 0.0 {
        next.min(target)
    } else {
        next.max(target)
    }
}

/// 整排节点占多宽。只有一个节点时没有横向跨度。
fn node_span(total: usize) -> f32 {
    NODE_GAP * total.saturating_sub(1) as f32
}

/// 抓下来的整屏画面磨小之后的那张底图。
///
/// 截图是异步的：叫一次 request，回调过一会儿才把像素塞进 shot；
/// 真正磨糊、上传纹理都在 poll 里做——那已经是下一帧，画面上还什么都没有。
pub struct Veil {
    tex: Option<egui::TextureHandle>,
    shot: Arc<Mutex<Option<egui::ColorImage>>>,
    asked: bool,
    /// 什么时候叫的截图。迟迟等不到就放行，不能让人对着没反应的窗口干等。
    asked_at: Option<std::time::Instant>,
}

impl Default for Veil {
    fn default() -> Self {
        Self::new()
    }
}

impl Veil {
    pub fn new() -> Self {
        Self {
            tex: None,
            shot: Arc::new(Mutex::new(None)),
            asked: false,
            asked_at: None,
        }
    }

    /// 启动那一刻叫一次：把这一帧的画面抓下来当底。
    ///
    /// 必须赶在弹窗第一次画出来之前——否则抓到的就是弹窗自己，
    /// 磨完再贴回去等于什么都没发生。
    pub fn request(&mut self, ctx: &egui::Context) {
        if self.asked {
            return;
        }
        self.asked = true;
        self.asked_at = Some(std::time::Instant::now());
        let slot = Arc::clone(&self.shot);
        // 回调是 `FnOnce + Send + 'static`，得自己带一份 ctx 进去。
        let wake = ctx.clone();
        ctx.request_screenshot(move |image| {
            if let Ok(mut slot) = slot.lock() {
                *slot = Some((*image).clone());
            }
            // 回调不自己催帧，不叫这一下这张底图要等到下次有人动鼠标才出现。
            wake.request_repaint();
        });
    }

    /// 每帧叫一次：截图回来了就磨糊、传纹理。
    pub fn poll(&mut self, ctx: &egui::Context) {
        let image = self.shot.lock().ok().and_then(|mut slot| slot.take());
        let Some(image) = image else { return };
        let small = blur::shrink_and_blur(&image);
        self.tex = Some(ctx.load_texture(
            "aicraft-launch-veil",
            small,
            egui::TextureOptions::LINEAR,
        ));
        ctx.request_repaint();
    }

    /// 底图能画了吗。
    ///
    /// 截图要走一趟显卡读回，慢的时候几十毫秒；等不到就退到一层白，
    /// 让弹窗先出来——卡在「什么都没有」比糊得不够好看糟糕得多。
    pub fn ready(&self) -> bool {
        self.tex.is_some()
            || self
                .asked_at
                .is_some_and(|at| at.elapsed() > std::time::Duration::from_millis(350))
    }


    /// 铺满整屏。抓图还没回来时退回一层半透明白，绝不挡着启动。
    pub fn paint(&self, ctx: &egui::Context) {
        let rect = ctx.content_rect();
        egui::Area::new(egui::Id::unique("aicraft-launch-veil"))
            .order(egui::Order::Middle)
            .fixed_pos(rect.min)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_min_size(rect.size());
                let painter = ui.painter();
                match &self.tex {
                    Some(tex) => {
                        painter.image(
                            tex.id(),
                            rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                        // 压白会让整屏发雾：本来底下的页面就是浅灰白，再提亮就什么都不剩了。
                        // 反过来压一点点暗，白卡片才立得住，糊住的界面也还看得出是「哪一页」。
                        painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(18));
                    }
                    None => {
                        painter.rect_filled(rect, 0.0, egui::Color32::from_white_alpha(226));
                    }
                }
            });
    }
}

/// 启动弹窗活着的时候，界面这边要记的全部东西。
///
/// 有它 = 正在启动。这比 `busy` 准：`busy` 也被下载、安装占着，
/// 而只有启动才该把整屏糊掉、只留一颗取消按钮。
pub struct Launch {
    /// 正在启动的实例名。
    pub instance: String,
    /// 糊掉的那张底图。
    pub veil: Veil,
    /// 已经按过取消，正在等后台收手。
    pub cancelling: bool,
    /// 交给后台那个线程的开关。
    pub token: aicraft_core::cancel::CancelToken,
    /// 轨道上那颗「走到哪儿」的游标（0..=1）。
    ///
    /// 后台报的是「第几步」，一步一跳；这里存的是画面上的连续位置，
    /// 每帧朝目标挪一点，看上去才是滑过去的。
    progress: f32,
    /// 上一次推进的时刻，用来算这一帧走了多久。
    last_tick: Option<std::time::Instant>,
}

impl Launch {
    pub fn new(instance: &str) -> Self {
        Self {
            instance: instance.to_string(),
            veil: Veil::new(),
            cancelling: false,
            token: aicraft_core::cancel::CancelToken::new(),
            progress: 0.0,
            last_tick: None,
        }
    }

    /// 每帧把游标朝目标挪一点。返回这一帧该画的位置。
    ///
    /// 第一次调用直接落在起点，不从 0 开始滑——弹窗刚出来就看见它动，
    /// 会让人以为是启动已经进行了一会儿。
    pub fn advance(&mut self, step: usize, total: usize, ctx: &egui::Context) -> f32 {
        let now = std::time::Instant::now();
        let dt = self
            .last_tick
            .map(|last| (now - last).as_secs_f32().min(0.25))
            .unwrap_or(0.0);
        self.last_tick = Some(now);
        let target = target_progress(step, total);
        self.progress = ease_toward(self.progress, target, dt);
        if (self.progress - target).abs() > 0.0005 {
            // 还没走到：自己催帧，别等玩家动鼠标才继续。
            ctx.request_repaint();
        }
        self.progress
    }

    /// 按了「取消启动」：按钮当场变成「正在取消…」，后台在下个步骤之间收手。
    pub fn cancel(&mut self) {
        self.cancelling = true;
        self.token.cancel();
    }
}

/// 这张弹窗要显示的全部内容。
pub struct View<'a> {
    /// 正在启动的实例名。
    pub instance: &'a str,
    /// 走到第几步（从 1 开始）。
    pub step: usize,
    /// 一共几步。
    pub total: usize,
    /// 这一步在做什么。
    pub label: &'a str,
    /// 已经按过「取消启动」，正在等后台收手。
    pub cancelling: bool,
    /// 轨道游标这一帧的位置（0..=1），由 `Launch::advance` 逐帧推出来。
    pub progress: f32,
}

/// 画弹窗。返回 true 表示这一帧点了「取消启动」。
pub fn show(ctx: &egui::Context, view: View<'_>) -> bool {
    let mut clicked = false;
    egui::Modal::new(egui::Id::unique("aicraft-launch-modal"))
        // 底已经自己铺好了，这里不要再压一层黑。
        .backdrop_color(egui::Color32::TRANSPARENT)
        .frame(theme::dialog_frame().inner_margin(egui::Margin::symmetric(24, 20)))
        .show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(
                RichText::new(format!("正在启动 {}", view.instance))
                    .size(15.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(16.0);
            node_rail(ui, view.progress, view.total);
            ui.add_space(12.0);
            ui.label(
                RichText::new(format!(
                    "第 {} / {} 步：{}",
                    view.step.min(view.total),
                    view.total,
                    view.label
                ))
                .size(12.0)
                .color(theme::TEXT_WEAK),
            );
            ui.add_space(6.0);
            if view.cancelling {
                ui.label(RichText::new("正在取消…").size(11.5).color(theme::DANGER));
            } else {
                ui.label(
                    RichText::new("游戏窗口出现后这里会自动收起。")
                        .size(11.5)
                        .color(theme::TEXT_FAINT),
                );
            }
            ui.add_space(16.0);
            // 取消按钮就摆在卡片右下角：整屏都被糊住了，它是唯一能按的东西，
            // 但也不必像首页那颗一样占满一整个角。
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if view.cancelling {
                    "正在取消…"
                } else {
                    "取消启动"
                };
                let size = Vec2::new(104.0, 32.0);
                if theme::cancel_launch_button(ui, !view.cancelling, label, size).clicked() {
                    clicked = true;
                }
            });
        });
    clicked
}

/// 一排节点：走过的填实色，正在做的那颗套一圈会呼吸的光晕。
///
/// `progress` 是 0..=1 的连续位置而不是整数步号：填色那一小段跟着游标一起滑，
/// 中间的轨道用一串短线段拼出来，看上去就是一条会走的光带，而不是一格一格跳。
fn node_rail(ui: &mut egui::Ui, progress: f32, total: usize) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, RAIL_HEIGHT), egui::Sense::hover());
    if total == 0 {
        return;
    }
    let painter = ui.painter();
    let span = node_span(total);
    let start = rect.center().x - span / 2.0;
    let y = rect.center().y;
    let time = ui.input(|i| i.time) as f32;
    let progress = progress.clamp(0.0, 1.0);
    let cursor = start + span * progress;

    if span > 0.0 {
        painter.line_segment(
            [egui::pos2(start, y), egui::pos2(start + span, y)],
            egui::Stroke::new(2.0, theme::TRACK),
        );
        if progress > 0.0 {
            painter.line_segment(
                [egui::pos2(start, y), egui::pos2(cursor, y)],
                egui::Stroke::new(2.0, theme::accent_fill()),
            );
        }
    }

    for index in 0..total {
        let center = egui::pos2(start + NODE_GAP * index as f32, y);
        // 光晕跟着游标走：它现在就在两颗点之间的某处，那里才是「正在做」的地方。
        if span > 0.0 && (cursor - center.x).abs() < NODE_GAP / 2.0 {
            let pulse = 0.5 + 0.5 * (time * 2.6).sin();
            painter.circle_filled(
                egui::pos2(cursor, y),
                NODE_RADIUS + 4.0 + pulse * 3.0,
                theme::accent_fill().gamma_multiply(0.16 + 0.12 * pulse),
            );
        }
        let fill = if center.x <= cursor + 0.01 {
            theme::accent_fill()
        } else {
            theme::TRACK
        };
        painter.circle_filled(center, NODE_RADIUS, fill);
    }

    // 两颗点之间也放一颗小游标：走到一半时，光带末端得有个明确的位置。
    if span > 0.0 && progress > 0.0 {
        painter.circle_filled(egui::pos2(cursor, y), 3.2, theme::accent_strong());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_node_sits_on_the_centre() {
        assert_eq!(node_span(1), 0.0);
        assert_eq!(node_span(0), 0.0);
    }

    #[test]
    fn the_rail_widens_one_gap_per_node() {
        assert_eq!(node_span(2), NODE_GAP);
        assert_eq!(node_span(6), NODE_GAP * 5.0);
    }

    #[test]
    fn the_first_step_waits_at_the_left_end() {
        // 第 1 步还没走出第一步的距离：游标停在最左那颗点上。
        assert_eq!(target_progress(1, 6), 0.0);
        // 最后一步才走到最右。
        assert_eq!(target_progress(6, 6), 1.0);
        // 中间几步均匀分布，第 3 步正好在 2/5 处。
        assert!((target_progress(3, 6) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn a_single_step_rail_is_already_at_its_end() {
        // 只有一步时没有横向跨度，游标只能算走到头，否则永远停在 0。
        assert_eq!(target_progress(1, 1), 1.0);
        assert_eq!(target_progress(1, 0), 1.0);
    }

    #[test]
    fn the_cursor_glides_instead_of_jumping() {
        // 目标在 1，游标从 0 起步：一帧只许走一部分，不能直接等于目标。
        let after_one_frame = ease_toward(0.0, 1.0, 1.0 / 60.0);
        assert!(after_one_frame > 0.0 && after_one_frame < 1.0);
        // 连着走十帧之后明显更近，而且不会越过目标。
        let mut at = 0.0;
        for _ in 0..10 {
            at = ease_toward(at, 1.0, 1.0 / 60.0);
        }
        assert!(at > after_one_frame && at <= 1.0);
    }

    #[test]
    fn a_cursor_that_arrived_stays_put() {
        // 已经到位就别再抖：差距小于阈值时直接落在目标上。
        assert_eq!(ease_toward(1.0, 1.0, 1.0 / 60.0), 1.0);
        assert_eq!(ease_toward(0.9999, 1.0, 1.0 / 60.0), 1.0);
    }

    #[test]
    fn cancelling_flips_the_switch_the_background_watches() {
        let mut launch = Launch::new("1.20.1-Fabric");
        assert!(!launch.cancelling && !launch.token.is_cancelled());
        launch.cancel();
        // 界面这边先换字，后台线程看到的是同一个开关。
        assert!(launch.cancelling);
        assert!(launch.token.clone().is_cancelled());
    }

    #[test]
    fn the_last_node_never_lights_up_before_its_turn() {
        // 第 5 步时游标还差最后一段，最后一颗不该提前亮。
        assert!(target_progress(5, 6) < 1.0);
        // 只有走到最后一步，游标才真的落到最右那颗上。
        assert_eq!(target_progress(6, 6), 1.0);
    }
}
