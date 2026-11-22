//! 配色与公共绘制件。
//!
//! 拆成单独模块，是为了让 `main.rs` 继续只管状态和流程：长什么样都收在这里，
//! 改配色时不用在几百行界面代码里翻颜色值。
//!
//! 现在的方向是「亮色 + 柔和层次」：近白的背景、白卡片、极细描边与一层
//! 很轻的投影撑出层次感，强调色仍是那抹薄荷绿，但放在亮底上要更深一点
//! （`accent_strong()`）才够清晰。

use egui::layers::ShapeIdx;
use egui::text::{CCursor, CCursorRange, CharIndex};
use egui::text_edit::TextEditState;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Margin, Painter, Pos2, Rect,
    Response, RichText, Sense, Shape, Stroke, StrokeKind, TextStyle, Ui, Vec2,
};
use std::hash::Hash;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---- 色板 ----
/// 面板底色（顶栏、侧栏、标题栏）
pub const PANEL: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// 中央区域的底色
pub const BG_BOTTOM: Color32 = Color32::from_rgb(0xED, 0xF1, 0xF7);

/// 界面各块底色的不透明度（只在摆了背景图时起作用）。
///
/// 放全局原子量而不是每帧从 Settings 传下来：底色被几十处调用，全都加一个
/// 参数会让每个 `theme::card()` 的调用点都变长，而这四个数只影响「白底
/// 透不透」，没有别的语义。
static ALPHA_CHROME: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(
    aicraft_core::settings::DEFAULT_ALPHA_CHROME,
);
static ALPHA_BACKGROUND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(
    aicraft_core::settings::DEFAULT_ALPHA_BACKGROUND,
);
static ALPHA_CARD: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(
    aicraft_core::settings::DEFAULT_ALPHA_CARD,
);

/// 现在有没有背景图。
static WALLPAPER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 有没有背景图（决定各大块底色要不要让出一点透明度）。
pub fn wallpaper_active() -> bool {
    WALLPAPER.load(Ordering::Relaxed)
}

/// 换背景图之后调一次：没图就把底色还原成不透明。
pub fn set_wallpaper(active: bool) {
    WALLPAPER.store(active, Ordering::Relaxed);
}

/// 灌入三块不透明度。启动时和设置页拖动时各调一次。
///
/// 取值就是 0..=255 全程：0 全透明、255 完全不透明，中间任意一档都合法。
pub fn set_alphas(alphas: aicraft_core::settings::UiAlphas) {
    ALPHA_CHROME.store(alphas.chrome, Ordering::Relaxed);
    ALPHA_BACKGROUND.store(alphas.background, Ordering::Relaxed);
    ALPHA_CARD.store(alphas.card, Ordering::Relaxed);
}

/// 顶栏底色。和侧栏共用一根滑杆，所以它俩永远同步。
pub fn panel_titlebar() -> Color32 {
    wallpaper_tint(PANEL, ALPHA_CHROME.load(Ordering::Relaxed))
}

/// 侧栏（导航栏、账户页左栏、管理页左栏）底色，跟顶栏同一档。
pub fn panel() -> Color32 {
    wallpaper_tint(PANEL, ALPHA_CHROME.load(Ordering::Relaxed))
}

/// 中央区域底色，用法同 panel。
pub fn bg_bottom() -> Color32 {
    wallpaper_tint(BG_BOTTOM, ALPHA_BACKGROUND.load(Ordering::Relaxed))
}

/// 卡片底色。
///
/// 跟 `CARD` 的区别就在透明度：卡片上要写字，默认给足 255，
/// 想看清底图时再自己往下拉。
pub fn card_fill() -> Color32 {
    wallpaper_tint(CARD, ALPHA_CARD.load(Ordering::Relaxed))
}

/// 「玻璃」卡片的底色。
///
/// 它本来就只有 150 的透明度，所以跟着卡片滑杆往下走、但不会比 150 更实——
/// 玩家把卡片调回不透明时，首页这块大面板也不该突然变成一块死白。
pub fn glass_fill() -> Color32 {
    let alpha = ALPHA_CARD.load(Ordering::Relaxed).min(150);
    with_alpha(CARD, alpha)
}

/// 有大块底色要画的公共判据：没背景图时一律不透明。
///
/// 滑杆上的值照样留着——玩家清掉背景再换一张，那份手感不用重调一遍；
/// 但没图可透的时候界面必须和没有这套功能时一模一样。
fn wallpaper_tint(color: Color32, alpha: u8) -> Color32 {
    if wallpaper_active() {
        with_alpha(color, alpha)
    } else {
        color
    }
}

/// 中央区域底色，但透明度再乘一个系数（滚动区两头的渐隐用）。
///
/// 渐隐本身就要把底色慢慢淡掉，叠加在「半透明底」上才不会露出底板的白边。
pub fn bg_bottom_faded(scale: f32) -> Color32 {
    let base = bg_bottom();
    with_alpha(base, (base.a() as f32 * scale.clamp(0.0, 1.0)) as u8)
}

/// 把一个不透明色换个 alpha。Color32 是预乘的，直接改字段会显得发灰，
/// 所以绕一圈走线性色。
fn with_alpha(color: Color32, alpha: u8) -> Color32 {
    let [r, g, b, _] = color.to_array();
    Color32::from_rgba_unmultiplied(r, g, b, alpha)
}
/// 卡片底色
pub const CARD: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// 鼠标悬停时的浅色块
pub const CARD_HOVER: Color32 = Color32::from_rgb(0xF3, 0xF6, 0xFA);
/// 描边：比背景深一点点就够
pub const BORDER: Color32 = Color32::from_rgb(0xE2, 0xE8, 0xF0);
/// 一套强调色：填充、浅底、亮底上可读的深色、以及压在填充上的字色。
#[derive(Clone, Copy)]
pub struct Accent {
    /// 按钮填充、滑杆、选中圆点
    pub base: Color32,
    /// 选中行的浅底
    pub soft: Color32,
    /// 亮底上可读的深色文字
    pub strong: Color32,
    /// 压在 `base` 上写字的颜色（浅色主题色上白字看不清，得转深）
    pub ink: Color32,
}

/// 16 种主题色，顺序与 `aicraft_core::settings::ThemeColor::ALL` 一致。
///
/// 前三个值是手调的，不是从 base 算出来的：白、黄这类浅色按公式推会糊成
/// 一片，深色按公式推又偏灰，所以干脆一张表写死。黄绿那行就是启动器一直
/// 在用的薄荷绿，换主题色功能上线时不能把默认观感改掉；它排在第一格，
/// 和默认选中的那一格对上。
const PALETTE: [(Color32, Color32, Color32, Color32); 16] = [
    // 黄绿（默认，也就是原来的薄荷绿）
    (
        Color32::from_rgb(0x10, 0xB9, 0x81),
        Color32::from_rgb(0xE6, 0xF7, 0xF0),
        Color32::from_rgb(0x04, 0x7E, 0x5A),
        Color32::WHITE,
    ),
    // 白色
    (
        Color32::from_rgb(0xE9, 0xEC, 0xEC),
        Color32::from_rgb(0xE9, 0xED, 0xF2),
        Color32::from_rgb(0x6B, 0x73, 0x7E),
        Color32::from_rgb(0x2B, 0x31, 0x3A),
    ),
    // 橙色
    (
        Color32::from_rgb(0xF9, 0x80, 0x1D),
        Color32::from_rgb(0xFE, 0xF0, 0xE5),
        Color32::from_rgb(0xA1, 0x5D, 0x29),
        Color32::WHITE,
    ),
    // 品红
    (
        Color32::from_rgb(0xC7, 0x4E, 0xBD),
        Color32::from_rgb(0xF9, 0xEB, 0xF7),
        Color32::from_rgb(0x83, 0x3F, 0x89),
        Color32::WHITE,
    ),
    // 淡蓝
    (
        Color32::from_rgb(0x3A, 0xB3, 0xDA),
        Color32::from_rgb(0xE8, 0xF6, 0xFB),
        Color32::from_rgb(0x2F, 0x7C, 0x9A),
        Color32::WHITE,
    ),
    // 黄色
    (
        Color32::from_rgb(0xFE, 0xD8, 0x3D),
        Color32::from_rgb(0xEF, 0xE8, 0xC1),
        Color32::from_rgb(0x73, 0x6C, 0x3C),
        Color32::from_rgb(0x3A, 0x30, 0x10),
    ),
    // 粉红
    (
        Color32::from_rgb(0xF3, 0x8B, 0xAA),
        Color32::from_rgb(0xFE, 0xF2, 0xF5),
        Color32::from_rgb(0x9E, 0x64, 0x7E),
        Color32::from_rgb(0x3B, 0x1E, 0x28),
    ),
    // 灰色
    (
        Color32::from_rgb(0x47, 0x4F, 0x52),
        Color32::from_rgb(0xEA, 0xEB, 0xEB),
        Color32::from_rgb(0x37, 0x40, 0x49),
        Color32::WHITE,
    ),
    // 淡灰
    (
        Color32::from_rgb(0x9D, 0x9D, 0x97),
        Color32::from_rgb(0xF4, 0xF4, 0xF3),
        Color32::from_rgb(0x6A, 0x6F, 0x72),
        Color32::WHITE,
    ),
    // 青色
    (
        Color32::from_rgb(0x16, 0x9C, 0x9C),
        Color32::from_rgb(0xE4, 0xF4, 0xF4),
        Color32::from_rgb(0x19, 0x6E, 0x75),
        Color32::WHITE,
    ),
    // 紫色
    (
        Color32::from_rgb(0x89, 0x32, 0xB8),
        Color32::from_rgb(0xF1, 0xE7, 0xF7),
        Color32::from_rgb(0x5E, 0x2E, 0x86),
        Color32::WHITE,
    ),
    // 蓝色
    (
        Color32::from_rgb(0x3C, 0x44, 0xAA),
        Color32::from_rgb(0xE9, 0xE9, 0xF5),
        Color32::from_rgb(0x30, 0x39, 0x7E),
        Color32::WHITE,
    ),
    // 棕色
    (
        Color32::from_rgb(0x83, 0x54, 0x32),
        Color32::from_rgb(0xF1, 0xEB, 0xE7),
        Color32::from_rgb(0x5B, 0x43, 0x36),
        Color32::WHITE,
    ),
    // 绿色
    (
        Color32::from_rgb(0x5E, 0x7C, 0x16),
        Color32::from_rgb(0xEC, 0xF0, 0xE4),
        Color32::from_rgb(0x44, 0x5B, 0x25),
        Color32::WHITE,
    ),
    // 红色
    (
        Color32::from_rgb(0xB0, 0x2E, 0x26),
        Color32::from_rgb(0xF6, 0xE7, 0xE6),
        Color32::from_rgb(0x76, 0x2C, 0x2E),
        Color32::WHITE,
    ),
    // 黑色
    (
        Color32::from_rgb(0x1D, 0x1D, 0x21),
        Color32::from_rgb(0xE5, 0xE5, 0xE5),
        Color32::from_rgb(0x1D, 0x22, 0x2B),
        Color32::WHITE,
    ),
];

/// 当前生效的主题色下标。界面只在主线程跑，用 Relaxed 就够。
static ACTIVE_THEME: AtomicUsize = AtomicUsize::new(0);

fn accent_index(color: aicraft_core::settings::ThemeColor) -> usize {
    use aicraft_core::settings::ThemeColor;
    // 下标必须和 `PALETTE` 的行序对齐；黄绿是默认色，排在 0。
    match color {
        ThemeColor::Lime => 0,
        ThemeColor::White => 1,
        ThemeColor::Orange => 2,
        ThemeColor::Magenta => 3,
        ThemeColor::LightBlue => 4,
        ThemeColor::Yellow => 5,
        ThemeColor::Pink => 6,
        ThemeColor::Gray => 7,
        ThemeColor::LightGray => 8,
        ThemeColor::Cyan => 9,
        ThemeColor::Purple => 10,
        ThemeColor::Blue => 11,
        ThemeColor::Brown => 12,
        ThemeColor::Green => 13,
        ThemeColor::Red => 14,
        ThemeColor::Black => 15,
    }
}

/// 某个主题色的完整配色（画色板预览时也要用）。
pub fn accent_of(color: aicraft_core::settings::ThemeColor) -> Accent {
    let (base, soft, strong, ink) = PALETTE[accent_index(color)];
    Accent {
        base,
        soft,
        strong,
        ink,
    }
}

/// 当前主题色。
pub fn accent() -> Accent {
    let (base, soft, strong, ink) = PALETTE[ACTIVE_THEME.load(Ordering::Relaxed)];
    Accent {
        base,
        soft,
        strong,
        ink,
    }
}

/// 强调色（按钮填充）
pub fn accent_fill() -> Color32 {
    accent().base
}

/// 强调色的浅底（选中行、选中背景）
pub fn accent_soft() -> Color32 {
    accent().soft
}

/// 亮底上可读的强调色文字
pub fn accent_strong() -> Color32 {
    accent().strong
}

/// 压在强调色填充上写字的颜色。
pub fn accent_ink() -> Color32 {
    accent().ink
}

/// 主题色的色块。`selected` 时套一圈深色描边并打勾，未选中只有一道浅边。
///
/// 白色、淡灰这些浅色在卡片底色上几乎看不出边界，所以无论选没选中都得
/// 描边，不能只靠填充色区分。
pub fn color_swatch(
    ui: &mut Ui,
    color: aicraft_core::settings::ThemeColor,
    selected: bool,
) -> Response {
    const SIZE: f32 = 30.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(SIZE), Sense::click());
    let accent = accent_of(color);
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(8), accent.base);
    painter.rect_stroke(
        rect.shrink(0.5),
        CornerRadius::same(8),
        Stroke::new(1.0, BORDER),
        StrokeKind::Inside,
    );
    if selected {
        painter.rect_stroke(
            rect.shrink(1.5),
            CornerRadius::same(7),
            Stroke::new(2.0, accent.strong),
            StrokeKind::Inside,
        );
        let c = rect.center();
        let path = vec![
            Pos2::new(c.x - 5.0, c.y + 0.2),
            Pos2::new(c.x - 1.6, c.y + 3.6),
            Pos2::new(c.x + 5.2, c.y - 3.6),
        ];
        painter.add(Shape::line(path, Stroke::new(2.0, accent.ink)));
    }
    hand(response)
}
/// 正文
pub const TEXT: Color32 = Color32::from_rgb(0x1E, 0x29, 0x3B);
/// 次要文字
pub const TEXT_WEAK: Color32 = Color32::from_rgb(0x6B, 0x7A, 0x8C);
/// 错误
pub const DANGER: Color32 = Color32::from_rgb(0xDC, 0x2F, 0x3C);
/// 警告（琥珀色）
pub const WARN: Color32 = Color32::from_rgb(0xB4, 0x6A, 0x00);
/// 调试、详细两级的弱化文字
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x93, 0x9F, 0xAE);
/// 细线（日志行之间、版本行之间）：比 `BORDER` 再淡一档，
/// 一天要出现十来条，和描边同浓度会抢正文的视线。
pub const RULE: Color32 = Color32::from_rgb(0xE8, 0xED, 0xF4);
/// 警告标签的浅底：琥珀字压在这个底上才读得清。
pub const WARN_TINT: Color32 = Color32::from_rgb(0xFD, 0xF3, 0xE3);
/// 危险动作图标键的悬停浅底：红字压上去要够淡，整列才不会一片警报色。
pub const DANGER_TINT: Color32 = Color32::from_rgb(0xFD, 0xEC, 0xEE);
/// 空轨道的底（内存条、滑杆槽）：比 `BORDER` 略深一点，
/// 才压得住上面那一段实色。
pub const TRACK: Color32 = Color32::from_rgb(0xE7, 0xED, 0xF5);

/// 日志等级对应的颜色：错误红、警告琥珀、信息正文、调试/详细灰。
pub fn level_color(level: aicraft_core::logging::Level) -> Color32 {
    use aicraft_core::logging::Level;
    match level {
        Level::Error => DANGER,
        Level::Warn => WARN,
        Level::Info => TEXT,
        Level::Debug => TEXT_WEAK,
        Level::Trace => TEXT_FAINT,
    }
}

/// 把整套配色与间距装进 egui 的全局样式。
///
/// 主题色变了要再调一次：强调色在 `visuals()` 里落地（悬停、选中、按下
/// 都吃这几项），只改全局变量不动样式的话，按钮还是旧颜色。
pub fn install(ctx: &egui::Context, color: aicraft_core::settings::ThemeColor) {
    ACTIVE_THEME.store(accent_index(color), Ordering::Relaxed);
    // 0.37 起样式按主题分开存，得先声明「一直用亮色」
    ctx.set_theme(egui::ThemePreference::Light);
    let mut style = (*ctx.style_of(egui::Theme::Light)).clone();
    style.visuals = visuals();

    style.spacing.item_spacing = Vec2::new(9.0, 9.0);
    style.spacing.button_padding = Vec2::new(13.0, 6.0);
    style.spacing.interact_size.y = 28.0;

    // 关掉「标签文字可选中」。
    //
    // egui 默认开着它，于是每一个 `ui.label(..)` 都变成能拖选的文本区域：
    // 鼠标一移上去就变成 I 形光标，看着像整页都能编辑。启动器的字是界面，
    // 不是文档 —— 玩家不会来这里复制版本号，只会觉得光标很怪。
    // 真要复制的地方（日志、路径）用的是 `TextEdit`，不受这个开关影响。
    style.interaction.selectable_labels = false;

    style.text_styles = [
        (
            egui::TextStyle::Heading,
            FontId::new(19.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Body,
            FontId::new(13.5, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Button,
            FontId::new(13.5, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Small,
            FontId::new(11.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Monospace,
            FontId::new(12.0, egui::FontFamily::Monospace),
        ),
    ]
    .into_iter()
    .collect();

    ctx.set_style_of(egui::Theme::Light, style);
}

/// 亮色主题的完整视觉参数。
fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    v.dark_mode = false;

    v.override_text_color = Some(TEXT);
    v.weak_text_color = Some(TEXT_WEAK);
    v.panel_fill = PANEL;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(14);
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(30),
    };
    v.popup_shadow = v.window_shadow;
    // 悬浮提示（tooltip）和下拉菜单在 egui 里共用这一个圆角，没有单独的
    // tooltip 圆角可调：改成 6，提示框看着利落，菜单也跟着收一点。
    v.menu_corner_radius = CornerRadius::same(6);
    // 输入框：比白再浅一点点的灰，一眼能看出「这里能打字」
    v.extreme_bg_color = Color32::from_rgb(0xF5, 0xF8, 0xFC);
    v.text_edit_bg_color = Some(Color32::from_rgb(0xF5, 0xF8, 0xFC));
    v.faint_bg_color = CARD_HOVER;
    v.code_bg_color = Color32::from_rgb(0xF2, 0xF6, 0xFA);
    v.hyperlink_color = accent_strong();
    v.selection.bg_fill = accent_soft();
    v.selection.stroke = Stroke::new(1.0, accent_fill());
    v.striped = false;
    v.button_frame = true;
    // 悬停在可点控件上时给手型。
    //
    // egui 默认是 `None`（跟着原生工具箱的习惯走，按钮不变光标），
    // 结果是：同一个界面上，一部分按钮有手型（各自写了 `on_hover_cursor`），
    // 另一部分没有 —— 看着像有的能点有的不能。
    // 这里是浏览器式的约定，全局统一之后就不用每个按钮各补一句。
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);

    // 不可交互的元素（分隔线、窗口描边等）
    v.widgets.noninteractive.bg_fill = CARD;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.corner_radius = CornerRadius::same(10);

    // 普通按钮：白底 + 细边
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0xFB, 0xFC, 0xFE);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.corner_radius = CornerRadius::same(9);

    // 悬停：底色压灰一点，边框深一点
    v.widgets.hovered.weak_bg_fill = CARD_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0xC3, 0xCF, 0xDE));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.corner_radius = CornerRadius::same(9);

    // 按下：浅绿 + 绿边
    v.widgets.active.weak_bg_fill = accent_soft();
    v.widgets.active.bg_stroke = Stroke::new(1.0, accent_fill());
    v.widgets.active.fg_stroke = Stroke::new(1.0, accent_strong());
    v.widgets.active.corner_radius = CornerRadius::same(9);

    v.widgets.open = v.widgets.active;
    v
}

/// 卡片能投下的一层极轻的阴影，用来在近白背景上分出层次。
fn soft_shadow() -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: [0, 2],
        blur: 10,
        spread: 0,
        color: Color32::from_black_alpha(14),
    }
}

/// 一张卡片：圆角、细边、内边距、轻投影。
pub fn card() -> Frame {
    Frame::NONE
        .fill(card_fill())
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(16, 14))
        .shadow(soft_shadow())
}

/// 「玻璃」卡片：半透明白底，浮在浅灰蓝的页面上。
///
/// 跟 `card()` 的差别只在透明度——首页右侧那块大面板用它，底下那层底色
/// 会淡淡透上来，比纯白一块更轻。描边也压淡一点，免得半透明的边上
/// 出现一道硬线。
pub fn glass_card() -> Frame {
    Frame::NONE
        .fill(glass_fill())
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 190),
        ))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::symmetric(20, 18))
        .shadow(egui::epaint::Shadow {
            offset: [0, 4],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(10),
        })
}

/// 浅灰内凹块（日志区、设备码展示）
pub fn inset() -> Frame {
    Frame::NONE
        .fill(Color32::from_rgb(0xF6, 0xF9, 0xFC))
        .stroke(Stroke::new(1.0, Color32::from_rgb(0xE6, 0xED, 0xF5)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 10))
}

/// 「当前选中」那张卡片：白底不变，描边和投影都换成主题绿。
///
/// 用在目录列表这种「一次只有一个当前项」的地方——扫描一眼就知道
/// 启动器现在认的是哪一行，不用去读那枚「当前」小标签。
pub fn selected_card() -> Frame {
    Frame::NONE
        .fill(card_fill())
        .stroke(Stroke::new(1.0, accent_fill()))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(16, 14))
        .shadow(egui::epaint::Shadow {
            offset: [0, 2],
            blur: 12,
            spread: 1,
            color: Color32::from_rgba_unmultiplied(0x10, 0xB9, 0x81, 46),
        })
}

/// 小标签：灰底圆角的一行小字，用来放「主类 / 库数量」这类属性。
/// 一枚标签的配色：底色、描边、文字。
///
/// 默认那套（`chip`）是中性灰，适合「Java 17」「95 个库」这种属性；
/// 需要一眼分辨的标签（比如加载器）用这组自己带颜色。
#[derive(Clone, Copy)]
pub struct ChipStyle {
    pub fill: Color32,
    pub stroke: Color32,
    pub text: Color32,
}

/// 加载器标签的配色：各取自家 logo 的主色，压淡成底、压深成字。
///
/// 用颜色而不是只靠文字，是为了「一眼扫过去就知道哪几个是魔改过的」：
/// 名字各不相同（`1.20.1-Fabric`、`ATM10 To the Sky`、`涟漪之篇·如涟漪之所见`），
/// 靠读字太慢，靠颜色一眼就分得开。
///
/// 原版用草方块的淡绿：MC 里最经典的那块草皮，比纯灰更像「这就是原样的东西」。
pub fn loader_chip(kind: Option<aicraft_core::loader::LoaderKind>) -> ChipStyle {
    use aicraft_core::loader::LoaderKind;
    match kind {
        // 草方块：顶面那种黄绿，底色调到几乎看不出颜色，字用草茎的深绿
        None => ChipStyle {
            fill: Color32::from_rgb(0xEA, 0xF6, 0xE2),
            stroke: Color32::from_rgb(0xCB, 0xE4, 0xBB),
            text: Color32::from_rgb(0x4E, 0x7B, 0x33),
        },
        // Fabric 的 logo 是米色方块，整体偏暖的卡其
        Some(LoaderKind::Fabric) => ChipStyle {
            fill: Color32::from_rgb(0xF7, 0xF1, 0xDF),
            stroke: Color32::from_rgb(0xE2, 0xD4, 0xB2),
            text: Color32::from_rgb(0x7A, 0x63, 0x33),
        },
        // Forge 的 logo 是一块铸铁砧：几乎不带色相的淡灰。
        //
        // 第一版给了一点蓝（`#46566E` 的藏青字 + 淡蓝底），本意是「铁器反光」，
        // 但和界面上那些冷蓝的属性标签太像，一列排下来分不清谁是谁。
        // 现在压成纯灰，只靠明暗说话 —— 灰是这一页里唯一「没有颜色」的那个，
        // 反而最好认。
        Some(LoaderKind::Forge) => ChipStyle {
            fill: Color32::from_rgb(0xF0, 0xF1, 0xF3),
            stroke: Color32::from_rgb(0xD5, 0xD8, 0xDD),
            text: Color32::from_rgb(0x56, 0x5C, 0x64),
        },
        // NeoForge 是狐狸火，但火苗的中间那层是橙不是红。
        //
        // 上一版取的是焰心外侧最浓的那圈（`#B4440E`），在亮底上读起来偏砖红，
        // 和「删除版本」那枚危险色挨得太近。现在往橙挪一档，饱和度也降下来：
        // 它应该像个和气的橘色徽章，不是警告。
        Some(LoaderKind::NeoForge) => ChipStyle {
            fill: Color32::from_rgb(0xFF, 0xF3, 0xE3),
            stroke: Color32::from_rgb(0xF7, 0xD4, 0xAC),
            text: Color32::from_rgb(0xB9, 0x6A, 0x1C),
        },
    }
}

/// 四枚加载器徽记的位图，编译期塞进 exe。
///
/// 用的是各家官方的 64×64 图（经 HMCL 仓库转存），比自绘的矢量形状更像
/// 玩家在别处见过的那个标志。四张合计 16 KB，且只有第一次画到时才解码。
static GRASS_PNG: &[u8] = include_bytes!("../assets/loader-grass.png");
static FABRIC_PNG: &[u8] = include_bytes!("../assets/loader-fabric.png");
static FORGE_PNG: &[u8] = include_bytes!("../assets/loader-forge.png");
static NEOFORGE_PNG: &[u8] = include_bytes!("../assets/loader-neoforge.png");

