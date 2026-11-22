//! 账户页里那张能展开的卡片：皮肤预览、换皮肤、换披风。
//!
//! 联网的部分都在这里，但一次只跑一件事：读档案、传皮肤、换披风都是
//! 几秒级的事，排队比并发好解释 —— 卡片上只转一个圈，不会同时转三个。
//!
//! 两处刻意的取舍：
//!
//! - 只服务正版账号。皮肤站（authlib-injector 那一系）用的是另一套端点，
//!   字段名和权限判定都不同，猜错一次就会把别人的皮肤站写坏。遇到这类
//!   账号，卡片里只留一句话，不摆按钮。
//! - 披风只能从已拥有的里挑。官方接口不接受上传披风图，所以这里列的是
//!   账号身上那几件，点一下就换。

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};

use egui::{Sense, Vec2};

use aicraft_core::auth::store::StoredAccount;
use aicraft_core::skinservice::{self, Variant, Wardrobe};

use crate::theme;

/// 一张展开的卡片要做的事。闭包里改不了 App，所以先记下来。
#[derive(PartialEq, Eq, Debug)]
pub enum Action {
    /// 切换成这个账号（进游戏就用它）
    Use,
    /// 展开 / 收起这一张
    Toggle,
    /// 续一次期
    Refresh,
    /// 从本机删掉这个账号
    Remove,
    /// 挑一张本地皮肤图（挑完由外面接着调 upload）
    Pick,
    /// 把皮肤恢复成原版默认那张
    ResetSkin,
    /// 上一次读档案失败了，再试一次
    Retry,
    /// 换披风；空串表示摘掉
    Cape(String),
}

/// 这个账号为什么不能在这里改材质；能改就是 None。
///
/// 两种都拦下来：离线名字背后压根没有服务器；皮肤站的端点又是另一套。
/// 界面上把按钮收掉，比按下去再弹一句「服务器拒绝了」体面得多。
pub fn not_editable(account: &StoredAccount) -> Option<&'static str> {
    if account.offline {
        return Some("离线名字用的是原版默认皮肤，不能换成别的图。");
    }
    if !skinservice::supports(account.api_root.as_deref()) {
        return Some("皮肤站账号的材质要在皮肤站自己的网站上改，这里不代办。");
    }
    None
}

/// 一次请求拿回来的东西：档案 + 那张皮肤的原图。
struct Snapshot {
    wardrobe: Wardrobe,
    /// (宽, 高, RGBA)。取不到就留空，卡片上画个占位。
    skin: Option<(usize, usize, Vec<u8>)>,
    /// 每一件披风的缩略图，连同它的名字与是否穿着。
    ///
    /// 一起在后台拉完：一张披风图只有几 KB，八件也就是几次往返。
    /// 分成「先列名字、点开才加载图」的话，玩家点了才看见一片灰框，
    /// 还不如展开时多等半秒。
    capes: Vec<CapeArt>,
}

/// 一件披风：名字、是否穿着、以及裁好的缩略图。
///
/// 缩略图是裁过的：披风贴图四周大半是透明，留着会让图案缩成一粒米。
pub struct CapeArt {
    pub id: String,
    pub label: String,
    pub active: bool,
    /// (宽, 高, RGBA)；图没取到就是 None，界面上退成一个色块。
    pixels: Option<(usize, usize, Vec<u8>)>,
}

impl CapeArt {
    /// 宽高比。披风是「一块长方形的布」，缩略图得按这个比例裁，
    /// 硬塞进正方形会把整块布压扁成一格色块。
    pub fn aspect(&self) -> f32 {
        match self.pixels.as_ref() {
            Some((width, height, _)) if *height > 0 => *width as f32 / *height as f32,
            _ => 1.0,
        }
    }
}

enum Reply {
    Loaded(Result<Snapshot, String>),
    Changed(Result<Snapshot, String>),
}

/// 一个账号的衣柜。
pub struct Room {
    /// 请求在飞
    pub loading: bool,
    /// 账号现在穿着什么
    pub wardrobe: Option<Wardrobe>,
    /// 上一次读取失败的原因
    pub error: Option<String>,
    /// 一句提示：(话, 是不是坏消息)
    pub note: Option<(String, bool)>,
    /// 这张卡片是不是展开着
    pub expanded: bool,
    /// 上传皮肤时选的模型
    pub variant: Variant,
    skin: Option<(usize, usize, Vec<u8>)>,
    /// 由 skin 生成的纹理；换皮肤后置空让它重建
    tex: Option<egui::TextureHandle>,
    capes: Vec<CapeArt>,
    /// 披风 id → 缩略图纹理。换过披风就整张清掉重烤。
    cape_tex: HashMap<String, egui::TextureHandle>,
}

impl Default for Room {
    fn default() -> Room {
        Room {
            loading: false,
            wardrobe: None,
            error: None,
            note: None,
            expanded: false,
            variant: Variant::Classic,
            skin: None,
            tex: None,
            capes: Vec::new(),
            cape_tex: HashMap::new(),
        }
    }
}

impl Room {
    fn put(&mut self, snapshot: Snapshot) {
        self.loading = false;
        self.error = None;
        self.skin = snapshot.skin;
        // 单选框跟着身上这张皮肤走：换完手一抖点了别的模型，
        // 再上传出去就不是原来那个人了。读回来先对齐一次。
        if let Some(worn) = snapshot.wardrobe.active_skin().and_then(|skin| skin.variant) {
            self.variant = worn;
        }
        self.wardrobe = Some(snapshot.wardrobe);
        self.capes = snapshot.capes;
        // 换了皮肤就要重画：纹理是照着上一张图烤出来的
        self.tex = None;
        self.cape_tex.clear();
    }

    fn fail(&mut self, message: String) {
        self.loading = false;
        self.error = Some(message);
        self.tex = None;
        self.cape_tex.clear();
    }
}

#[derive(Default)]
pub struct State {
    rooms: HashMap<String, Room>,
    /// 现在展开着哪个账号
    pub active: Option<String>,
    rx: Option<Receiver<(String, Reply)>>,
}

impl State {
    pub fn new() -> State {
        State::default()
    }

    /// 取（必要时新建）某个账号的衣柜。
    pub fn room(&mut self, uuid: &str) -> &mut Room {
        self.rooms.entry(uuid.to_string()).or_default()
    }

    pub fn room_ref(&self, uuid: &str) -> Option<&Room> {
        self.rooms.get(uuid)
    }

    /// 有没有请求在飞。
    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    pub fn is_expanded(&self, uuid: &str) -> bool {
        self.rooms
            .get(uuid)
            .map(|room| room.expanded)
            .unwrap_or(false)
    }

    /// 展开一张卡片；同一时间只展开一张。
    pub fn expand(&mut self, uuid: &str) {
        self.active = Some(uuid.to_string());
        for (key, room) in self.rooms.iter_mut() {
            room.expanded = key == uuid;
        }
        self.rooms.entry(uuid.to_string()).or_default().expanded = true;
    }

