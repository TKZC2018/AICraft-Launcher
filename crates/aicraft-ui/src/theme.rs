//! 配色与公共绘制件。
//!
//! 拆成单独模块，是为了让 `main.rs` 继续只管状态和流程：长什么样都收在这里，
//! 改配色时不用在几百行界面代码里翻颜色值。
//!
//! 现在的方向是「亮色 + 柔和层次」：近白的背景、白卡片、极细描边与一层
//! 很轻的投影撑出层次感，强调色仍是那抹薄荷绿，但放在亮底上要更深一点
//! （`ACCENT_STRONG`）才够清晰。

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Response,
    RichText, Sense, Stroke, StrokeKind, Ui, Vec2,
};

// ---- 色板 ----
/// 面板底色（顶栏、侧栏、标题栏）
pub const PANEL: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// 中央区域的底色
pub const BG_BOTTOM: Color32 = Color32::from_rgb(0xED, 0xF1, 0xF7);
/// 卡片底色
pub const CARD: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// 鼠标悬停时的浅色块
pub const CARD_HOVER: Color32 = Color32::from_rgb(0xF3, 0xF6, 0xFA);
/// 描边：比背景深一点点就够
pub const BORDER: Color32 = Color32::from_rgb(0xE2, 0xE8, 0xF0);
/// 强调色（按钮填充）
pub const ACCENT: Color32 = Color32::from_rgb(0x10, 0xB9, 0x81);
/// 强调色的浅底（选中行、选中背景）
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0xE6, 0xF7, 0xF0);
/// 亮底上可读的强调色文字
pub const ACCENT_STRONG: Color32 = Color32::from_rgb(0x04, 0x7E, 0x5A);
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
pub fn install(ctx: &egui::Context) {
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
    v.hyperlink_color = ACCENT_STRONG;
    v.selection.bg_fill = ACCENT_SOFT;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
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
    v.widgets.active.weak_bg_fill = ACCENT_SOFT;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT_STRONG);
    v.widgets.active.corner_radius = CornerRadius::same(9);

    v.widgets.open = v.widgets.active;
    v
}