/// 上传到显存之后的一组句柄，按 `egui::Context` 缓存一次。
struct LoaderTextures {
    grass: egui::TextureHandle,
    fabric: egui::TextureHandle,
    forge: egui::TextureHandle,
    neoforge: egui::TextureHandle,
}

impl LoaderTextures {
    fn load(ctx: &egui::Context) -> Self {
        // 解码失败时给一张 1×1 的透明图，画出来是空的但不炸——四张图都是
        // 编译期嵌进来的，正常路径上不会走到这儿。
        let blank = || {
            ctx.load_texture(
                "loader-blank",
                egui::ColorImage::new([1, 1], vec![Color32::TRANSPARENT]),
                egui::TextureOptions::LINEAR,
            )
        };
        let one = |name: &str, bytes: &[u8]| match aicraft_core::png::decode(bytes) {
            Ok(img) => {
                let (chunks, _) = img.rgba.as_chunks::<4>();
                let pixels: Vec<Color32> = chunks
                    .iter()
                    .map(|c| Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                    .collect();
                ctx.load_texture(
                    name,
                    egui::ColorImage::new([img.width, img.height], pixels),
                    egui::TextureOptions::LINEAR,
                )
            }
            Err(_) => blank(),
        };
        Self {
            grass: one("loader-grass", GRASS_PNG),
            fabric: one("loader-fabric", FABRIC_PNG),
            forge: one("loader-forge", FORGE_PNG),
            neoforge: one("loader-neoforge", NEOFORGE_PNG),
        }
    }

    fn get(&self, kind: Option<aicraft_core::loader::LoaderKind>) -> &egui::TextureHandle {
        use aicraft_core::loader::LoaderKind;
        match kind {
            None => &self.grass,
            Some(LoaderKind::Fabric) => &self.fabric,
            Some(LoaderKind::Forge) => &self.forge,
            Some(LoaderKind::NeoForge) => &self.neoforge,
        }
    }
}

fn loader_textures(ctx: &egui::Context) -> std::sync::Arc<LoaderTextures> {
    let id = egui::Id::unique("aicraft-loader-textures");
    if let Some(cached) = ctx.data(|d| d.get_temp::<std::sync::Arc<LoaderTextures>>(id)) {
        return cached;
    }
    let fresh = std::sync::Arc::new(LoaderTextures::load(ctx));
    ctx.data_mut(|d| d.insert_temp(id, fresh.clone()));
    fresh
}

/// 加载器的小徽记：官方 64×64 位图缩到目标尺寸。
///
/// 纹理按 `egui::Context` 缓存（`loader_textures`），每帧只是重画同一个
/// `TextureId`，不会反复解码或上传。
pub fn loader_glyph(ui: &Ui, rect: Rect, kind: Option<aicraft_core::loader::LoaderKind>) {
    let textures = loader_textures(ui.ctx());
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    ui.painter()
        .image(textures.get(kind).id(), rect, uv, Color32::WHITE);
}

/// 代理端（Velocity）的徽记：一个点分出两条路。
///
/// 没有官方彩色 logo 可用（Velocity 仓库里没有位图），而它跟原版共用草方块
/// 会让人以为这是个能直接开的服——所以画一个「转发」的图形：左一个点，
/// 右侧分成上下两条，末端各一个点。
pub fn proxy_glyph(ui: &Ui, rect: Rect) {
    let painter = ui.painter();
    let color = accent_strong();
    let radius = rect.width().min(rect.height()) * 0.09;
    let left = Pos2::new(rect.left() + rect.width() * 0.28, rect.center().y);
    let mid_x = rect.left() + rect.width() * 0.52;
    let right_x = rect.left() + rect.width() * 0.76;
    let top = Pos2::new(right_x, rect.center().y - rect.height() * 0.22);
    let bottom = Pos2::new(right_x, rect.center().y + rect.height() * 0.22);
    let stroke = Stroke::new(rect.width() * 0.055, color);
    painter.line_segment([left, Pos2::new(mid_x, rect.center().y)], stroke);
    painter.line_segment([Pos2::new(mid_x, rect.center().y), top], stroke);
    painter.line_segment([Pos2::new(mid_x, rect.center().y), bottom], stroke);
    for point in [left, top, bottom] {
        painter.circle_filled(point, radius, color);
    }
}

/// 带徽记的标签：左边一枚 12px 图标，右边是字，其余和 [`chip`] 一样。
pub fn chip_loader(
    ui: &mut Ui,
    kind: Option<aicraft_core::loader::LoaderKind>,
    text: &str,
) -> Response {
    let style = loader_chip(kind);
    let font = FontId::new(11.5, egui::FontFamily::Proportional);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, style.text);
    // 图标 12 + 间隔 5 + 字 + 左右内边距
    let size = Vec2::new(galley.size().x + 12.0 + 5.0 + 16.0, galley.size().y + 7.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(7), style.fill);
    painter.rect_stroke(
        rect,
        CornerRadius::same(7),
        Stroke::new(1.0, style.stroke),
        StrokeKind::Inside,
    );
    let icon_size = 12.0;
    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.left() + 8.0 + icon_size / 2.0, rect.center().y),
        Vec2::splat(icon_size),
    );
    loader_glyph(ui, icon_rect, kind);
    painter.galley(
        Pos2::new(
            icon_rect.right() + 5.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        style.text,
    );
    response
}

/// 只有字的加载器标签：配色仍按加载器分，但不画徽记。
///
/// 资源下载页一行底排常常要并排三四个标签，每个都带一枚 8px 图标时，
/// 图标比字还抢眼，读起来像一排小印章。那里只留文字，颜色照样能把
/// Fabric / Forge / NeoForge 分开。
pub fn chip_loader_plain(ui: &mut Ui, kind: Option<aicraft_core::loader::LoaderKind>, text: &str) {
    let style = loader_chip(kind);
    let font = FontId::new(11.5, egui::FontFamily::Proportional);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, style.text);
    let size = galley.size() + Vec2::new(16.0, 7.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(7), style.fill);
    painter.rect_stroke(
        rect,
        CornerRadius::same(7),
        Stroke::new(1.0, style.stroke),
        StrokeKind::Inside,
    );
    painter.galley(rect.center() - galley.size() / 2.0, galley, style.text);
}

/// 向导里选加载器的那一排按钮：图标 + 名字，选中时浅绿底、绿边、深绿字。
///
/// 用自绘而不是 `selectable_label`，是因为后者只画字——四个选项排在一起时
/// 「Fabric / Forge / NeoForge」三个词头两个字母完全一样，扫一眼分不出来，
/// 有图标就在读字之前先把它们分开了。
pub fn loader_pick(
    ui: &mut Ui,
    kind: Option<aicraft_core::loader::LoaderKind>,
    picked: bool,
) -> Response {
    let text = match kind {
        Some(k) => k.label(),
        None => "原版",
    };
    let font = FontId::new(12.5, egui::FontFamily::Proportional);
    let color = if picked { accent_strong() } else { TEXT };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let icon_size = 14.0;
    let size = Vec2::new(
        icon_size + 6.0 + galley.size().x + 22.0,
        galley.size().y.max(icon_size) + 12.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let fill = if picked {
        accent_soft()
    } else if response.hovered() {
        CARD_HOVER
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(7), fill);
    }
    if picked {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(7),
            Stroke::new(1.0, accent_fill()),
            StrokeKind::Inside,
        );
    }
    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.left() + 11.0 + icon_size / 2.0, rect.center().y),
        Vec2::splat(icon_size),
    );
    loader_glyph(ui, icon_rect, kind);
    ui.painter().galley(
        Pos2::new(
            icon_rect.right() + 6.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        color,
    );
    hand(response)
}

/// 单选圆点。HMCL 那种「自动 / 手动」二选一用的就是它。
///
/// 不直接用 `ui.radio_value`：egui 的圆点在浅色主题下是灰圈 + 灰点，
/// 选中态不够明确。这里自己画：16px 圆环，选中的填主题绿并留一圈白芯，
/// 未选中的只有一道 1.4px 的灰边。
pub fn radio(ui: &mut Ui, selected: bool, text: &str) -> Response {
    let font = FontId::new(12.5, egui::FontFamily::Proportional);
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        font,
        if selected { TEXT } else { TEXT_WEAK },
    );
    let size = Vec2::new(
        DOT + 9.0 + galley.size().x,
        (DOT + 8.0).max(galley.size().y + 8.0),
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    let center = egui::Pos2::new(rect.left() + DOT / 2.0, rect.center().y);
    if selected {
        painter.circle_filled(center, DOT / 2.0, accent_fill());
        painter.circle_filled(center, DOT / 2.0 - 3.4, Color32::WHITE);
    } else {
        painter.circle_stroke(
            center,
            DOT / 2.0 - 0.7,
            Stroke::new(1.4, Color32::from_rgb(0xC3, 0xD0, 0xDE)),
        );
    }
    let text_pos = egui::Pos2::new(
        rect.left() + DOT + 9.0,
        rect.center().y - galley.size().y / 2.0,
    );
    painter.galley(text_pos, galley, if selected { TEXT } else { TEXT_WEAK });
    hand(response)
}

/// 单选圆点的直径。
const DOT: f32 = 15.0;

/// 一条横向滑块。返回值是「玩家动了没有」，值在 `value` 里。
///
/// 自己画而不直接用 `egui::Slider`：那个控件会带一串数字和可拖动的标签，
/// 我们要的是「细轨 + 一个圆钮」，数字放到右边的输入框里去。
/// `value` 是 0.0..=1.0 的比例，由调用方换算成 MiB。
pub fn slider(ui: &mut Ui, value: &mut f32, enabled: bool) -> Response {
    let width = ui.available_width().max(60.0);
    let (rect, mut response) =
        ui.allocate_exact_size(Vec2::new(width, TRACK_H + 14.0), Sense::click_and_drag());
    if !enabled {
        response = response.on_disabled_hover_text("自动分配时不能拖动");
    }
    let track =
        egui::Rect::from_center_size(rect.center(), Vec2::new(rect.width() - KNOB, TRACK_H));
    let painter = ui.painter();
    let line = if enabled {
        Stroke::new(TRACK_H, Color32::from_rgb(0xDD, 0xE5, 0xEF))
    } else {
        Stroke::new(TRACK_H, Color32::from_rgb(0xEC, 0xF0, 0xF5))
    };
    painter.hline(track.x_range(), track.center().y, line);

    let ratio = value.clamp(0.0, 1.0);
    let fill_right = track.left() + track.width() * ratio;
    if enabled && ratio > 0.0 {
        painter.hline(
            track.left()..=fill_right,
            track.center().y,
            Stroke::new(TRACK_H, accent_fill()),
        );
    }
    let knob = egui::Pos2::new(fill_right, track.center().y);
    let knob_color = if enabled {
        accent_fill()
    } else {
        Color32::from_rgb(0xC8, 0xD3, 0xE0)
    };
    painter.circle_filled(knob, KNOB / 2.0, knob_color);

    if enabled && (response.dragged() || response.clicked())
        && let Some(pos) = response.interact_pointer_pos() {
        let next = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
        if (next - *value).abs() > f32::EPSILON {
            *value = next;
            response.mark_changed();
        }
    }
    if enabled {
        response = hand(response);
    }
    response
}

/// 滑块轨道厚度与圆钮直径。
const TRACK_H: f32 = 4.0;
const KNOB: f32 = 15.0;

/// 「名字 + 滑杆 + 靠右的读数」一整行。
///
/// 滑杆会一直撑到右边那列数字的左沿，读数则贴着卡片右沿右对齐——
/// 这样上下几行的轨道起点、终点都落在同一根竖线上，读数也是。
/// 读数由 `text` 现算，所以拖动的那一帧数字就跟着变。
pub fn slider_row(ui: &mut Ui, label: &str, ratio: &mut f32, text: impl Fn(f32) -> String) -> bool {
    // 名字和读数各占一列定宽，字是画进去的：普通 label 会按字宽收缩，
    // 于是「侧边栏」那行的轨道就比「顶部标题栏」那行多伸出去一截。
    const LABEL_W: f32 = 78.0;
    const PERCENT_W: f32 = 30.0;
    const GAP: f32 = 4.0;
    const ROW_H: f32 = 24.0;
    let font = FontId::new(12.0, egui::FontFamily::Proportional);
    let small = FontId::new(11.0, egui::FontFamily::Proportional);
    // 宽度按整行预算算死，别拿「当前还剩多少」去凑：横排里多伸出去一点，
    // 父级可用宽度就跟着变宽，下一行的轨道于是又长一截，几行会参差不齐。
    // 用整行开始时的可用宽度：横排里头它会一路缩水，下一行就短一截。
    let full_w = ui.available_width();
    let slider_w = (full_w - LABEL_W - PERCENT_W - GAP * 2.0).max(80.0);
    let mut changed = false;
    ui.horizontal(|ui| {
        // 横排默认会给每个控件之间塞一点间距；这里三块的宽度是算好的，
        // 再叠上间距就会超出可用宽度，把父级撑宽，下一行跟着变长。
        ui.spacing_mut().item_spacing.x = 0.0;
        let (name_rect, _) = ui.allocate_exact_size(Vec2::new(LABEL_W, ROW_H), Sense::hover());
        ui.painter().text(
            name_rect.left_center(),
            Align2::LEFT_CENTER,
            label,
            font,
            TEXT_WEAK,
        );
        ui.add_space(GAP);
        let drag = ui
            .allocate_ui_with_layout(
                Vec2::new(slider_w, ROW_H),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| slider(ui, ratio, true),
            )
            .inner;
        changed = drag.changed();
        ui.add_space(GAP);
        let (pct_rect, _) = ui.allocate_exact_size(Vec2::new(PERCENT_W, ROW_H), Sense::hover());
        ui.painter().text(
            pct_rect.right_center(),
            Align2::RIGHT_CENTER,
            text(*ratio),
            small,
            TEXT_WEAK,
        );
    });
    changed
}

pub fn chip(ui: &mut Ui, text: &str) {
    let font = FontId::new(11.5, egui::FontFamily::Proportional);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, TEXT_WEAK);
    let size = galley.size() + Vec2::new(16.0, 7.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(7),
        Color32::from_rgb(0xF1, 0xF5, 0xF9),
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(7),
        Stroke::new(1.0, Color32::from_rgb(0xE6, 0xEC, 0xF4)),
        StrokeKind::Inside,
    );
    let pos = rect.center() - galley.size() / 2.0;
    painter.galley(pos, galley, TEXT_WEAK);
}

/// 版本列表的一整行：整行可点，选中时浅绿底 + 左侧一道竖线。
///
/// 右边原来还印着一枚「Java 17」：那是启动时才用得上的信息，一列排下来
/// 每行都缀一个，眼睛反而先去读它。它的用处只有启动那一下，不必逐行读。
///
/// `loader` 是挂的加载器（`None` = 纯原版）。它同时决定行首那枚 30px 的
/// 版本图标 —— 一列排下来，哪几个是 Fabric、哪个是 Forge，看形状就分开了，
/// 不必逐行读名字。名字右边只留一小截加载器名做补充（图标认不出时的兜底）。
///
/// 每一行自己就是一张白卡片：圆角 + 细边，浮在浅灰蓝的页面底上。
/// 选中时描边换成主题绿、底色转成浅绿，跟目录列表里那张「当前」卡片同一套语言。
///
/// `reserve_right` 是右端留给浮动图标键的宽度：行里的字（尤其是那枚加载器
/// 标签）不许爬到图标底下。图标本身不在这里画 —— 它们得注册在这一行之后，
/// 命中时才压得住整行，否则点齿轮会连着把这一行也选中。
pub fn version_item(
    ui: &mut Ui,
    name: &str,
    loader: Option<aicraft_core::loader::LoaderKind>,
    selected: bool,
    reserve_right: f32,
) -> Response {
    // 30px 图标塞进 44px 的行里会显得顶格，行高提到 52 才有呼吸感。
    let height = 52.0;
    let (slot, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    // 行与行之间留一点缝，卡片才不会连成一片。
    let rect = slot.shrink2(Vec2::new(0.0, 1.5));

    let fill = if selected {
        accent_soft()
    } else if response.hovered() {
        CARD_HOVER
    } else {
        CARD
    };
    ui.painter().rect_filled(rect, CornerRadius::same(9), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(9),
        Stroke::new(1.0, if selected { accent_fill() } else { BORDER }),
        StrokeKind::Inside,
    );

    let text_color = if selected { accent_strong() } else { TEXT };
    let inner = rect.shrink2(Vec2::new(14.0, 0.0));
    // 行首的版本图标：按加载器选图，比名字先被看见。
    const ICON: f32 = 30.0;
    let icon_rect = Rect::from_center_size(
        Pos2::new(inner.left() + ICON / 2.0, inner.center().y),
        Vec2::splat(ICON),
    );
    loader_glyph(ui, icon_rect, loader);
    let name_left = icon_rect.right() + 12.0;
    let name_font = FontId::new(13.5, egui::FontFamily::Proportional);
    // 先量一下名字多宽，标签才知道该从哪儿起 —— 版本名长短差别很大
    // （`1.20.1` 和 `涟漪之篇·如涟漪之所见` 能差十倍），写死偏移会错位。
    let name_width = ui
        .painter()
        .layout_no_wrap(name.to_owned(), name_font.clone(), text_color)
        .size()
        .x;
    ui.painter().text(
        Pos2::new(name_left, inner.center().y),
        Align2::LEFT_CENTER,
        name,
        name_font,
        text_color,
    );

    {
        // 名字右边补一枚小标签：行首那个大图标说「它是谁」，这枚说
        // 「它叫什么」——图标认不准的时候，还是得有一行字能读。
        // 标签里不再放 10px 图标，一行两个同样的图标只会看着重复。
        let label = loader.map(|kind| kind.label()).unwrap_or("原版");
        let style = loader_chip(loader);
        let galley = ui.painter().layout_no_wrap(
            label.to_owned(),
            FontId::new(10.5, egui::FontFamily::Proportional),
            style.text,
        );
        let chip_size = galley.size() + Vec2::new(16.0, 3.0);
        let left = name_left + name_width + 8.0;
        // 名字太长时会把这一行撑满，那就干脆不画标签：
        // 宁可少一枚标签，也不要让文字顶到卡片边上。
        let right_limit = inner.right() - 14.0 - reserve_right;
        if left + chip_size.x <= right_limit {
            let chip_rect = Rect::from_min_size(
                Pos2::new(left, inner.center().y - chip_size.y / 2.0),
                chip_size,
            );
            let painter = ui.painter();
            painter.rect_filled(chip_rect, CornerRadius::same(5), style.fill);
            painter.rect_stroke(
                chip_rect,
                CornerRadius::same(5),
                Stroke::new(1.0, style.stroke),
                StrokeKind::Inside,
            );
            painter.galley(
                Pos2::new(
                    chip_rect.center().x - galley.size().x / 2.0,
                    chip_rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                style.text,
            );
        }
    }

    if selected {
        let bar = egui::Rect::from_min_size(
            Pos2::new(rect.left() + 3.0, rect.top() + 14.0),
            Vec2::new(3.0, height - 28.0),
        );
        ui.painter().rect_filled(bar, CornerRadius::same(2), accent_fill());
    }

    response
}

/// 主按钮：绿底白字。用来标出「这里的下一步就是它」。
/// 弹窗统一用的底板：白底、细描边、圆角 12、18×16 的内边距。
///
/// 内边距不能省：`egui::Modal` 的默认框几乎没有留白，文字会贴着边，
/// 看着像被裁掉了。向导和几个确认框共用这一份，免得各写一套又各不相同。
pub fn dialog_frame() -> Frame {
    Frame::NONE
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(12.0)
        .inner_margin(Margin::symmetric(18, 16))
}

/// 弹窗里的按钮高度：几个确认框的按钮要一样高，不然并排看着会歪。
pub const DIALOG_BUTTON_HEIGHT: f32 = 34.0;

/// 画一个「定尺寸 + 文字居中」的按钮。
///
/// 为什么要包一层布局：`Button::min_size` 只是把按钮**撑大**，按钮里的文字
/// 是按**外层 Ui 的对齐方式**摆的。左边那种默认左对齐的横排里，多出来的宽度
/// 全堆在文字右边 —— 看着就是「保存」两个字缩在左上角。这里先给按钮一块
/// 精确大小的地，再用「居中且撑满」的布局放进去，文字自然落在正中间。
fn fixed_button(ui: &mut Ui, button: egui::Button<'_>, enabled: bool, size: Vec2) -> Response {
    ui.allocate_ui_with_layout(
        size,
        egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
        |ui| ui.add_enabled(enabled, button),
    )
    .inner
}

/// 弹窗里的主按钮（确认类动作）。宽度由调用方给，好让并排的按钮等宽。
pub fn dialog_primary_button(ui: &mut Ui, text: &str, enabled: bool, min_width: f32) -> Response {
    let button = egui::Button::new(
        RichText::new(text)
            .size(13.5)
            .strong()
            .color(accent_ink()),
    )
    .fill(accent_fill())
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(10));
    fixed_button(
        ui,
        button,
        enabled,
        Vec2::new(min_width, DIALOG_BUTTON_HEIGHT),
    )
}

/// 弹窗里的次要按钮（取消 / 次操作）。
pub fn dialog_button(ui: &mut Ui, text: &str, enabled: bool, min_width: f32) -> Response {
    let button = egui::Button::new(RichText::new(text).size(13.5).color(TEXT))
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10));
    fixed_button(
        ui,
        button,
        enabled,
        Vec2::new(min_width, DIALOG_BUTTON_HEIGHT),
    )
}

/// 白底、悬停变绿的一枚按钮。
///
/// 底色得随悬停变，而 `egui::Button::fill` 是固定值（`fill` 一旦给了就覆盖
/// 样式表里那套 hover/active 配色），所以只能自己画：先 `allocate` 占位，
/// 再按 `response.hovered()` 挑底色，最后把文字居中画上去。
///
/// 悬停时底色是 `accent_fill()`、字转白——和主按钮一个观感，玩家一眼知道「点这里是往下走」。
pub fn hover_green_button(ui: &mut Ui, text: &str, enabled: bool, min_width: f32) -> Response {
    let height = DIALOG_BUTTON_HEIGHT;
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(min_width, height),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );

    let hot = enabled && response.hovered();
    let (fill, stroke, ink) = if !enabled {
        (CARD, Stroke::new(1.0, BORDER), TEXT_FAINT)
    } else if hot {
        (accent_fill(), Stroke::NONE, accent_ink())
    } else {
        (CARD, Stroke::new(1.0, BORDER), TEXT)
    };

    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(10), fill);
    if stroke != Stroke::NONE {
        painter.rect_stroke(rect, CornerRadius::same(10), stroke, StrokeKind::Inside);
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::new(13.5, egui::FontFamily::Proportional),
        ink,
    );

    if enabled {
        hand(response)
    } else {
        response
    }
}

/// 弹窗里的破坏性按钮（确认删除）：红字红边，底还是白的，不抢主按钮的色。
pub fn dialog_danger_button(ui: &mut Ui, text: &str, enabled: bool, min_width: f32) -> Response {
    let button = egui::Button::new(RichText::new(text).size(13.5).strong().color(DANGER))
        .fill(CARD)
        .stroke(Stroke::new(1.0, DANGER))
        .corner_radius(CornerRadius::same(10));
    fixed_button(
        ui,
        button,
        enabled,
        Vec2::new(min_width, DIALOG_BUTTON_HEIGHT),
    )
}

/// 一个确认框要说的话。
///
/// 五个确认框（删版本、删账号、删文件、恢复默认、补全缺失）原来各写一遍
/// `Modal::new(...).frame(dialog_frame())` + 标题 + 一行说明 + 取消/确认两颗键，
/// 除了文案与宽度之外一模一样。抄到第六个的时候就会开始走样 —— 有的按钮 92 宽、
/// 有的 108，有的标题后面忘了留那 6 像素。统一收到这里，调用方只说「问什么」。
pub struct Confirm<'a> {
    /// 弹窗自己的 Id。每处必须不同：同一帧里两个同名 Modal 会互相抢焦点。
    pub id: &'a str,
    /// 第一行，问句。
    pub title: &'a str,
    /// 问句下面加粗的那一行：版本名、账号名这类「你要动的是这一个」。
    /// 没有具体对象时给 `None`（比如「恢复默认设置」）。
    pub subject: Option<&'a str>,
    /// 说明文字，一行一条。颜色由 [`Confirm::danger`] 决定。
    pub lines: &'a [&'a str],
    /// 确认键是不是红的（不可逆的删除用红）。
    pub danger: bool,
    /// 正文要不要用警示色。删除类通常要，但「删的只是自己那一层、不碰存档」
    /// 这种半安全的场合该保持中性 —— 语气比按钮更值得细调。
    pub warn: bool,
    /// 确认键上的字。
    pub confirm: &'a str,
    /// 弹窗宽度。默认的几个都是 380，管理页那个文件删除窄一点（370）。
    pub width: f32,
}

/// 确认框里玩家的选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// 按了确认键。
    Confirmed,
    /// 按了取消、点了遮罩，或者按了 Esc。
    Dismissed,
    /// 还在等。
    Pending,
}