    /// 账号被移除：衣柜里的东西一并丢掉，别留着幽灵卡片。
    pub fn forget(&mut self, uuid: &str) {
        self.rooms.remove(uuid);
        if self.active.as_deref() == Some(uuid) {
            self.active = None;
        }
    }

    pub fn collapse(&mut self, uuid: &str) {
        if let Some(room) = self.rooms.get_mut(uuid) {
            room.expanded = false;
        }
        if self.active.as_deref() == Some(uuid) {
            self.active = None;
        }
    }

    /// 卡片刚展开时叫一次：手上还没有档案就去读。
    ///
    /// 读失败过就停在原地等人点「重试」，不然每次重绘都会再敲一遍网络。
    pub fn ensure(&mut self, account: &StoredAccount) {
        if self
            .rooms
            .get(&account.uuid)
            .map(|room| room.expanded && room.error.is_some())
            .unwrap_or(false)
        {
            return;
        }
        if let Some(reason) = not_editable(account) {
            // 改不了材质的账号：卡片照常展开，只是不摆按钮。这句不是错误，
            // 所以走 note 而不是 error —— 红字会让人以为出事了。
            let room = self.room(&account.uuid);
            room.note = Some((reason.to_string(), false));
            return;
        }
        let ready = self
            .rooms
            .get(&account.uuid)
            .map(|room| room.wardrobe.is_some() || room.loading || room.error.is_some())
            .unwrap_or(false);
        if ready {
            return;
        }
        self.load(account);
    }

    /// 手动重新读一遍（失败之后按「重试」）。
    pub fn load(&mut self, account: &StoredAccount) {
        if let Some(reason) = not_editable(account) {
            self.room(&account.uuid).note = Some((reason.to_string(), false));
            return;
        }
        let token = account.access_token.clone();
        self.spawn(&account.uuid, false, move || {
            let wardrobe = skinservice::fetch_wardrobe(&aicraft_core::auth::CurlHttp, &token)?;
            Ok(snapshot(wardrobe))
        });
    }

    /// 上传一张皮肤。png 已经是读好的字节。
    pub fn upload(&mut self, account: &StoredAccount, png: Vec<u8>) {
        if !skinservice::supports(account.api_root.as_deref()) {
            return;
        }
        let token = account.access_token.clone();
        let uuid = account.uuid.clone();
        let variant = self.room(&uuid).variant;
        self.room(&uuid).note = None;
        self.spawn(&uuid, true, move || {
            let wardrobe =
                skinservice::upload_skin(&aicraft_core::auth::CurlHttp, &token, &png, variant)?;
            Ok(snapshot(wardrobe))
        });
    }

    /// 把皮肤恢复成原版默认那张。
    pub fn reset_skin(&mut self, account: &StoredAccount) {
        if !skinservice::supports(account.api_root.as_deref()) {
            return;
        }
        let token = account.access_token.clone();
        let uuid = account.uuid.clone();
        self.room(&uuid).note = None;
        self.spawn(&uuid, true, move || {
            let wardrobe = skinservice::reset_skin(&aicraft_core::auth::CurlHttp, &token)?;
            Ok(snapshot(wardrobe))
        });
    }

    /// 换一件披风；cape_id 为空就摘掉。
    pub fn set_cape(&mut self, account: &StoredAccount, cape_id: &str) {
        if !skinservice::supports(account.api_root.as_deref()) {
            return;
        }
        let token = account.access_token.clone();
        let uuid = account.uuid.clone();
        let cape_id = cape_id.to_string();
        self.room(&uuid).note = None;
        self.spawn(&uuid, true, move || {
            let wardrobe = if cape_id.trim().is_empty() {
                skinservice::clear_cape(&aicraft_core::auth::CurlHttp, &token)?
            } else {
                skinservice::set_cape(&aicraft_core::auth::CurlHttp, &token, &cape_id)?
            };
            Ok(snapshot(wardrobe))
        });
    }