/// 中央区域背景：一整片浅灰蓝，不做渐变。
///
/// 早先这里铺的是一道「上浅下深」的渐变，结果上半截偏白、下半截偏灰蓝，
/// 空页面看着像分成了两块不同颜色的区域。统一成一个底色之后，白卡片
/// 浮在上面才干净。
pub fn paint_background(ui: &Ui) {
    let rect = ui.max_rect();
    if !rect.is_positive() {
        return;
    }
    ui.painter().rect_filled(rect, 0.0, BG_BOTTOM);
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
        .fill(CARD)
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
        .fill(Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 150))
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
        .fill(CARD)
        .stroke(Stroke::new(1.0, ACCENT))
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
    let color = ACCENT_STRONG;
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
    let color = if picked { ACCENT_STRONG } else { TEXT };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let icon_size = 14.0;
    let size = Vec2::new(
        icon_size + 6.0 + galley.size().x + 22.0,
        galley.size().y.max(icon_size) + 12.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let fill = if picked {
        ACCENT_SOFT
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
            Stroke::new(1.0, ACCENT),
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
        painter.circle_filled(center, DOT / 2.0, ACCENT);
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
            Stroke::new(TRACK_H, ACCENT),
        );
    }
    let knob = egui::Pos2::new(fill_right, track.center().y);
    let knob_color = if enabled {
        ACCENT
    } else {
        Color32::from_rgb(0xC8, 0xD3, 0xE0)
    };
    painter.circle_filled(knob, KNOB / 2.0, knob_color);

    if enabled && (response.dragged() || response.clicked()) {
        if let Some(pos) = response.interact_pointer_pos() {
            let next = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
            if (next - *value).abs() > f32::EPSILON {
                *value = next;
                response.mark_changed();
            }
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
/// `loader` 是挂的加载器（`None` = 纯原版）。它同时决定行首那枚 30px 的
/// 版本图标 —— 一列排下来，哪几个是 Fabric、哪个是 Forge，看形状就分开了，
/// 不必逐行读名字。名字右边只留一小截加载器名做补充（图标认不出时的兜底）。
///
/// 每一行自己就是一张白卡片：圆角 + 细边，浮在浅灰蓝的页面底上。
/// 选中时描边换成主题绿、底色转成浅绿，跟目录列表里那张「当前」卡片同一套语言。
pub fn version_item(
    ui: &mut Ui,
    name: &str,
    java: Option<u32>,
    loader: Option<aicraft_core::loader::LoaderKind>,
    selected: bool,
) -> Response {
    // 30px 图标塞进 44px 的行里会显得顶格，行高提到 52 才有呼吸感。
    let height = 52.0;
    let (slot, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    // 行与行之间留一点缝，卡片才不会连成一片。
    let rect = slot.shrink2(Vec2::new(0.0, 1.5));

    let fill = if selected {
        ACCENT_SOFT
    } else if response.hovered() {
        CARD_HOVER
    } else {
        CARD
    };
    ui.painter().rect_filled(rect, CornerRadius::same(9), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(9),
        Stroke::new(1.0, if selected { ACCENT } else { BORDER }),
        StrokeKind::Inside,
    );

    let text_color = if selected { ACCENT_STRONG } else { TEXT };
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
        // 名字太长时会挤到右边那组属性上，那就干脆不画：
        // 宁可少一枚标签，也不要两段文字叠在一起。
        let right_limit = inner.right() - 90.0;
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

    let java_text = match java {
        Some(major) => format!("Java {}", major),
        None => "Java ?".to_string(),
    };
    ui.painter().text(
        inner.right_center(),
        Align2::RIGHT_CENTER,
        java_text,
        FontId::new(11.0, egui::FontFamily::Proportional),
        if selected { ACCENT_STRONG } else { TEXT_WEAK },
    );

    if selected {
        let bar = egui::Rect::from_min_size(
            Pos2::new(rect.left() + 3.0, rect.top() + 14.0),
            Vec2::new(3.0, height - 28.0),
        );
        ui.painter().rect_filled(bar, CornerRadius::same(2), ACCENT);
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
            .color(Color32::WHITE),
    )
    .fill(ACCENT)
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(10));
    fixed_button(
        ui,
        button,
        enabled,
        Vec2::new(min_width, DIALOG_BUTTON_HEIGHT),
    )
}

/// 弹窗里的次要按钮（取消 / 以后再说）。
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

pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let button = egui::Button::new(
        RichText::new(text)
            .size(14.0)
            .strong()
            .color(Color32::WHITE),
    )
    .fill(ACCENT)
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
            .color(Color32::WHITE),
    )
    .fill(ACCENT)
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(12));
    fixed_button(ui, button, enabled, Vec2::new(width, 52.0))
}

/// 首页那颗按钮旁边的小按钮：一个方角、三条横线，点开/悬停时把版本列表叫出来。
///
/// 尺寸和 `launch_button` 一样高（52），并排放在一起才不显高低。
/// `active` 是「菜单正开着」，这时底色与线条都提亮一档，让人知道列表是从这儿出来的。
pub fn menu_button(ui: &mut Ui, active: bool) -> Response {
    let size = Vec2::new(52.0, 52.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let lit = active || response.hovered();
    let fill = if lit { ACCENT_SOFT } else { CARD };
    let stroke = if lit { ACCENT } else { BORDER };
    ui.painter().rect(
        rect,
        CornerRadius::same(12),
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );

    let color = if lit { ACCENT_STRONG } else { TEXT_WEAK };
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
        ACCENT_SOFT
    } else {
        Color32::TRANSPARENT
    };
    let edge = if lit { ACCENT } else { BORDER };
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
        ACCENT_STRONG
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
        ACCENT_SOFT
    } else {
        Color32::TRANSPARENT
    };
    let edge = if lit { ACCENT } else { BORDER };
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
        ACCENT_STRONG
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
        ui.painter().rect_filled(rect, 6.0, ACCENT_SOFT);
    }

    let color = if !enabled {
        TEXT_FAINT
    } else if response.hovered() {
        ACCENT_STRONG
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

/// 账户页左栏里的一行账号：名字 + 类型（离线 / 正版 / 第三方）。
///
/// 比 `nav_item` 多一行小字，所以单独画：列表里要一眼看出
/// 「这个名字是离线的」还是「这个是皮肤站的」，否则玩家切错账号
/// 还会以为启动器把令牌弄丢了。
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
            // 脸就还是 `rect` 那么大 —— 多出来的那圈壳自然溢出到外面。
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
            painter.rect_filled(rect, corner, ACCENT_SOFT);
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
                ACCENT_STRONG,
            );
        }
    }
}