/// 画一个确认框，把「按了什么」交回去；关掉的状态由调用方自己清。
///
/// 这里只管画和收集按键，不碰任何状态：确认之后要删文件还是改设置，
/// 是调用方的事 —— 弹窗本身不该知道「版本」和「账号」的区别。
pub fn confirm_dialog(ctx: &egui::Context, busy: bool, ask: Confirm<'_>) -> Answer {
    let mut confirmed = false;
    let mut dismissed = false;

    let response = egui::Modal::new(egui::Id::unique(ask.id))
        .frame(dialog_frame())
        .show(ctx, |ui| {
            ui.set_width(ask.width);
            ui.label(RichText::new(ask.title).size(15.0).strong());
            if let Some(subject) = ask.subject {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(subject)
                        .size(12.5)
                        .color(TEXT)
                        .strong(),
                );
            }
            if !ask.lines.is_empty() {
                ui.add_space(6.0);
            }
            let ink = if ask.warn { WARN } else { TEXT_WEAK };
            for line in ask.lines {
                ui.label(RichText::new(*line).size(11.5).color(ink));
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if dialog_button(ui, "取消", !busy, 92.0).clicked() {
                    dismissed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let hit = if ask.danger {
                        dialog_danger_button(ui, ask.confirm, !busy, 92.0)
                    } else {
                        dialog_primary_button(ui, ask.confirm, !busy, 108.0)
                    };
                    if hit.clicked() {
                        confirmed = true;
                    }
                });
            });
        });

    if response.should_close() {
        dismissed = true;
    }
    if confirmed {
        Answer::Confirmed
    } else if dismissed {
        Answer::Dismissed
    } else {
        Answer::Pending
    }
}
pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let button = egui::Button::new(
        RichText::new(text)
            .size(14.0)
            .strong()
            .color(accent_ink()),
    )
    .fill(accent_fill())
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(10));
    fixed_button(ui, button, enabled, Vec2::new(132.0, 36.0))
}

/// 首页那颗唯一的按钮：大、绿、醒目，一页只有它一个动作。
/// 宽度由调用方给（右下角那块能有多宽就多宽），高度固定 52。
pub fn launch_button(ui: &mut Ui, enabled: bool, width: f32) -> Response {
    let button = egui::Button::new(
        RichText::new("启动游戏")
            .size(17.0)
            .strong()
            .color(accent_ink()),
    )
    .fill(accent_fill())
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(12));
    fixed_button(ui, button, enabled, Vec2::new(width, 52.0))
}

/// 启动弹窗右下角那颗小红按钮。
///
/// 它不再是首页那颗「启动游戏」的替身——位置挪进了卡片里，尺寸也跟着收小，
/// 免得在一张只讲进度的卡片上抢戏。红色还是保留：这是唯一能中断的出口。
pub fn cancel_launch_button(ui: &mut Ui, enabled: bool, text: &str, size: Vec2) -> Response {
    let button = egui::Button::new(
        RichText::new(text)
            .size(13.0)
            .strong()
            .color(Color32::WHITE),
    )
    .fill(DANGER)
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(8));
    fixed_button(ui, button, enabled, size)
}

/// 首页那颗按钮旁边的小按钮：一个方角、三条横线，点开/悬停时把版本列表叫出来。
///
/// 尺寸和 `launch_button` 一样高（52），并排放在一起才不显高低。
/// `active` 是「菜单正开着」，这时底色与线条都提亮一档，让人知道列表是从这儿出来的。
pub fn menu_button(ui: &mut Ui, active: bool) -> Response {
    let size = Vec2::new(52.0, 52.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let lit = active || response.hovered();
    let fill = if lit { accent_soft() } else { CARD };
    let stroke = if lit { accent_fill() } else { BORDER };
    ui.painter().rect(
        rect,
        CornerRadius::same(12),
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );

    let color = if lit { accent_strong() } else { TEXT_WEAK };
    let line = Stroke::new(1.6, color);
    let c = rect.center();
    let half_w = 8.0;
    for dy in [-5.0_f32, 0.0, 5.0] {
        ui.painter().line_segment(
            [
                Pos2::new(c.x - half_w, c.y + dy),
                Pos2::new(c.x + half_w, c.y + dy),
            ],
            line,
        );
    }

    hand(response)
}

/// 自绘控件收尾用：悬停时给手型光标。
///
/// egui 的 `Button` 会读 `Visuals::interact_cursor`，但我们自己 `allocate_exact_size`
/// 画出来的玩意儿（导航项、图标键、窗口键）不经过 `Button`，一个都不会变手型。
/// 每个都手写一遍 `on_hover_cursor` 容易漏，抽成这一句。
pub fn hand(response: Response) -> Response {
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// 输入框的右键菜单：剪切 / 复制 / 粘贴 / 全选。
///
/// 用法是「先放控件，再把响应和文本交给它」：
///
/// ```ignore
/// let mut resp = ui.add(egui::TextEdit::singleline(&mut state.query));
/// theme::input_menu(&mut resp, &mut state.query, false);
/// ```
///
/// 剪贴板走系统的（`Cargo.toml` 里给 eframe 开了 `clipboard`），菜单是自己画的，
/// 跟亮色主题一个样子，不弹系统那种灰扑扑的菜单。
///
/// 几个 egui 的坑在这里一次绕掉：
/// - **右键会把选区收掉**。`TextCursorState::pointer_interaction` 只看
///   `pointer.any_pressed()`，不区分左右键，于是右键一按，选中的一段就塌成光标。
///   这里在没按鼠标的帧把选区存一份，右键抬起时塞回去，菜单开着的时候那段字还是亮的。
/// - **复制、剪切不借 egui 的手**。`Event::Copy`/`ViewportCommand::RequestCopy`
///   要求那一帧 `has_focus(id)` 且选区非空，而点菜单按钮本身就会让输入框失焦
///   （`SurrenderFocusOn::Clicks`），再下一帧 `TextEdit` 又会把没焦点时的选区压成光标。
///   所以这里直接读选区、直接 `ctx.copy_text`，剪切再自己删掉那几个字。
/// - **粘贴只能等下一帧**。`ViewportCommand::RequestPaste` 要 eframe 下一帧去问
///   系统剪贴板，再当输入事件送回来；这里负责把焦点还给输入框。
/// - 密码框不给剪切/复制，和 egui 自己的 `copy_if_not_password` 保持一致。
pub fn input_menu(resp: &mut Response, text: &mut String, password: bool) {
    let ctx = resp.ctx.clone();
    let id = resp.id;
    // 选区暂存在 ctx 里，键挂在输入框自己的 Id 下面，不会串到别的框。
    let sel_key = id.with("input-menu-selection");

    if resp.secondary_clicked() {
        // 右键抬起：把按下前的那段选区还回去，菜单里的复制才有东西。
        if let Some(range) = ctx.data_mut(|d| d.remove_temp::<CCursorRange>(sel_key))
            && !range.is_empty()
            && let Some(mut state) = TextEditState::load(&ctx, id)
        {
            state.cursor.set_char_range(Some(range));
            state.store(&ctx, id);
        }
    } else if !ctx.input(|i| i.pointer.any_down()) && ctx.memory(|m| m.has_focus(id)) {
        // 只记「没按键、还拿着焦点」的帧：按住右键的那几帧选区已经塌了，
        // 点完菜单按钮失焦之后的那几帧也一样（`TextEdit` 会把选区压成光标），
        // 记下来都会把好端端的一段字覆盖掉。
        if let Some(state) = TextEditState::load(&ctx, id)
            && let Some(range) = state.cursor.char_range()
        {
            ctx.data_mut(|d| d.insert_temp(sel_key, range));
        }
    }

    let selection = input_menu_selection(&ctx, id, sel_key);
    let can_edit = selection.is_some() && !password;

    let cut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::X);
    let copy = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::C);
    let paste = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::V);
    let all = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::A);

    let mut edited = false;
    egui::Popup::context_menu(resp)
        .frame(input_menu_frame())
        .width(168.0)
        .show(|ui| {
            ui.set_min_width(152.0);
            ui.spacing_mut().item_spacing.y = 2.0;

            if input_menu_row(ui, "剪切", &ctx.format_shortcut(&cut), can_edit) {
                if let Some(range) = input_menu_selection(&ctx, id, sel_key) {
                    input_menu_cut(&ctx, id, text, range);
                    edited = true;
                }
                ui.close();
            }
            if input_menu_row(ui, "复制", &ctx.format_shortcut(&copy), can_edit) {
                if let Some(range) = input_menu_selection(&ctx, id, sel_key) {
                    ctx.copy_text(range.slice_str(text).to_owned());
                }
                ui.close();
            }
            if input_menu_row(ui, "粘贴", &ctx.format_shortcut(&paste), true) {
                input_menu_command(&ctx, id, egui::ViewportCommand::RequestPaste);
                ui.close();
            }
            ui.add_space(2.0);
            ui.separator();
            ui.add_space(2.0);
            if input_menu_row(ui, "全选", &ctx.format_shortcut(&all), !text.is_empty()) {
                if let Some(mut state) = TextEditState::load(&ctx, id) {
                    state
                        .cursor
                        .set_char_range(Some(CCursorRange::select_all_str(text)));
                    state.store(&ctx, id);
                }
                ctx.memory_mut(|m| m.request_focus(id));
                ui.close();
            }
        });

    if edited {
        resp.mark_changed();
    }
}

/// 现在这段字被选中了多少：优先读输入框自己的状态，读不到再用右键前存的那份。
///
/// 菜单开着的时候点按钮会让输入框失焦，下一帧 `TextEdit` 就把选区压成光标了，
/// 所以「有没有选中」这件事得在两处都看一眼。
fn input_menu_selection(ctx: &egui::Context, id: Id, sel_key: Id) -> Option<CCursorRange> {
    let live = TextEditState::load(ctx, id).and_then(|state| state.cursor.char_range());
    live.filter(|range| !range.is_empty())
        .or_else(|| ctx.data_mut(|d| d.get_temp::<CCursorRange>(sel_key)))
        .filter(|range| !range.is_empty())
}

/// 剪切：选中的那段进系统剪贴板，再从文本里删掉，光标留在原处。
///
/// 不走 egui 的 `Event::Cut`，因为它要求那一帧输入框还有焦点，而点菜单按钮
/// 已经让它失焦了（见 `input_menu` 的注释）。
fn input_menu_cut(ctx: &egui::Context, id: Id, text: &mut String, range: CCursorRange) {
    let chars = range.as_sorted_char_range();
    ctx.copy_text(range.slice_str(text).to_owned());
    let bytes = input_menu_bytes(text, &chars);
    text.replace_range(bytes, "");
    input_menu_caret(ctx, id, chars.start);
    ctx.memory_mut(|m| m.request_focus(id));
}

/// 字符下标换成字节下标：`String::replace_range` 只认字节，中文一个字三个字节。
fn input_menu_bytes(text: &str, chars: &core::ops::Range<CharIndex>) -> core::ops::Range<usize> {
    let at = |index: CharIndex| {
        text.char_indices()
            .nth(index.0)
            .map_or(text.len(), |(byte, _)| byte)
    };
    at(chars.start)..at(chars.end)
}

/// 剪切完把光标放到删掉的位置，免得下一帧还指着一个越界的下标。
fn input_menu_caret(ctx: &egui::Context, id: Id, at: CharIndex) {
    if let Some(mut state) = TextEditState::load(ctx, id) {
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(at))));
        state.store(ctx, id);
    }
}

/// 右键菜单的一行：整宽、24 高、右边挂快捷键。
fn input_menu_row(ui: &mut Ui, label: &str, shortcut: &str, enabled: bool) -> bool {
    let button = egui::Button::new(RichText::new(label).size(13.0))
        .shortcut_text(RichText::new(shortcut).size(11.0));
    ui.add_enabled_ui(enabled, |ui| {
        ui.add_sized([ui.available_width(), 24.0], button)
    })
    .inner
    .clicked()
}

/// 菜单里的动作都是「先把焦点还回去，再让 egui 下一帧去碰剪贴板」。
fn input_menu_command(ctx: &egui::Context, id: Id, command: egui::ViewportCommand) {
    ctx.memory_mut(|m| m.request_focus(id));
    ctx.send_viewport_cmd(command);
}

/// 右键菜单的框：跟首页那个版本菜单一个调子，只是小一号。
fn input_menu_frame() -> Frame {
    Frame::NONE
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(6, 6))
        .shadow(egui::epaint::Shadow {
            offset: [0, 2],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(14),
        })
}

/// 标题行里的「刷新」小图标：一段圆弧加个箭头。
///
/// 三个列表页（下载版本 / 目录列表 / 版本列表）的刷新动作都用它，免得各自画一套。
/// 只画图标不写字，省下标题行的横向空间；说明放在悬停提示里。
pub fn refresh_button(ui: &mut Ui, enabled: bool) -> Response {
    // 方框边长对齐标题行里 17pt 文字的行高（egui 约 20px）：
    // 比字高出一截的话，它会把整行顶高，图标看着也偏大。
    let side = 20.0_f32;
    let (rect, response) = ui.allocate_exact_size(
        Vec2::splat(side),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );

    let lit = enabled && response.hovered();
    let fill = if lit {
        accent_soft()
    } else {
        Color32::TRANSPARENT
    };
    let edge = if lit { accent_fill() } else { BORDER };
    ui.painter().rect(
        rect,
        CornerRadius::same(6),
        fill,
        Stroke::new(1.0, edge),
        StrokeKind::Inside,
    );

    let color = if !enabled {
        TEXT_FAINT
    } else if lit {
        accent_strong()
    } else {
        TEXT_WEAK
    };

    // 圆弧留一个缺口，箭头落在缺口那一端，看上去才是「转起来」的意思。
    let c = rect.center();
    let radius = side * 0.24;
    let start = 40.0_f32.to_radians();
    let sweep = 265.0_f32.to_radians();
    let steps = 26_usize;
    let points: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let a = start + sweep * (i as f32 / steps as f32);
            Pos2::new(c.x + radius * a.cos(), c.y + radius * a.sin())
        })
        .collect();
    ui.painter()
        .add(egui::Shape::line(points, Stroke::new(1.5, color)));

    // 顺时针方向的切向量是 (-sin, cos)，箭头沿它往前伸一点。
    let end = start + sweep;
    let (sin, cos) = end.sin_cos();
    let tip_dir = Vec2::new(-sin, cos);
    let radial = Vec2::new(cos, sin);
    let base = Pos2::new(c.x + radius * cos, c.y + radius * sin);
    let tri = vec![
        base + tip_dir * 2.7,
        base - tip_dir * 0.9 + radial * 2.3,
        base - tip_dir * 0.9 - radial * 2.3,
    ];
    ui.painter()
        .add(egui::Shape::convex_polygon(tri, color, Stroke::NONE));

    if enabled {
        hand(response)
    } else {
        response
    }
}

/// 标题行里的「恢复默认」小图标：一个往回拐的箭头。
///
/// 和 [`refresh_button`] 同一套尺寸与配色，放在一起才是一组而不是两个按钮。
/// 区别在箭头方向：刷新是顺时针转回来，恢复默认是**逆时针**倒退回去 ——
/// 图标自己就把「重来一遍」和「退回去」说清了。
pub fn reset_button(ui: &mut Ui, enabled: bool) -> Response {
    let side = 20.0_f32;
    let (rect, response) = ui.allocate_exact_size(
        Vec2::splat(side),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );

    let lit = enabled && response.hovered();
    let fill = if lit {
        accent_soft()
    } else {
        Color32::TRANSPARENT
    };
    let edge = if lit { accent_fill() } else { BORDER };
    ui.painter().rect(
        rect,
        CornerRadius::same(6),
        fill,
        Stroke::new(1.0, edge),
        StrokeKind::Inside,
    );

    let color = if !enabled {
        TEXT_FAINT
    } else if lit {
        accent_strong()
    } else {
        TEXT_WEAK
    };

    // 圆弧从缺口那一端逆时针绕回来，箭头指回起点。
    let c = rect.center();
    let radius = side * 0.24;
    let start = 40.0_f32.to_radians();
    let sweep = 265.0_f32.to_radians();
    let steps = 26_usize;
    // 负号 = 逆时针，和刷新图标方向相反。
    let points: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let a = start - sweep * (i as f32 / steps as f32);
            Pos2::new(c.x + radius * a.cos(), c.y + radius * a.sin())
        })
        .collect();
    ui.painter()
        .add(egui::Shape::line(points, Stroke::new(1.5, color)));

    let end = start - sweep;
    let (sin, cos) = end.sin_cos();
    // 逆时针方向的切向量是 (sin, -cos)。
    let tip_dir = Vec2::new(sin, -cos);
    let radial = Vec2::new(cos, sin);
    let base = Pos2::new(c.x + radius * cos, c.y + radius * sin);
    let tri = vec![
        base + tip_dir * 2.7,
        base - tip_dir * 0.9 + radial * 2.3,
        base - tip_dir * 0.9 - radial * 2.3,
    ];
    ui.painter()
        .add(egui::Shape::convex_polygon(tri, color, Stroke::NONE));

    if enabled {
        hand(response)
    } else {
        response
    }
}

/// 展开 / 收起用的小箭头。
///
/// 不用「▾ ▴」这类字符：中文字体里经常没有这两个字形，屏幕上会变成一个
/// 空方框。两条线自己画出来，任何机器都一样。
pub fn chevron_button(ui: &mut Ui, open: bool, enabled: bool) -> Response {
    let side = 24.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let response = if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    };

    if enabled && response.hovered() {
        ui.painter().rect_filled(rect, 6.0, accent_soft());
    }

    let color = if !enabled {
        TEXT_FAINT
    } else if response.hovered() {
        accent_strong()
    } else {
        TEXT_WEAK
    };
    let center = rect.center();
    let half = 3.8;
    let points = if open {
        vec![
            egui::pos2(center.x - half, center.y + half * 0.6),
            egui::pos2(center.x, center.y - half * 0.6),
            egui::pos2(center.x + half, center.y + half * 0.6),
        ]
    } else {
        vec![
            egui::pos2(center.x - half, center.y - half * 0.6),
            egui::pos2(center.x, center.y + half * 0.6),
            egui::pos2(center.x + half, center.y - half * 0.6),
        ]
    };
    ui.painter()
        .add(egui::Shape::line(points, Stroke::new(1.6, color)));
    if enabled {
        hand(response)
    } else {
        response
    }
}

/// 管理页左上角那颗「返回」：一颗左指的小箭头，外面一个圆角框。
///
/// 不写成「‹ 返回」四个字：那一栏内宽只有 154，整串字会一路顶到中轴，
/// 跟居中的标题撞在一起。意思交给悬停提示补。
///
/// 箭头自己画线而不是打「‹」这个字符 —— 中文字体里它的字形偏小偏细，
/// 放进框里像随手点了一下；两条线能画到正好撑满。
pub fn back_button(ui: &mut Ui) -> Response {
    const W: f32 = 30.0;
    const H: f32 = 24.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(W, H), Sense::click());
    let hovered = response.hovered();

    // 平时白底细边，悬停换成淡绿底 + 深绿边：跟这一栏里别的可点项一个手感。
    let (fill, stroke, ink) = if hovered {
        (accent_soft(), Stroke::new(1.0, accent_strong()), accent_strong())
    } else {
        (card_fill(), Stroke::new(1.0, BORDER), TEXT_WEAK)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(7),
        fill,
        stroke,
        StrokeKind::Inside,
    );

    // 「<」：上下一撇各收在中心左侧，尖端朝左。
    let c = rect.center();
    let (reach, half) = (2.7, 4.2);
    let stroke = Stroke::new(1.7, ink);
    ui.painter().line_segment(
        [Pos2::new(c.x + reach, c.y - half), Pos2::new(c.x - reach, c.y)],
        stroke,
    );
    ui.painter().line_segment(
        [Pos2::new(c.x - reach, c.y), Pos2::new(c.x + reach, c.y + half)],
        stroke,
    );

    hand(response)
}

/// 一条会跟着展开进度连续翻面的小箭头。
///
/// [`chevron_button`] 只管「开着 / 收着」两态；折叠动画里箭头得一路转过去，
/// 走到一半卡住再跳一下最扎眼。`openness` 0 朝下、1 朝上。
pub fn chevron_glyph(painter: &Painter, center: Pos2, openness: f32, color: Color32) {
    let half = 3.6;
    // reach 为正时尖端压在中心下面（箭头朝下），转成负的就翻上去了。
    let reach = half * 0.55 * (1.0 - 2.0 * openness.clamp(0.0, 1.0));
    let stroke = Stroke::new(1.4, color);
    let (tip, left, right) = (
        Pos2::new(center.x, center.y + reach),
        Pos2::new(center.x - half, center.y - reach),
        Pos2::new(center.x + half, center.y - reach),
    );
    painter.line_segment([left, tip], stroke);
    painter.line_segment([tip, right], stroke);
}

/// 首页版本标题旁边那颗齿轮：点开就是这一份实例的管理页。
///
/// 跟标题栏的窗口按钮同一套手感：平时只是一枚灰图标，悬停才浮出一块淡底。
/// 图形直接复用侧边栏那枚「设置」——同一件事在哪儿都该长同一个样子。
pub fn gear_button(ui: &mut Ui, enabled: bool) -> Response {
    // 标题是 26 号字，字身本身就有二十来像素高。按钮只留 26 见方时，
    // 齿轮缩在里头只有 17 像素，跟旁边的字一比就显得怯生生的。
    const SIDE: f32 = 34.0;
    const GLYPH: f32 = 24.0;
    // 灰着的时候连点击都不该收：`Sense::click()` 照样会认下这一下，
    // 调用方一个疏忽就跳进了一个「没有版本可管」的管理页。
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(SIDE), sense);
    let response = if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    };

    if enabled && response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(9), accent_soft());
    }

    let color = if !enabled {
        TEXT_FAINT
    } else if response.hovered() {
        accent_strong()
    } else {
        TEXT_WEAK
    };
    // 字号 26 的标题旁边，齿轮得比侧边栏那枚再大一点点才压得住；
    // 上抬 1px 是因为文字的视觉中心在字身中线上方一点，正中对齐看着偏低。
    nav_glyph(
        ui.painter(),
        rect.center() - Vec2::new(0.0, 1.0),
        GLYPH,
        NavGlyph::Settings,
        color,
    );

    if enabled { hand(response) } else { response }
}

/// 版本行末尾那两枚图标键的边长（逻辑像素）。
///
/// 行高 52、行内文字 13.5：26 见方的键既点得着，又不会把卡片撑得不像一行字。
pub const ROW_ICON_SIDE: f32 = 26.0;

/// 两枚图标键之间的缝。
pub const ROW_ICON_GAP: f32 = 2.0;

/// 一行右端一共要让出多宽（含贴边的那几像素内边距）。
///
/// 交给 [`version_item`] 的 `reserve_right` 用：行里的文字不许压到图标上面。
pub const ROW_ICONS_RESERVE: f32 = ROW_ICON_SIDE * 2.0 + ROW_ICON_GAP + 14.0;

/// 一行末尾那枚浮动图标键：齿轮（管理）与垃圾桶（删除）。
///
/// 行本身整块可点，两枚图标得**在这一行之后**才注册 —— egui 的距离相等时
/// 取最后登记的那个（也就是压在最上面的），于是指针压在图标上点到的就是
/// 图标，压在行的别处仍然选中整行。反过来先画图标再画行，行会把它们盖住。
///
/// 不 `allocate`：这是给已经画完的行补两枚图标，光标一动，下面那一行就会
/// 凭空多出一截空白。位置由调用方按行的 `rect` 算好了传进来。
pub fn row_icon_button(
    ui: &mut Ui,
    rect: Rect,
    id_salt: impl Hash + std::fmt::Debug,
    glyph: NavGlyph,
    enabled: bool,
    danger: bool,
) -> Response {
    // 灰着的时候连点击都不收：一个疏忽就会删掉一份实例。
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let response = ui.interact(rect, ui.scope_id().with(id_salt), sense);

    let lit = enabled && response.hovered();
    if lit {
        let tint = if danger { DANGER_TINT } else { accent_soft() };
        ui.painter().rect_filled(rect, CornerRadius::same(7), tint);
    }
    let color = if !enabled {
        TEXT_FAINT
    } else if lit && danger {
        DANGER
    } else if lit {
        accent_strong()
    } else if danger {
        // 平时也给垃圾桶一点红：这一列里只有它不可逆，得先认出来。
        Color32::from_rgb(0xB0, 0x5A, 0x64)
    } else {
        TEXT_WEAK
    };
    // 图标占方键的七成：再大会顶到键的圆角上去，看着像被裁了一圈；
    // 再小，齿轮的六边形就糊成一只圆环，认不出是齿轮。
    let glyph_size = rect.width().min(rect.height()) * 0.7;
    nav_glyph(ui.painter(), rect.center(), glyph_size, glyph, color);

    if enabled { hand(response) } else { response }
}

/// 下拉框展开 / 收起各用多久（秒）。
const COMBO_ANIM_SECS: f32 = 0.17;

/// 一个带展开动画的下拉框，替掉 `egui::ComboBox`。
///
/// egui 自己的 `ComboBox` 走 `Popup::menu` → `Area`，开关是「这一帧画、下一帧不画」，
/// 中间没有可以插手的钩子；`Area` 的 `fade_in` 也只淡入不位移。所以这里把按钮和
/// 弹出层都自己画一遍，只为了在「画 / 不画」之间塞进一段动画：
///
/// - 位置：紧贴按钮下沿（不留缝），`constrain` 保证不越出窗口；
/// - 位移：起手时整块缩在按钮下沿后面，再沿直线拉到位（`Pull`）；
/// - 遮挡：被按钮挡着的那截靠裁剪切掉，所以是「从框里拉出来」而不是悬空淡入；
/// - 透明度：跟着同一条曲线走，拉出来的过程不会先闪一下。
///
/// 收起时反过来播一遍：`animate_bool_with_time_and_easing` 在 target 为 false 时会
/// 自动反向缓动，所以只要每帧都调一次、并在 `t` 还没归零时继续画，收起动画就能跑完。
///
/// 打开 / 关闭的判定自己管（存在 `ctx` 的 temp 数据里），因为没走 `Popup`：
/// 点按钮切换，点别处或按 Esc 收起，点菜单里任意一处也收起（等价 `CloseOnClick`）。
///
/// `menu_contents` 和 `ComboBox::show_ui` 的闭包一样，往里面摆 `combo_value` 即可
/// （要自己判点击就用 `combo_item`）。文字默认靠左；要居中用 `combo_centered`。
pub fn combo(
    ui: &mut Ui,
    id_salt: &str,
    selected_text: impl Into<egui::WidgetText>,
    width: f32,
    menu_contents: impl FnOnce(&mut Ui),
) -> Response {
    combo_impl(ui, id_salt, selected_text, width, false, menu_contents)
}