    /// 收结果。返回的 UUID 表示这个账号换了皮肤，外面要清掉旧头像。
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<String> {
        let rx = self.rx.take()?;
        let mut changed = None;
        loop {
            match rx.try_recv() {
                Ok((uuid, reply)) => {
                    let room = self.rooms.entry(uuid.clone()).or_default();
                    match reply {
                        Reply::Loaded(Ok(snapshot)) => room.put(snapshot),
                        Reply::Loaded(Err(message)) => room.fail(message),
                        Reply::Changed(Ok(snapshot)) => {
                            room.put(snapshot);
                            room.note = Some(("已经换好了，进游戏就能看到。".to_string(), false));
                            changed = Some(uuid.clone());
                        }
                        Reply::Changed(Err(message)) => {
                            room.loading = false;
                            room.note = Some((message, true));
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.rx = Some(rx);
                    ctx.request_repaint_after(std::time::Duration::from_millis(120));
                    return changed;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        changed
    }

    fn spawn<F>(&mut self, uuid: &str, changed: bool, job: F)
    where
        F: FnOnce() -> Result<Snapshot, String> + Send + 'static,
    {
        if self.rx.is_some() {
            // 上一件事还没回来：这一件先不受理，卡片上仍然只转一个圈。
            self.room(uuid).note = Some(("上一件事还没做完，等它一下。".to_string(), true));
            return;
        }
        self.room(uuid).loading = true;
        self.room(uuid).error = None;
        let key = uuid.to_string();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let reply = if changed {
                Reply::Changed(job())
            } else {
                Reply::Loaded(job())
            };
            let _ = tx.send((key, reply));
        });
    }

    /// 这张皮整图的宽高比。没有图就是 1.0（正方形）。
    pub fn skin_aspect(&self, uuid: &str) -> f32 {
        match self.rooms.get(uuid).and_then(|room| room.skin.as_ref()) {
            Some((width, height, _)) if *height > 0 => *width as f32 / *height as f32,
            _ => 1.0,
        }
    }

    /// 皮肤原图烤成纹理（卡片左侧那张大预览）。
    pub fn texture(&mut self, ctx: &egui::Context, uuid: &str) -> Option<egui::TextureId> {
        let room = self.rooms.get_mut(uuid)?;
        if room.tex.is_none() {
            let (width, height, rgba) = room.skin.as_ref()?;
            if *width == 0 || *height == 0 || rgba.len() < width * height * 4 {
                return None;
            }
            let image = egui::ColorImage::from_rgba_unmultiplied([*width, *height], rgba);
            let name = format!("aicraft-wardrobe-{uuid}");
            room.tex = Some(ctx.load_texture(name, image, egui::TextureOptions::NEAREST));
        }
        room.tex.as_ref().map(|tex| tex.id())
    }

    /// 把每件披风的缩略图都烤成纹理，按 id 交出去。
    ///
    /// 一次全烤：一张披风图几 KB，八件也只是八张小纹理。缺图的那几件直接
    /// 不进表，界面上按「没有图」退成色块，不会留一格空白。
    pub fn cape_textures(
        &mut self,
        ctx: &egui::Context,
        uuid: &str,
    ) -> HashMap<String, egui::TextureId> {
        let Some(room) = self.rooms.get_mut(uuid) else {
            return HashMap::new();
        };
        if room.cape_tex.is_empty() {
            for art in room.capes.iter() {
                let Some((width, height, rgba)) = art.pixels.as_ref() else {
                    continue;
                };
                if *width == 0 || *height == 0 || rgba.len() < width * height * 4 {
                    continue;
                }
                let image = egui::ColorImage::from_rgba_unmultiplied([*width, *height], rgba);
                let name = format!("aicraft-cape-{uuid}-{}", art.id);
                let texture = ctx.load_texture(name, image, egui::TextureOptions::NEAREST);
                room.cape_tex.insert(art.id.clone(), texture);
            }
        }
        room.cape_tex
            .iter()
            .map(|(id, tex)| (id.clone(), tex.id()))
            .collect()
    }
}

/// 档案 + 皮肤原图，一次凑齐。
fn snapshot(wardrobe: Wardrobe) -> Snapshot {
    let skin = wardrobe
        .active_skin()
        .and_then(|skin| skinservice::fetch_skin_png(&skin.url).ok())
        .and_then(|bytes| {
            aicraft_core::png::decode(&bytes)
                .ok()
                .filter(|image| image.width == 64 && image.height >= 32)
                .map(|image| (image.width, image.height, image.rgba))
        });
    let capes = wardrobe
        .capes
        .iter()
        .enumerate()
        .map(|(index, cape)| CapeArt {
            id: cape.id.clone(),
            label: cape
                .alias
                .clone()
                .filter(|alias| !alias.trim().is_empty())
                .unwrap_or_else(|| format!("披风 {}", index + 1)),
            active: cape.active,
            pixels: fetch_cape(&cape.url),
        })
        .collect();
    Snapshot {
        wardrobe,
        skin,
        capes,
    }
}

/// 下载一张披风图。失败就当没有，不让整张卡片跟着挂掉。
///
/// 披风图常见 64×32、46×22、22×17 甚至 10×16 —— 尺寸五花八门，只要不是
/// 空图都收。它不是拿去上传的，只是拿来当缩略图。
fn fetch_cape(url: &str) -> Option<(usize, usize, Vec<u8>)> {
    let bytes = skinservice::fetch_skin_png(url).ok()?;
    let image = aicraft_core::png::decode(&bytes).ok()?;
    cape_art(image)
}

/// 从一张披风贴图里切出「后背」那一片。
///
/// 官方披风是 64×32 的展开图：后背、里面、两侧的边并排躺在同一张图里，
/// 真正从身后看到的就是 x 1..11、y 1..17 这一块（10 宽 16 高，竖着的
/// 长方形）。按坐标切，得到的就是官网展示的那张画。
///
/// 别的尺寸（有的披风站只给一小块）按不透明边界猜一次：切掉四周的透明
/// 留白，再取左边那片。
fn cape_art(image: aicraft_core::png::Image) -> Option<(usize, usize, Vec<u8>)> {
    if let Some(back) = official_back(&image) {
        return Some(back);
    }
    trim_opaque(image).and_then(keep_the_back)
}

/// 官方 64×32 布局里后背那一片。尺寸不对就返回 None，交给猜的那条路。
fn official_back(image: &aicraft_core::png::Image) -> Option<(usize, usize, Vec<u8>)> {
    // 后背固定落在 (1,1)-(11,17)；图比这小说明不是这套布局。
    const LEFT: usize = 1;
    const TOP: usize = 1;
    const RIGHT: usize = 11;
    const BOTTOM: usize = 17;
    if image.width <= RIGHT || image.height <= BOTTOM {
        return None;
    }
    let mut rgba = Vec::with_capacity((RIGHT - LEFT) * (BOTTOM - TOP) * 4);
    for y in TOP..BOTTOM {
        for x in LEFT..RIGHT {
            rgba.extend_from_slice(&image.pixel(x, y));
        }
    }
    Some((RIGHT - LEFT, BOTTOM - TOP, rgba))
}

/// 从「一整条摊开的布」里留下后背那一片。
///
/// 披风贴图是展开图：后背、里面、左右两条边并排躺在同一张图里。整张铺进
/// 缩略图会得到一条横的、左右花纹不一样的怪东西 —— 那就是两张面贴在一起。
/// 游戏里真正从身后看到的是最左边那一片，宽高约 10:16，是竖着的长方形。
fn keep_the_back(piece: (usize, usize, Vec<u8>)) -> Option<(usize, usize, Vec<u8>)> {
    let (width, height, rgba) = piece;
    let back = back_width(width, height);
    if back >= width {
        return Some((width, height, rgba));
    }
    let mut out = Vec::with_capacity(back * height * 4);
    for y in 0..height {
        let start = y * width * 4;
        out.extend_from_slice(&rgba[start..start + back * 4]);
    }
    Some((back, height, out))
}

/// 后背那一片的宽度。
///
/// 正版披风的后背是 10×16，占宽高比约 0.63；连上里面那一面并排摊开就是
/// 约 1.25。所以「明显比这个比例宽」的图按并排两片算，从中间劈开；
/// 本来就竖着的（有的披风站只给一块后背）原样留着，不去猜。
fn back_width(width: usize, height: usize) -> usize {
    if height == 0 || width * 10 <= height * 9 {
        return width;
    }
    width / 2
}

/// 把图四周的透明边切掉。
///
/// 披风贴图是「一块布摊平」，真正有颜色的只占左上角一小块，四周全是透明。
/// 直接整张铺进缩略图，图案会小得像一粒米。切到不透明边界之后，
/// 不管这张图是 64×32 还是 22×17，缩略图里都是一块填满的布。
fn trim_opaque(image: aicraft_core::png::Image) -> Option<(usize, usize, Vec<u8>)> {
    let (width, height) = (image.width, image.height);
    if width == 0 || height == 0 {
        return None;
    }
    let mut left = width;
    let mut top = height;
    let mut right = 0;
    let mut bottom = 0;
    for y in 0..height {
        for x in 0..width {
            if image.pixel(x, y)[3] > 8 {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x);
                bottom = bottom.max(y);
            }
        }
    }
    if left > right || top > bottom {
        return None;
    }
    let (w, h) = (right - left + 1, bottom - top + 1);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in top..=bottom {
        for x in left..=right {
            rgba.extend_from_slice(&image.pixel(x, y));
        }
    }
    Some((w, h, rgba))
}

/// 一张卡片要用到的这一行的信息。
pub struct Row<'a> {
    pub uuid: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
    pub selected: bool,
    pub can_refresh: bool,
    /// 这个账号的材质能不能在这里改。皮肤站账号是 false：端点不同，
    /// 与其猜错一次写坏别人的站，不如把按钮收起来。
    pub editable: bool,
    pub skin: Option<&'a egui::TextureHandle>,
    /// 纯离线名字：身上没有服务器给的材质，原版按 UUID 挑 Steve 或 Alex。
    pub offline: bool,
}

/// 卡片外框。留白和圆角固定，跟选没选中无关。
///
/// egui 的 `Frame::begin` 会把 `stroke.width` 算进 `total_margin` 再从可用区域
/// 里扣掉，所以描边一旦交给 Frame，选中卡的内容就会比没选中的往里缩
/// 0.6px —— 两张卡上下挨着，一眼能看出内容错位。这里干脆让 Frame 不带描边，
/// 边框由 [`card_border`] 交给调用方自己画，粗细只影响那一笔画在哪儿，不动布局。
fn card_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(theme::card_fill())
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(14, 11))
}