pub fn account_row(
    ui: &mut Ui,
    name: &str,
    kind: &str,
    selected: bool,
    skin: Option<&egui::TextureHandle>,
) -> AccountRow {
    let width = ui.available_width();
    let height = 42.0;
    let (rect, row) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    if selected {
        ui.painter().rect_filled(
            rect.shrink2(Vec2::new(0.0, 1.5)),
            CornerRadius::same(9),
            ACCENT_SOFT,
        );
        // 左侧竖条：和版本列表里选中那一行同一套视觉语言
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.top() + 7.0),
            Vec2::new(3.0, height - 14.0),
        );
        ui.painter().rect_filled(bar, CornerRadius::same(2), ACCENT);
    } else if row.hovered() {
        ui.painter().rect_filled(
            rect.shrink2(Vec2::new(0.0, 1.5)),
            CornerRadius::same(9),
            CARD_HOVER,
        );
    }

    // 右侧那个「×」：常显但很淡，指针放上去才变成警示色。
    // 常显是有意的——埋进悬停里的话，玩家根本不知道这里能删。
    let delete_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 17.0, rect.center().y),
        Vec2::splat(18.0),
    );
    let delete = ui.interact(
        delete_rect,
        ui.make_persistent_id(("acct-delete", name)),
        Sense::click(),
    );
    if delete.hovered() {
        ui.painter()
            .circle_filled(delete_rect.center(), 9.0, DANGER.gamma_multiply(0.12));
    }
    let cross_color = if delete.hovered() { DANGER } else { TEXT_FAINT };
    let arm = 3.6;
    let center = delete_rect.center();
    let stroke = Stroke::new(1.4, cross_color);
    ui.painter().line_segment(
        [
            egui::pos2(center.x - arm, center.y - arm),
            egui::pos2(center.x + arm, center.y + arm),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            egui::pos2(center.x + arm, center.y - arm),
            egui::pos2(center.x - arm, center.y + arm),
        ],
        stroke,
    );

    // 头像贴在左边，文字整体往右让出来
    let avatar_size = 26.0;
    let avatar_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 12.0 + avatar_size / 2.0, rect.center().y),
        Vec2::splat(avatar_size),
    );
    draw_avatar(ui, avatar_rect, name, skin);

    let inner = egui::Rect::from_min_max(
        egui::pos2(avatar_rect.right() + 10.0, rect.top()),
        egui::pos2(rect.right() - 12.0, rect.bottom()),
    );
    // 名字长了要裁掉，否则会画到「×」上面去
    let text_painter = ui.painter().with_clip_rect(egui::Rect::from_min_max(
        inner.left_top(),
        egui::pos2(delete_rect.left() - 6.0, rect.bottom()),
    ));
    let text_color = if selected { ACCENT_STRONG } else { TEXT };
    text_painter.text(
        inner.left_top() + Vec2::new(0.0, 6.0),
        Align2::LEFT_TOP,
        name,
        FontId::new(12.5, egui::FontFamily::Proportional),
        text_color,
    );
    text_painter.text(
        inner.left_bottom() - Vec2::new(0.0, 6.0),
        Align2::LEFT_BOTTOM,
        kind,
        FontId::new(10.0, egui::FontFamily::Proportional),
        if selected { ACCENT } else { TEXT_FAINT },
    );

    AccountRow { row, delete }
}

/// [`account_row`] 画出来的两处可点区域。
///
/// 分成两个 `Response` 而不是一个：点名字是「切过去」，点「×」是
/// 「删掉」，两件事落点重合只会在小屏幕上互相抢点击。
pub struct AccountRow {
    /// 整行的点击：切换到这个账号。
    pub row: Response,
    /// 右侧那个「×」。
    pub delete: Response,
}