/// 和 `combo` 一样，只是按钮上的字和菜单里的每一项都居中。
///
/// 资源下载页那一排筛选框宽度一样、字长短不一，靠左排会参差不齐，
/// 居中之后每颗按钮的字都落在同一条竖线上。
pub fn combo_centered(
    ui: &mut Ui,
    id_salt: &str,
    selected_text: impl Into<egui::WidgetText>,
    width: f32,
    menu_contents: impl FnOnce(&mut Ui),
) -> Response {
    combo_impl(ui, id_salt, selected_text, width, true, menu_contents)
}

/// 浮层上沿的圆角：展开时削平接缝那两角，收起时原样还回去。
///
/// 跟着按钮那一帧的状态走，别用「点击之后」的 `open` —— 那样点下去那半秒里
/// 按钮下沿还直着、浮层上沿已经圆回来，接缝处会错开一拍。
fn popup_corner_radius(open: bool, radius: CornerRadius) -> CornerRadius {
    if open {
        CornerRadius {
            nw: 0,
            ne: 0,
            ..radius
        }
    } else {
        radius
    }
}

fn combo_impl(
    ui: &mut Ui,
    id_salt: &str,
    selected_text: impl Into<egui::WidgetText>,
    width: f32,
    centered: bool,
    menu_contents: impl FnOnce(&mut Ui),
) -> Response {
    let ctx = ui.ctx().clone();
    let button_id = ui.make_persistent_id(id_salt);
    let open_id = button_id.with("combo-open");
    let area_id = button_id.with("combo-popup");
    let anim_id = button_id.with("combo-anim");

    let mut open = ctx.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    // 收起要跑小半秒，这几帧里浮层还挂在按钮下面、上沿被裁剪线切成一条直边。
    // 所以「接缝要不要削平」不能只看 open：上一帧进度还大于零，就说明浮层仍在
    // 屏幕上，按钮的下沿也得继续直着 —— 先圆回去的话，圆角按钮下面会横着一条
    // 方角白边，还从圆角两侧露出两枚小三角，看着像两张纸错着叠在一起。
    let prev_t = ctx
        .data(|d| d.get_temp::<f32>(anim_id.with("t")))
        .unwrap_or(0.0);
    let seam_square = open || prev_t > 0.004;

    let response = combo_button(ui, selected_text.into(), width, open, seam_square, centered);

    if response.clicked() {
        // 点按钮永远是「开 / 关」。
        open = !open;
    } else if open && ui.input(|i| i.pointer.any_click() || i.key_pressed(egui::Key::Escape)) {
        // 开着的时候任何一次点击都收：点菜单项是「选完了」，点别处是
        // 「算了」。egui 的 `CloseOnClick` 就是这个语义，连点菜单里也收。
        open = false;
    }

    ctx.data_mut(|d| d.insert_temp(open_id, open));

    // 每帧都要推进一次，哪怕已经收起：最后那几帧全靠它把弹出层画完。
    // 紧贴按钮下沿：留缝的话浮层像悬在半空，从框里拉出来的感觉就断了。
    let pos = response.rect.left_bottom();
    let layer = SpringLayer::new(area_id, anim_id, open, pos)
        .pivot(Align2::LEFT_TOP)
        .width(width)
        // 从按钮下沿那条线后面拉出来，而不是悬在缝里淡入。
        .pull_from(response.rect.bottom(), true);
    spring_layer(&ctx, layer, |ui| {
        egui::containers::menu::menu_style(ui.style_mut());
        // 上沿两个角削平，和按钮的下沿对齐成一条直线：两块拼在一起才像同一个
        // 控件展开，各自留圆角就露出「两个东西」。收起时也照样削着 —— 浮层还在
        // 往上收，一圆回来接缝就断，两侧还会翘出两枚小三角。
        let corner = popup_corner_radius(
            seam_square,
            ui.style().visuals.menu_corner_radius,
        );
        egui::Frame::popup(ui.style())
            .corner_radius(corner)
            .show(ui, |ui| {
                // 弹出层至少和按钮一样宽。左右各扣掉菜单边距和一道描边 ——
                // Frame 的可见宽度是「内容 + 内边距 + 描边」，只扣边距的话
                // 右边会多伸出两像素，接缝处那条竖线就对不齐了。
                let margin = ui.spacing().menu_margin.sum().x + 2.0;
                ui.set_min_width((width - margin).max(40.0));
                egui::ScrollArea::vertical()
                    .max_height(ui.spacing().combo_height)
                    .show(ui, |ui| {
                        // 按钮很窄时菜单也会很窄，开着换行会把标签拆得七零八落。
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                        menu_contents(ui);
                    });
            });
    });

    response
}

/// 浮层滑出来的距离里，除了自身高度之外再留的余量。
///
/// 浮层紧贴按钮下沿，光靠自身高度刚好差一点点才全藏住；多留这几个像素
/// 是给阴影和圆角留的，起手时连描边都在按钮后面。
const PULL_MARGIN: f32 = 8.0;

/// 「从框里拉出来」：整块先藏在按钮那条边后面，再滑到位。
///
/// 藏起来的那截靠**裁剪**而不是透明度，所以看起来是被按钮挡着、一点点拉出来的，
/// 而不是在半空里凭空淡出来 —— 这是这段动画和「缩放淡入」最要紧的区别。
#[derive(Clone, Copy, Debug)]
pub struct Pull {
    /// 挡住浮层的那条横线，也就是按钮的边。
    edge: f32,
    /// 浮层在这条线下面（下拉框）还是上面（往上弹的菜单）。
    down: bool,
}

impl Pull {
    fn new(edge: f32, down: bool) -> Self {
        Self { edge, down }
    }

    /// `t = 0` 时整块还藏在按钮后面，`t = 1` 正好就位。
    ///
    /// 滑多远得等浮层画完、量出高度才知道，所以 `height` 是这时候传进来的；
    /// 加上 `PULL_MARGIN` 之后，起手时它连圆角和阴影都在线里侧。
    fn offset(&self, t: f32, height: f32) -> Vec2 {
        let hidden = (height + PULL_MARGIN) * (1.0 - t);
        Vec2::new(0.0, if self.down { -hidden } else { hidden })
    }

    /// 只留这条线外侧的部分：线里侧的那截算「还在按钮里」。
    ///
    /// 浮层被窗口挤得离按钮远了（贴着窗口底边时 egui 会把它往上推），那条线就
    /// 退回浮层自己靠近按钮的那条边 —— 否则会被切掉半截。
    fn clip(&self, bounds: Rect) -> Rect {
        if self.down {
            let line = self.edge.min(bounds.top());
            Rect::from_min_max(Pos2::new(bounds.left(), line), bounds.max)
        } else {
            let line = self.edge.max(bounds.bottom());
            Rect::from_min_max(bounds.min, Pos2::new(bounds.right(), line))
        }
    }
}

/// 一层带弹性进出动画的浮层要摆在哪儿、什么时候画。
///
/// 参数太多，攒成一个结构体，调用处才看得懂每个数是干什么的。
pub struct SpringLayer {
    /// 浮层自己的 `Id`，也决定它的层级。
    pub area_id: Id,
    /// 动画进度存在这个 `Id` 下面。
    pub anim_id: Id,
    /// 这一帧想不想让它开着。
    pub open: bool,
    /// 浮层贴着哪个点长出来（同时是缩放的圆心）。
    pub pos: Pos2,
    /// 浮层的哪个角对准 `pos`。
    pub pivot: Align2,
    /// 想要的最小宽度；`None` 就让内容自己撑。
    pub width: Option<f32>,
    /// 从哪条线后面拉出来。
    ///
    /// `None` 就不裁剪，改用缩放 + 淡入：整块始终完整，四角都在。
    pub pull: Option<Pull>,
    /// 缩放淡入时额外往「长出来的方向」偏多少像素。
    pub drift: f32,
}

impl SpringLayer {
    pub fn new(area_id: Id, anim_id: Id, open: bool, pos: Pos2) -> Self {
        Self {
            area_id,
            anim_id,
            open,
            pos,
            pivot: Align2::LEFT_TOP,
            width: None,
            pull: None,
            drift: 0.0,
        }
    }

    pub fn pivot(mut self, pivot: Align2) -> Self {
        self.pivot = pivot;
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// 让浮层从 `edge` 这条线后面拉出来。
    ///
    /// `edge` 是按钮的那条边（下拉框传下边，往上弹的菜单传上边），`down` 说浮层
    /// 往哪边长。滑多远由浮层自己的高度算，所以不用在这里给数。
    pub fn pull_from(mut self, edge: f32, down: bool) -> Self {
        self.pull = Some(Pull::new(edge, down));
        self
    }

    /// 不从按钮后面拉，改成缩放 + 淡入，并额外往出长的那边偏 `drift` 像素。
    ///
    /// 裁剪式「拉出来」只适合按钮和浮层一样宽的场合（下拉框）：裁剪线横在浮层
    /// 底下，按钮会替它把那截直边挡住。按钮比浮层窄的时候没人挡，浮层两条底边
    /// 就被齐刷刷切平，看着像卡片被削掉一截 —— 首页那枚三条杠按钮就是这么露馅的。
    /// 这种地方用这个：四角始终完整，靠缩放交代「从按钮那个角长出来」。
    pub fn drift(mut self, drift: f32) -> Self {
        self.drift = drift;
        self
    }
}

/// 缩放淡入的变换：`t = 0` 时缩到 94% 并往 `origin` 那边偏，`t = 1` 归位。
fn spring_transform(t: f32, origin: Pos2, drift: f32) -> egui::emath::TSTransform {
    let scale = 0.94 + 0.06 * t;
    let mut translation = origin.to_vec2() * (1.0 - scale);
    translation.y += drift * (1.0 - t);
    egui::emath::TSTransform::new(translation, scale)
}

/// 画一层带弹性进出动画的浮层，返回它的 `InnerResponse`（拿得到矩形）。
///
/// 收起时 `open` 是 false，但动画还没播完，所以还要继续画几帧；真播完了返回
/// `None`。开 / 关的判定由调用方负责，这里只管画。
pub fn spring_layer<R>(
    ctx: &egui::Context,
    layer: SpringLayer,
    content: impl FnOnce(&mut Ui) -> R,
) -> Option<egui::InnerResponse<R>> {
    let SpringLayer {
        area_id,
        anim_id,
        open,
        pos,
        pivot,
        width,
        pull,
        drift,
    } = layer;

    // 「拉出来」用两头慢、中间快的曲线：起手像被拽了一下才动，收尾自己滑进去。
    // `back_out` 前段冲得太狠（两成时间就走完九成距离），看着像硬切，不能用来算位移。
    let easing: fn(f32) -> f32 = if pull.is_some() {
        egui::emath::easing::quadratic_in_out
    } else {
        egui::emath::easing::back_out
    };
    let t = ctx.animate_bool_with_time_and_easing(anim_id, open, COMBO_ANIM_SECS, easing);
    let alpha = t.clamp(0.0, 1.0);
    // 把这一帧的进度记到 ctx 上：调用方下一帧要靠它判断「浮层还挂在屏幕上没有」，
    // 好决定接缝两侧什么时候才能各自圆回去。取不到就是还没动过，按 0 算。
    ctx.data_mut(|d| d.insert_temp(anim_id.with("t"), alpha));

    if !open && alpha <= 0.004 {
        return None;
    }

    let mut area = egui::Area::new(area_id)
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .pivot(pivot)
        .constrain(true)
        // 淡入由下面那条曲线负责，不要 egui 自带的那一份，否则两层叠起来。
        .fade_in(false)
        // 收起动画那几帧不吃点击：那时候用户已经点了别处，
        // 挡在上面会把这一下吞掉。
        .interactable(open);
    if let Some(width) = width {
        area = area.default_width(width);
    }

    Some(area.show(ctx, |ui| {
        // 拉出来这段不靠透明度：整块本来就被按钮挡着，再淡入就成了「凭空显形」，
        // 和「从框里滑出来」正好相反。不裁剪那种才继续用缩放 + 淡入。
        let transform = if pull.is_none() {
            ui.multiply_opacity(alpha);
            Some(spring_transform(t, pos, drift))
        } else {
            None
        };
        let layer_id = ui.layer_id();
        let start = ctx.graphics(|g| {
            g.get(layer_id)
                .map_or(ShapeIdx(0), |list| list.next_idx())
        });
        // 只取内容的结果；外层 InnerResponse 的矩形是 Area 的，调用方要的是内容那个。
        let inner = match transform {
            Some(transform) => ui.with_visual_transform(transform, content).inner,
            None => content(ui),
        };
        if let Some(pull) = pull {
            let bounds = ui.min_rect();
            let clip = pull.clip(bounds);
            let offset = pull.offset(t.clamp(0.0, 1.0), bounds.height());
            if offset != Vec2::ZERO {
                // 画完再统一挪：裁剪线得钉在按钮边上不动，只有图形往外走，看着
                // 才像从按钮里被拉出来。所以不能用 with_visual_transform ——
                // 它连裁剪框一起搬，挪完就露不出这条边了。
                //
                // 但每条形状自己的裁剪框必须跟着挪：滚动区留在原位的话，被滚出
                // 列表那一截字会被挪回框里、露在白底外面（只剩字没有底）。
                // 顺序是「先跟着挪，再和钉死的那条线取交集」。
                ctx.graphics_mut(|g| {
                    let list = g.entry(layer_id);
                    let end = list.next_idx();
                    for i in start.0..end.0 {
                        list.mutate_shape(ShapeIdx(i), |shape| {
                            shape.shape.translate(offset);
                            shape.clip_rect = shape.clip_rect.translate(offset).intersect(clip);
                        });
                    }
                });
            }
        }
        inner
    }))
}

/// 下拉菜单里的一行：整行铺满、字居中，选中时垫一层强调色。
///
/// 和 `ui.selectable_label` 的区别只在「整行 + 居中」——下拉框的宽度是固定的，
/// 让每一项都撑满整行，鼠标随便点哪儿都算点中，看着也整齐。
///
/// 只报「点没点中」，不报「值变没变」—— 值的事交给 `combo_value`。
pub fn combo_item(ui: &mut Ui, selected: bool, text: &str) -> Response {
    let font = TextStyle::Button.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER);
    let height = galley.size().y + ui.spacing().button_padding.y * 2.0;
    let width = ui.available_width().max(galley.size().x);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());

    let visuals = ui.style().interact_selectable(&response, selected);
    if selected || response.hovered() {
        ui.painter()
            .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
    }
    let pos = Pos2::new(
        rect.center().x - galley.size().x * 0.5,
        rect.center().y - galley.size().y * 0.5,
    );
    ui.painter().galley(pos, galley, visuals.text_color());

    response
}

/// 下拉菜单里的一个可选值：选中就写回去，并像 `selectable_value` 一样
/// 在值真的变了时把 `changed` 打开。
pub fn combo_value<T: PartialEq + Clone>(ui: &mut Ui, current: &mut T, value: T, text: &str) -> Response {
    let selected = *current == value;
    let mut response = combo_item(ui, selected, text);
    if response.clicked() && !selected {
        *current = value;
        response.mark_changed();
    }
    response
}

/// `combo` 的那颗按钮：白底细边，右边一枚小三角，样子和 `ComboBox` 一模一样。
///
/// `centered` 时字落在按钮正中；否则照旧从左边距起笔。
///
/// `seam_square` 说下沿要不要削平：不光是展开时，收起动画没走完之前也得削着。
/// 浮层还在下面、按钮下沿已经圆回去的话，接缝两侧会露出两枚小三角。
fn combo_button(
    ui: &mut Ui,
    text: egui::WidgetText,
    width: f32,
    open: bool,
    seam_square: bool,
    centered: bool,
) -> Response {
    let pad = ui.spacing().button_padding;
    let arrow_w = ui.spacing().icon_width;
    let inner_w = (width - pad.x * 2.0 - arrow_w).max(24.0);

    // 先按不换行量一遍：文字比给定宽度短，按钮就跟着文字长；
    // 长了才截断，行高才不会因为换行而变。
    let natural = text.clone().into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let galley = if natural.size().x <= inner_w {
        natural
    } else {
        text.into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            inner_w,
            egui::TextStyle::Button,
        )
    };

    let size = Vec2::new(
        (pad.x * 2.0 + galley.size().x + arrow_w).max(width),
        (galley.size().y + pad.y * 2.0).max(ui.spacing().interact_size.y),
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let visuals = if open {
        ui.style().visuals.widgets.open
    } else {
        *ui.style().interact(&response)
    };

    // 接缝没散之前把下沿两个角削平：浮层就贴在这条边上，留圆角会在接缝处
    // 露出两道月牙形的缺口，看着像「浮层掉在按钮下面」而不是「从框里长出来」。
    let corner = if seam_square {
        CornerRadius {
            sw: 0,
            se: 0,
            ..visuals.corner_radius
        }
    } else {
        visuals.corner_radius
    };

    let painter = ui.painter();
    painter.rect(
        rect,
        corner,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        StrokeKind::Inside,
    );

    let text_left = if centered {
        // 居中的是**文字区**（右边那截留给三角），不是整颗按钮：拿整颗按钮
        // 的中线去对，字长的时候会一头撞上三角。宽了就退回左边距起笔再截断，
        // 不然截断后剩下的那截会被裁掉脑袋。
        (rect.left() + pad.x + (inner_w - galley.size().x) * 0.5).max(rect.left() + pad.x)
    } else {
        rect.left() + pad.x
    };
    let text_pos = Pos2::new(text_left, rect.center().y - galley.size().y * 0.5);
    let text_clip = Rect::from_min_max(
        Pos2::new(text_left - 1.0, rect.top()),
        Pos2::new(text_left + inner_w + 1.0, rect.bottom()),
    );
    painter
        .with_clip_rect(text_clip)
        .galley(text_pos, galley, visuals.fg_stroke.color);

    // 小三角照抄 egui 的 `paint_default_icon`：宽 0.7、高 0.45 的居中三角。
    let arrow_rect = Rect::from_center_size(
        Pos2::new(rect.right() - pad.x - arrow_w * 0.5, rect.center().y),
        Vec2::splat(arrow_w),
    );
    let triangle = Rect::from_center_size(
        arrow_rect.center(),
        Vec2::new(arrow_rect.width() * 0.7, arrow_rect.height() * 0.45),
    );
    painter.add(Shape::rotated_triangle(
        triangle,
        0.0,
        visuals.fg_stroke.color,
    ));

    response
}

/// 账户头像：有皮肤就画那张脸，没有就画个首字母占位。
///
/// 离线名字背后没有皮肤服务器可查，所以离线账户永远走首字母那条路 ——
/// 这不是「没加载出来」的兜底，而是它本来就没有皮肤。
pub fn draw_avatar(ui: &mut Ui, rect: egui::Rect, name: &str, skin: Option<&egui::TextureHandle>) {
    let painter = ui.painter();
    let round = if rect.width() > 8.0 {
        rect.width() * crate::AVATAR_ROUNDING
    } else {
        rect.width() * 0.5
    };
    let corner = CornerRadius::same(round.round().max(1.0) as u8);
    // 一圈极淡的描边：皮肤底色常常和卡片一样白，没有这条线头像会糊在背景里
    let ring = Stroke::new(1.0, Color32::from_black_alpha(38));
    match skin {
        Some(texture) => {
            // 3D 头像的纹理比脸大一圈（脸 32 + 两侧留白）。按边长放大画，
            // 脸就还是 rect 那么大 —— 多出来的那圈壳自然溢出到外面。
            let size = texture.size_vec2();
            let ratio = if size.x > 0.0 {
                size.x / aicraft_core::skin::HEAD_PX as f32
            } else {
                1.0
            };
            let draw = egui::Rect::from_center_size(rect.center(), rect.size() * ratio);
            painter.image(
                texture.id(),
                draw,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            // 纯 2D 才描边：3D 头像的边界已经由那圈壳自己交代了，
            // 再画一圈线会横穿在壳上。
            if (ratio - 1.0).abs() < 0.01 {
                painter.rect_stroke(rect, corner, ring, StrokeKind::Inside);
            }
        }
        None => {
            painter.rect_filled(rect, corner, accent_soft());
            painter.rect_stroke(rect, corner, ring, StrokeKind::Inside);
            let letter = name
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_else(|| "?".to_string());
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                letter,
                FontId::new(13.0, egui::FontFamily::Proportional),
                accent_strong(),
            );
        }
    }
}

/// 账户页左栏的第一项：当前在用的账号，这一项本身就是「已保存账户」。
///
/// 头像、名字、类型挤在一行 34 高的导航行里，字只好缩一号；
/// 选中底照旧由 [`NavHighlight`] 统一挪过去，这里只画悬停浅底。
pub fn account_item(
    ui: &mut Ui,
    name: &str,
    kind: &str,
    skin: Option<&egui::TextureHandle>,
    selected: bool,
) -> Response {
    // 比下面几个页签高一头：头像要塞得进去，选中底也该跟着撑开。
    let (rect, response, color) = nav_row(ui, 42.0, selected);
    let avatar = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 18.0, rect.center().y),
        Vec2::splat(26.0),
    );
    draw_avatar(ui, avatar, name, skin);
    let text_left = avatar.right() + 8.0;
    let painter = ui.painter().with_clip_rect(egui::Rect::from_min_max(
        egui::pos2(text_left, rect.top()),
        egui::pos2(rect.right() - 4.0, rect.bottom()),
    ));
    if kind.is_empty() {
        painter.text(
            egui::pos2(text_left, rect.center().y),
            Align2::LEFT_CENTER,
            name,
            FontId::new(13.0, egui::FontFamily::Proportional),
            color,
        );
    } else {
        painter.text(
            egui::pos2(text_left, rect.center().y - 1.0),
            Align2::LEFT_BOTTOM,
            name,
            FontId::new(12.5, egui::FontFamily::Proportional),
            color,
        );
        painter.text(
            egui::pos2(text_left, rect.center().y + 1.0),
            Align2::LEFT_TOP,
            kind,
            FontId::new(9.5, egui::FontFamily::Proportional),
            TEXT_FAINT,
        );
    }
    hand(response)
}

/// 侧栏那块会滑动的选中底。
///
/// 选中底不是「每行各画各的」：整栏只有一块，换项的时候它从旧的一行
/// 滑到新的一行，中途还看得见它在动。
///
/// 做法是先占一个空图形槽（[`egui::Shape::Noop`]），等这一栏所有行都量完、
/// 知道目标在哪一行了，再把这个槽换成真正的圆角块。槽位占在文字之前，
/// 所以它永远垫在字下面，不用操心谁盖住谁。
pub struct NavHighlight {
    slot: ShapeIdx,
    /// 底要去的那一行。用 `Cell` 而不是 `&mut`：两层导航是各自的闭包，
    /// 一块底却要两边都看得见，`&NavHighlight` 是可复制的，传起来最省事。
    target: std::cell::Cell<Option<Rect>>,
}

impl NavHighlight {
    /// 在这一栏开始之前占位。
    pub fn begin(ui: &Ui) -> Self {
        Self {
            slot: ui.painter().add(egui::Shape::Noop),
            target: std::cell::Cell::new(None),
        }
    }

    /// 报告一行的位置；`selected` 为真的那一行就是底要去的地方。
    pub fn row(&self, rect: Rect, selected: bool) {
        if selected {
            self.target.set(Some(rect));
        }
    }

    /// 收尾：把底画出来。
    ///
    /// 动画的值是「底的中线该在哪」，不是「离目标还差多少」——这样中途改
    /// 主意再点另一行时，它会从当前所在的位置接着走，而不是跳回起点。
    ///
    /// `animate` 为假时用 0 时长补间：值是立刻落到目标的，但 egui 里记的
    /// 那个数也跟着更新了 —— 否则关掉动画玩一会儿再打开，底会从很久以前
    /// 停的那一行一路滑过来。
    pub fn finish(&self, ui: &Ui, id: egui::Id, animate: bool) {
        let Some(rect) = self.target.get() else {
            return;
        };
        let secs = if animate { NAV_SLIDE_SECS } else { 0.0 };
        let center_y = ui
            .ctx()
            .animate_value_with_time(id, rect.center().y, secs);
        let block = nav_highlight_rect(rect, center_y);
        ui.painter().set(
            self.slot,
            egui::Shape::rect_filled(block, CornerRadius::same(8), accent_soft()),
        );
    }
}

/// 选中底最后落在哪：横竖尺寸都照目标那一行，纵向中线换成动画给的值。
///
/// 只动纵向：侧栏只有一列，横向永远整行铺满，不需要跟着滑。
pub fn nav_highlight_rect(row: Rect, center_y: f32) -> Rect {
    Rect::from_center_size(Pos2::new(row.center().x, center_y), row.size())
}

/// 导航行的公共底子：撑满宽度的一块可点区，返回位置与配色。
///
/// 主侧边栏的三种行（带图标的、二级项的、纯文字的）只有高度和字距不同，
/// 悬停与选中这两件事一模一样；抽出来省得改配色时要改三处。
///
/// 选中底不在这里画：整栏只有一块，得等所有行都排完才知道它在哪一行
/// （见 [`NavHighlight`]）。这里只画悬停那种「只在指针下才出现」的浅底。
fn nav_row(ui: &mut Ui, height: f32, selected: bool) -> (Rect, Response, Color32) {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());

    if !selected && response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), CARD_HOVER);
    }

    let color = if selected { accent_strong() } else { TEXT_WEAK };
    (rect, response, color)
}

/// 左侧导航的一项：选中时深绿字，平时只是灰字。
///
/// 选中那块浅绿底不归它画 —— 整栏只有一块，由 [`NavHighlight`] 统一挪过去；
/// 调用方把返回值里的 `rect` 报给那块底就行。
///
/// 不带图标 —— 账户页、管理页的左栏用它，那里的条目标题本身已经说得很清楚，
/// 配图标反而更花。主侧边栏请用 [`nav_item_glyph`]。
pub fn nav_item(ui: &mut Ui, text: &str, selected: bool, centered: bool) -> Response {
    let (rect, response, color) = nav_row(ui, 34.0, selected);
    // 居中的那几栏（管理页）字都围着中轴排，整列看着才是一条竖线；
    // 账户页那栏顶上还有头像行，字居中会跟头像错开，所以留给它顶格。
    let (pos, align) = if centered {
        (rect.center(), Align2::CENTER_CENTER)
    } else {
        (rect.left_center() + Vec2::new(14.0, 0.0), Align2::LEFT_CENTER)
    };
    ui.painter().text(
        pos,
        align,
        text,
        FontId::new(13.0, egui::FontFamily::Proportional),
        color,
    );

    hand(response)
}