/// 卡片边框那一笔：选中实一些，只展开不选中时虚一些，都没有就是普通描边。
fn card_border(selected: bool, expanded: bool) -> egui::Stroke {
    if selected {
        egui::Stroke::new(1.6, theme::accent_fill())
    } else if expanded {
        egui::Stroke::new(1.0, theme::accent_fill().gamma_multiply(0.40))
    } else {
        egui::Stroke::new(1.0, theme::BORDER)
    }
}

/// 圆角半径，外框和描边得用同一个值才对得上。
const CARD_RADIUS: u8 = 12;

/// 卡片头部被点了一下之后该做什么。
///
/// 分两片：左边大片是「用这个账号」，右边那截箭头才是展开。已经是当前
/// 账号时再点一次什么都不做 —— 换账号会把这张卡挪到列表最前面，手滑
/// 点一下就把顺序搅一遍，不值当。
fn header_action(body: bool, arrow: bool, selected: bool, busy: bool) -> Option<Action> {
    if busy {
        return None;
    }
    if arrow {
        return Some(Action::Toggle);
    }
    if body && !selected {
        return Some(Action::Use);
    }
    None
}

/// 画一张账号卡片。展开状态下把皮肤与披风都摆进来。
pub fn card(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    state: &mut State,
    row: Row<'_>,
    busy: bool,
) -> Option<Action> {
    let expanded = state.is_expanded(row.uuid);
    // 离线名字那张默认皮肤不再摆进卡片（卡片上只剩「删掉」这一件事），
    // 所以也不用按 UUID 现烤一张贴上来了。
    let tex = state.texture(ctx, row.uuid);
    let cape_tex = if expanded {
        state.cape_textures(ctx, row.uuid)
    } else {
        HashMap::new()
    };

    let mut variant = Variant::Classic;
    let mut loading = false;
    let mut note: Option<(String, bool)> = None;
    let mut error: Option<String> = None;
    let mut capes: Vec<(String, String, bool, f32)> = Vec::new();
    let mut has_wardrobe = false;
    if let Some(room) = state.room_ref(row.uuid) {
        variant = room.variant;
        loading = room.loading;
        note = room.note.clone();
        error = room.error.clone();
        has_wardrobe = room.wardrobe.is_some();
        for art in room.capes.iter() {
            capes.push((art.id.clone(), art.label.clone(), art.active, art.aspect()));
        }
    }

    let mut action: Option<Action> = None;
    let mut picked_variant = variant;

    // 底色一律是白卡：换个底色会让整张卡在页面里跳出来，而且展开之后
    // 内容一多，浅绿底反而看不清。选中与否只由边框说。
    let frame = card_frame();
    let card = frame.show(ui, |ui| {
        ui.set_min_width(ui.available_width());

        let width = ui.available_width();
        let (head, _) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::hover());
        // 头部切成两片：左边这一大片是「用这个账号启动」，右边那一小截
        // 箭头才展开。展开会去网上拉档案，是个重动作，不该藏在整张卡
        // 底下 —— 想换账号的人一按就换，想看皮肤的人去点箭头。
        let arrow_w = 34.0;
        let body = egui::Rect::from_min_max(
            head.min,
            egui::pos2(head.right() - arrow_w, head.bottom()),
        );
        let arrow = egui::Rect::from_min_max(
            egui::pos2(head.right() - arrow_w, head.top()),
            head.max,
        );
        // 同一个滚动区里每张卡都走这同一个 ui，id 得带上 uuid 才不打架。
        let body_resp = ui.interact(
            body,
            ui.scope_id().with(("card-body", row.uuid)),
            Sense::click(),
        );
        let arrow_resp = ui
            .interact(arrow, ui.scope_id().with(("card-arrow", row.uuid)), Sense::click())
            .on_hover_text("展开皮肤与披风");
        let (body_resp, arrow_resp) = if busy {
            (body_resp, arrow_resp)
        } else {
            (
                body_resp.on_hover_cursor(egui::CursorIcon::PointingHand),
                arrow_resp.on_hover_cursor(egui::CursorIcon::PointingHand),
            )
        };
        let avatar = egui::Rect::from_center_size(
            egui::pos2(head.left() + 14.0, head.center().y),
            Vec2::splat(26.0),
        );
        theme::draw_avatar(ui, avatar, row.name, row.skin);

        let text_left = avatar.right() + 10.0;
        let clip = egui::Rect::from_min_max(
            egui::pos2(text_left, head.top()),
            egui::pos2(head.right() - arrow_w - 2.0, head.bottom()),
        );
        let painter = ui.painter().with_clip_rect(clip);
        let name_color = if row.selected {
            theme::accent_strong()
        } else {
            theme::TEXT
        };
        painter.text(
            egui::pos2(text_left, head.center().y - 1.0),
            egui::Align2::LEFT_BOTTOM,
            row.name,
            egui::FontId::new(13.0, egui::FontFamily::Proportional),
            name_color,
        );
        painter.text(
            egui::pos2(text_left, head.center().y + 1.0),
            egui::Align2::LEFT_TOP,
            row.kind,
            egui::FontId::new(10.0, egui::FontFamily::Proportional),
            theme::TEXT_FAINT,
        );
        // 展开 / 收起交给 CollapsingState：它自带一段 openness 动画，
        // 也会记住上一次完全展开时的高度，用来逐帧裁出正在长出来的那截。
        let state_id = ui.scope_id().with(("card-open", row.uuid));
        let mut collapsing =
            egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                state_id,
                false,
            );
        if collapsing.is_open() != expanded {
            // 外面改了主意（换了展开的那一张、或者自己收起）：先让动画跟上。
            collapsing.set_open(expanded);
        }
        let openness = collapsing.openness(ui.ctx());

        if arrow_resp.hovered() && !busy {
            ui.painter().rect_filled(
                arrow.shrink(4.0),
                egui::CornerRadius::same(7),
                theme::BG_BOTTOM,
            );
        }
        chevron(
            ui.painter(),
            egui::pos2(arrow.center().x, head.center().y),
            openness,
            if arrow_resp.hovered() {
                theme::TEXT
            } else {
                theme::TEXT_WEAK
            },
        );
        if let Some(picked) = header_action(
            body_resp.clicked(),
            arrow_resp.clicked(),
            row.selected,
            busy,
        ) {
            action = Some(picked);
        }

        // 动画没在跑、也还没开：这一帧什么都不用画。
        if !expanded && openness <= 0.0 {
            return;
        }

        // 展开动画交给 CollapsingState：它按 openness 逐帧裁出正在长出来的
        // 那截高度，所以下面这一整块照常画，露多少由它决定。
        collapsing.show_body_unindented(ui, |ui| {
            ui.add_space(9.0);
            let seam = egui::Rect::from_min_size(
                egui::pos2(ui.max_rect().left(), ui.cursor().top()),
                Vec2::new(width, 1.0),
            );
            ui.painter().rect_filled(seam, 0.0, theme::BORDER);
    
            // 档案还没回来：中间什么都不摆，只留一个转圈和底下两颗按钮。
            // 半截内容（空白的预览框、「还没读到」）并排放着，看着像已经失败了。
            if waits_for_wardrobe(&row, loading) {
                ui.add_space(11.0);
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(width, SKELETON_H), Sense::hover());
                ui.put(
                    rect,
                    egui::Spinner::new().size(24.0).color(theme::accent_fill()),
                );
                actions_row(ui, &row, busy, loading, &mut action);
                return;
            }

            // 离线名字：卡片上能对它做的事只有「删掉」。没有令牌可续、没有
            // 材质可换，那张预览图也就没必要占一整块地方。
            if row.offline {
                ui.add_space(9.0);
                actions_row(ui, &row, busy, loading, &mut action);
                return;
            }

            // 皮肤站账号：材质要去皮肤站自己的网站上改，这里没有可摆的东西，
            // 也不该摆 —— 一个换了也不会生效的预览框比空着更误导。只留底下
            // 那两颗真正对这个账号有用的按钮：续期、从本机移除。
            if is_skin_station(&row) {
                ui.add_space(9.0);
                actions_row(ui, &row, busy, loading, &mut action);
                return;
            }

            ui.add_space(11.0);
            ui.horizontal_top(|ui| {
                let (rect, _) = ui.allocate_exact_size(skin_preview_size(), Sense::hover());
                let aspect = state.skin_aspect(row.uuid);
                skin_preview(ui.painter(), rect, tex, row.name, aspect);
                ui.add_space(14.0);
    
                ui.vertical(|ui| {
                    ui.set_min_width((width - 170.0).max(200.0));
                    ui.add_space(7.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("模型")
                                .size(11.0)
                                .color(theme::TEXT_FAINT),
                        );
                        for option in [Variant::Classic, Variant::Slim] {
                            if theme::radio(ui, picked_variant == option, option.label()).clicked() {
                                picked_variant = option;
                            }
                        }
                    });
                    if row.editable {
                        ui.add_space(7.0);
                        ui.horizontal(|ui| {
                            if small_button(ui, "上传皮肤…", !busy && !loading).clicked() {
                                action = Some(Action::Pick);
                            }
                            if small_button(ui, "恢复默认皮肤", !busy && !loading).clicked() {
                                action = Some(Action::ResetSkin);
                            }
                        });
                    }
                });
            });
    
            // 皮肤站账号到此为止：披风只能在官方那边换。
            if !row.editable {
                return;
            }
    
            ui.add_space(12.0);
            let wearing_none = !capes.iter().any(|(_, _, active, _)| *active);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("披风")
                        .size(11.0)
                        .color(theme::TEXT_FAINT),
                );
                if !capes.is_empty() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // 「不披」本质上是摘掉披风，不是一件披风。放在格子里会让
                        // 那一格没有缩略图，整行高度就参差了 —— 挪到标题行右侧。
                        if small_button(ui, "取消披风", !busy && !loading && !wearing_none).clicked() {
                            action = Some(Action::Cape(String::new()));
                        }
                    });
                }
            });
            ui.add_space(4.0);
            if !has_wardrobe {
                ui.label(
                    egui::RichText::new("读到账号资料后，这里会列出你拥有的披风。")
                        .size(11.0)
                        .color(theme::TEXT_FAINT),
                );
            } else if capes.is_empty() {
                ui.label(
                    egui::RichText::new("这个账号名下还没有披风。")
                        .size(11.0)
                        .color(theme::TEXT_FAINT),
                );
            } else {
                // 一行摆几件由宽度决定，格间距再按剩下的空间摊平：凑满一行时
                // 最后一件的右边缘正好压在这块内容区的右边（和「取消披风」
                // 那颗按钮齐平）。用 horizontal_wrapped 的话间距是固定的，
                // 差几个像素就空出一条歪的右边线。
                let (cols, gap) = cape_columns(ui.available_width());
                for row_capes in capes.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for (id, label, active, aspect) in row_capes {
                            let art = cape_tex.get(id).copied();
                            if cape_chip(ui, label, art, *aspect, *active, !busy && !loading)
                                .clicked()
                            {
                                action = Some(Action::Cape(id.clone()));
                            }
                        }
                    });
                    ui.add_space(8.0);
                }
            }
    
            if let Some((text, bad)) = note.clone() {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(text)
                        .size(11.0)
                        .color(if bad { theme::DANGER } else { theme::TEXT_WEAK }),
                );
            }
            if let Some(text) = error.clone() {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(text).size(11.0).color(theme::DANGER));
                    if small_button(ui, "重试", !busy && !loading).clicked() {
                        action = Some(Action::Retry);
                    }
                });
            }
            ui.add_space(10.0);
            actions_row(ui, &row, busy, loading, &mut action);
        });
    });
    // 边框自己画：交给 Frame 会把描边宽度算进布局，两张卡的内容就错开了。
    ui.painter().rect_stroke(
        card.response.rect,
        egui::CornerRadius::same(CARD_RADIUS),
        card_border(row.selected, expanded),
        egui::StrokeKind::Inside,
    );

    if picked_variant != variant {
        state.room(row.uuid).variant = picked_variant;
    }
    if let Some(room) = state.rooms.get_mut(row.uuid)
        && !matches!(action, Some(Action::Toggle)) {
        // 展开 / 收起之外的动作都算动过这张卡：旧提示先撤掉，
        // 免得上一句「已经换好了」一直挂在新结果上。
        room.note = None;
    }
    action
}