/// 左侧导航的一项：选中时浅绿底 + 深绿字，平时只是灰字。
pub fn nav_item(ui: &mut Ui, text: &str, selected: bool) -> Response {
    let height = 34.0;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());

    let fill = if selected {
        ACCENT_SOFT
    } else if response.hovered() {
        CARD_HOVER
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    }

    let color = if selected { ACCENT_STRONG } else { TEXT_WEAK };
    ui.painter().text(
        rect.left_center() + Vec2::new(14.0, 0.0),
        Align2::LEFT_CENTER,
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
        ACCENT_SOFT
    } else if response.hovered() {
        CARD_HOVER
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
    }
    let pos = rect.center() - galley.size() / 2.0;
    let color = if selected { ACCENT_STRONG } else { TEXT };
    ui.painter().galley(pos, galley, color);

    hand(response)
}

/// 侧边栏图标：一个圆角方框，靠左一道竖线。
///
/// 这就是「这一栏可以收起来」的通用记号——读者在编辑器、文件管理器里
/// 见过无数次，不用学。早先用的是 `«` / `»` 两个书名号，13px 下只剩
/// 两道细尖线，而且方向容易被读成「翻页」而不是「收栏」。
fn sidebar_glyph(painter: &egui::Painter, rect: Rect, color: Color32) {
    let stroke = Stroke::new(1.4, color);
    // 描边压在界线内侧，否则方框会比同高度的文字大一圈、显得松散
    painter.rect_stroke(rect, CornerRadius::same(4), stroke, StrokeKind::Inside);
    let split = rect.left() + rect.width() * 0.36;
    painter.line_segment(
        [
            Pos2::new(split, rect.top() + 1.2),
            Pos2::new(split, rect.bottom() - 1.2),
        ],
        stroke,
    );
}

/// 导航底部的收起 / 展开开关。
///
/// 展开时「图标 + 收起」靠左，和上面那些导航项对齐；收起时只剩图标居中。
/// 两种状态画同一个图标——它表示的是「面板」本身，不是方向。
pub fn nav_toggle(ui: &mut Ui, collapsed: bool) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());

    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), CARD_HOVER);
    }

    let color = if response.hovered() {
        ACCENT_STRONG
    } else {
        TEXT_WEAK
    };
    const GLYPH: f32 = 15.0;
    if collapsed {
        sidebar_glyph(
            ui.painter(),
            Rect::from_center_size(rect.center(), Vec2::splat(GLYPH)),
            color,
        );
    } else {
        let glyph = Rect::from_min_size(
            Pos2::new(rect.left() + 14.0, rect.center().y - GLYPH / 2.0),
            Vec2::splat(GLYPH),
        );
        sidebar_glyph(ui.painter(), glyph, color);
        ui.painter().text(
            Pos2::new(glyph.right() + 8.0, rect.center().y),
            Align2::LEFT_CENTER,
            "收起",
            FontId::new(13.0, egui::FontFamily::Proportional),
            color,
        );
    }

    hand(response)
}

/// 收起状态下的导航项：只画一个字，居中。省掉文字但不省掉可点区域。
pub fn nav_item_icon(ui: &mut Ui, symbol: &str, selected: bool) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());

    let fill = if selected {
        ACCENT_SOFT
    } else if response.hovered() {
        CARD_HOVER
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    }

    let color = if selected { ACCENT_STRONG } else { TEXT_WEAK };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        symbol,
        FontId::new(15.0, egui::FontFamily::Proportional),
        color,
    );

    hand(response)
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
pub fn nav_item_sub(ui: &mut Ui, text: &str, selected: bool) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());

    let fill = if selected {
        ACCENT_SOFT
    } else if response.hovered() {
        CARD_HOVER
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    }

    let color = if selected { ACCENT_STRONG } else { TEXT_WEAK };
    ui.painter().text(
        rect.left_center() + Vec2::new(26.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        FontId::new(12.5, egui::FontFamily::Proportional),
        color,
    );

    hand(response)
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