/// 一行看起来就是按钮的文字：平时只有字，指针一上来浮出浅底和圆角。
///
/// 用它替代「`Label` + `Sense::click()`」那种写法 —— 后者严格说也能点，
/// 但悬停时毫无变化、指针也不变形，看着像一段不小心能按的文字。
/// 这里补上三件该有的信号：底色、圆角、手型光标。
///
/// `selected` 用 [`nav_item`] 同一套配色（浅绿底 + 深绿字），
/// 这样它和左栏那些真正的页签看起来才是一家。
pub fn text_button(ui: &mut Ui, text: &str, selected: bool) -> Response {
    // 走 `into_galley` 而不是 `painter().layout_no_wrap`：后者拿不到
    // `strong` 这类文本样式，字会明显比原来的标题轻一档。
    let galley = egui::WidgetText::from(RichText::new(text).size(15.0).strong()).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Body,
    );
    // 左右各留 8px：文字紧贴边框时，底色看着像把字框住了。
    let size = Vec2::new(galley.size().x + 16.0, galley.size().y + 6.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    // 选中优先于悬停：已经在这一页上了，再显示「可点」的浅灰反而糊。
    let fill = if selected {
        accent_soft()
    } else if response.hovered() {
        CARD_HOVER
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
    }
    let pos = rect.center() - galley.size() / 2.0;
    let color = if selected { accent_strong() } else { TEXT };
    ui.painter().galley(pos, galley, color);

    hand(response)
}

/// 一个开关：左边是文字，右边是沟槽 + 圆点。
///
/// 这一版只画，不改值——调用方拿到 `clicked()` 自己去处理。因为用在
/// 「按下去就不可逆」的地方，得先弹确认框，不能让开关自己先翻过去。
///
/// `on` 就是当前显示的样子：确认之前它不会因为点了就动。
pub fn switch(ui: &mut Ui, text: &str, on: bool, enabled: bool) -> Response {
    const TRACK_W: f32 = 34.0;
    const TRACK_H: f32 = 19.0;
    const KNOB: f32 = 15.0;
    const ROW_H: f32 = 26.0;

    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());

    let response = if enabled {
        response
    } else {
        ui.interact(rect, response.id, Sense::hover())
    };

    let track = Rect::from_min_size(
        Pos2::new(rect.right() - TRACK_W, rect.center().y - TRACK_H / 2.0),
        Vec2::new(TRACK_W, TRACK_H),
    );
    let fill = if !enabled {
        Color32::from_rgb(0xE6, 0xEC, 0xF3)
    } else if on {
        accent_fill()
    } else if response.hovered() {
        Color32::from_rgb(0xC9, 0xD6, 0xE4)
    } else {
        Color32::from_rgb(0xDD, 0xE5, 0xEF)
    };
    ui.painter()
        .rect_filled(track, CornerRadius::same((TRACK_H / 2.0) as u8), fill);

    let knob_x = if on {
        track.right() - KNOB / 2.0 - 2.0
    } else {
        track.left() + KNOB / 2.0 + 2.0
    };
    ui.painter().circle_filled(
        Pos2::new(knob_x, track.center().y),
        KNOB / 2.0,
        if enabled {
            Color32::WHITE
        } else {
            Color32::from_rgb(0xF7, 0xFA, 0xFC)
        },
    );

    if !text.is_empty() {
        let color = if enabled { TEXT } else { TEXT_FAINT };
        ui.painter().text(
            Pos2::new(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            text,
            FontId::new(12.0, egui::FontFamily::Proportional),
            color,
        );
    }

    if enabled {
        hand(response)
    } else {
        response
    }
}

/// 侧栏选中底滑动过去的时长（秒）。
///
/// 再长就像在拖影，再短就只剩「闪一下」——0.18 秒刚好够眼睛跟上它去了哪一行。
pub const NAV_SLIDE_SECS: f32 = 0.18;

// ---- 列表逐行进场 ----
//
// 版本列表、下载版本、资源下载三个列表共用这一套：每行按序号排队淡入，
// 并从下方轻轻升到位。三处各写一遍的话，时长和曲线迟早会走散。

/// 单行进场的时长（秒）。
///
/// 比换页淡入长一点点：这一版行要实实在在从下方滑到位，时间太短就只剩
/// 「闪一下就没了」，看不出它升上来。拖到半秒又会显钝。
pub const ROW_REVEAL_SECS: f32 = 0.30;

/// 相邻两行进场之间错开的时间（秒）。
///
/// 0.04 秒约两三帧：眼睛能明确看出「一个接一个」，又不会把十几行的
/// 出场时间拉得太长（十行约 0.66 秒，二十行约 1.06 秒）。
pub const ROW_REVEAL_STAGGER: f32 = 0.04;

/// 错峰排到第几行就不再往后推。
///
/// 再往后堆，列表底部的行要等太久；超过这个序号的行共用一个起始时刻，
/// 等于「一起进场」——那时候它们大多还在滚动区外面，本来也看不见。
pub const ROW_REVEAL_STAGGER_CAP: usize = 12;

/// 单行进场时从下方多少像素处开始。
///
/// 比换页的 14px 大一点：行是整张卡片，步子小了看起来只是「抖了一下」；
/// 16px 差不多是卡片高度的五分之一，抬眼的余光就能看见它在动。
pub const ROW_REVEAL_RISE_PX: f32 = 16.0;

/// 一整批行全部进场完毕需要多久（秒）。
pub fn row_reveal_total() -> f32 {
    ROW_REVEAL_SECS + ROW_REVEAL_STAGGER * ROW_REVEAL_STAGGER_CAP as f32
}

/// 第 index 行此刻的进场进度：0 = 还没出来，1 = 完全就位。
///
/// 返回 None 表示这一行早就进场完了，调用方走最普通的画法——这条捷径
/// 很重要：稳态下每帧都算一遍补间，长列表会白白烧掉绘制预算。
pub fn row_reveal_at(index: usize, elapsed: f32) -> Option<f32> {
    let stagger = ROW_REVEAL_STAGGER * index.min(ROW_REVEAL_STAGGER_CAP) as f32;
    let local = (elapsed - stagger) / ROW_REVEAL_SECS;
    if local >= 1.0 {
        return None;
    }
    Some(local.clamp(0.0, 1.0))
}

/// 进场曲线：从下方 `ROW_REVEAL_RISE_PX` 处升到原位。
///
/// 三次缓出——起步快、越接近落点越慢，最后几乎贴着原位滑进去。
/// 用线性的话行会匀速往上推、到位那一下嘎然而止，像被按了暂停。
pub fn row_reveal_rise(progress: f32) -> f32 {
    let t = progress.clamp(0.0, 1.0);
    let rest = 1.0 - t;
    ROW_REVEAL_RISE_PX * rest * rest * rest
}

/// 读一批行进场已经播了多久；播完（或动画关着）返回 None 并清掉时间戳。
///
/// 时间戳存在调用方的 State 里，这里只负责推进和收尾：`ctx` 用来
/// 在播放途中把下一帧叫醒，不然 egui 会睡过去、动画卡在中间。
///
/// `delay` 是整批行起步前的等待。还没到点时返回 0 而不是 None：None 的
/// 意思是「不用动画」，那会让整批行当场跳到终点、提前露出来；返回 0 则
/// 让它们停在起点（透明、微微下沉），位置照占，版面不会跳。
pub fn row_reveal_elapsed(
    slot: &mut Option<std::time::Instant>,
    animate: bool,
    ctx: &egui::Context,
    delay: f32,
) -> Option<f32> {
    if !animate {
        *slot = None;
        return None;
    }
    let elapsed = slot.as_ref()?.elapsed().as_secs_f32() - delay;
    if elapsed < 0.0 {
        ctx.request_repaint();
        return Some(0.0);
    }
    if elapsed >= row_reveal_total() {
        *slot = None;
        return None;
    }
    ctx.request_repaint();
    Some(elapsed)
}

/// 画一行会进场的列表项：`elapsed` 为 None 时就是普通画法。
///
/// 位移交给 `with_visual_transform`（只挪画出来的东西，点击区域不动），
/// 透明度交给 `set_opacity`。裁剪范围不动：滚动区底边该削就削，
/// 硬撑开裁剪会让行画到列表外面去，压住下面的详情卡片。
pub fn reveal_row<R>(
    ui: &mut egui::Ui,
    index: usize,
    elapsed: Option<f32>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let Some(progress) = elapsed.and_then(|elapsed| row_reveal_at(index, elapsed)) else {
        return add(ui);
    };
    let shift =
        egui::emath::TSTransform::from_translation(egui::vec2(0.0, row_reveal_rise(progress)));
    ui.scope(|ui| {
        ui.set_opacity(progress);
        ui.with_visual_transform(shift, add).inner
    })
    .inner
}

// ---- 滚动区上下沿的淡入淡出 ----
//
// 列表能滚的时候，被切断的行在边缘是「一刀齐」的：上面那行只剩半截字，
// 下面那行突然没了，看起来像内容漏出去了。两头各蒙一层底色渐变，滚出来
// 的那半截就是淡着出现、淡着消失，边界自然消失了。

/// 上下沿渐变各占多高（逻辑像素）。
///
/// 约一行卡片的三分之一：再窄了看不出过渡，再宽会吃掉半行内容。
pub const SCROLL_FADE_PX: f32 = 26.0;

/// 上、下沿遮罩此刻的不透明度（0 = 完全透明，1 = 完全被底色盖住）。
///
/// `overflow` 是可滚动的总距离，`offset` 是已经滚了多少：
/// 顶到最上面时上沿不该有遮罩（那里是列表真正的开头），
/// 滚到最底下时下沿同理。两端各用一个宽度 `SCROLL_FADE_PX` 的过渡，
/// 中间就是实心的——不这样，滚到一半时两头会一起蒙着，看着像整列发灰。
pub fn scroll_fade_alphas(overflow: f32, offset: f32) -> (f32, f32) {
    if overflow <= 0.5 {
        return (0.0, 0.0);
    }
    let top = (offset / SCROLL_FADE_PX).clamp(0.0, 1.0);
    let bottom = ((overflow - offset) / SCROLL_FADE_PX).clamp(0.0, 1.0);
    (top, bottom)
}

/// 给滚动区补上两头的渐隐：`show` 完之后调一次，画在内容上面。
///
/// 内容没超出、压根滚不动时什么都不画——静态列表本来就完整，蒙一层
/// 底色只会让它看起来没加载完。
pub fn fade_scrolled<R>(ui: &egui::Ui, out: &egui::containers::scroll_area::ScrollAreaOutput<R>) {
    let rect = out.inner_rect;
    if !rect.is_positive() {
        return;
    }
    let overflow = out.content_size.y - rect.height();
    let (top, bottom) = scroll_fade_alphas(overflow, out.state.clamped_offset().y);
    if top <= 0.0 && bottom <= 0.0 {
        return;
    }
    let painter = ui.painter_at(rect);
    let band = SCROLL_FADE_PX.min(rect.height() / 2.0);
    // 实色永远压在「被切断的那条边」上，往列表中间逐渐散开：
    // 上沿是上实下透明，下沿是上透明下实——方向反了的话，压在最边上那行
    // 反而最清楚，看起来就像内容是从边上漏出去的。
    if top > 0.0 {
        let band = Rect::from_min_max(rect.left_top(), egui::pos2(rect.right(), rect.top() + band));
        painter.add(gradient_band(band, true, bg_bottom_faded(top)));
    }
    if bottom > 0.0 {
        let band = Rect::from_min_max(
            egui::pos2(rect.left(), rect.bottom() - band),
            rect.right_bottom(),
        );
        painter.add(gradient_band(band, false, bg_bottom_faded(bottom)));
    }
}

/// 一条竖向渐变：`solid_at_top` 为真时上边是实色、往下散成透明，为假时反过来。
///
/// 用一个四顶点的网格画：上下两排顶点各给一个颜色，GPU 自己插值。
/// 底色是预乘透明的，插到全透明那一端自然就「散开」了，不会出灰边。
fn gradient_band(rect: Rect, solid_at_top: bool, solid: Color32) -> egui::Shape {
    let clear = Color32::TRANSPARENT;
    let (top_color, bottom_color) = if solid_at_top {
        (solid, clear)
    } else {
        (clear, solid)
    };
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top_color);
    mesh.colored_vertex(rect.right_top(), top_color);
    mesh.colored_vertex(rect.right_bottom(), bottom_color);
    mesh.colored_vertex(rect.left_bottom(), bottom_color);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    egui::Shape::mesh(mesh)
}

/// 版本管理那几页的标题行从右边滑进来要多久。
///
/// 比列表的逐行进场短一点：标题只有一行，拖长了会显得整页都慢半拍。
pub const TITLE_SLIDE_SECS: f32 = 0.26;

/// 标题行起步时停在右侧多少像素处。
///
/// 只挪这么点：标题是「弹出来」而不是「从屏幕外飞进来」，步子大了
/// 反而像页面在抖。
pub const TITLE_SLIDE_PX: f32 = 18.0;

/// 标题下面那排工具栏（搜索框、筛选按钮）比标题晚多久起步。
///
/// 差半拍就够：两条一起动像整块页头在平移，错开一点才读得出「先标题、
/// 再工具」的先后。
pub const TOOLBAR_SLIDE_DELAY: f32 = 0.09;

/// 页头整套动画的总时长：标题 + 工具栏依次走完。
///
/// 过了这个点，时间戳就该收掉了——再留着也只是白占一个字段。
pub const HEAD_TOTAL_SECS: f32 = TOOLBAR_SLIDE_DELAY + TITLE_SLIDE_SECS;

/// 资源下载页的第二排筛选（安装到 / 版本 / 排序）比第一排再晚多久起步。
///
/// 那一页比「下载版本」多一排条件：三层依次进来才排得开，一起动会糊成一片。
pub const TOOLBAR_2_SLIDE_DELAY: f32 = TOOLBAR_SLIDE_DELAY + 0.07;

/// 资源下载页页头整套动画的总时长：标题 + 两排筛选依次走完。
pub const RES_HEAD_TOTAL_SECS: f32 = TOOLBAR_2_SLIDE_DELAY + TITLE_SLIDE_SECS;

/// 列表要等页头落位之后才开始出现，末尾再留一点余量。
///
/// 写成算式而不是直接填数字：以后调快慢时不用回来手算第二遍。
pub const ROW_REVEAL_DELAY: f32 = HEAD_TOTAL_SECS + 0.03;

/// 读一次「页头动画已经跑了多久」（秒）；整套播完（或动画关着）返回 None。
///
/// 标题、工具栏、列表三段各有各的起步点，所以这里交出去的是**经过的时间**，
/// 由各段自己用 [`slide_progress`] 折成 0 到 1 的进度。
pub fn head_elapsed(
    slot: &mut Option<std::time::Instant>,
    animate: bool,
    ctx: &egui::Context,
) -> Option<f32> {
    head_elapsed_with(slot, animate, ctx, HEAD_TOTAL_SECS)
}

/// 同上，但「整套播完」的判定由调用方给。
///
/// 页头层数不一样的页面总时长也不一样（资源下载比「下载版本」多一排筛选）；
/// 用固定的 [`HEAD_TOTAL_SECS`] 收尾，晚起步的那一排会被半路掐掉。
pub fn head_elapsed_with(
    slot: &mut Option<std::time::Instant>,
    animate: bool,
    ctx: &egui::Context,
    total: f32,
) -> Option<f32> {
    if !animate {
        *slot = None;
        return None;
    }
    let elapsed = slot.as_ref()?.elapsed().as_secs_f32();
    if elapsed >= total {
        *slot = None;
        return None;
    }
    ctx.request_repaint();
    Some(elapsed)
}

/// 把「已经跑了多久」折成某一段自己的进度（0 = 起点，1 = 到位）。
///
/// 还没到这一段的起步点时给 0（停在起点、看不见），而不是 None——
/// None 的意思是「不用动画」，那会让它当场跳到终点。
pub fn slide_progress(elapsed: Option<f32>, delay: f32, secs: f32) -> Option<f32> {
    let elapsed = elapsed? - delay;
    if elapsed < 0.0 {
        return Some(0.0);
    }
    Some((elapsed / secs).clamp(0.0, 1.0))
}

/// 标题行这一刻离最终位置还有多远（正数 = 偏右）。
///
/// 三次缓出：起步快、尾巴慢慢收，落位那一下才不显得急。
pub fn title_slide_offset(progress: f32) -> f32 {
    let t = progress.clamp(0.0, 1.0);
    TITLE_SLIDE_PX * (1.0 - t) * (1.0 - t) * (1.0 - t)
}

/// 把标题行画成「从右侧滑进来」：`progress` 为 None 时就是普通画法。
///
/// 位移交给 `with_visual_transform`（只挪画出来的东西，点击区域不动）；
/// 右沿的裁剪范围临时放宽一点，否则往右挪的那一下，最右边那颗按钮
/// 会被面板边裁掉半截。
pub fn title_slide<R>(
    ui: &mut egui::Ui,
    progress: Option<f32>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let Some(progress) = progress else {
        return add(ui);
    };
    let offset = title_slide_offset(progress);
    ui.scope(|ui| {
        let clip = ui.clip_rect();
        ui.set_clip_rect(egui::Rect::from_min_max(
            clip.min,
            egui::pos2(clip.max.x + TITLE_SLIDE_PX + 2.0, clip.max.y),
        ));
        let shift = egui::emath::TSTransform::from_translation(egui::vec2(offset, 0.0));
        ui.with_visual_transform(shift, add).inner
    })
    .inner
}

/// 导航栏换层时，两层左右错开的距离（逻辑像素）。
///
/// 取整个侧栏内容区的宽度：旧的一层整条推出去、新的那条从右沿顶进来，
/// 像把一列卡片整体往左推走，中途不会有两行字叠在同一处。
const NAV_SWITCH_SLIDE_PX: f32 = 126.0;

/// 换层时那道分界线在内容区里的位置（0 = 最左，1 = 最右）。
///
/// `progress` 是 0 到 1 的换层进度（0 = 停在一级，1 = 停在二级目录）。
/// 旧的一层只画在线的左边、新的一层只画在线的右边，线随进度匀速往左走，
/// 所以整列内容看起来就是一路推过去，中间既不断档也不会两行字叠在一处。
pub fn nav_layer_seam(progress: f32) -> f32 {
    1.0 - progress.clamp(0.0, 1.0)
}

/// 换层时这一层要左右错开多少像素（正数往右）。
///
/// 两层同时朝左走：新的一层从右边滑到自己的位子，旧的一层从原位
/// 一路滑出去。位移随进度匀速变化，看着就是一条线推过去。
pub fn nav_layer_offset(level_two: bool, progress: f32) -> f32 {
    let t = progress.clamp(0.0, 1.0);
    let travel = NAV_SWITCH_SLIDE_PX * (1.0 - t);
    if level_two {
        travel
    } else {
        -NAV_SWITCH_SLIDE_PX * t
    }
}

/// 账户页左栏滑进滑出的时长（秒）。
///
/// 比换页的淡入稍长一点：那是一整条 178px 的面板在走，步子太急会像被拽了一下。
pub const ACCOUNT_PAGE_SLIDE_SECS: f32 = 0.24;

/// 账户页左栏这一刻离原位多远（0 = 站定，负数 = 已经往左滑出去了）。
///
/// `progress` 是「回到原位」的程度：0 = 整条藏在窗口左边外，1 = 正好站定。
/// 三次缓出 —— 进来时起步快、靠近位子慢慢收；出去时把进度倒着喂，
/// 于是先轻轻离开、再一路滑走。
pub fn account_nav_offset(progress: f32, width: f32) -> f32 {
    let away = 1.0 - progress.clamp(0.0, 1.0);
    -width * away * away * away
}

/// 画账户页左边那一条（返回按钮 + 四个页签），返回它占的矩形。
///
/// 和 `egui::Panel::left` 的区别只有一个：这一条要能整块左右挪。Panel 只会
/// 按尺寸摆好就画，而滑进滑出得把背景、按钮、文字一起平移，还得在窗口
/// 左沿之外裁掉 —— 所以这里自己开一块 `new_child`，矩形整体平移，
/// 裁剪范围取「挪过之后还落在原处的那一截」。
pub fn account_page_nav<R>(
    ui: &mut Ui,
    bounds: Rect,
    offset: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    let shifted = bounds.translate(Vec2::new(offset, 0.0));
    // 滑出去的那半截要落在窗口外，也就是落在 `bounds` 左边；
    // 两个矩形的交集正好是「还看得见的那一块」。
    let visible = shifted.intersect(bounds).intersect(ui.clip_rect());

    // 背景跟着一起挪：只挪文字会留下一整块白底，看着像两条栏叠着。
    let painter = ui.painter().with_clip_rect(visible);
    painter.rect_filled(shifted, 0.0, panel());

    let mut nav = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("account-nav")
            .max_rect(shifted.shrink2(Vec2::new(12.0, 14.0)))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    nav.set_clip_rect(visible);
    add(&mut nav)
}

/// 画导航栏的一层（一级那六项，或某套二级目录），返回它占的矩形。
///
/// 两层都从同一个 `top` 起笔，各自按 `progress` 左右错开一整个宽度；
/// 中间那道分界线决定这一层露出哪一侧，所以两层从不叠在一起。
/// 过场途中这一层不接点击：按钮正在移动，点下去容易点到隔壁那层。
pub fn nav_layer<R>(
    ui: &mut Ui,
    home: Rect,
    level_two: bool,
    progress: f32,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> (Rect, R) {
    let offset = nav_layer_offset(level_two, progress);
    // 整层往左/往右挪，靠的是把这块可用的矩形挪开，而不是画完再平移图形：
    // 平移图形会连裁剪框一起搬走，滑出去的那半截就会压到右边的正文上。
    let shifted = home.translate(Vec2::new(offset, 0.0));
    let builder = egui::UiBuilder::new().max_rect(shifted);

    // 一道移动的分界线：旧的那层只留线左边，新的那层只留线右边。
    // 两层各挪各的、谁也不压在谁身上，所以看着是「推」，不是「叠」。
    let seam = home.left() + home.width() * nav_layer_seam(progress);
    let clip = if level_two {
        Rect::from_min_max(Pos2::new(seam, home.top()), home.max)
    } else {
        Rect::from_min_max(home.min, Pos2::new(seam, home.bottom()))
    };

    // 用 `new_child` 而不是 `scope_builder`：后者会把这一层的包围盒
    // 记进外层 `Ui` 的 `min_rect`，滑到侧栏外的那一截就会把整块导航面板
    // 撑宽、把右边的正文挤走。这里只要画，不要占位。
    let mut layer = ui.new_child(builder);
    // 挪出去的那半截裁掉，只留分界线这一侧，别压到隔壁那层。
    layer.set_clip_rect(clip);
    if progress.clamp(0.0, 1.0) > 0.0 && progress.clamp(0.0, 1.0) < 1.0 {
        // 过场中两层都在动，谁也不接点击：点下去多半点到隔壁那层。
        layer.disable();
    }
    let out = add_contents(&mut layer);
    let inner = layer.min_rect();

    (inner.translate(Vec2::new(-offset, 0.0)), out)
}

/// 导航项：一枚图标 + 一行文字。
///
/// 图标轴心固定在左边距 14 加半径 9 的位置；文字居中在「图标右侧剩下的
/// 那一块」里 —— 标签有长有短（主页 / 版本管理），顶格排会让短的缩在左边、
/// 长的一路顶到右边，看着像两套缩进。
///
/// 选中那块浅绿底同样交给 [`NavHighlight`] 统一挪，这里只负责字与图标。
pub fn nav_item_glyph(ui: &mut Ui, glyph: NavGlyph, text: &str, selected: bool) -> Response {
    /// 图标边长。18 比文字（13）略高，配 34px 的行高刚好。
    const GLYPH: f32 = 18.0;
    /// 图标与文字之间的最小空隙。
    const GAP: f32 = 9.0;
    /// 图标轴心（相对内容区左沿）：左边距 14 + 半径 9。
    const ICON_AXIS: f32 = 23.0;
    /// 文字中线往左让出的距离。
    ///
    /// 图标占掉左侧之后，右边剩的那块比视觉重心偏右，纯居中会让整列字
    /// 看着往右飘。往左收一点，字就贴着图标站住了。
    const TEXT_BIAS: f32 = 9.0;

    let (rect, response, color) = nav_row(ui, 34.0, selected);
    let icon_center = Pos2::new(rect.left() + ICON_AXIS, rect.center().y);
    nav_glyph(ui.painter(), icon_center, GLYPH, glyph, color);

    let text_left = icon_center.x + GLYPH / 2.0 + GAP;
    let text_center_x = (text_left + rect.right()) * 0.5 - TEXT_BIAS;
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        FontId::new(13.0, egui::FontFamily::Proportional),
        color,
    );
    ui.painter().galley(
        Pos2::new(
            text_center_x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        color,
    );

    hand(response)
}

/// 侧边栏的六枚图标。
///
/// 全部按一张 24×24 的方格设计，再用 [`nav_glyph`] 缩放到实际尺寸 ——
/// 坐标可以照着手绘稿抄，改形状时也不必关心将来画多大。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NavGlyph {
    Home,
    Versions,
    Resources,
    Settings,
    Personalize,
    More,
    /// 垃圾桶：删掉这一份。
    Trash,
}