/// 皮肤站账号：材质在人家站上改，启动器这边既不读也不写。
fn is_skin_station(row: &Row<'_>) -> bool {
    !row.offline && !row.editable
}

/// 正文该先让位给转圈吗。
///
/// 只有「真要去拉档案」的账号才等：离线名字的皮肤是本地按 UUID 挑的，
/// 皮肤站账号压根不去拉，它们一展开就该看见东西。
fn waits_for_wardrobe(row: &Row<'_>, loading: bool) -> bool {
    loading && row.editable
}

/// 卡片最底下那一行：左边「刷新令牌」，右边「从本机移除」。
///
/// 正文还没回来的时候也要画这一行 —— 读档案只是一件事，玩家想撤回了
/// 照样该有地方点，不该被一个转圈钉在原地等。
fn actions_row(
    ui: &mut egui::Ui,
    row: &Row<'_>,
    busy: bool,
    loading: bool,
    action: &mut Option<Action>,
) {
    ui.horizontal(|ui| {
        // 「用这个账号」不再单独占一颗按钮：卡片头部点一下就是它。
        if row.can_refresh && small_button(ui, "刷新令牌", !busy && !loading).clicked() {
            *action = Some(Action::Refresh);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let remove = egui::Button::new(
                egui::RichText::new("从本机移除")
                    .size(11.5)
                    .color(theme::DANGER),
            )
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::new(1.0, theme::DANGER.gamma_multiply(0.45)))
            .corner_radius(egui::CornerRadius::same(8))
            .min_size(Vec2::new(0.0, 26.0));
            if ui.add_enabled(!busy, remove).clicked() {
                *action = Some(Action::Remove);
            }
        });
    });
}

fn small_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let button = egui::Button::new(egui::RichText::new(text).size(11.5))
        .fill(theme::BG_BOTTOM)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(egui::CornerRadius::same(8))
        .min_size(Vec2::new(0.0, 27.0));
    ui.add_enabled(enabled, button)
}

/// 一件披风：上面是那张披风图，下面是名字。
///
/// 披风图本身就是一块布，直接按原尺寸铺开会糊成一片；这里只取它
/// 左边那截「背面」，也就是游戏里从身后看过去的样子。
///
/// 名字放到图下面之后，格子宽度只由缩略图决定，比横着排一圈名字窄了
/// 一半多；长名字折成两行，超出两行的部分裁掉（宁可少一个字，也别让
/// 它把整张卡片撑高）。
fn cape_chip(
    ui: &mut egui::Ui,
    text: &str,
    art: Option<egui::TextureId>,
    aspect: f32,
    active: bool,
    enabled: bool,
) -> egui::Response {
    let color = if active {
        theme::accent_strong()
    } else {
        theme::TEXT_WEAK
    };
    let fill = if active {
        theme::accent_fill().gamma_multiply(0.14)
    } else {
        egui::Color32::TRANSPARENT
    };
    let border = if active {
        theme::accent_fill().gamma_multiply(0.45)
    } else {
        theme::BORDER
    };

    let thumb_w = thumb_width(aspect);
    let (rect, response) = ui.allocate_exact_size(chip_size(), Sense::click());
    let response = if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    };
    let painter = ui.painter();
    let corner = egui::CornerRadius::same(8);
    painter.rect(
        rect,
        corner,
        fill,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );

    // 预览图在上半，左右居中。
    if let Some(art) = art {
        let frame = egui::Rect::from_center_size(
            egui::pos2(rect.center().x, rect.top() + INNER + THUMB_H / 2.0),
            Vec2::new(thumb_w, THUMB_H),
        );
        // 图已经在 fetch 时切到不透明边界，所以整张铺满就是那块布。
        painter.image(
            art,
            frame,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        painter.rect_stroke(
            frame,
            egui::CornerRadius::same(3),
            egui::Stroke::new(1.0, egui::Color32::from_black_alpha(30)),
            egui::StrokeKind::Inside,
        );
    }

    // 名字在下半，折行后居中；超出行高的部分裁掉，不往外顶。
    let band = egui::Rect::from_min_max(
        egui::pos2(rect.left() + INNER, rect.top() + INNER + THUMB_H + NAME_GAP),
        egui::pos2(rect.right() - INNER, rect.bottom() - INNER),
    );
    // 折行之后每一行都要各自居中：painter.layout 出来的是左对齐的 galley，
    // 整块挪到中间只会让最长那行贴边，短的那行（「15th」）还是偏左。
    let mut job = egui::text::LayoutJob::simple(
        text.to_string(),
        egui::FontId::new(11.0, egui::FontFamily::Proportional),
        if enabled { color } else { theme::TEXT_FAINT },
        band.width(),
    );
    job.halign = egui::Align::Center;
    let galley = painter.layout_job(job);
    let top = band.top() + ((band.height() - galley.size().y) / 2.0).max(0.0);
    // halign = Center 时 galley 里每一行都以 x = 0 为轴心展开，
    // 所以落点直接给格子的中线，不要再减半个宽度。
    painter
        .with_clip_rect(band)
        .galley(egui::pos2(rect.center().x, top), galley, color);
    response
}

/// 一格披风的宽度。全取同一个值，换行后列才对齐。
const CHIP_W: f32 = 96.0;
/// 卡片内边距。
const INNER: f32 = 8.0;
/// 预览图的高度。后背是 10:16 的竖条，宽度按比例换算。
const THUMB_H: f32 = 56.0;
/// 名字区的高度：11px 的字折两行刚好放得下。
const NAME_H: f32 = 30.0;
/// 图与名字之间的空隙。
const NAME_GAP: f32 = 6.0;

/// 一格披风占的大小。
/// 一排能摆几件披风、格与格之间留多宽。
///
/// 先按最小间距试算能放下几件，再把省下来的宽度平均塞进格缝里 —— 这样
/// 满行时最后一件的右边缘刚好落在内容区右边，跟标题行那颗按钮对齐。
/// 间距太宽会把格子拆得七零八落，所以最多到 GAP_MAX。
fn cape_columns(width: f32) -> (usize, f32) {
    let cols = ((width + GAP_MIN) / (CHIP_W + GAP_MIN)).floor().max(1.0) as usize;
    if cols <= 1 {
        return (1, 0.0);
    }
    let spare = width - cols as f32 * CHIP_W;
    let gap = (spare / (cols - 1) as f32).clamp(GAP_MIN, GAP_MAX);
    (cols, gap)
}

/// 格与格之间最少留这么宽。
const GAP_MIN: f32 = 8.0;

/// 摊平之后最多到这么宽，再宽就不像一排了。
const GAP_MAX: f32 = 40.0;

fn chip_size() -> Vec2 {
    Vec2::new(CHIP_W, INNER * 2.0 + THUMB_H + NAME_GAP + NAME_H)
}

/// 缩略图的宽度：按后背的宽高比换算，夹一个区间免得细成一条线。
fn thumb_width(aspect: f32) -> f32 {
    (THUMB_H * aspect).clamp(26.0, 46.0)
}

/// 箭头的两条胳膊在竖轴上的位置：(尖端, 两端)。
///
/// openness 从 0 走到 1，箭头就从「朝下」翻成「朝上」—— 跟着展开动画
/// 一起转，而不是到最后一帧才跳一下。
fn chevron_ys(center_y: f32, half: f32, openness: f32) -> (f32, f32) {
    // openness=0 时 reach 为正：尖端压在中心下面，箭头朝下（收起）。
    // openness=1 时 reach 变负：尖端翻到上面，箭头朝上（展开）。
    let reach = half * 0.55 * (1.0 - 2.0 * openness.clamp(0.0, 1.0));
    (center_y + reach, center_y - reach)
}

fn chevron(painter: &egui::Painter, center: egui::Pos2, openness: f32, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.4, color);
    let half = 3.6;
    let (tip_y, base_y) = chevron_ys(center.y, half, openness);
    painter.line_segment(
        [egui::pos2(center.x - half, base_y), egui::pos2(center.x, tip_y)],
        stroke,
    );
    painter.line_segment(
        [egui::pos2(center.x, tip_y), egui::pos2(center.x + half, base_y)],
        stroke,
    );
}