/// 在 `center` 处描一枚 `size` 见方的导航图标。
///
/// 用 `Painter` 画线而不是贴图片：图标只有十来条折线，几十字节的代码就够，
/// 而一张 PNG 要跟着分辨率准备两套、还得操心缩放糊不糊。
///
/// 线宽随尺寸等比缩放（设计格里是 1.65），顶点各补一个圆帽 —— `epaint` 的
/// 折线是方头方角的，不补帽子在拐弯和收笔处会露出缺口。
pub fn nav_glyph(painter: &Painter, center: Pos2, size: f32, glyph: NavGlyph, color: Color32) {
    /// 设计格的边长。
    const GRID: f32 = 24.0;
    /// 线宽（设计格单位）。
    const W: f32 = 1.65;

    let scale = size / GRID;
    let stroke = Stroke::new(W * scale, color);
    let pen = GlyphPen {
        painter,
        center,
        scale,
        stroke,
    };

    match glyph {
        NavGlyph::Home => {
            pen.line(&[(3.0, 11.4), (12.0, 3.6), (21.0, 11.4)]);
            pen.line(&[(5.4, 9.7), (5.4, 20.2), (18.6, 20.2), (18.6, 9.7)]);
            pen.line(&[(9.8, 20.2), (9.8, 14.4), (14.2, 14.4), (14.2, 20.2)]);
        }
        NavGlyph::Versions => {
            pen.rounded(&FOLDER, 1.5);
        }
        NavGlyph::Resources => {
            pen.rounded(&FOLDER, 1.5);
            pen.line(&[(12.0, 11.4), (12.0, 16.0)]);
            pen.line(&[(9.6, 13.8), (12.0, 16.2), (14.4, 13.8)]);
        }
        NavGlyph::Settings => {
            // 顶角从 0° 起排，六条边才是「上边水平」的 —— 从 30° 起排会变成
            // 尖角朝上，看着像一枚徽章而不是齿轮。
            //
            // 圆角只给 1.9：2.8 在 20px 上会让六条直边几乎消失，缩成一个圆环，
            // 和中间的圆叠在一起就成了张唱片。留一点直角，才认得出是六边形。
            let mut hex = [(0.0f32, 0.0f32); 6];
            for (i, slot) in hex.iter_mut().enumerate() {
                let a = i as f32 * 60.0f32.to_radians();
                *slot = (12.0 + 9.4 * a.cos(), 12.0 + 9.4 * a.sin());
            }
            pen.rounded(&hex, 1.9);
            pen.circle(12.0, 12.0, 3.4);
        }
        NavGlyph::Personalize => {
            // 笔杆竖直朝上；刷毛是下宽上窄的扇面，毛口三道波浪。
            // 缩到 20px 时，一整块平底会糊成一只瓶子，波浪口才认得出是毛。
            pen.rounded(&[(11.3, 3.0), (12.7, 3.0), (12.7, 8.2), (11.3, 8.2)], 0.05);
            pen.rounded(&[(10.1, 8.2), (13.9, 8.2), (13.9, 11.4), (10.1, 11.4)], 0.05);
            pen.line_close(&[
                (10.1, 11.4),
                (7.6, 18.0),
                (9.0, 18.6),
                (10.5, 16.8),
                (12.0, 18.8),
                (13.5, 16.8),
                (15.0, 18.6),
                (16.4, 18.0),
                (13.9, 11.4),
            ]);
        }
        NavGlyph::Trash => {
            // 盖、提手、桶身、两道竖纹。桶身往上收一点（上宽下窄）：等宽的
            // 两条边缩到 20px 会读成一只方盒子。
            pen.line(&[(4.4, 6.6), (19.6, 6.6)]);
            pen.line(&[(9.6, 6.6), (9.6, 4.4), (14.4, 4.4), (14.4, 6.6)]);
            pen.line(&[(6.6, 6.6), (7.4, 19.6), (16.6, 19.6), (17.4, 6.6)]);
            pen.line(&[(10.5, 10.2), (10.5, 16.0)]);
            pen.line(&[(13.5, 10.2), (13.5, 16.0)]);
        }
        NavGlyph::More => {
            for (x0, y0) in [(3.2, 3.2), (13.1, 3.2), (3.2, 13.1), (13.1, 13.1)] {
                pen.rounded(
                    &[(x0, y0), (x0 + 7.7, y0), (x0 + 7.7, y0 + 7.7), (x0, y0 + 7.7)],
                    1.9,
                );
            }
        }
    }
}

/// 版本管理 / 资源下载共用的文件夹外形。
const FOLDER: [(f32, f32); 6] = [
    (3.0, 6.4),
    (9.5, 6.4),
    (11.4, 8.9),
    (21.0, 8.9),
    (21.0, 18.8),
    (3.0, 18.8),
];

/// 把一个多边形的每个角用二次曲线抹圆，返回新的顶点序列。
///
/// 圆角半径会被夹到相邻两条边的一半以内 —— 否则短边上的两个圆角会互相
/// 吃掉对方，多边形会自己拧成麻花。
fn rounded_outline(pts: &[(f32, f32)], radius: f32) -> Vec<(f32, f32)> {
    let n = pts.len();
    if n < 3 {
        return pts.to_vec();
    }
    /// 每个角上用几段折线近似那截圆弧。6 段在 20px 上已经看不出棱角。
    const STEPS: usize = 6;
    let mut out: Vec<(f32, f32)> = Vec::with_capacity(n * (STEPS + 1));
    for i in 0..n {
        let prev = pts[(i + n - 1) % n];
        let here = pts[i];
        let next = pts[(i + 1) % n];

        let v1 = (prev.0 - here.0, prev.1 - here.1);
        let v2 = (next.0 - here.0, next.1 - here.1);
        let l1 = (v1.0 * v1.0 + v1.1 * v1.1).sqrt();
        let l2 = (v2.0 * v2.0 + v2.1 * v2.1).sqrt();
        if l1 < 1e-4 || l2 < 1e-4 {
            out.push(here);
            continue;
        }
        let r = radius.min(l1 * 0.5).min(l2 * 0.5);
        let a = (here.0 + v1.0 / l1 * r, here.1 + v1.1 / l1 * r);
        let b = (here.0 + v2.0 / l2 * r, here.1 + v2.1 / l2 * r);
        out.push(a);
        for s in 1..STEPS {
            let t = s as f32 / STEPS as f32;
            let m = 1.0 - t;
            out.push((
                m * m * a.0 + 2.0 * m * t * here.0 + t * t * b.0,
                m * m * a.1 + 2.0 * m * t * here.1 + t * t * b.1,
            ));
        }
        out.push(b);
    }
    out
}
/// 把设计格坐标翻译成屏幕坐标的一支笔。
struct GlyphPen<'a> {
    painter: &'a Painter,
    center: Pos2,
    scale: f32,
    stroke: Stroke,
}

impl GlyphPen<'_> {
    fn at(&self, x: f32, y: f32) -> Pos2 {
        Pos2::new(
            self.center.x + (x - 12.0) * self.scale,
            self.center.y + (y - 12.0) * self.scale,
        )
    }

    /// 一条不闭合的折线。
    fn line(&self, pts: &[(f32, f32)]) {
        self.poly(pts, false);
    }

    /// 一条首尾相接的折线。
    fn line_close(&self, pts: &[(f32, f32)]) {
        self.poly(pts, true);
    }

    fn poly(&self, pts: &[(f32, f32)], closed: bool) {
        if pts.len() < 2 {
            return;
        }
        let mut path: Vec<Pos2> = pts.iter().map(|&(x, y)| self.at(x, y)).collect();
        if closed {
            path.push(path[0]);
        }
        // 顶点补圆帽：折线本身是方头的，拐角和收笔处不补齐会露出小缺口。
        let r = self.stroke.width * 0.5;
        for p in &path {
            self.painter.circle_filled(*p, r, self.stroke.color);
        }
        self.painter.add(Shape::line(path, self.stroke));
    }

    /// 倒角多边形：每个角用一段二次曲线抹圆，半径自动收到边长的一半以内。
    fn rounded(&self, pts: &[(f32, f32)], radius: f32) {
        self.poly(&rounded_outline(pts, radius), true);
    }

    fn circle(&self, cx: f32, cy: f32, r: f32) {
        self.painter
            .circle_stroke(self.at(cx, cy), r * self.scale, self.stroke);
    }
}
/// 二级目录的返回行：「‹ 版本管理」，点一下退回一级。
///
/// 不做成 [`nav_item`] 那样一整块高亮——它不是同级的页面入口，
/// 而是「你现在在这一层里」的说明，点击只是附带能力。
pub fn nav_back(ui: &mut Ui, text: &str) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());

    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), CARD_HOVER);
    }

    ui.painter().text(
        rect.left_center() + Vec2::new(12.0, 0.0),
        Align2::LEFT_CENTER,
        format!("‹ {text}"),
        FontId::new(12.5, egui::FontFamily::Proportional),
        TEXT_FAINT,
    );

    hand(response)
}

/// 二级目录里的页面项：比一级项左缩进一格，看得出它归属上面那一行。
///
/// 选中底同样由 [`NavHighlight`] 统一画：一级与二级目录换层时，那一块
/// 会跟着滑到新的位置，而不是各画各的、换层瞬间凭空跳一下。
pub fn nav_item_sub(ui: &mut Ui, text: &str, selected: bool) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());

    if !selected && response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), CARD_HOVER);
    }

    let color = if selected { accent_strong() } else { TEXT_WEAK };
    ui.painter().text(
        rect.left_center() + Vec2::new(26.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        FontId::new(12.5, egui::FontFamily::Proportional),
        color,
    );

    hand(response)
}

/// 二级目录里的「动作项」：长得和 [`nav_item_sub`] 一样，但它不是一页 ——
/// 点下去会发生一件事（比如弹出资源管理器）。
///
/// 不能点的时候字变淡、连点击都不收：留一个按得下去却什么都不发生的行，
/// 比灰着更让人困惑。
pub fn nav_action_sub(ui: &mut Ui, text: &str, enabled: bool, centered: bool) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), sense);

    if enabled && response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), CARD_HOVER);
    }

    let color = if enabled { TEXT_WEAK } else { TEXT_FAINT };
    let (pos, align) = if centered {
        (rect.center(), Align2::CENTER_CENTER)
    } else {
        (rect.left_center() + Vec2::new(26.0, 0.0), Align2::LEFT_CENTER)
    };
    ui.painter().text(
        pos,
        align,
        text,
        FontId::new(12.5, egui::FontFamily::Proportional),
        color,
    );

    if enabled { hand(response) } else { response }
}

/// 状态点 + 文字，用在标题栏的账号状态。
pub fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

/// 标题栏右侧的账户块：头像 + 名字，整块都能点。
///
/// 头像和名字放进同一个描边圆角里，而不是「一颗悬在外面的头像 + 一个按钮」。
/// 分开摆的时候，头像浮在框外、名字缩在框里，看着像两个不相关的东西；
/// 收进一个框之后，它们才是「同一个账户的两种表示」。
///
/// `skin` 为 None 或名字为空都会退化：没有纹理就画首字母，没有账号就
/// 只画一个灰点 —— 调用方不用为这两种情况分支。
pub fn account_chip(
    ui: &mut Ui,
    name: &str,
    muted: bool,
    skin: Option<&egui::TextureHandle>,
) -> Response {
    let font = FontId::new(12.0, egui::FontFamily::Proportional);
    let galley =
        ui.painter()
            .layout_no_wrap(name.to_owned(), font, if muted { TEXT_WEAK } else { TEXT });
    // 高度 28：比窗口按钮（26）略高一点，视觉上它才是这一段的「主」。
    let height = 28.0;
    let width = galley.size().x + height + 10.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());

    // 圆角 6，不做成完整胶囊（那要 14）。
    //
    // 头像那圈圆角是 `AVATAR_ROUNDING`（0.14 × 22 ≈ 3px，几乎是方角），
    // 外壳却圆成半圆，两者放在一起就是一个圆肚里嵌着一块方砖，很不协调。
    // 6px 让外框也跟着"只是磨了个角"，跟头像属于同一族弧度。
    const CHIP_ROUNDING: u8 = 6;
    let fill = if response.hovered() {
        CARD_HOVER
    } else {
        PANEL
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(CHIP_ROUNDING), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(CHIP_ROUNDING),
        Stroke::new(1.0, BORDER),
        StrokeKind::Inside,
    );

    // 头像贴着左边内缘，直径比胶囊矮 6px，上下各留 3px。
    let side = height - 6.0;
    let avatar = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 3.0, rect.top() + 3.0),
        Vec2::splat(side),
    );
    draw_avatar(ui, avatar, name, skin);

    let text_pos = egui::pos2(
        avatar.right() + 6.0,
        rect.center().y - galley.size().y / 2.0,
    );
    ui.painter()
        .galley(text_pos, galley, if muted { TEXT_WEAK } else { TEXT });

    hand(response)
}

/// 标题栏右侧的窗口控制按钮。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WindowIcon {
    Minimize,
    Close,
}

/// 画一个窗口控制按钮：平时只是灰图标，悬停才浮出底色；
/// 关闭键悬停变红，和系统标题栏的约定一致。
pub fn window_button(ui: &mut Ui, icon: WindowIcon, is_close: bool) -> Response {
    let size = Vec2::new(36.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if response.hovered() {
        let fill = if is_close {
            DANGER
        } else {
            Color32::from_rgb(0xEE, 0xF2, 0xF7)
        };
        ui.painter().rect_filled(rect, CornerRadius::same(7), fill);
    }

    let color = if response.hovered() && is_close {
        Color32::WHITE
    } else {
        TEXT_WEAK
    };
    let c = rect.center();
    let stroke = Stroke::new(1.3, color);
    let half = 4.5;
    let painter = ui.painter();
    match icon {
        WindowIcon::Minimize => {
            painter.line_segment(
                [
                    Pos2::new(c.x - half, c.y + 2.5),
                    Pos2::new(c.x + half, c.y + 2.5),
                ],
                stroke,
            );
        }
        WindowIcon::Close => {
            painter.line_segment(
                [
                    Pos2::new(c.x - half, c.y - half),
                    Pos2::new(c.x + half, c.y + half),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x + half, c.y - half),
                    Pos2::new(c.x - half, c.y + half),
                ],
                stroke,
            );
        }
    }

    hand(response)
}
#[cfg(test)]
mod tests {
    use super::*;

    /// 没有背景图的时候，各块底色必须还是原来的不透明原色。
    ///
    /// 滑杆上的值留着不冲突：玩家清掉背景再换一张，之前调的手感还在；
    /// 但没有图可透的时候界面得和「没有这套功能」时一模一样。
    #[test]
    fn without_a_wallpaper_every_panel_stays_opaque() {
        set_wallpaper(false);
        set_alphas(aicraft_core::settings::UiAlphas {
            chrome: 130,
            background: 110,
            card: 140,
        });
        assert_eq!(panel_titlebar(), PANEL);
        assert_eq!(panel(), PANEL);
        assert_eq!(bg_bottom(), BG_BOTTOM);
        assert_eq!(card_fill(), CARD);
        assert_eq!(panel_titlebar().a(), 255);

        with_a_wallpaper_each_panel_keeps_its_own_alpha();
    }

    /// 跟上面那个用例共用同一组全局原子量，所以并到同一个测试里跑：
    /// cargo test 默认多线程，两个用例同时改全局状态会互相踩。
    fn with_a_wallpaper_each_panel_keeps_its_own_alpha() {
        set_wallpaper(true);
        set_alphas(aicraft_core::settings::UiAlphas {
            chrome: 200,
            background: 160,
            card: 240,
        });
        // 顶栏与侧栏共用一根滑杆：读的是同一个原子量。
        assert_eq!(panel_titlebar().a(), 200);
        assert_eq!(panel().a(), 200);
        assert_eq!(bg_bottom().a(), 160);
        assert_eq!(card_fill().a(), 240);
        // Color32 是预乘的，所以只看 alpha：调子还是那一支白/浅灰蓝。

        // 收尾：把全局状态还原，免得影响同进程里别的用例。
        set_wallpaper(false);
        set_alphas(aicraft_core::settings::UiAlphas::default());
    }