/// 皮肤预览占多大一块。
///
/// 整张展开图是 64×64 的正方形（老皮肤 64×32 是横条），预览框按方形留，
/// 图自己按比例缩进去 —— 留成方形是为了老皮肤那一档不会把卡片撑扁。
pub fn skin_preview_size() -> Vec2 {
    Vec2::splat(PREVIEW_PX)
}

/// 预览框的边长。
pub const PREVIEW_PX: f32 = 116.0;

/// 读档案时中间那块留给转圈的高度。
///
/// 取预览框那么高，是为了档案回来、正文铺开时卡片不至于先矮后高跳一下。
const SKELETON_H: f32 = PREVIEW_PX + 12.0;

/// 皮肤预览：把整张展开图按原样摆出来，和官网那一页一样。
///
/// 不裁、不拼、不叠：帽子层、四肢展开图、背面全都在原位。玩家一眼就能
/// 看出这张皮到底画成什么样，而不是只看一张被裁过的脸。
///
/// 灰底一直留着：它把这块位置和白色卡片分开，有没有图都该在。图外面那圈
/// 深色细线则去掉 —— 展开图本身是整块像素画，套一圈线看着像相框。
fn skin_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    tex: Option<egui::TextureId>,
    name: &str,
    aspect: f32,
) {
    let corner = egui::CornerRadius::same(10);
    painter.rect_filled(rect, corner, theme::BG_BOTTOM);
    let Some(tex) = tex else {
        // 没有图时多一圈描边：光一个字母飘着认不出这是预览位。
        painter.rect_stroke(
            rect,
            corner,
            egui::Stroke::new(1.0, theme::BORDER),
            egui::StrokeKind::Inside,
        );
        let letter = name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string());
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            letter,
            egui::FontId::new(24.0, egui::FontFamily::Proportional),
            theme::accent_strong(),
        );
        return;
    };
    // 图按自己的宽高比缩进框里、居中，四周留一点灰底当留白。
    let inner = rect.shrink(8.0);
    let size = if aspect >= 1.0 {
        Vec2::new(inner.width(), inner.width() / aspect)
    } else {
        Vec2::new(inner.height() * aspect, inner.height())
    };
    let shown = egui::Rect::from_center_size(inner.center(), size);
    painter.image(
        tex,
        shown,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(uuid: &str, api_root: Option<&str>) -> StoredAccount {
        StoredAccount {
            uuid: uuid.to_string(),
            name: "Tester".to_string(),
            access_token: "token".to_string(),
            refresh_token: None,
            expires_at: 0,
            api_root: api_root.map(|root| root.to_string()),
            client_token: None,
            offline: false,
        }
    }

    #[test]
    fn only_one_card_is_open_at_a_time() {
        let mut state = State::new();
        state.expand("a");
        state.expand("b");
        assert!(!state.is_expanded("a"));
        assert!(state.is_expanded("b"));
        assert_eq!(state.active.as_deref(), Some("b"));
        state.collapse("b");
        assert!(!state.is_expanded("b"));
        assert!(state.active.is_none());
    }

    #[test]
    fn an_offline_name_has_no_material_to_change() {
        let mut offline = account("offline", None);
        offline.offline = true;
        assert!(not_editable(&offline).is_some());
        // 正版账号能改，这是这张卡片存在的理由。
        assert!(not_editable(&account("official", None)).is_none());
    }

    #[test]
    fn a_third_party_account_is_left_alone() {
        let mut state = State::new();
        // 皮肤站的端点不一样，这里不该去敲网络，只留一句话。
        state.ensure(&account("skin-station", Some("https://example.invalid")));
        assert!(!state.busy());
        let room = state.room_ref("skin-station").expect("room exists");
        assert!(room.wardrobe.is_none());
        // 不是错误，只是一句「这里不代办」。
        assert!(room.error.is_none());
        assert!(room.note.is_some());
    }

    #[test]
    fn a_removed_account_takes_its_room_with_it() {
        let mut state = State::new();
        state.expand("gone");
        assert!(state.is_expanded("gone"));
        state.forget("gone");
        assert!(state.room_ref("gone").is_none());
        assert!(state.active.is_none());
    }

    #[test]
    fn a_legacy_skin_is_wider_than_it_is_tall() {
        // 64×32 的老皮肤是横条：预览要按 2:1 缩，不能拉成正方形。
        let mut state = State::new();
        let room = state.room("legacy");
        room.skin = Some((64, 32, vec![0u8; 64 * 32 * 4]));
        assert!((state.skin_aspect("legacy") - 2.0).abs() < f32::EPSILON);
        // 没图的时候按正方形来，占位块不会被压扁
        assert!((state.skin_aspect("nobody") - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn only_the_border_says_which_card_is_selected() {
        // 选中的卡边框是实色、比普通描边厚；没选中的就是普通描边。
        let picked = card_border(true, false);
        assert_eq!(picked.color, theme::accent_fill());
        assert!(picked.width > 1.0);
        let plain = card_border(false, false);
        assert_eq!(plain.color, theme::BORDER);
        assert_eq!(plain.width, 1.0);
        // 只展开没选中：介于两者之间。
        let opened = card_border(false, true).color;
        assert_ne!(opened, theme::BORDER);
        assert_ne!(opened, theme::accent_fill());
    }

    #[test]
    fn the_card_border_never_nudges_the_contents() {
        // 选中卡的描边 1.6，普通卡 1.0。要是把描边交给 Frame，它的宽度会被
        // 算进 total_margin 再从布局里扣掉，内容起点每边就差 0.6px，
        // 两张卡一上一下摆着能明显看出来错位。
        let ctx = egui::Context::default();
        let content_min = |selected: bool| {
            let mut min = egui::Pos2::ZERO;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 400.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let card = card_frame().show(ui, |ui| {
                        min = ui.max_rect().min;
                    });
                    // 跟 card() 里一样，边框在 Frame 之外自己画。
                    ui.painter().rect_stroke(
                        card.response.rect,
                        egui::CornerRadius::same(CARD_RADIUS),
                        card_border(selected, false),
                        egui::StrokeKind::Inside,
                    );
                },
            );
            // 不落地贴图，否则 egui 会在 drop 时抱怨有未应用的 delta。
            out.textures_delta.clear();
            min
        };

        assert_eq!(content_min(true), content_min(false));
    }

    #[test]
    fn a_skin_station_card_keeps_only_its_buttons() {
        let mut row = Row {
            uuid: "u",
            name: "Tester",
            kind: "第三方账户",
            selected: true,
            can_refresh: true,
            editable: false,
            skin: None,
            offline: false,
        };
        assert!(is_skin_station(&row));
        // 离线名字也改不了材质，但它有本地那张默认皮肤要摆出来。
        row.offline = true;
        assert!(!is_skin_station(&row));
        // 正版账号走完整正文。
        row.offline = false;
        row.editable = true;
        assert!(!is_skin_station(&row));
    }

    #[test]
    fn only_a_loading_wardrobe_hides_the_body() {
        let mut row = Row {
            uuid: "u",
            name: "Tester",
            kind: "微软账户",
            selected: true,
            can_refresh: true,
            editable: true,
            skin: None,
            offline: false,
        };
        assert!(waits_for_wardrobe(&row, true));
        assert!(!waits_for_wardrobe(&row, false));
        // 皮肤站账号：卡片里只留一句话，没有可等的东西。
        row.editable = false;
        assert!(!waits_for_wardrobe(&row, true));
    }

    #[test]
    fn a_loaded_skin_gets_no_outline_around_it() {
        // 灰底留着（它把预览位和白色卡片分开），去掉的是图外面那圈深色细线：
        // 展开图是整块像素画，套一圈线看着像相框。数形状：有图那一帧是
        // 底色 + 图两个，没有图才多出描边和那个字母。
        let ctx = egui::Context::default();
        let shapes = |tex: Option<egui::TextureId>| {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 400.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(skin_preview_size(), Sense::hover());
                    skin_preview(ui.painter(), rect, tex, "Tester", 1.0);
                },
            );
            out.textures_delta.clear();
            out.shapes
                .iter()
                .filter(|s| !matches!(s.shape, egui::Shape::Noop))
                .count()
        };
        // 没有图：底色 + 描边 + 那个字母，三样都在。
        assert!(shapes(None) >= 3);
        // 有图：灰底 + 图。不多不少，正好没有那圈线。
        assert_eq!(shapes(Some(egui::TextureId::Managed(0))), 2);
    }

    #[test]
    fn cape_rows_reach_the_right_edge() {
        // 满行时最后一件的右边缘必须压着内容区右边：cols 件 + (cols-1) 个格缝
        // 正好等于整块宽度，差一点都不行。
        for width in [420.0_f32, 640.0, 840.0, 1120.0] {
            let (cols, gap) = cape_columns(width);
            assert!(cols >= 1);
            let span = cols as f32 * CHIP_W + (cols - 1) as f32 * gap;
            assert!((span - width).abs() < 0.5, "{width} 宽时排出来 {span}");
        }
        // 太窄就一件一行，别硬塞。
        assert_eq!(cape_columns(CHIP_W).0, 1);
    }

    #[test]
    fn a_cape_card_is_a_tall_narrow_box() {
        // 名字挪到图下面之后，格子高度大于宽度，一排能放五六件。
        let size = chip_size();
        assert!(size.y > size.x, "{size:?}");
        assert_eq!(size.x, CHIP_W);
        assert_eq!(
            size.y,
            INNER * 2.0 + THUMB_H + NAME_GAP + NAME_H
        );
        // 缩略图宽度跟着后背的宽高比走，但不会细成一条线。
        assert!(thumb_width(10.0 / 16.0) >= 26.0);
        assert!(thumb_width(2.0) <= 46.0);
    }

    #[test]
    fn the_official_cape_gives_up_its_back_panel() {
        // 64×32 的展开图，按坐标切出 (1,1)-(11,17) 那块。
        let raw: Vec<u8> = (0..64 * 32).flat_map(|_| [255u8, 0, 0, 255]).collect();
        let image = aicraft_core::png::Image {
            width: 64,
            height: 32,
            rgba: raw,
        };
        let (width, height, rgba) = official_back(&image).expect("a 64x32 cape has a back panel");
        assert_eq!((width, height), (10, 16));
        assert_eq!(rgba.len(), 10 * 16 * 4);
        // 太小的图不认这套坐标，交给猜的那条路。
        let small = aicraft_core::png::Image {
            width: 8,
            height: 16,
            rgba: vec![255; 8 * 16 * 4],
        };
        assert!(official_back(&small).is_none());
    }

    #[test]
    fn the_back_piece_is_the_tall_left_half() {
        // 64×32 的展开图，切到不透明边界后是 20×16（后背 + 里面并排），
        // 只留下左边那块 10×16。
        let (width, height, rgba) = keep_the_back((20, 16, vec![255; 20 * 16 * 4])).unwrap();
        assert_eq!((width, height), (10, 16));
        assert_eq!(rgba.len(), 10 * 16 * 4);
        // 本来就比宽度高的图（只有一块后背）原样留着。
        let (width, height, _) = keep_the_back((8, 16, vec![255; 8 * 16 * 4])).unwrap();
        assert_eq!((width, height), (8, 16));
    }

    #[test]
    fn the_switch_starts_on_the_worn_variant() {
        let mut room = Room::default();
        assert_eq!(room.variant, Variant::Classic);
        // put 之后单选框跟着身上的皮肤走，这是「换完再点错」的防线。
        let snapshot = Snapshot {
            wardrobe: Wardrobe {
                name: "Tester".to_string(),
                uuid: "u".to_string(),
                skins: vec![skinservice::Skin {
                    id: "s".to_string(),
                    active: true,
                    url: String::new(),
                    variant: Some(Variant::Slim),
                }],
                capes: Vec::new(),
            },
            skin: None,
            capes: Vec::new(),
        };
        room.put(snapshot);
        assert_eq!(room.variant, Variant::Slim);
        assert!(!room.loading);
    }

    #[test]
    fn the_arrow_turns_over_as_the_panel_opens() {
        // 关着：尖端在下、两端在上，箭头朝下。
        let (tip, base) = chevron_ys(100.0, 3.6, 0.0);
        assert!(tip > base);
        // 开着：翻过来。
        let (tip, base) = chevron_ys(100.0, 3.6, 1.0);
        assert!(tip < base);
        // 动画到一半：两条胳膊收拢到中线上，不是硬跳。
        let (tip, base) = chevron_ys(100.0, 3.6, 0.5);
        assert!((tip - 100.0).abs() < 0.001 && (base - 100.0).abs() < 0.001);
        // 越界的 openness 夹在 0..1，不会翻过头。
        let (tip, _) = chevron_ys(100.0, 3.6, 5.0);
        assert!((tip - (100.0 - 3.6 * 0.55)).abs() < 0.01);
    }

    #[test]
    fn the_blank_part_of_a_card_is_the_launch_button() {
        // 头部大片：换账号。
        assert!(matches!(
            header_action(true, false, false, false),
            Some(Action::Use)
        ));
        // 已经是当前账号：再点一下不做事，免得列表顺序被手滑搅乱。
        assert!(header_action(true, false, true, false).is_none());
        // 空白处和箭头同时点到（理论上不会），箭头优先。
        assert!(matches!(
            header_action(true, true, true, false),
            Some(Action::Toggle)
        ));
    }

    #[test]
    fn the_arrow_is_the_only_way_in() {
        assert!(matches!(
            header_action(false, true, false, false),
            Some(Action::Toggle)
        ));
        // 点在两片之间或者卡片正文上：什么都不做。
        assert!(header_action(false, false, false, false).is_none());
        // 有请求在飞的时候整张卡都不接手势。
        assert!(header_action(true, true, false, true).is_none());
        assert!(header_action(true, false, false, true).is_none());
    }
}