    /// 无头跑一帧：把输入框摆上去，可选地送一次右键按下/抬起。
    ///
    /// 返回输入框的中心点，后面模拟右键就点这儿。
    fn run_frame(
        ctx: &egui::Context,
        text: &mut String,
        id: Id,
        click: Option<(Pos2, bool)>,
    ) -> Pos2 {
        let mut events = Vec::new();
        if let Some((pos, pressed)) = click {
            events.push(egui::Event::PointerMoved(pos));
            events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }

        let mut center = Pos2::ZERO;
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 200.0))),
                events,
                ..Default::default()
            },
            |ui| {
                let mut resp = ui.add(egui::TextEdit::singleline(text).id(id));
                input_menu(&mut resp, text, false);
                center = resp.rect.center();
            },
        );
        // 不落地贴图，否则 egui 会在 drop 时抱怨有未应用的 delta。
        out.textures_delta.clear();
        center
    }

    /// 右键菜单最要紧的一条：右键不该把已经选好的一段字弄丢。
    ///
    /// egui 的 `pointer_interaction` 只看 `pointer.any_pressed()`，左右键都算，
    /// 右键一按就把选区塌成光标，菜单里的「复制」于是变成复制空气。
    /// `input_menu` 靠「没按键的帧记一份、右键抬起时塞回去」把这个坑填上。
    #[test]
    fn right_clicking_keeps_the_selection() {
        let ctx = egui::Context::default();
        let mut text = String::from("hello world");
        let id = Id::unique("input-menu-test");

        let center = run_frame(&ctx, &mut text, id, None);
        ctx.memory_mut(|m| m.request_focus(id));
        let mut state = TextEditState::load(&ctx, id).unwrap();
        state
            .cursor
            .set_char_range(Some(CCursorRange::select_all_str(&text)));
        state.store(&ctx, id);

        run_frame(&ctx, &mut text, id, None);
        run_frame(&ctx, &mut text, id, Some((center, true)));
        run_frame(&ctx, &mut text, id, Some((center, false)));

        let range = TextEditState::load(&ctx, id)
            .and_then(|state| state.cursor.char_range())
            .expect("右键之后选区该还在");
        assert_eq!(range.slice_str(&text), "hello world");
    }

    /// 点菜单按钮会让输入框失焦，失焦后 `TextEdit` 把选区压成光标，
    /// 于是「复制」拿到的是空的。`input_menu_selection` 要能从暂存里把那段字捞回来。
    #[test]
    fn the_selection_survives_losing_focus() {
        let ctx = egui::Context::default();
        let mut text = String::from("hello world");
        let id = Id::unique("input-menu-focus-test");
        let sel_key = id.with("input-menu-selection");

        run_frame(&ctx, &mut text, id, None);
        ctx.memory_mut(|m| m.request_focus(id));
        let mut state = TextEditState::load(&ctx, id).unwrap();
        state
            .cursor
            .set_char_range(Some(CCursorRange::select_all_str(&text)));
        state.store(&ctx, id);
        run_frame(&ctx, &mut text, id, None);

        ctx.memory_mut(|m| m.surrender_focus(id));
        run_frame(&ctx, &mut text, id, None);

        let range = input_menu_selection(&ctx, id, sel_key).expect("失焦后仍该记得刚才选了什么");
        assert_eq!(range.slice_str(&text), "hello world");
    }

    /// 剪切要同时做两件事：选中的字进剪贴板、文本里删掉它。
    #[test]
    fn cutting_puts_the_text_on_the_clipboard() {
        let ctx = egui::Context::default();
        let mut text = String::from("hello world");
        let id = Id::unique("input-menu-cut-test");

        input_menu_cut(
            &ctx,
            id,
            &mut text,
            CCursorRange::two(CCursor::new(6), CCursor::new(11)),
        );

        assert_eq!(text, "hello ");
        let copied: Vec<String> = ctx.output(|o| {
            o.commands
                .iter()
                .filter_map(|cmd| match cmd {
                    egui::OutputCommand::CopyText(s) => Some(s.clone()),
                    _ => None,
                })
                .collect()
        });
        assert_eq!(copied, ["world".to_owned()]);
    }

    /// 剪切要按字节删，中文一个字三个字节，用字符下标直接切会切在半个字上。
    #[test]
    fn cutting_counts_bytes_not_chars() {
        let text = String::from("中文abc");
        let bytes = input_menu_bytes(&text, &(CharIndex(1)..CharIndex(3)));
        assert_eq!(&text[bytes], "文a");
    }

    /// 正方形的四个角抹圆后，每角补出 7 个点（两端 + 5 段中间）。
    #[test]
    fn rounding_a_square_adds_vertices() {
        let square = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let out = rounded_outline(&square, 2.0);
        assert_eq!(out.len(), 4 * 7);
    }

    /// 抹圆后的点必须仍落在原多边形里 —— 圆角只许往里收，不许往外鼓。
    #[test]
    fn rounding_never_leaves_the_square() {
        let square = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        for (x, y) in rounded_outline(&square, 3.0) {
            assert!((-0.001..=10.001).contains(&x), "x 越界：{x}");
            assert!((-0.001..=10.001).contains(&y), "y 越界：{y}");
        }
    }

    /// 半径比边长还大时夹到半条边，不能把矩形拧成别的形状。
    #[test]
    fn an_overlarge_radius_is_clamped_to_the_edge() {
        let rect = [(0.0, 0.0), (4.0, 0.0), (4.0, 20.0), (0.0, 20.0)];
        for (x, y) in rounded_outline(&rect, 100.0) {
            assert!((0.0..=4.0).contains(&x), "x 越界：{x}");
            assert!((0.0..=20.0).contains(&y), "y 越界：{y}");
        }
    }

    /// 顶点不足三个时没有「内角」可言，原样返回。
    #[test]
    fn fewer_than_three_points_are_left_alone() {
        let line = [(0.0, 0.0), (5.0, 5.0)];
        assert_eq!(rounded_outline(&line, 1.0), line.to_vec());
    }

    /// 十六种主题色一种都不能少，下标也得对得上。
    #[test]
    fn every_theme_color_lands_on_its_own_row() {
        use aicraft_core::settings::ThemeColor;
        assert_eq!(ThemeColor::ALL.len(), PALETTE.len());
        for (i, color) in ThemeColor::ALL.iter().enumerate() {
            assert_eq!(accent_index(*color), i, "{} 的下标不对", color.label());
        }
    }

    /// 默认的黄绿必须还是原来那套薄荷绿：换主题色不能顺手把默认观感改了。
    #[test]
    fn the_default_theme_is_the_mint_green_it_always_was() {
        let a = accent_of(aicraft_core::settings::ThemeColor::Lime);
        assert_eq!(a.base, Color32::from_rgb(0x10, 0xB9, 0x81));
        assert_eq!(a.soft, Color32::from_rgb(0xE6, 0xF7, 0xF0));
        assert_eq!(a.strong, Color32::from_rgb(0x04, 0x7E, 0x5A));
        assert_eq!(a.ink, Color32::WHITE);
    }

    /// 浅色主题色（白、黄、粉）压不住白字，字色必须换成深色，否则按钮上的
    /// 「启动游戏」会白得看不见。
    #[test]
    fn a_pale_theme_writes_dark_text_on_itself() {
        use aicraft_core::settings::ThemeColor;
        for color in [ThemeColor::White, ThemeColor::Yellow, ThemeColor::Pink] {
            let a = accent_of(color);
            assert_ne!(a.ink, Color32::WHITE, "{} 上的白字看不清", color.label());
        }
    }

    /// 深色主题色反过来：字得是白的。
    #[test]
    fn a_dark_theme_writes_white_text_on_itself() {
        use aicraft_core::settings::ThemeColor;
        for color in [
            ThemeColor::Gray,
            ThemeColor::Blue,
            ThemeColor::Purple,
            ThemeColor::Black,
        ] {
            assert_eq!(
                accent_of(color).ink,
                Color32::WHITE,
                "{} 上的字该是白的",
                color.label()
            );
        }
    }

    /// 分界线从最右边一路匀速走到最左边，两头正好贴住侧栏的两沿。
    #[test]
    fn the_seam_walks_from_one_edge_to_the_other() {
        assert_eq!(nav_layer_seam(0.0), 1.0, "起点该在最右边");
        assert_eq!(nav_layer_seam(1.0), 0.0, "终点该在最左边");
        assert_eq!(nav_layer_seam(0.5), 0.5, "半程该在正中间");
        let mut last = nav_layer_seam(0.0);
        for step in 1..=20 {
            let next = nav_layer_seam(step as f32 / 20.0);
            assert!(next <= last, "第 {} 步分界线往回走了", step);
            last = next;
        }
    }

    /// 选中底只纵向挪：横向尺寸、圆角位置都照目标那一行，不会跟着变形。
    #[test]
    fn the_highlight_keeps_the_row_width_and_follows_the_center() {
        let row = Rect::from_min_size(Pos2::new(10.0, 100.0), Vec2::new(126.0, 34.0));
        let block = nav_highlight_rect(row, row.center().y);
        assert_eq!(block, row, "动画走到目标时该和目标那一行完全重合");

        let midway = nav_highlight_rect(row, 40.0);
        assert_eq!(midway.center().y, 40.0, "纵向中线跟着动画走");
        assert_eq!(midway.center().x, row.center().x, "横向不挪");
        assert_eq!(midway.size(), row.size(), "尺寸不变，看着才是同一块底在动");
    }

    /// 两层朝同一个方向走：新的一层从右边滑进来，旧的一层往左边滑出去。
    #[test]
    fn the_nav_layers_slide_the_same_way() {
        assert!(nav_layer_offset(true, 0.0) > 0.0, "进场那层该在右边等着");
        assert!(nav_layer_offset(false, 1.0) < 0.0, "退场那层该往左走");
        assert_eq!(nav_layer_offset(false, 0.0), 0.0);
        assert_eq!(nav_layer_offset(true, 1.0), 0.0);
    }

    /// 标题从右边起步，最后正好落在原位。
    #[test]
    fn a_title_starts_on_the_right_and_lands_in_place() {
        assert_eq!(title_slide_offset(0.0), TITLE_SLIDE_PX);
        assert_eq!(title_slide_offset(1.0), 0.0);
        // 越界也夹住：中途改主意或掉一帧，都不该把它甩到屏幕外。
        assert_eq!(title_slide_offset(1.6), 0.0);
        assert_eq!(title_slide_offset(-0.4), TITLE_SLIDE_PX);
    }

    /// 账户页左栏：整条宽度滑进来，站定时位移正好归零。
    #[test]
    fn the_account_nav_slides_a_whole_width_and_stops_flush() {
        const W: f32 = 178.0;
        // 站在左沿外：整整一条宽度。
        assert_eq!(account_nav_offset(0.0, W), -W);
        // 到位：不多不少，正好 0 —— 差一点就会把内容推歪。
        assert_eq!(account_nav_offset(1.0, W), 0.0);
        // 越界也夹住。
        assert_eq!(account_nav_offset(-0.7, W), -W);
        assert_eq!(account_nav_offset(2.3, W), 0.0);
    }

    /// 只往右走、不回头，而且越靠近位子越慢（缓出）。
    #[test]
    fn the_account_nav_only_ever_moves_right() {
        const W: f32 = 178.0;
        let mut previous = account_nav_offset(0.0, W);
        for step in 1..=20 {
            let here = account_nav_offset(step as f32 / 20.0, W);
            assert!(
                here >= previous - f32::EPSILON,
                "第 {} 步反而往左跑了：{} < {}",
                step,
                here,
                previous
            );
            previous = here;
        }
        // 前半程走掉的比后半程多，收尾才是慢慢靠上去而不是撞上去。
        let first_half = account_nav_offset(0.5, W) - account_nav_offset(0.0, W);
        let second_half = account_nav_offset(1.0, W) - account_nav_offset(0.5, W);
        assert!(first_half > second_half);
    }

    /// 只往左走，不回头：一路单调收窄，看着才是「弹进来」而不是抖。
    #[test]
    fn a_title_only_ever_moves_left() {
        let mut previous = title_slide_offset(0.0);
        for step in 1..=20 {
            let here = title_slide_offset(step as f32 / 20.0);
            assert!(
                here <= previous + f32::EPSILON,
                "第 {} 步反而往右跑了：{} > {}",
                step,
                here,
                previous
            );
            previous = here;
        }
    }

    /// 页头三段是依次起步的：标题先走，工具栏等一会儿，清单最后。
    #[test]
    fn the_header_moves_in_three_beats() {
        let at = |secs: f32| Some(secs);
        // 起步那一刻：标题在起点，工具栏还在起点等着。
        assert_eq!(slide_progress(at(0.0), 0.0, TITLE_SLIDE_SECS), Some(0.0));
        assert_eq!(
            slide_progress(at(0.0), TOOLBAR_SLIDE_DELAY, TITLE_SLIDE_SECS),
            Some(0.0)
        );
        // 标题走完时，工具栏已经起步但还没到位。
        let toolbar = slide_progress(at(TITLE_SLIDE_SECS), TOOLBAR_SLIDE_DELAY, TITLE_SLIDE_SECS)
            .expect("工具栏该在播");
        assert!(toolbar > 0.0 && toolbar < 1.0, "工具栏进度是 {}", toolbar);
        // 工具栏也走完之后，两段都停在 1。
        assert_eq!(
            slide_progress(at(1.0), TOOLBAR_SLIDE_DELAY, TITLE_SLIDE_SECS),
            Some(1.0)
        );
        // 动画关着（None）时整段不播。
        assert_eq!(slide_progress(None, 0.0, TITLE_SLIDE_SECS), None);
    }

    /// 资源下载的页头多一排筛选，三段也是依次起步、后一段不早于前一段。
    #[test]
    fn the_resource_header_has_a_third_beat() {
        // 三段的起步顺序：标题 → 第一排 → 第二排。
        let beats = std::hint::black_box([0.0, TOOLBAR_SLIDE_DELAY, TOOLBAR_2_SLIDE_DELAY]);
        assert!(beats.windows(2).all(|pair| pair[1] > pair[0]), "起步点：{:?}", beats);
        let at = |secs: f32| Some(secs);
        // 第一排刚起步时，第二排还在原地等着。
        assert_eq!(
            slide_progress(at(TOOLBAR_SLIDE_DELAY), TOOLBAR_2_SLIDE_DELAY, TITLE_SLIDE_SECS),
            Some(0.0)
        );
        // 到第二排起步点时它才动，并且还没到位。
        let second = slide_progress(
            at(TOOLBAR_2_SLIDE_DELAY),
            TOOLBAR_2_SLIDE_DELAY,
            TITLE_SLIDE_SECS,
        )
        .expect("第二排该在播");
        assert_eq!(second, 0.0);
        let mid = slide_progress(
            at(TOOLBAR_2_SLIDE_DELAY + TITLE_SLIDE_SECS / 2.0),
            TOOLBAR_2_SLIDE_DELAY,
            TITLE_SLIDE_SECS,
        )
        .expect("第二排该在播");
        assert!(mid > 0.0 && mid < 1.0, "第二排进度是 {}", mid);
        // 整套播完那一刻，三段都停在 1。
        for delay in [0.0, TOOLBAR_SLIDE_DELAY, TOOLBAR_2_SLIDE_DELAY] {
            assert_eq!(slide_progress(at(RES_HEAD_TOTAL_SECS), delay, TITLE_SLIDE_SECS), Some(1.0));
        }
    }

    /// 页头层数多的页面要按自己的总时长收尾，否则最后一排会被半路掐掉。
    #[test]
    fn a_taller_header_keeps_its_own_deadline() {
        let ctx = egui::Context::default();
        let mut slot = Some(std::time::Instant::now());
        // 默认时长已经过了，但按资源下载那份更长的时长还没到：时间戳留着。
        let long = RES_HEAD_TOTAL_SECS.max(HEAD_TOTAL_SECS) + 100.0;
        assert!(head_elapsed_with(&mut slot, true, &ctx, long).is_some());
        assert!(slot.is_some());
        // 动画关掉时立刻收尾。
        assert_eq!(head_elapsed_with(&mut slot, false, &ctx, long), None);
        assert!(slot.is_none());
    }

    /// 列表的起步点排在页头之后：延迟没走完时停在 0，而不是当场跳到终点。
    #[test]
    fn the_list_waits_for_the_header() {
        let ctx = egui::Context::default();
        let mut slot = Some(std::time::Instant::now());
        // 刚起跑：延迟还没过，进度是 0（所有行透明地停在起点）。
        assert_eq!(
            row_reveal_elapsed(&mut slot, true, &ctx, ROW_REVEAL_DELAY),
            Some(0.0)
        );
        // 时间戳还在，说明它没有把自己收掉、下一帧还会继续推进。
        assert!(slot.is_some());
        // 关掉动画时立刻收尾。
        let mut slot = Some(std::time::Instant::now());
        assert_eq!(row_reveal_elapsed(&mut slot, false, &ctx, 0.0), None);
        assert!(slot.is_none());
    }

    /// 内容没超出、压根滚不动时两头都不蒙：静态列表本来就完整。
    #[test]
    fn a_list_that_fits_is_left_alone() {
        assert_eq!(scroll_fade_alphas(0.0, 0.0), (0.0, 0.0));
        // 差半个像素以内也不算能滚（浮点余数会造出这种假溢出）。
        assert_eq!(scroll_fade_alphas(0.4, 0.0), (0.0, 0.0));
    }

    /// 停在最上面时，上沿是列表真正的开头，不该蒙；下沿则该蒙实。
    #[test]
    fn the_top_edge_is_clear_at_the_very_top() {
        let (top, bottom) = scroll_fade_alphas(400.0, 0.0);
        assert_eq!(top, 0.0);
        assert_eq!(bottom, 1.0);
    }

    /// 滚到最底下时反过来：下沿是结尾，蒙实的是上沿。
    #[test]
    fn the_bottom_edge_is_clear_at_the_very_end() {
        let (top, bottom) = scroll_fade_alphas(400.0, 400.0);
        assert_eq!(top, 1.0);
        assert_eq!(bottom, 0.0);
    }

    /// 滚到中间时两头都实心，遮罩只在各自那段 SCROLL_FADE_PX 里过渡。
    #[test]
    fn the_mask_only_climbs_inside_its_own_band() {
        let overflow = 400.0;
        // 刚往下挪一点点：上沿还几乎透明，下沿已经全实。
        let (top, bottom) = scroll_fade_alphas(overflow, SCROLL_FADE_PX / 2.0);
        assert!((top - 0.5).abs() < 1e-6, "上沿应当是半透明，实际 {top}");
        assert_eq!(bottom, 1.0);
        // 走完一整段之后不再变：中间是纯实心，不会越滚越黑。
        let (top, bottom) = scroll_fade_alphas(overflow, SCROLL_FADE_PX * 3.0);
        assert_eq!(top, 1.0);
        assert_eq!(bottom, 1.0);
    }

    /// 滚过头 / 越界时钳住，别让遮罩反向跑。
    #[test]
    fn the_mask_never_goes_past_its_two_ends() {
        let (top, bottom) = scroll_fade_alphas(400.0, -20.0);
        assert_eq!(top, 0.0);
        assert_eq!(bottom, 1.0);
        let (top, bottom) = scroll_fade_alphas(400.0, 9999.0);
        assert_eq!(top, 1.0);
        assert_eq!(bottom, 0.0);
    }

    /// 无头跑一帧下拉框，返回按钮矩形。`time` 用来推进动画。
    fn run_combo(ctx: &egui::Context, time: f64, events: Vec<egui::Event>) -> (Rect, Id) {
        let mut rect = Rect::NOTHING;
        let mut popup_id = Id::NULL;
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                let mut picked = 0;
                let resp = combo(ui, "test-combo", "选项", 120.0, |ui| {
                    ui.selectable_value(&mut picked, 1, "一");
                    ui.selectable_value(&mut picked, 2, "二");
                });
                rect = resp.rect;
                popup_id = ui.make_persistent_id("test-combo").with("combo-popup");
            },
        );
        out.textures_delta.clear();
        (rect, popup_id)
    }

    /// 点开下拉框并跑到动画结束，返回按钮矩形、浮层矩形与最后一次的输出。
    fn run_combo_open(ctx: &egui::Context) -> (Rect, Rect, egui::FullOutput) {
        let mut time = 0.0_f64;
        let (rect, popup_id) = run_combo(ctx, time, Vec::new());
        let center = rect.center();
        let click = |pos: Pos2, pressed: bool| {
            let mut events = vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }];
            if pressed {
                events.insert(0, egui::Event::PointerMoved(pos));
            }
            events
        };
        time += 0.016;
        run_combo(ctx, time, click(center, true));
        time += 0.016;
        run_combo(ctx, time, click(center, false));
        for _ in 0..30 {
            time += 0.016;
            run_combo(ctx, time, Vec::new());
        }
        time += 0.016;
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
                time: Some(time),
                ..Default::default()
            },
            |ui| {
                let mut picked = 0;
                combo(ui, "test-combo", "选项", 120.0, |ui| {
                    ui.selectable_value(&mut picked, 1, "一");
                    ui.selectable_value(&mut picked, 2, "二");
                });
            },
        );
        let popup = ctx
            .memory(|m| m.area_rect(popup_id))
            .expect("点开了就该有浮层");
        out.textures_delta.clear();
        (rect, popup, out)
    }

    /// 展开时按钮下沿、浮层上沿都不该有圆角：两块拼成一条直线，
    /// 才像同一个控件拉出来的，各自留圆角就在接缝处露出两个东西。
    #[test]
    fn the_button_and_the_popup_meet_without_round_corners() {
        let ctx = egui::Context::default();
        let (button, popup, out) = run_combo_open(&ctx);

        // Frame::popup 有阴影，会画成 Shape::Vec([阴影, 底])，得拆开看。
        fn rects(shape: &Shape, out: &mut Vec<egui::epaint::RectShape>) {
            match shape {
                Shape::Rect(r) => out.push(r.clone()),
                Shape::Vec(v) => {
                    for s in v {
                        rects(s, out);
                    }
                }
                _ => {}
            }
        }
        let mut all = Vec::new();
        for clipped in &out.shapes {
            rects(&clipped.shape, &mut all);
        }

        let mut button_shape = None;
        let mut popup_shape = None;
        for r in &all {
            if (r.rect.center() - button.center()).length() < 2.0 {
                button_shape = Some(r.corner_radius);
            } else if (r.rect.top() - popup.top()).abs() < 3.0
                && (r.rect.left() - popup.left()).abs() < 3.0
            {
                popup_shape = Some(r.corner_radius);
            }
        }

        let button_corner = button_shape.expect("按钮得画出来");
        assert_eq!(
            (button_corner.sw, button_corner.se),
            (0, 0),
            "展开时按钮下沿该是直角，实际 {:?}",
            button_corner
        );
        assert!(
            button_corner.nw > 0 && button_corner.ne > 0,
            "上沿两个角还是圆的，{:?}",
            button_corner
        );

        let popup_corner = popup_shape.expect("浮层得画出来");
        assert_eq!(
            (popup_corner.nw, popup_corner.ne),
            (0, 0),
            "浮层上沿该是直角，实际 {:?}",
            popup_corner
        );
        assert!(
            popup_corner.sw > 0 && popup_corner.se > 0,
            "下沿两个角还是圆的，{:?}",
            popup_corner
        );
    }

    /// 收起途中接缝两侧得一直削着，等浮层真的收没了才一起圆回来。
    ///
    /// 浮层在往上收的那几帧里还挂在按钮下面，上沿又是被裁剪线切出来的一条直边。
    /// 这时候按钮要是先圆回去，圆角两侧就翘出两枚小三角、中间横着一条方角白边，
    /// 看着像两张纸错着叠在一起。
    #[test]
    fn the_seam_stays_square_while_the_popup_is_still_going_back() {
        // 纯函数这一层先钉死：不削的时候四角原样返回，削的时候只动上沿。
        let base = CornerRadius::same(6);
        let round = popup_corner_radius(false, base);
        assert_eq!(
            (round.nw, round.ne, round.sw, round.se),
            (6, 6, 6, 6),
            "不削的时候四角都该是圆的，实际 {:?}",
            round
        );
        let squared = popup_corner_radius(true, base);
        assert_eq!(
            (squared.nw, squared.ne, squared.sw, squared.se),
            (0, 0, 6, 6),
            "削的时候只动上沿两个角，实际 {:?}",
            squared
        );

        // 再看真画出来的形状：点开、跑完动画、再点一下收起，然后在收起途中
        // 同时盯住浮层的上沿和按钮的下沿 —— 两边都不该有圆角。
        let ctx = egui::Context::default();
        let (rect, _popup, _out) = run_combo_open(&ctx);
        let center = rect.center();
        let mut time = 5.0_f64;
        let mut seen: Vec<egui::epaint::RectShape> = Vec::new();

        // 一帧里画出来的矩形都摊平收进来：按钮、浮层底板、还有一堆小装饰。
        fn rects(shape: &Shape, out: &mut Vec<egui::epaint::RectShape>) {
            match shape {
                Shape::Rect(r) => out.push(r.clone()),
                Shape::Vec(v) => {
                    for s in v {
                        rects(s, out);
                    }
                }
                _ => {}
            }
        }

        for step in 0..8 {
            time += 0.016;
            let events = match step {
                0 => vec![
                    egui::Event::PointerMoved(center),
                    egui::Event::PointerButton {
                        pos: center,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                1 => vec![egui::Event::PointerButton {
                    pos: center,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                _ => Vec::new(),
            };
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let mut picked = 0;
                    combo(ui, "test-combo", "选项", 120.0, |ui| {
                        ui.selectable_value(&mut picked, 1, "一");
                        ui.selectable_value(&mut picked, 2, "二");
                    });
                },
            );
            out.textures_delta.clear();

            let mut all = Vec::new();
            for clipped in &out.shapes {
                rects(&clipped.shape, &mut all);
            }
            // 浮层比按钮高得多，靠高度把两者分出来；收起途中它还在往上滑，
            // 所以只能认「比按钮高一截」这一条。
            let popup: Vec<_> = all
                .iter()
                .filter(|r| r.rect.height() > 40.0 && r.rect.width() > 100.0)
                .cloned()
                .collect();
            if popup.is_empty() {
                // 浮层已经收没了，这一帧不该再管按钮的角。
                continue;
            }

            // 按钮是不动的，直接按它这一帧该待的位置认回来。
            let button = all.iter().find(|r| {
                (r.rect.min - rect.min).length() < 1.5 && (r.rect.max - rect.max).length() < 1.5
            });
            if let Some(button) = button {
                assert_eq!(
                    (button.corner_radius.sw, button.corner_radius.se),
                    (0, 0),
                    "浮层还挂着的时候按钮下沿也得削着，实际 {:?} rect={:?}",
                    button.corner_radius,
                    button.rect
                );
            }
            seen.extend(popup);
        }

        assert!(!seen.is_empty(), "收起途中总该还看得见浮层");
        for r in &seen {
            assert_eq!(
                (r.corner_radius.nw, r.corner_radius.ne),
                (0, 0),
                "收起途中浮层上沿还得削着，实际 {:?} rect={:?}",
                r.corner_radius,
                r.rect
            );
        }

        // 收干净之后再走几帧，按钮得把四个角还回来 —— 一直直着就是修过头了。
        let mut settled = None;
        for _ in 0..8 {
            time += 0.016;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
                    time: Some(time),
                    ..Default::default()
                },
                |ui| {
                    let mut picked = 0;
                    combo(ui, "test-combo", "选项", 120.0, |ui| {
                        ui.selectable_value(&mut picked, 1, "一");
                        ui.selectable_value(&mut picked, 2, "二");
                    });
                },
            );
            out.textures_delta.clear();
            let mut all = Vec::new();
            for clipped in &out.shapes {
                rects(&clipped.shape, &mut all);
            }
            settled = all.iter().find(|r| {
                (r.rect.min - rect.min).length() < 1.5 && (r.rect.max - rect.max).length() < 1.5
            }).cloned();
        }
        let settled = settled.expect("按钮一直都在");
        assert!(
            settled.corner_radius.sw > 0 && settled.corner_radius.se > 0,
            "收完之后按钮下沿得圆回来，实际 {:?}",
            settled.corner_radius
        );
    }

    /// 点一下按钮就展开、点别处就收起 —— 这套开关逻辑是手写的，得自己验。
    #[test]
    fn the_combo_opens_on_a_click_and_closes_on_the_next_one() {
        let ctx = egui::Context::default();

        let (rect, area_id) = run_combo(&ctx, 0.0, Vec::new());
        assert!(ctx.memory(|m| m.area_rect(area_id)).is_none(), "还没点，不该有弹出层");

        // egui 的点击要跨两帧才认得出来：按下那一帧只登记，抬起时才判。
        let center = rect.center();
        let press = |pos: Pos2| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        let release = |pos: Pos2| {
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]
        };

        run_combo(&ctx, 0.016, press(center));
        run_combo(&ctx, 0.032, release(center));
        assert!(ctx.memory(|m| m.area_rect(area_id)).is_some(), "点完该弹出来");

        // 不动鼠标：开着就该一直开着，不会自己收。
        let mut time = 0.048;
        run_combo(&ctx, time, Vec::new());
        assert!(ctx.memory(|m| m.area_rect(area_id)).is_some());

        // 动画跑完之后，弹出层该老老实实贴在按钮正下方、左沿对齐。
        for _ in 0..20 {
            time += 0.016;
            run_combo(&ctx, time, Vec::new());
        }
        let popup = ctx.memory(|m| m.area_rect(area_id)).expect("该有弹出层");
        assert!(
            (popup.left() - rect.left()).abs() < 1.0,
            "左沿该和按钮对齐，实际 {} vs {}",
            popup.left(),
            rect.left()
        );
        assert!(
            (popup.top() - rect.bottom()).abs() < 1.0,
            "该紧贴按钮下沿（不重叠、不留缝），实际 {} vs {}",
            popup.top(),
            rect.bottom()
        );

        // 点窗口另一头：收起。收起是动画，所以这一帧还得画。
        let away = Pos2::new(380.0, 280.0);
        time += 0.016;
        run_combo(&ctx, time, press(away));
        time += 0.016;
        run_combo(&ctx, time, release(away));
        assert!(
            ctx.memory(|m| m.area_rect(area_id)).is_some(),
            "收起动画第一帧还得画，不然会硬切"
        );

        // 动画播完（0.17 秒）之后彻底消失。
        // 注意不能用 `area_rect` 判断：egui 的 `Areas` 只增不减，收起之后
        // 那个矩形还留在记忆里。要问的是「开关状态」和「还在不在画」。
        for _ in 0..20 {
            time += 0.016;
            run_combo(&ctx, time, Vec::new());
        }
        let open = ctx
            .data(|d| d.get_temp::<bool>(area_id.with("combo-open")))
            .unwrap_or(false);
        assert!(!open, "动画播完开关该是关着的");
    }

    /// 菜单里每一项都撑满整行、字居中 —— 不然下拉框一宽，字全挤在左边。
    #[test]
    fn a_combo_row_fills_the_width_and_centers_its_text() {
        let ctx = egui::Context::default();
        let mut row = Rect::NOTHING;
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 120.0))),
                time: Some(0.0),
                ..Default::default()
            },
            |ui| {
                ui.set_width(200.0);
                row = combo_item(ui, false, "一").rect;
            },
        );
        let mut center = f32::NAN;
        for clipped in &out.shapes {
            if let egui::Shape::Text(text) = &clipped.shape {
                center = text.pos.x + text.galley.size().x * 0.5;
            }
        }
        out.textures_delta.clear();

        assert!(
            (row.width() - 200.0).abs() < 0.5,
            "整行该铺满可用宽度 200，实际 {}",
            row.width()
        );
        assert!(
            (center - row.center().x).abs() < 0.5,
            "字心该落在行中线 {} 上，实际 {}",
            row.center().x,
            center
        );
    }

    /// 点中没选的那一项才写回去；点已经选中的那一项不该把值再翻一遍。
    #[test]
    fn a_combo_row_only_writes_back_when_the_choice_changes() {
        let ctx = egui::Context::default();

        /// 跑一帧两行菜单，返回（「二」那一行，选中值，有没有报 changed）。
        fn frame(
            ctx: &egui::Context,
            picked: i32,
            time: f64,
            events: Vec<egui::Event>,
        ) -> (Rect, i32, bool) {
            let mut row = Rect::NOTHING;
            let mut now = picked;
            let mut changed = false;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 120.0))),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.set_width(160.0);
                    combo_value(ui, &mut now, 1, "一");
                    let resp = combo_value(ui, &mut now, 2, "二");
                    row = resp.rect;
                    changed = resp.changed();
                },
            );
            out.textures_delta.clear();
            (row, now, changed)
        }

        // 第一帧只是把位置量出来（点要等鼠标动过才认）。
        let (row, picked, _) = frame(&ctx, 1, 0.0, Vec::new());
        assert_eq!(picked, 1);

        let at = row.center();
        let click = |pressed: bool| {
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };

        frame(&ctx, 1, 0.016, click(true));
        let (_, picked, changed) = frame(&ctx, 1, 0.032, click(false));
        assert!(changed, "点中「二」该报 changed");
        assert_eq!(picked, 2, "选中的值该换成 2");

        // 再点一次同一项：值没变，就不该再报一次 changed。
        frame(&ctx, 2, 0.048, click(true));
        let (_, picked, changed) = frame(&ctx, 2, 0.064, click(false));
        assert!(!changed, "选中的还是它，不该再报 changed");
        assert_eq!(picked, 2);
    }

    /// 按钮上的字默认顶格，居中的那颗要落在正中。
    #[test]
    fn a_centered_combo_puts_its_label_on_the_middle() {
        /// 跑一帧，返回（字心，期望的字心）。
        ///
        /// 居中对的是**文字区**——右边那截留给三角，所以期望值不是按钮中线，
        /// 而是按钮内边距之间、再让出三角那一截之后的中点。
        fn probe(centered: bool) -> (f32, f32) {
            let ctx = egui::Context::default();
            let mut rect = Rect::NOTHING;
            let mut expected = f32::NAN;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 120.0))),
                    time: Some(0.0),
                    ..Default::default()
                },
                |ui| {
                    let mut picked = 0;
                    let resp = if centered {
                        combo_centered(ui, "center-test", "全部版本", 160.0, |ui| {
                            combo_value(ui, &mut picked, 1, "一");
                        })
                    } else {
                        combo(ui, "center-test", "全部版本", 160.0, |ui| {
                            combo_value(ui, &mut picked, 1, "一");
                        })
                    };
                    rect = resp.rect;
                    let spacing = ui.spacing();
                    let pad = spacing.button_padding.x;
                    let inner: f32 = (160.0 - pad * 2.0 - spacing.icon_width).max(24.0);
                    expected = rect.left() + pad + inner * 0.5;
                },
            );
            let mut center = f32::NAN;
            for clipped in &out.shapes {
                if let egui::Shape::Text(text) = &clipped.shape {
                    center = text.pos.x + text.galley.size().x * 0.5;
                }
            }
            out.textures_delta.clear();
            (center, expected)
        }

        let (center, middle) = probe(true);
        assert!(
            (center - middle).abs() < 1.0,
            "居中的字该落在文字区中线 {} 上，实际 {}",
            middle,
            center
        );

        let (center, middle) = probe(false);
        assert!(
            center < middle - 10.0,
            "不居中的那颗还是顶格：字心 {} 不该靠近中线 {}",
            center,
            middle
        );
    }

    /// 拉出来这段：起手整块藏在按钮后面，收尾正好就位，中途不甩太远。
    #[test]
    fn the_pull_starts_hidden_behind_the_button_and_lands_on_the_spot() {
        // 下拉框：往下长，按钮下沿是 100。
        let down = Pull::new(100.0, true);
        let height = 180.0;

        // 起手要藏得够深：浮层顶端被推到按钮下沿之上（含余量），整块才真的在线里侧。
        let start = down.offset(0.0, height);
        assert!(
            start.y <= -(height + PULL_MARGIN),
            "起手该整块缩在按钮后面，实际位移 {}",
            start.y
        );

        // 收尾不偏不倚。
        assert_eq!(down.offset(1.0, height), Vec2::ZERO, "就位之后不偏不倚");

        // 往上弹的菜单是反方向。
        let up = Pull::new(100.0, false);
        assert!(up.offset(0.0, height).y >= height + PULL_MARGIN);
        assert_eq!(up.offset(1.0, height), Vec2::ZERO);

        // 走完一遍真实的时间线：单调往里收，一帧都不许往外退。
        let mut last = f32::NEG_INFINITY;
        for i in 0..=100 {
            let eased = egui::emath::easing::quadratic_in_out(i as f32 / 100.0);
            let y = down.offset(eased, height).y;
            assert!(y >= last, "滑动该是单调的，第 {} 步退回去了", i);
            last = y;
        }

        // 头几帧得真的「还没怎么动」：一开就窜出去一大截，看着就是硬切。
        let early = down.offset(egui::emath::easing::quadratic_in_out(0.1), height).y;
        assert!(
            early < -height * 0.85,
            "头一成时间该只挪了一点点（还基本藏着），实际已经到 {}",
            early
        );
    }

    /// 裁剪线钉在按钮那条边上：藏在里侧的那截被切掉，外侧的留着。
    #[test]
    fn the_pull_clips_everything_still_behind_the_button() {
        // 下拉框：按钮下沿在 100，浮层从那儿往下长。
        let below = Rect::from_min_max(Pos2::new(20.0, 100.0), Pos2::new(140.0, 300.0));
        let clip = Pull::new(100.0, true).clip(below);
        assert_eq!(clip.top(), 100.0, "按钮下沿以上该切掉");
        assert_eq!(clip.bottom(), below.bottom());

        // 往上弹的菜单：按钮上沿在 300，浮层从那儿往上长。
        let above = Rect::from_min_max(Pos2::new(20.0, 100.0), Pos2::new(140.0, 300.0));
        let clip = Pull::new(300.0, false).clip(above);
        assert_eq!(clip.bottom(), 300.0, "按钮上沿以下该切掉");
        assert_eq!(clip.top(), above.top());

        // 浮层被窗口挤到按钮另一边时，那条线退回浮层自己的边，不能把它切没了。
        let pushed = Rect::from_min_max(Pos2::new(20.0, 40.0), Pos2::new(140.0, 96.0));
        let clip = Pull::new(100.0, true).clip(pushed);
        assert!(clip.is_positive(), "被挤开之后不该整块被切掉");
        assert_eq!(clip.top(), pushed.top());
    }

    /// 拉出来的那几帧：浮层被按钮那条线切着，只露出线外侧的部分。
    ///
    /// 光验 `Pull` 自己不够 —— 真正决定观感的是「图形挪了、裁剪线没挪」这件事，
    /// 得看画出来的那一堆形状。
    #[test]
    fn the_pulled_layer_slides_out_from_behind_the_button() {
        let ctx = egui::Context::default();
        let area_id = Id::unique("pull-frame-area");
        let anim_id = Id::unique("pull-frame-anim");
        let color = Color32::from_rgb(0x12, 0x34, 0x56);

        /// 跑一帧，返回这一帧里那块已知颜色的矩形。
        fn panel(ctx: &egui::Context, area: Id, anim: Id, open: bool, time: f64, color: Color32) -> (Rect, Rect) {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
                    time: Some(time),
                    ..Default::default()
                },
                |ui| {
                    // 和下拉框一样：浮层紧贴按钮下沿（120），不留缝。
                    let layer = SpringLayer::new(area, anim, open, Pos2::new(20.0, 120.0))
                        .pivot(Align2::LEFT_TOP)
                        .width(160.0)
                        .pull_from(120.0, true);
                    spring_layer(ui.ctx(), layer, |ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::new(160.0, 120.0), Sense::hover());
                        ui.painter().rect_filled(rect, 0.0, color);
                    });
                },
            );
            out.textures_delta.clear();
            let mut painted = Rect::NOTHING;
            let mut clip = Rect::NOTHING;

            for shape in &out.shapes {
                if let Shape::Rect(r) = &shape.shape
                    && r.fill == color
                {
                    painted = r.rect;
                    clip = shape.clip_rect;
                }
            }
            (painted, clip)
        }

        // 先关一帧，把动画登记成 0。
        let mut time = 0.0_f64;
        panel(&ctx, area_id, anim_id, false, time, color);

        // 开起来。Area 第一帧要先量一遍尺寸（那一帧什么都不画），所以往下多走几步，
        // 找到第一个真画出来的帧。
        let mut first: Option<(Rect, Rect)> = None;
        for _ in 0..5 {
            time += 0.016;
            let (painted, clip) = panel(&ctx, area_id, anim_id, true, time, color);
            if painted.is_positive() {
                first = Some((painted, clip));
                break;
            }
        }
        let (painted, clip) = first.expect("开起来之后总得画出来");
        assert!(
            clip.top() >= 120.0 - 0.5,
            "按钮下沿以上该被切掉，实际裁剪线 {}",
            clip.top()
        );
        assert!(
            painted.top() < 120.0,
            "起手该还压在按钮后面，实际顶边 {}",
            painted.top()
        );

        // 一格一格往前推，中途总得有一帧是「露了一半、另一半还压在按钮里」。
        // （不能一步跳过去：egui 每帧最多推进一个 stable_dt。）
        let mut half_out = 0;
        for _ in 0..20 {
            time += 0.016;
            let (painted, clip) = panel(&ctx, area_id, anim_id, true, time, color);
            let at_rest = (painted.top() - 120.0).abs() < 1.0;
            if !at_rest {
                assert!(
                    clip.top() >= 120.0 - 0.5,
                    "拉的过程中裁剪线得钉在按钮下沿，实际 {}",
                    clip.top()
                );
            }
            if painted.top() < 120.0 && painted.bottom() > 120.0 {
                half_out += 1;
            }
        }
        assert!(half_out > 0, "拉出来的过程里该有几帧是半遮着的，一帧都没有说明是硬切");

        // 跑完：老老实实紧贴按钮下沿，不再被切。
        for _ in 0..40 {
            time += 0.016;
            panel(&ctx, area_id, anim_id, true, time, color);
        }
        let (painted, _) = panel(&ctx, area_id, anim_id, true, time, color);
        assert!(
            (painted.top() - 120.0).abs() < 1.0,
            "就位后该紧贴按钮下沿，实际顶边 {}",
            painted.top()
        );
        assert!(
            (painted.height() - 120.0).abs() < 1.0,
            "高度不该被裁掉，实际 {}",
            painted.height()
        );
    }

    /// 拉出来的时候，每块图形自己的裁剪框也得跟着一起挪。
    ///
    /// 下拉框的内容装在滚动区里，滚动区给列表项设的裁剪框比浮层小一圈。要是只挪
    /// 图形、不挪这层裁剪框，被滚出列表的那一截字就会被挪回可见区、露在白底外面，
    /// 而底板因为裁剪框和浮层一致反而被切光 —— 看起来就是「只有字、没有框」。
    #[test]
    fn a_clip_of_its_own_rides_along_with_the_shape() {
        let ctx = egui::Context::default();
        let area_id = Id::unique("pull-clip-area");
        let anim_id = Id::unique("pull-clip-anim");
        let color = Color32::from_rgb(0x12, 0x34, 0x56);

        /// 跑一帧，返回那块已知颜色的矩形和它自己的裁剪框。
        fn panel(
            ctx: &egui::Context,
            area: Id,
            anim: Id,
            open: bool,
            time: f64,
            color: Color32,
        ) -> Option<(Rect, Rect)> {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
                    time: Some(time),
                    ..Default::default()
                },
                |ui| {
                    let layer = SpringLayer::new(area, anim, open, Pos2::new(20.0, 120.0))
                        .pivot(Align2::LEFT_TOP)
                        .width(160.0)
                        .pull_from(120.0, true);
                    spring_layer(ui.ctx(), layer, |ui| {
                        // 和滚动区一样：给这一块单独收一圈裁剪框，比浮层小。
                        let mut child = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(Rect::from_min_size(Pos2::new(20.0, 120.0), Vec2::new(160.0, 120.0))),
                        );
                        let narrow = Rect::from_min_size(Pos2::new(20.0, 140.0), Vec2::new(160.0, 80.0));
                        child.set_clip_rect(narrow);
                        child.painter().rect_filled(narrow, 0.0, color);
                    });
                },
            );
            out.textures_delta.clear();
            for shape in &out.shapes {
                if let Shape::Rect(r) = &shape.shape
                    && r.fill == color
                    && r.rect.is_positive()
                {
                    return Some((r.rect, shape.clip_rect));
                }
            }
            None
        }

        let mut time = 0.0_f64;
        panel(&ctx, area_id, anim_id, false, time, color);

        // 收两帧中间态：偏移不同，但「图形和它自己裁剪框的相对位置」该一直不变。
        let mut seen: Vec<(Rect, Rect)> = Vec::new();
        for _ in 0..24 {
            time += 0.016;
            if let Some((painted, clip)) = panel(&ctx, area_id, anim_id, true, time, color) {
                seen.push((painted, clip));
            }
        }
        assert!(seen.len() >= 3, "动画期间该拍到几帧，实际 {}", seen.len());

        let first = seen[0];
        for (painted, clip) in &seen {
            assert!(
                ((painted.top() - clip.top()) - (first.0.top() - first.1.top())).abs() < 0.5,
                "裁剪框没跟着图形一起挪：矩形 {:?} 的裁剪框 {:?}，第一帧是 {:?} / {:?}",
                painted,
                clip,
                first.0,
                first.1
            );
        }

        // 而且要真的挪过：抓到的那几帧里至少有一帧还没站定（站定在 140），
        // 不然上面那圈断言全是空转。
        assert!(
            seen.iter().any(|(painted, _)| painted.top() < 140.0 - 0.5),
            "动画该把图形挪过一段，实际一路都在 140 没动",
        );
    }

    /// 缩放淡入的变换：起手缩到 94%、归位时严丝合缝。
    #[test]
    fn the_spring_transform_starts_small_and_settles_exactly() {
        let origin = Pos2::new(200.0, 260.0);

        let start = spring_transform(0.0, origin, 5.0);
        assert!(
            (start.scaling - 0.94).abs() < 1e-4,
            "起手该缩到 94%，实际 {}",
            start.scaling
        );
        // 往 origin 那边偏，再加上 drift 那一下。
        assert!(start.translation.length() > 1.0, "起手该离 origin 有一点距离");
        assert!(
            start.translation.y > start.translation.x,
            "drift 是竖直方向的那一份，该压过横向的偏移"
        );

        let end = spring_transform(1.0, origin, 5.0);
        assert!((end.scaling - 1.0).abs() < 1e-4, "归位时该是原大小");
        assert!(
            end.translation.length() < 1e-4,
            "归位时不该还偏着，实际 {:?}",
            end.translation
        );
    }

    /// 不裁剪的那条路（首页版本菜单）：动画期间整块始终完整，四角都在。
    ///
    /// 之前首页用裁剪式 pull，浮层底边被那条裁剪线整条切平，圆角就没了 —— 因为
    /// 按钮只有四十来像素宽，切出来的直边没人挡。这里钉住「每一帧的裁剪框都盖得住
    /// 整块浮层、四个角都还是圆的」这件事。
    #[test]
    fn a_drifting_layer_is_never_sliced_and_keeps_all_four_corners() {
        const RADIUS: u8 = 10;
        let ctx = egui::Context::default();
        let area_id = Id::unique("drift-frame-area");
        let anim_id = Id::unique("drift-frame-anim");
        let color = Color32::from_rgb(0x12, 0x34, 0x56);

        /// 跑一帧，返回这一帧里那块浮层的（矩形，裁剪框，圆角）。
        fn panel(
            ctx: &egui::Context,
            area: Id,
            anim: Id,
            open: bool,
            time: f64,
            color: Color32,
        ) -> Option<(Rect, Rect, CornerRadius)> {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
                    time: Some(time),
                    ..Default::default()
                },
                |ui| {
                    // 和首页一样：从按钮上沿往上长，不裁剪，缩放 + 淡入。
                    let layer = SpringLayer::new(area, anim, open, Pos2::new(200.0, 260.0))
                        .pivot(Align2::RIGHT_BOTTOM)
                        .width(160.0)
                        .drift(5.0);
                    spring_layer(ui.ctx(), layer, |ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::new(160.0, 120.0), Sense::hover());
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(RADIUS), color);
                    });
                },
            );
            out.textures_delta.clear();

            for shape in &out.shapes {
                if let Shape::Rect(r) = &shape.shape
                    && r.fill == color
                {
                    return Some((r.rect, shape.clip_rect, r.corner_radius));
                }
            }
            None
        }

        // 先关一帧，把动画登记成 0。
        let mut time = 0.0_f64;
        panel(&ctx, area_id, anim_id, false, time, color);

        // 一帧一帧往前推，把动画跑完。Area 头一帧只量尺寸、什么都不画，那种帧跳过。
        let mut frames = 0;
        // 还没就位的帧（缩放 ≠ 1）。有一帧就说明缩放真挂上去了。
        // 注意 back_out 是过冲曲线，这些帧的高度会略大于 120，不一定更小。
        let mut animated = 0;
        // 步子迈小一点：Area 头两帧只量尺寸不画，等真画出来的那一帧动画已经走了一段，
        // 用 16ms 一步会直接跳过缩得最小的那几帧。
        for _ in 0..120 {
            time += 0.004;
            let Some((rect, clip, corner)) = panel(&ctx, area_id, anim_id, true, time, color)
            else {
                continue;
            };
            frames += 1;
            if (rect.height() - 120.0).abs() > 0.5 {
                animated += 1;
            }

            // 四角都还在，而且是同一个半径 —— 有任何一角变成 0 就是被削平了。
            assert!(
                corner.nw > 0 && corner.nw == corner.ne && corner.nw == corner.sw && corner.nw == corner.se,
                "第 {frames} 帧四角不齐平：{corner:?}"
            );
            // 裁剪框必须盖得住整块浮层，盖不住就是被切了。
            assert!(
                clip.min.x <= rect.min.x + 0.01
                    && clip.min.y <= rect.min.y + 0.01
                    && clip.max.x >= rect.max.x - 0.01
                    && clip.max.y >= rect.max.y - 0.01,
                "第 {frames} 帧浮层被裁了：{rect:?} 对 {clip:?}"
            );
            // 等比缩放：宽高比不该变。
            let ratio = rect.width() / rect.height();
            assert!(
                (ratio - 160.0 / 120.0).abs() < 0.02,
                "第 {frames} 帧宽高比 {} 变了，说明被切过",
                ratio
            );
        }
        assert!(frames > 5, "动画该跑好几帧，实际只有 {frames} 帧");

        assert!(animated > 0, "一帧都没缩放动画的痕迹，说明变换根本没生效");

        // 跑完：老老实实占满 160×120、四角 10。
        for _ in 0..40 {
            time += 0.016;
            panel(&ctx, area_id, anim_id, true, time, color);
        }
        let (rest, _, rest_corner) =
            panel(&ctx, area_id, anim_id, true, time, color).expect("就位之后还得画着");
        assert_eq!(rest_corner, CornerRadius::same(RADIUS));
        assert!(
            (rest.width() - 160.0).abs() < 1.0,
            "就位后该占满宽度，实际 {}",
            rest.width()
        );
        assert!(
            (rest.height() - 120.0).abs() < 1.0,
            "就位后该占满高度，实际 {}",
            rest.height()
        );
    }

    /// 收起动画播完之后就不该再画了，否则那一层会一直挂在窗口上。
    #[test]
    fn a_closed_spring_layer_stops_drawing_once_the_animation_ends() {
        let ctx = egui::Context::default();
        let area_id = Id::unique("spring-test-area");
        let anim_id = Id::unique("spring-test-anim");

        let mut drawn = 0;
        let mut time = 0.0_f64;
        for _ in 0..30 {
            time += 0.016;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
                    time: Some(time),
                    ..Default::default()
                },
                |ui| {
                    let layer = SpringLayer::new(area_id, anim_id, false, Pos2::new(10.0, 10.0));
                    if spring_layer(ui.ctx(), layer, |ui| {
                        ui.label("浮层");
                    })
                    .is_some()
                    {
                        drawn += 1;
                    }
                },
            );
            out.textures_delta.clear();
        }
        assert_eq!(drawn, 0, "本来就没开过，一帧都不该画");
    }

    /// 版本行末尾那两枚图标键：点图标只算图标，点行别处才选中整行。
    ///
    /// 这两件事靠的是**注册顺序** —— 距离相等时 egui 取最后登记的那个。
    /// 所以这条用例真正钉住的是「图标画在行之后」：谁先画谁就被盖住。
    #[test]
    fn a_row_icon_swallows_the_click_before_the_row_does() {
        /// 一帧跑下来：三处点击各自有没有认，外加三个矩形。
        fn frame(ctx: &egui::Context, time: f64, events: Vec<egui::Event>) -> ((bool, bool, bool), (Rect, Rect, Rect)) {
            let mut hits = (false, false, false);
            let mut rects = (Rect::NOTHING, Rect::NOTHING, Rect::NOTHING);
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(420.0, 120.0))),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let row = version_item(ui, "1.20.1", None, false, ROW_ICONS_RESERVE);
                    let side = ROW_ICON_SIDE;
                    let center_y = row.rect.center().y;
                    let delete = Rect::from_center_size(
                        Pos2::new(row.rect.right() - 14.0 - side * 0.5, center_y),
                        Vec2::splat(side),
                    );
                    let manage = Rect::from_center_size(
                        Pos2::new(delete.left() - ROW_ICON_GAP - side * 0.5, center_y),
                        Vec2::splat(side),
                    );
                    let manage_resp = row_icon_button(
                        ui,
                        manage,
                        ("manage-version", 0),
                        NavGlyph::Settings,
                        true,
                        false,
                    );
                    let delete_resp = row_icon_button(
                        ui,
                        delete,
                        ("delete-version", 0),
                        NavGlyph::Trash,
                        true,
                        true,
                    );
                    hits = (row.clicked(), manage_resp.clicked(), delete_resp.clicked());
                    rects = (row.rect, manage, delete);
                },
            );
            out.textures_delta.clear();
            (hits, rects)
        }

        /// 在 `at` 上按下再抬起，返回这一下被谁认下了。
        ///
        /// 每次点击都要一只新的 `Context`：egui 的输入历史不许时间倒退，
        /// 同一只 ctx 上从头再跑一遍会当场 panic。
        fn click_at(at: Pos2) -> (bool, bool, bool) {
            let ctx = egui::Context::default();
            frame(&ctx, 0.0, Vec::new());
            let mut hits = (false, false, false);
            for (n, pressed) in [true, false].into_iter().enumerate() {
                let events = vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ];
                let (this, _) = frame(&ctx, 0.016 * (n as f64 + 1.0), events);
                hits = (hits.0 || this.0, hits.1 || this.1, hits.2 || this.2);
            }
            hits
        }

        let ctx = egui::Context::default();
        let (_, (row, manage, delete)) = frame(&ctx, 0.0, Vec::new());
        assert!(row.width() > 200.0);
        // 齿轮：只有管理响，整行不该跟着被选中。
        let hits = click_at(manage.center());
        assert_eq!(hits, (false, true, false), "点齿轮只该进管理页");
        // 垃圾桶：同理，而且不能顺手把整行也选上。
        let hits = click_at(delete.center());
        assert_eq!(hits, (false, false, true), "点垃圾桶只该走删除");
        // 行本身（离图标足够远的地方）：照旧整行可点。
        let hits = click_at(Pos2::new(row.left() + 60.0, row.center().y));
        assert_eq!(hits, (true, false, false), "行的别处仍然选中整行");
    }
    /// 首页那颗齿轮：点得着；没有版本可选时点不着。
    ///
    /// 它自己只负责「有没有被点」这一件事，跳转到管理页是调用方的事，
    /// 所以这里只验手感：可点时认得出点击、禁用时彻底收不到。
    #[test]
    fn the_gear_reacts_to_clicks_only_when_it_is_enabled() {
        /// 空跑一帧拿到齿轮的矩形：点击要落在它身上，而位置得先问一次才知道。
        fn gear_rect(ctx: &egui::Context, enabled: bool) -> Rect {
            let mut rect = Rect::NOTHING;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 100.0))),
                    time: Some(0.0),
                    ..Default::default()
                },
                |ui| rect = gear_button(ui, enabled).rect,
            );
            out.textures_delta.clear();
            rect
        }

        /// 在 `at` 上按下再抬起，返回这两帧里齿轮有没有被认成点击。
        fn clicked_at(ctx: &egui::Context, at: Pos2, enabled: bool, start: f64) -> bool {
            let mut clicked = false;
            let mut time = start;
            for pressed in [true, false] {
                time += 0.016;
                let events = vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ];
                let mut out = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 100.0))),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ui| clicked |= gear_button(ui, enabled).clicked(),
                );
                out.textures_delta.clear();
            }
            clicked
        }

        let ctx = egui::Context::default();
        let center = gear_rect(&ctx, true).center();
        assert!(
            clicked_at(&ctx, center, true, 0.032),
            "可点的齿轮该认得出这一下"
        );

        // 禁用：同样的两帧下去，一声都不该响。
        let ctx = egui::Context::default();
        let center = gear_rect(&ctx, false).center();
        assert!(
            !clicked_at(&ctx, center, false, 0.032),
            "没选版本时齿轮是灰的，点了也不该有反应"
        );
    }

    /// 管理页那一栏的字围着中轴排：给同一个字框，居中的应当正好落在正中。
    ///
    /// 账户页那栏不走居中（顶上还有一行头像），所以两种对齐都得留着 ——
    /// 这条用例同时把「另一种确实没被顺手改掉」钉住。
    #[test]
    fn a_centered_nav_row_puts_its_text_on_the_middle() {
        /// 跑一帧，返回（文字横向中心，这一行的横向中心）。
        fn probe(centered: bool) -> (f32, f32) {
            let ctx = egui::Context::default();
            let mut row = Rect::NOTHING;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 60.0))),
                    time: Some(0.0),
                    ..Default::default()
                },
                |ui| {
                    ui.set_width(160.0);
                    row = nav_item(ui, "模组 0", false, centered).rect;
                },
            );
            let mut center = f32::NAN;
            for clipped in &out.shapes {
                if let egui::Shape::Text(text) = &clipped.shape {
                    center = text.pos.x + text.galley.size().x * 0.5;
                }
            }
            out.textures_delta.clear();
            (center, row.center().x)
        }

        let (center, row_center) = probe(true);
        assert!(
            (center - row_center).abs() < 0.5,
            "居中的字该落在行中线 {} 上，实际 {}",
            row_center,
            center
        );

        let (center, row_center) = probe(false);
        assert!(
            center < row_center - 10.0,
            "不居中的那栏还是顶格：字心 {} 不该靠近行中线 {}",
            center,
            row_center
        );
    }

    /// 返回按钮：指针压上去要换底、点一下要认得出，悬停提示挂在调用方。
    #[test]
    fn the_back_button_lights_up_under_the_pointer_and_answers_a_click() {
        /// 跑一帧，返回（按钮矩形，底色，认没认成点击）。
        fn frame(
            ctx: &egui::Context,
            at: Option<Pos2>,
            pressed: Option<bool>,
            time: f64,
        ) -> (Rect, Option<Color32>, bool) {
            let mut events = Vec::new();
            if let Some(pos) = at {
                events.push(egui::Event::PointerMoved(pos));
            }
            if let (Some(pos), Some(pressed)) = (at, pressed) {
                events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            let mut rect = Rect::NOTHING;
            let mut clicked = false;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(120.0, 60.0))),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let resp = back_button(ui);
                    rect = resp.rect;
                    clicked = resp.clicked();
                },
            );
            // 圆角框是这一层里唯一的矩形，底色从它身上取。
            let mut fill = None;
            for clipped in &out.shapes {
                if let egui::Shape::Rect(r) = &clipped.shape {
                    fill = Some(r.fill);
                }
            }
            out.textures_delta.clear();
            (rect, fill, clicked)
        }

        let ctx = egui::Context::default();
        let (rect, idle, _) = frame(&ctx, None, None, 0.0);
        let (_, hovered, _) = frame(&ctx, Some(rect.center()), None, 0.016);
        assert_eq!(idle, Some(card_fill()), "没指针时是白底");
        assert_eq!(hovered, Some(accent_soft()), "指针压上来该换成淡绿底");

        // 按下、抬起两帧：第二帧才认得出这一下。
        let center = rect.center();
        frame(&ctx, Some(center), Some(true), 0.032);
        let (_, _, clicked) = frame(&ctx, Some(center), Some(false), 0.048);
        assert!(clicked, "点下去该返回上一页");
    }

    /// 确认框的底板与两颗键都得真画出来，而且什么都没按时不该有答案。
    ///
    /// 这条用例防的是「弹窗还在、按钮没了」这类只在真窗口里才看得出的坏法：
    /// 两颗键包在 `horizontal` 里，布局一改就可能整排被挤没，
    /// 而点空只是没反应，肉眼根本看不出坏了。
    #[test]
    fn the_confirm_dialog_paints_its_buttons_and_stays_pending_until_pressed() {
        let ctx = egui::Context::default();
        let mut answer = Answer::Confirmed;
        let mut buttons = 0;

        // 头一帧只量尺寸、什么都不画，得多跑几帧才看得到按钮。
        for step in 0..4 {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(500.0, 400.0))),
                    time: Some(step as f64 * 0.016),
                    ..Default::default()
                },
                |ui| {
                    answer = confirm_dialog(
                        ui.ctx(),
                        false,
                        Confirm {
                            id: "aicraft-test-confirm",
                            title: "删除此版本？",
                            subject: Some("26.3"),
                            lines: &["删了就没了。"],
                            danger: true,
                            warn: true,
                            confirm: "确认删除",
                            width: 380.0,
                        },
                    );
                },
            );
            out.textures_delta.clear();

            // 两颗键都是 DIALOG_BUTTON_HEIGHT 高；取最后一帧数。
            buttons = 0;
            for shape in &out.shapes {
                if let Shape::Rect(r) = &shape.shape
                    && (r.rect.height() - DIALOG_BUTTON_HEIGHT).abs() < 0.5
                {
                    buttons += 1;
                }
            }
        }

        assert_eq!(answer, Answer::Pending, "一帧都没点，不该有答案");
        assert_eq!(buttons, 2, "取消与确认两颗键都得画出来，实际 {buttons} 颗");
    }
}
