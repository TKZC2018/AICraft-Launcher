//! 「资源下载」页：从 Modrinth 逛模组并装进某个实例。
//!
//! 和「下载版本」页一样，这里**不碰网络**：它只把玩家选的条件整理成
//! [`Action`]，真正发请求、下载、落盘由 `main.rs` 拿到动作后跑在后台线程里。
//! 这样列表滚动时不会因为一个请求卡住整帧。
//!
//! 图标走 [`crate::icons`]：Modrinth 给的多是 webp，那边用 `image` crate
//! 解码、按项目 id 缓存纹理；这边只管「这一格有没有图可画」，没有就画占位。

use std::collections::HashMap;

use egui::{Align2, Color32, CornerRadius, FontId, RichText, Sense, Stroke, StrokeKind, Vec2};

use aicraft_core::loader::LoaderKind;
use aicraft_core::modrinth::{ModDetail, ModFile, ModHit, ModVersion, ProjectKind, Sort};

use crate::icons;
use crate::theme;

/// 一页能装多少条，给玩家自己挑。
///
/// 5 到 100：5 适合一屏一屏地细看，100 适合先扫一遍再决定。
/// Modrinth 单次最多给 100，再多就得再翻一页。
const PAGE_CHOICES: [usize; 6] = [5, 10, 15, 20, 50, 100];

/// 默认每页 20 条 —— 滚两屏的量，也是 Modrinth 自己的默认值。
const DEFAULT_PAGE: usize = 20;

/// 结果卡片的高度。固定下来，滚动条才不会因为标题长短一跳一跳。
const ROW_H: f32 = 82.0;

/// 卡片左边那块图标的边长。
///
/// 和 `ROW_H` 一起定死：模组图标原始尺寸从 32 到 512 都有，让它们自己撑开
/// 会让列表里的字高低乱跳。
const ICON_SIZE: f32 = 52.0;

/// 图标四个角的圆角。和卡片本身的 10 错开一点，看着才像「嵌在里面」。
const ICON_ROUNDING: u8 = 8;

/// 详情页头部的图标比列表里大一号。
const DETAIL_ICON: f32 = 64.0;

/// 详情页里简介正文最多占多高。再长就让它在自己的框里滚，
/// 否则一个写了几千字的模组说明会把「所有版本」挤到屏幕外。
const BODY_MAX_H: f32 = 150.0;

/// 页面状态（全是界面自己的事，磁盘与网络都不在这儿）。
pub struct State {
    /// 进过这一页没有（第一次进来才自动搜一次）
    pub open: bool,
    /// 现在看的是哪一类（模组 / 资源包 / 光影包）
    pub kind: ProjectKind,
    pub query: String,
    /// `kind.categories()` 里的下标
    pub category: usize,
    /// `None` = 不限加载器
    pub loader: Option<LoaderKind>,
    /// `None` = 全部游戏版本
    pub game_version: Option<String>,
    pub sort: Sort,
    pub hits: Vec<ModHit>,
    pub total: u64,
    pub offset: usize,
    /// 一页装多少条（`PAGE_CHOICES` 里的一个）。改它会回到第一页重搜。
    pub page_size: usize,
    pub loading: bool,
    pub error: Option<String>,
    /// 装进哪个实例（版本目录名）。空 = 还没选。
    pub target: Option<String>,
    /// 数据包装进哪个存档。空 = 还没选。
    ///
    /// 数据包只对某一个存档生效，所以除了实例还要再挑一次存档；
    /// 别的类别不看这个字段。
    pub world: Option<String>,
    /// 上一次同步时主页选中的那个版本。
    ///
    /// 安装目标默认跟着主页走：主页一换版本，这里就跟着换。存下上一次
    /// 同步的值，才能分清「主页换了」和「只是玩家自己在下拉里挑了别的」——
    /// 前者要跟着改，后者得留着。
    pub synced_home: Option<String>,
    /// 装完/失败后留一句话在工具栏上，不用去翻日志
    pub notice: Option<(String, bool)>,
    /// 模组图标的纹理（下载与解码在 `main.rs` 的后台线程里跑）
    pub icons: icons::Cache,
    /// 打开了某个模组的详情页时这里是 `Some`
    pub detail: Option<DetailView>,
    /// 正在飞的那次搜索搜的是哪一类。
    ///
    /// 切换分类时旧请求还在路上（一次搜索要几百毫秒），如果不管它，
    /// 迟到的旧结果会把新页面填成上一类的东西。收结果时比对这一项，
    /// 对不上就丢掉并重搜。
    pub searching: Option<ProjectKind>,
    /// 哪一类已经搜过一次了。
    ///
    /// 用来区分「还没来得及搜」和「搜过了但一条都没命中」——后者也是
    /// `hits.is_empty()`，只按结果判空会陷入「搜空 → 以为没搜 → 再搜」的死循环。
    pub searched: Option<ProjectKind>,
    /// 这一批结果开始「逐行进场」的时刻；播完置回 None。
    ///
    /// 跟 `hits` 一起换：新结果到了才重播，翻页或只是滚列表都不重播。
    pub reveal_at: Option<std::time::Instant>,
}

/// 详情页的状态。
///
/// 详情和版本列表分两次请求（一个接口一份），所以两个字段各自可能是空的：
/// 先到的先显示，另一个还在路上。
pub struct DetailView {
    /// 项目 id，用来认出「晚到的结果是不是当前这一页的」
    pub project: String,
    /// 卡片的标题，详情还没回来时先用它撑住抬头
    pub title: String,
    pub detail: Option<ModDetail>,
    pub versions: Option<Vec<ModVersion>>,
    pub error: Option<String>,
    /// 版本列表的本地筛选（服务端一份全表拉下来，这里过）
    pub loader: Option<LoaderKind>,
    pub game_version: Option<String>,
    /// 「要装进去的那个实例」用的是哪个游戏版本。
    ///
    /// 玩家手动在筛选里选了版本时以他的选择为准（见 `preferred_game`），
    /// 这个只在没选的时候顶上——所以它是「提示」，不是筛选条件。
    pub game_hint: Option<String>,
    /// 「要装进去的那个实例」用的是什么加载器（原版实例是 `None`）。
    ///
    /// 和 `game_hint` 一起决定置顶哪一版：Fabric 实例就置顶 Fabric 版，
    /// 原版实例没有加载器可言，那就把适配这个游戏版本的所有加载器一视同仁。
    pub loader_hint: Option<LoaderKind>,
    /// 玩家手动开合过的版本分组（键是游戏版本，`true` = 展开）。
    ///
    /// 没点过的分组走默认（见 `group_open`）：跟着当前实例的版本开，
    /// 其余收起来。存「手动改过的」而不是「全部状态」，这样玩家在筛选里
    /// 换了个游戏版本，没动过的分组会自己跟着换，不必逐个再点一遍。
    pub toggled: HashMap<String, bool>,
}

impl DetailView {
    pub fn new(project: String, title: String) -> Self {
        DetailView {
            project,
            title,
            detail: None,
            versions: None,
            error: None,
            loader: None,
            game_version: None,
            game_hint: None,
            loader_hint: None,
            toggled: HashMap::new(),
        }
    }

    /// 当前筛选下能显示的版本。
    fn visible_versions(&self) -> Vec<&ModVersion> {
        let Some(all) = &self.versions else {
            return Vec::new();
        };
        all.iter()
            .filter(|v| {
                self.loader
                    .is_none_or(|kind| v.loaders.iter().any(|l| l == kind.id()))
            })
            .filter(|v| {
                self.game_version
                    .as_ref()
                    .is_none_or(|gv| v.game_versions.iter().any(|v| v == gv))
            })
            .collect()
    }

    /// 「当前这个版本」指哪个游戏版本。
    ///
    /// 玩家手选的筛选优先——他既然特意切到 1.20.1，就别再拿实例的版本
    /// 去给别的行打标记。
    fn preferred_game(&self) -> Option<&String> {
        self.game_version.as_ref().or(self.game_hint.as_ref())
    }

    /// 这个版本支持不支持 `preferred_game()`。
    fn matches_game(&self, version: &ModVersion) -> bool {
        // 还没选游戏版本时不做限制：这时候置顶的就是最简单的「最新一版」
        self.preferred_game()
            .is_none_or(|gv| version.game_versions.iter().any(|v| v == gv))
    }

    /// 「当前这个版本」指哪个加载器。同理，手选的筛选优先。
    fn preferred_loader(&self) -> Option<LoaderKind> {
        self.loader.or(self.loader_hint)
    }

    /// 这个版本支持不支持 `preferred_loader()`。
    ///
    /// 没有加载器偏好时一律算支持——原版实例本来就不挑加载器，
    /// 这时候要的是「适配这个游戏版本的最新一版」，不管它给谁做的。
    fn matches_loader(&self, version: &ModVersion) -> bool {
        self.preferred_loader()
            .is_none_or(|kind| version.loaders.iter().any(|l| l == kind.id()))
    }

    /// 游戏版本和加载器两头都对得上——「能直接装进这个实例」。
    fn fits(&self, version: &ModVersion) -> bool {
        self.matches_game(version) && self.matches_loader(version)
    }

    /// 置顶那一版。
    ///
    /// 认「能显示出来的版本里，第一条同时适配当前游戏版本和加载器的」：
    /// API 按发布时间倒序给，所以第一条就是最新的一版。不再拿整表第一条
    /// 当最新版——那是「全世界最新」，常常是玩家装不进去的那个游戏版本。
    fn pinned_version(&self) -> Option<&ModVersion> {
        self.visible_versions().into_iter().find(|v| self.fits(v))
    }

    /// 这个分组该不该展开。
    ///
    /// 默认只有「当前实例那个游戏版本」是开的，其余收起来——一个模组动辄
    /// 两百多个版本，全铺开要滚很久才见到别的游戏版本。
    fn group_open(&self, game: &str) -> bool {
        self.toggled
            .get(game)
            .copied()
            .unwrap_or_else(|| self.preferred_game().is_some_and(|gv| gv == game))
    }

    /// 把能显示的版本拆成「置顶的一个最新版」+「按游戏版本分组」。
    ///
    /// 置顶只留一个：作者最新那一版，且必须在当前筛选下可见（不然
    /// 「最新版」会跑到筛选外面去）。它不计入任何分组，避免同一版出现两次。
    fn grouped_versions(&self) -> (Option<&ModVersion>, Vec<VersionGroup<'_>>) {
        let pinned = self.pinned_version();
        let mut groups: Vec<VersionGroup> = Vec::new();
        for version in self.visible_versions() {
            if pinned.is_some_and(|p| p.id == version.id) {
                continue;
            }
            // 分组的键取第一个游戏版本：Modrinth 的列表就是「按最先声明的排」，
            // 拿它分组和玩家在网页上看到的目录一致。
            let game = version
                .game_versions
                .first()
                .cloned()
                .unwrap_or_else(|| "未知版本".to_string());
            match groups.iter_mut().find(|g| g.game == game) {
                Some(group) => group.versions.push(version),
                None => groups.push(VersionGroup {
                    game,
                    versions: vec![version],
                }),
            }
        }
        // 新版在前；组内保持 API 的发布时间倒序（没有重排）
        groups.sort_by_key(|g| std::cmp::Reverse(version_key(&g.game)));
        (pinned, groups)
    }
}

/// 一个游戏版本下的所有版本。
struct VersionGroup<'a> {
    game: String,
    versions: Vec<&'a ModVersion>,
}

impl Default for State {
    fn default() -> Self {
        State {
            open: false,
            kind: ProjectKind::Mod,
            query: String::new(),
            searching: None,
            searched: None,
            category: 0,
            loader: None,
            game_version: None,
            sort: Sort::Relevance,
            hits: Vec::new(),
            total: 0,
            offset: 0,
            page_size: DEFAULT_PAGE,
            loading: false,
            error: None,
            target: None,
            world: None,
            synced_home: None,
            notice: None,
            icons: icons::Cache::default(),
            detail: None,
            reveal_at: None,
        }
    }
}

impl State {
    /// 第一次进这一页要不要自动搜一次。
    /// 当前筛选的一句话写法，给操作日志用。
    pub fn filter_text(&self) -> String {
        let table = self.kind.categories();
        let category = table
            .get(self.category)
            .map(|(_, label)| *label)
            .unwrap_or("全部分类");
        let mut text = format!(
            "{} / 「{}」/ {} / {}",
            self.kind.label(),
            if self.query.trim().is_empty() {
                "（空关键词）"
            } else {
                self.query.trim()
            },
            category,
            self.game_version.as_deref().unwrap_or("全部版本")
        );
        // 只有模组有加载器这一维，资源包 / 光影包的日志里不该出现「全部加载器」
        if self.kind.has_loader() {
            let loader = self
                .loader
                .map(|l| l.label().to_string())
                .unwrap_or_else(|| "全部加载器".to_string());
            text.push_str(" / ");
            text.push_str(&loader);
        }
        text.push_str(" / 按");
        text.push_str(self.sort.label());
        text
    }

    /// 换一类东西看：清掉上一类的分类下标与结果，等界面去重搜。
    pub fn switch_kind(&mut self, kind: ProjectKind) {
        if self.kind == kind {
            return;
        }
        self.kind = kind;
        self.category = 0;
        // 加载器只对模组有意义，切走就清掉，免得回来时还挂着上次的条件
        if !kind.has_loader() {
            self.loader = None;
        }
        self.hits.clear();
        self.total = 0;
        self.offset = 0;
        self.detail = None;
        self.error = None;
        self.notice = None;
        self.searched = None;
    }

    pub fn needs_fetch(&self) -> bool {
        self.open
            && !self.loading
            && self.hits.is_empty()
            && self.error.is_none()
            && self.searched != Some(self.kind)
    }

    /// 这一页实际要多少条。
    ///
    /// 兜住两种情况：`page_size` 没在候选表里（旧状态或手改的配置），
    /// 以及超过 Modrinth 单次 100 条的上限。
    pub fn page_limit(&self) -> usize {
        if PAGE_CHOICES.contains(&self.page_size) {
            self.page_size
        } else {
            DEFAULT_PAGE
        }
    }

    /// 按当前条件拼出搜索请求。
    pub fn query_spec(&self) -> aicraft_core::modrinth::SearchQuery {
        let table = self.kind.categories();
        aicraft_core::modrinth::SearchQuery {
            kind: self.kind,
            query: self.query.clone(),
            game_version: self.game_version.clone(),
            loader: self.loader,
            category: Some(
                table
                    .get(self.category)
                    .map(|(id, _)| (*id).to_string())
                    .unwrap_or_default(),
            ),
            sort: Some(self.sort),
            offset: self.offset,
            limit: self.page_limit(),
        }
    }

    /// 选中一个实例。
    ///
    /// `preferred` 是主页上当前选中的那个版本，安装目标默认跟着它走：
    /// 主页换了版本，这里也跟着换 —— 玩家刚在主页点开哪个版本，多半就是
    /// 想往哪个里面装东西。
    ///
    /// 主页没换的时候，玩家在下拉里自己挑的那个要留着；只有它被删掉了
    /// 才回退。主页那个版本没装成实例（或者压根没有实例）时，退回列表里的
    /// 第一个。
    pub fn pick_target(&mut self, instances: &[String], preferred: Option<&str>) {
        let home_changed = self.synced_home.as_deref() != preferred;
        if home_changed {
            self.synced_home = preferred.map(str::to_string);
            self.target = preferred
                .filter(|name| instances.iter().any(|i| i == name))
                .map(str::to_string)
                .or_else(|| instances.first().cloned());
            return;
        }
        let valid = self
            .target
            .as_deref()
            .is_some_and(|t| instances.iter().any(|i| i == t));
        if !valid {
            self.target = instances.first().cloned();
        }
    }
}

impl State {
    /// 让选中的存档跟着实际存在的存档走。
    ///
    /// 存档不像实例那样有「主页选中的那个」可以跟，所以只在它不存在了
    /// 的时候退回第一个；玩家手挑的那个只要还在就留着。
    pub fn pick_world(&mut self, worlds: &[String]) {
        let valid = self
            .world
            .as_deref()
            .is_some_and(|w| worlds.iter().any(|i| i == w));
        if !valid {
            self.world = worlds.first().cloned();
        }
    }
}

/// 玩家在这一页表达的动作。
pub enum Action {
    /// 按当前条件重新搜（换筛选、翻页、点刷新都走它）
    Search(ProjectKind),
    /// 进详情页：`(project_id, 标题)`
    OpenDetail(String, String),
    /// 退出详情页，回到搜索结果
    CloseDetail,
    /// 装详情页里选中的那一版（模组 / 资源包 / 光影包）
    InstallFile(ModFile),
    /// 把详情页里选中的那个数据包装进当前存档
    InstallDatapack(ModFile),
    /// 下载详情页里选中的那一版整合包，下完交给导入流程
    DownloadPack(ModFile),
    /// 用浏览器打开一个链接
    OpenUrl(String),
}

impl Action {
    /// 给操作日志用的一句话。
    ///
    /// 搜索**不**把整串筛选条件拼进来：那串 URL 参数又长又难读，不如让
    /// 筛选本身各记一条（`resources::page` 里变更加了 `detail`）。
    pub fn describe(&self) -> String {
        match self {
            Action::Search(kind) => format!("资源下载：搜索{}", kind.unit()),
            Action::OpenDetail(_, title) => format!("资源下载：打开「{}」的详情", title),
            Action::CloseDetail => "资源下载：返回搜索结果".to_string(),
            Action::InstallFile(file) => format!("资源下载：下载 {}", file.filename),
            Action::InstallDatapack(file) => format!("资源下载：下载数据包 {}", file.filename),
            Action::DownloadPack(file) => format!("资源下载：下载整合包 {}", file.filename),
            Action::OpenUrl(url) => format!("资源下载：打开链接 {}", url),
        }
    }
}

/// 这一页要用到的两份「装到哪儿」清单。
///
/// 摊成两个参数时 `page` 会数到八个，读起来一串 `&[String]` 分不清谁是谁；
/// 收进一个结构体，调用处也顺带能看清各自是什么。
pub struct Targets<'a> {
    /// 当前游戏目录里已经装好的实例名
    pub instances: &'a [String],
    /// 安装目标那个实例下的存档名（数据包要挑一个才放得进去）
    pub worlds: &'a [String],
}

/// 画整页。///
/// `instances` 是当前游戏目录里已经装好的实例名，用来选装到哪儿；
/// `preferred` 是主页当前选中的版本，第一次进来时默认装到它里面；
/// `busy` 是全局忙碌提示（正在装模组时它会说「正在装 xxx」）。
pub fn page(
    ui: &mut egui::Ui,
    state: &mut State,
    targets: Targets<'_>,
    preferred: Option<&str>,
    busy: Option<&str>,
    animate: bool,
    // 页头动画已经跑了多久（秒）；None = 早就播完，或者动画关着。
    //
    // 和 `target` 那三个参数一样，时间由 `main.rs` 拿着
    // （`title_slide_at` 是 App 上的一份），这里只把三段折成各自的进度。
    head: Option<f32>,
) -> Option<Action> {
    let Targets { instances, worlds } = targets;
    state.pick_target(instances, preferred);
    if state.kind.installs_into_world() {
        state.pick_world(worlds);
    }
    let mut action = None;
    let mut dirty = false;

    // 详情页是一个「子页」而不是浮层：它要放长简介和几十个版本，
    // 用弹窗会挤，用整页替换反而干净——反正返回按钮就在左上角。
    if state.detail.is_some() {
        return detail_page(ui, state, worlds, busy);
    }

    // ---- 标题行 ----
    let title = theme::slide_progress(head, 0.0, theme::TITLE_SLIDE_SECS);
    theme::title_slide(ui, title, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(state.kind.label()).size(17.0).strong());
            ui.add_space(4.0);
            if theme::refresh_button(ui, !state.loading && busy.is_none())
                .on_hover_text("重新搜索")
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                state.offset = 0;
                action = Some(Action::Search(state.kind));
            }
            ui.add_space(6.0);
            if state.loading {
                ui.spinner();
                ui.label(
                    RichText::new("正在向 Modrinth 查询…")
                        .size(11.0)
                        .color(theme::TEXT_WEAK),
                );
            } else if let Some(text) = busy {
                ui.spinner();
                ui.label(RichText::new(text).size(11.0).color(theme::accent_strong()));
            } else if let Some((text, ok)) = &state.notice {
                ui.label(RichText::new(text).size(11.0).color(if *ok {
                    theme::accent_strong()
                } else {
                    theme::DANGER
                }));
            }
            // 这里不再报「共多少个」：页脚已经用「第 1–20 条」说清了范围，
            // 标题行留给标题本身，两处都报总数反而显得吵。
        });
    });
    ui.add_space(10.0);

    let first = theme::slide_progress(head, theme::TOOLBAR_SLIDE_DELAY, theme::TITLE_SLIDE_SECS);
    theme::title_slide(ui, first, |ui| {
        // ---- 第一行筛选：搜索框 + 分类 / 加载器 ----
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(16.0);
                // 加载器只对模组有意义：资源包和光影包在 Modrinth 上挂的是
                // 「谁能读它」（minecraft / iris / optifine），不是加载器。
                if state.kind.has_loader() {
                    let loader_label = state
                        .loader
                        .map(|l| l.label().to_string())
                        .unwrap_or_else(|| "全部加载器".to_string());
                    let combo = theme::combo_centered(
                        ui,
                        "res-loader",
                        RichText::new(loader_label).size(12.0),
                        112.0,
                        |ui| {
                            if theme::combo_value(ui, &mut state.loader, None, "全部加载器")
                                .changed()
                            {
                                dirty = true;
                            }
                            for kind in LoaderKind::ALL {
                                if theme::combo_value(
                                    ui,
                                    &mut state.loader,
                                    Some(kind),
                                    kind.label(),
                                )
                                .changed()
                                {
                                    dirty = true;
                                }
                            }
                        },
                    );
                    theme::hand(combo);
                }

                let table = state.kind.categories();
                let category_label = table
                    .get(state.category)
                    .map(|(_, label)| *label)
                    .unwrap_or("全部分类");
                let mut picked = state.category;
                let combo = theme::combo_centered(
                    ui,
                    "res-category",
                    RichText::new(category_label).size(12.0),
                    112.0,
                    |ui| {
                        for (i, (_, label)) in table.iter().enumerate() {
                            theme::combo_value(ui, &mut picked, i, label);
                        }
                    },
                );
                theme::hand(combo);
                if picked != state.category {
                    state.category = picked;
                    dirty = true;
                }

                let mut resp = ui.add(
                    egui::TextEdit::singleline(&mut state.query)
                        .hint_text(format!("搜索{}…", state.kind.unit()))
                        .desired_width(ui.available_width().max(150.0)),
                );
                theme::input_menu(&mut resp, &mut state.query, false);
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    state.offset = 0;
                    action = Some(Action::Search(state.kind));
                }
            });
        });
    });
    ui.add_space(8.0);

    // ---- 第二行：装到哪个实例 + 版本 + 排序 ----
    let second = theme::slide_progress(head, theme::TOOLBAR_2_SLIDE_DELAY, theme::TITLE_SLIDE_SECS);
    theme::title_slide(ui, second, |ui| {
        ui.horizontal(|ui| {
            // 整合包不往现有实例里装：它是整个实例的蓝图，下载完另起一个。
            // 摆一个「安装到」下拉只会让人以为选中的那个会被改动。
            if state.kind.installs_into_instance() {
                ui.label(RichText::new("安装到").size(11.5).color(theme::TEXT_WEAK));
                ui.add_space(2.0);
                let current = state
                    .target
                    .clone()
                    .unwrap_or_else(|| "（还没有装好的实例）".to_string());
                let enabled = !instances.is_empty();
                ui.add_enabled_ui(enabled, |ui| {
                    // 数据包那一行还要塞一个「存档」下拉，实例那个框得让点地方，
                    // 不然「排序」会被挤到右边界外去。
                    let target_w = if state.kind.installs_into_world() {
                        150.0
                    } else {
                        220.0
                    };
                    let combo = theme::combo_centered(
                        ui,
                        "res-target",
                        RichText::new(current).size(12.0),
                        target_w,
                        |ui| {
                            for name in instances {
                                let picked = state.target.as_deref() == Some(name.as_str());
                                if theme::combo_item(ui, picked, name).clicked() {
                                    state.target = Some(name.clone());
                                }
                            }
                        },
                    );
                    theme::hand(combo);
                });
                if !enabled {
                    ui.label(
                        RichText::new(format!("先装一个游戏版本，{}才有地方放", state.kind.unit()))
                            .size(11.0)
                            .color(theme::TEXT_FAINT),
                    );
                }

                // 数据包只对某一个存档生效：先挑存档，再说装哪个版本。
                if state.kind.installs_into_world() {
                    ui.add_space(12.0);
                    ui.label(RichText::new("存档").size(11.5).color(theme::TEXT_WEAK));
                    ui.add_space(2.0);
                    let current = state
                        .world
                        .clone()
                        .unwrap_or_else(|| "（还没有存档）".to_string());
                    let enabled = !worlds.is_empty();
                    ui.add_enabled_ui(enabled, |ui| {
                        let combo = theme::combo_centered(
                            ui,
                            "res-world",
                            RichText::new(current).size(12.0),
                            130.0,
                            |ui| {
                                for name in worlds {
                                    let picked = state.world.as_deref() == Some(name.as_str());
                                    if theme::combo_item(ui, picked, name).clicked() {
                                        state.world = Some(name.clone());
                                    }
                                }
                            },
                        );
                        theme::hand(combo);
                    });
                    if !enabled {
                        ui.label(
                            RichText::new("先把游戏开一次，存档生成后才有地方放数据包")
                                .size(11.0)
                                .color(theme::TEXT_FAINT),
                        );
                    }
                }

                // 版本与排序挨在一起：它们是一组语义（看哪个版本、按什么顺序排），
                // 排在同一行读起来才顺，也把第一行腾给真正的关键词与分类筛选。
                ui.add_space(12.0);
            }
            ui.label(RichText::new("版本").size(11.5).color(theme::TEXT_WEAK));
            ui.add_space(2.0);
            let version_label = state
                .game_version
                .clone()
                .unwrap_or_else(|| "全部版本".to_string());
            let mut picked_version = state.game_version.clone();
            let combo = theme::combo_centered(
                ui,
                "res-version",
                RichText::new(version_label).size(12.0),
                112.0,
                |ui| {
                    theme::combo_value(ui, &mut picked_version, None, "全部版本");
                    for version in game_version_choices(instances) {
                        theme::combo_value(ui, &mut picked_version, Some(version.clone()), &version);
                    }
                },
            );
            theme::hand(combo);
            if picked_version != state.game_version {
                state.game_version = picked_version;
                dirty = true;
            }

            ui.add_space(12.0);
            ui.label(RichText::new("排序").size(11.5).color(theme::TEXT_WEAK));
            ui.add_space(2.0);
            let combo = theme::combo_centered(
                ui,
                "res-sort",
                RichText::new(state.sort.label()).size(12.0),
                104.0,
                |ui| {
                    for sort in Sort::ALL {
                        if theme::combo_value(ui, &mut state.sort, sort, sort.label()).changed() {
                            dirty = true;
                        }
                    }
                },
            );
            theme::hand(combo);
        });
    });
    ui.add_space(10.0);

    if dirty {
        // 换了筛选条件就回第一页：停在 offset=200 的一页空白列表上
        // 会让人以为「没结果」，其实是条件变了。
        aicraft_core::audit::detail(format!("资源下载：筛选改为 {}", state.filter_text()));
        state.offset = 0;
        action = Some(Action::Search(state.kind));
    }

    // ---- 结果列表 ----
    // 底下只留「第几到第几条 / 每页 / 上一页下一页」那一行的位置：6 像素
    // 行前间距加一行按钮，34 够用。原先留给 40，滚到底会多出一条白边。
    let list_height = (ui.available_height() - 34.0).max(120.0);
    // 逐行进场的当前时刻；None 表示这一批早就进场完了，逐行走普通画法。
    //
    // 这里不给延迟：结果是从网上一批批取回来的，到达时刻本来就不定；
    // 再往后压一截，每次改筛选都要多等一次，划不来。
    let reveal = theme::row_reveal_elapsed(&mut state.reveal_at, animate, ui.ctx(), 0.0);
    let list_out = egui::ScrollArea::vertical()
        .id_salt("resources-list")
        .auto_shrink([false, false])
        .max_height(list_height)
        .show(ui, |ui| {
            if let Some(error) = &state.error {
                theme::card().show(ui, |ui| {
                    ui.label(RichText::new(error).size(12.0).color(theme::DANGER));
                });
                return;
            }
            if state.hits.is_empty() {
                if !state.loading {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(format!("没有找到符合条件的{}", state.kind.unit()))
                                .size(12.5)
                                .color(theme::TEXT_WEAK),
                        );
                        ui.add_space(4.0);
                        let hint = if state.kind.has_loader() {
                            "换个关键词，或者把版本 / 加载器放宽一点"
                        } else {
                            "换个关键词，或者把版本放宽一点"
                        };
                        ui.label(RichText::new(hint).size(11.0).color(theme::TEXT_FAINT));
                    });
                }
                return;
            }
            ui.spacing_mut().item_spacing.y = 6.0;
            // 分开借两个字段：`hits` 只读，`icons` 要可变（缺图时登记请求）。
            let (hits, icons) = (&state.hits, &mut state.icons);
            for (index, hit) in hits.iter().enumerate() {
                let opened = theme::reveal_row(ui, index, reveal, |ui| row(ui, hit, icons));
                if opened {
                    action = Some(Action::OpenDetail(hit.id.clone(), hit.title.clone()));
                }
            }
        });
    theme::fade_scrolled(ui, &list_out);

    // ---- 翻页 ----
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let from = state.offset + 1;
        let to = (state.offset + state.hits.len()).min(state.total as usize);
        ui.label(
            RichText::new(if state.hits.is_empty() {
                String::new()
            } else {
                format!("第 {}–{} 条", from, to)
            })
            .size(11.0)
            .color(theme::TEXT_WEAK),
        );
        ui.add_space(10.0);
        ui.label(RichText::new("每页").size(11.0).color(theme::TEXT_WEAK));
        let combo = theme::combo_centered(
            ui,
            "res-page-size",
            RichText::new(state.page_size.to_string()).size(11.5),
            52.0,
            |ui| {
                for size in PAGE_CHOICES {
                    if theme::combo_value(ui, &mut state.page_size, size, &size.to_string())
                        .changed()
                    {
                        // 换了每页条数就回第一页：停在 offset=200 的空白页上
                        // 会让人以为「没结果」，其实是每页变少了。
                        state.offset = 0;
                        action = Some(Action::Search(state.kind));
                    }
                }
            },
        );
        theme::hand(combo);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let page = state.page_limit();
            let next_ok = state.offset + page < state.total as usize && !state.loading;
            if ui
                .add_enabled(
                    next_ok,
                    egui::Button::new(RichText::new("下一页").size(12.0)),
                )
                .clicked()
            {
                state.offset += page;
                action = Some(Action::Search(state.kind));
            }
            let prev_ok = state.offset > 0 && !state.loading;
            if ui
                .add_enabled(
                    prev_ok,
                    egui::Button::new(RichText::new("上一页").size(12.0)),
                )
                .clicked()
            {
                state.offset = state.offset.saturating_sub(page);
                action = Some(Action::Search(state.kind));
            }
        });
    });

    action
}

/// 把 markdown 粗粗压成能看的纯文本。
///
/// 只做几件最影响观感的事：去掉图片、把 `[字](链接)` 留成 `字`、
/// 去掉 `**` `#` 这类标记、砍掉空行。模组简介里常塞一堆徽章图片，
/// 原样显示就是一屏 `[[![Buy me a coffee](https://...)](https://...)]`。
fn plain_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find(['[', '!', '*', '`', '#', '<']) {
        out.push_str(&rest[..idx]);
        let tail = &rest[idx..];
        if tail.starts_with('<') {
            // 简介里还混着裸 HTML（`<center>`、`<img src=...>`）。
            // 标签连同它里面的内容一起丢掉，否则会露出半截源码。
            match tail.find('>') {
                Some(end) => rest = &tail[end + 1..],
                None => rest = tail.strip_prefix('<').unwrap_or(tail),
            }
        } else if tail.starts_with('!') && tail[1..].starts_with('[') {
            // 图片：整段直接丢
            if let Some(end) = skip_link(&tail[1..]) {
                rest = &tail[1 + end..];
            } else {
                rest = tail.strip_prefix('!').unwrap_or(tail);
            }
        } else if tail.starts_with('[') {
            // 链接：留文字，丢地址
            match link_parts(tail) {
                Some((label, used)) => {
                    // 标签里还可能套一层图片（徽章就是 `[![图](a)](b)`）
                    out.push_str(&plain_markdown(label));
                    rest = &tail[used..];
                }
                None => {
                    rest = tail.strip_prefix('[').unwrap_or(tail);
                    out.push('[');
                }
            }
        } else if let Some(rest_after) = tail.strip_prefix("**").or_else(|| tail.strip_prefix("__"))
        {
            rest = rest_after;
        } else if tail.starts_with('#') {
            // 标题符号连它后面的那个空格一起吃掉，免得行首留下突兀的空白
            rest = tail
                .strip_prefix("# ")
                .or_else(|| tail.strip_prefix('#'))
                .unwrap_or(tail);
        } else if tail.starts_with('`') {
            rest = tail.strip_prefix('`').unwrap_or(tail);
        } else {
            out.push_str(&tail[..1]);
            rest = &tail[1..];
        }
    }
    out.push_str(rest);

    // 连续空行压成一个，行尾空格去掉
    let mut lines: Vec<&str> = Vec::new();
    for line in out.lines() {
        let line = line.trim_end();
        if line.is_empty() && lines.last().is_some_and(|l: &&str| l.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    lines.join("\n").trim().to_string()
}

/// `[...](...)` 从 `text` 的 `[` 起有多长，`None` 表示没有闭括号。
fn skip_link(text: &str) -> Option<usize> {
    link_parts(text).map(|(_, used)| used)
}

/// 拆 `[标签](地址)`，返回 `(标签, 整段长度)`。
///
/// 括号要按层数配对：徽章的写法是 `[![图](地址)](链接)`，内层图片的
/// `]` 先出现，照着第一个 `]` 切会把整段切成两半、留下半截 `](链接)`。
fn link_parts(text: &str) -> Option<(&str, usize)> {
    if !text.starts_with('[') {
        return None;
    }
    let mut depth = 0usize;
    let mut close = None;
    for (i, c) in text.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let after = &text[close + 1..];
    if !after.starts_with('(') {
        return None;
    }
    let mut depth = 0usize;
    let mut end = None;
    for (i, c) in after.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    Some((&text[1..close], close + 1 + end + 1))
}

/// 详情页：抬头 + 简介 + 所有版本。
///
/// 数据是 `main.rs` 取回来塞进 `state.detail` 的，这里只负责画；
/// 还没到的那部分显示「正在加载」，不会挡住先到的那一半。
fn detail_page(
    ui: &mut egui::Ui,
    state: &mut State,
    worlds: &[String],
    busy: Option<&str>,
) -> Option<Action> {
    let target = state.target.clone();
    let world = state.world.clone();
    let mut picked_file: Option<ModFile> = None;
    let mut open_url: Option<String> = None;
    let mut action = None;
    let icons = &mut state.icons;
    let kind = state.kind;
    let view = state.detail.as_mut()?;

    // ---- 抬头：返回 + 标题 ----
    ui.horizontal(|ui| {
        if theme::dialog_button(ui, "← 返回", true, 74.0).clicked() {
            action = Some(Action::CloseDetail);
        }
        ui.add_space(8.0);
        let title = view
            .detail
            .as_ref()
            .map(|d| d.title.clone())
            .unwrap_or_else(|| view.title.clone());
        ui.label(RichText::new(title).size(17.0).strong());
        if view.detail.is_none() && view.error.is_none() {
            ui.add_space(6.0);
            ui.spinner();
        }
    });
    ui.add_space(10.0);

    if let Some(error) = &view.error {
        theme::card().show(ui, |ui| {
            ui.label(RichText::new(error).size(12.0).color(theme::DANGER));
        });
    }

    let list_height = (ui.available_height() - 20.0).max(160.0);
    egui::ScrollArea::vertical()
        .id_salt("res-detail")
        .auto_shrink([false, false])
        .max_height(list_height)
        .show(ui, |ui| {
            if let Some(detail) = &view.detail {
                if let Some(url) = detail_head(ui, detail, icons, kind) {
                    open_url = Some(url);
                }
                ui.add_space(10.0);
            }

            // ---- 版本列表 ----
            //
            // 先画筛选（它要可变借用 `view`），再取过滤后的列表：
            // 反过来的话 `versions` 会一直借着 `view` 不放。
            if let Some(picked) = detail_filters(ui, view, &target, &world, worlds, kind) {
                state.world = Some(picked);
            }
            ui.add_space(6.0);

            let shown = view.visible_versions().len();
            let (pinned, groups) = view.grouped_versions();
            let total_versions = view.versions.as_ref().map(Vec::len).unwrap_or(0);
            ui.horizontal(|ui| {
                let head = if view.versions.is_some() {
                    format!("所有版本（{} / {}）", shown, total_versions)
                } else {
                    "所有版本".to_string()
                };
                ui.label(RichText::new(head).size(13.0).strong());
                if view.versions.is_none() && view.error.is_none() {
                    ui.spinner();
                }
            });
            ui.add_space(4.0);

            if view.versions.is_some() && shown == 0 {
                ui.add_space(16.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("这个筛选下没有版本")
                            .size(12.0)
                            .color(theme::TEXT_WEAK),
                    );
                    ui.add_space(3.0);
                    ui.label(
                        RichText::new("把加载器或游戏版本放宽一点")
                            .size(11.0)
                            .color(theme::TEXT_FAINT),
                    );
                });
            }

            ui.spacing_mut().item_spacing.y = 6.0;
            // 整合包不需要「装进哪个实例」这个前提：它自己长成一个新实例。
            let can_install = (target.is_some() || !kind.installs_into_instance())
                && (world.is_some() || !kind.installs_into_world())
                && busy.is_none();

            // ---- 置顶的那一个 ----
            //
            // 整页只有这一个「最新版」，其余全按游戏版本收进分组。
            if let Some(version) = pinned {
                let marks = Marks {
                    matched_game: view
                        .matches_game(version)
                        .then(|| view.preferred_game().cloned())
                        .flatten(),
                    pinned: true,
                };
                if version_row(ui, version, can_install, &marks)
                    && let Some(file) = &version.file {
                    picked_file = Some(file.clone());
                }
                if !groups.is_empty() {
                    ui.add_space(4.0);
                }
            }

            // ---- 按游戏版本分组 ----
            //
            // 一个分组的表头与它下面那些行共用一张卡片：同一游戏版本的三种
            // 加载器本来就该读成「一组」，各自一张卡片反而像三件不相干的东西。
            let mut toggle: Option<(String, bool)> = None;
            for group in &groups {
                let open = view.group_open(&group.game);
                let (clicked, picked) =
                    group_card(ui, &group.game, &group.versions, open, can_install);
                if clicked {
                    toggle = Some((group.game.clone(), !open));
                }
                if let Some(index) = picked
                    && let Some(file) = &group.versions[index].file {
                    picked_file = Some(file.clone());
                }
                ui.add_space(2.0);
            }
            if let Some((game, open)) = toggle {
                view.toggled.insert(game, open);
            }
        });

    if let Some(url) = open_url {
        action = Some(Action::OpenUrl(url));
    }
    if let Some(file) = picked_file {
        if kind.installs_into_instance() && target.is_none() {
            state.notice = Some(("还没有可用的实例，先把游戏版本装好".to_string(), false));
        } else if kind.installs_into_world() && world.is_none() {
            state.notice = Some(("还没有存档，先把游戏开一次让它生成".to_string(), false));
        } else if busy.is_some() {
            state.notice = Some(("还有任务在跑，等它结束再装".to_string(), false));
        } else {
            state.notice = Some((format!("正在下载 {}…", file.filename), true));
            action = Some(if kind.installs_into_world() {
                Action::InstallDatapack(file)
            } else if kind.installs_into_instance() {
                Action::InstallFile(file)
            } else {
                Action::DownloadPack(file)
            });
        }
    }
    action
}

/// 详情页里那排外链按钮。
///
/// 宽度按文字量出来再留一点余量 —— `dialog_button` 的 `min_width` 是下限
/// 不是自适应，给死值反而会把长标签截掉。
fn link_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let width = ui
        .painter()
        .layout_no_wrap(
            label.to_owned(),
            FontId::new(13.5, egui::FontFamily::Proportional),
            theme::TEXT,
        )
        .size()
        .x
        + 26.0;
    theme::dialog_button(ui, label, true, width)
}

/// 这个项目在 Modrinth 上的主页地址。
///
/// 三类东西在站点上分属不同目录，slug 之前那一段就是 `ProjectKind::id()`。
fn project_page_url(kind: ProjectKind, slug: &str) -> String {
    format!("https://modrinth.com/{}/{}", kind.id(), slug)
}

/// MC 模组百科的条目地址。
fn mcmod_page_url(class_id: &str) -> String {
    format!("https://www.mcmod.cn/class/{}.html", class_id)
}

/// 详情页顶部的简介卡片。点了外链就把地址传出去。
fn detail_head(
    ui: &mut egui::Ui,
    detail: &ModDetail,
    icons: &mut icons::Cache,
    kind: ProjectKind,
) -> Option<String> {
    let mut opened: Option<String> = None;
    theme::card().show(ui, |ui| {
        let (slot, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), DETAIL_ICON + 6.0),
            Sense::hover(),
        );
        let icon_rect = egui::Rect::from_min_size(slot.min, Vec2::splat(DETAIL_ICON));
        icons.want(&detail.id, detail.icon_url.as_deref());
        match icons.texture(&detail.id) {
            Some(texture) => {
                egui::Image::from_texture(texture)
                    .corner_radius(ICON_ROUNDING)
                    .paint_at(ui, icon_rect);
            }
            None => {
                ui.painter().rect_filled(
                    icon_rect,
                    CornerRadius::same(ICON_ROUNDING),
                    theme::BG_BOTTOM,
                );
            }
        }
        let text_left = icon_rect.right() + 12.0;
        let room = (slot.right() - text_left).max(60.0);
        let title = elide(
            ui,
            &detail.title,
            FontId::new(16.0, egui::FontFamily::Proportional),
            theme::TEXT,
            room,
        );
        ui.painter()
            .galley(egui::pos2(text_left, slot.top() + 6.0), title, theme::TEXT);
        let sub = if detail.author.is_empty() {
            detail.description.clone()
        } else {
            format!("by {}　·　{}", detail.author, detail.description)
        };
        let sub = elide(ui, &sub, FontId::proportional(11.0), theme::TEXT_WEAK, room);
        ui.painter().galley(
            egui::pos2(text_left, slot.top() + 32.0),
            sub,
            theme::TEXT_WEAK,
        );

        ui.add_space(4.0);
        // 不用 ♡ 这类符号：默认字体里没有，会显示成方块
        let mut stats = vec![
            format!("↓ {} 下载", detail.downloads_text()),
            format!("{} 人收藏", detail.follows_text()),
        ];
        if !detail.license.is_empty() {
            stats.push(format!("许可 {}", detail.license));
        }
        if !detail.updated.is_empty() {
            stats.push(format!("更新于 {}", detail.updated));
        }
        ui.label(
            RichText::new(stats.join("　·　"))
                .size(11.0)
                .color(theme::TEXT_FAINT),
        );

        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            for loader in &detail.loaders {
                if let Some(kind) = LoaderKind::from_id(loader) {
                    theme::chip_loader_plain(ui, Some(kind), kind.label());
                }
            }
            for cat in &detail.categories {
                if LoaderKind::from_id(cat).is_some() {
                    continue;
                }
                let label = aicraft_core::modrinth::category_label(cat)
                    .map(str::to_string)
                    .unwrap_or_else(|| cat.clone());
                theme::chip(ui, &label);
            }
        });

        if !detail.body.trim().is_empty() {
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .id_salt("res-detail-body")
                .max_height(BODY_MAX_H)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(plain_markdown(detail.body.trim()))
                            .size(11.5)
                            .color(theme::TEXT_WEAK),
                    );
                });
        }

        // 项目主页和百科页永远排在最前：想看清楚这是个什么东西，
        // 这两个页面比卡片里这几行字有用得多。
        // 老接口偶尔不给 slug，那就退回用项目 id：Modrinth 的地址两样都认。
        let handle = if detail.slug.is_empty() {
            detail.id.as_str()
        } else {
            detail.slug.as_str()
        };
        let mut links: Vec<(&str, String)> = vec![
            ("Modrinth 页面", project_page_url(kind, handle)),
        ];
        if kind == ProjectKind::Mod
            && let Some(id) = aicraft_core::modnames::mcmod_id(&detail.slug) {
            links.push(("MC 模组百科", mcmod_page_url(id)));
        }
        for (label, url) in [
            ("源码", detail.source_url.clone()),
            ("问题反馈", detail.issues_url.clone()),
        ] {
            if let Some(url) = url {
                links.push((label, url));
            }
        }
        if !links.is_empty() {
            ui.add_space(8.0);
            // 按钮宽度跟着字走：这几个标签长短差得多，一律 84 会把
            // 「MC 模组百科」挤成省略号。窄窗口里让它们自己换行。
            ui.horizontal_wrapped(|ui| {
                for (label, url) in links {
                    if link_button(ui, label).clicked() {
                        opened = Some(url);
                    }
                }
            });
        }
    });
    opened
}

/// 详情页的版本筛选。两张表都在本地列表里找，不重新请求。
///
/// 返回玩家在这一排里改出来的存档名（数据包专用；别的类别恒为 `None`）。
fn detail_filters(
    ui: &mut egui::Ui,
    view: &mut DetailView,
    target: &Option<String>,
    world: &Option<String>,
    worlds: &[String],
    kind: ProjectKind,
) -> Option<String> {
    let mut picked_world: Option<String> = None;
    let mut all_loaders: Vec<LoaderKind> = Vec::new();
    let mut all_versions: Vec<String> = Vec::new();
    if let Some(all) = &view.versions {
        for v in all {
            for l in &v.loaders {
                if let Some(kind) = LoaderKind::from_id(l)
                    && !all_loaders.contains(&kind) {
                    all_loaders.push(kind);
                }
            }
            for gv in &v.game_versions {
                if !all_versions.contains(gv) {
                    all_versions.push(gv.clone());
                }
            }
        }
    }
    all_versions.sort_by_key(|v| std::cmp::Reverse(version_key(v)));

    ui.horizontal(|ui| {
        // 资源包和光影包的版本列表里不会有加载器，摆一个永远筛空的
        // 下拉框只会让人以为「这模组坏了」。
        if kind.has_loader() {
            let loader_label = view
                .loader
                .map(|l| l.label().to_string())
                .unwrap_or_else(|| "全部加载器".to_string());
            let combo = theme::combo_centered(
                ui,
                "res-detail-loader",
                RichText::new(loader_label).size(12.0),
                112.0,
                |ui| {
                    theme::combo_value(ui, &mut view.loader, None, "全部加载器");
                    for kind in &all_loaders {
                        theme::combo_value(ui, &mut view.loader, Some(*kind), kind.label());
                    }
                },
            );
            theme::hand(combo);
        }

        let version_label = view
            .game_version
            .clone()
            .unwrap_or_else(|| "全部版本".to_string());
        let combo = theme::combo_centered(
            ui,
            "res-detail-version",
            RichText::new(version_label).size(12.0),
            112.0,
            |ui| {
                theme::combo_value(ui, &mut view.game_version, None, "全部版本");
                for gv in &all_versions {
                    theme::combo_value(ui, &mut view.game_version, Some(gv.clone()), gv);
                }
            },
        );
        theme::hand(combo);


        // 数据包要挑存档，这里也放一个：进详情页之后发现挑错了，
        // 不必退回列表页再改一次。
        if kind.installs_into_world() {
            let current = world
                .clone()
                .unwrap_or_else(|| "（还没有存档）".to_string());
            let enabled = !worlds.is_empty();
            let mut choice = world.clone();
            ui.add_enabled_ui(enabled, |ui| {
                let combo = theme::combo_centered(
                    ui,
                    "res-detail-world",
                    RichText::new(current).size(12.0),
                    150.0,
                    |ui| {
                        for name in worlds {
                            theme::combo_value(ui, &mut choice, Some(name.clone()), name);
                        }
                    },
                );
                theme::hand(combo);
            });
            if choice != *world {
                picked_world = choice;
            }
        }

        // 整合包不会装进任何现有实例，说「装进 X」是在骗人。
        if let Some(t) = target.as_ref().filter(|_| kind.installs_into_instance()) {
            ui.add_space(6.0);
            let where_to = match world.as_ref().filter(|_| kind.installs_into_world()) {
                Some(w) => format!("装进 {} / {}", t, w),
                None => format!("装进 {}", t),
            };
            ui.label(
                RichText::new(where_to)
                    .size(11.0)
                    .color(theme::TEXT_FAINT),
            );
        }
    });
    picked_world
}

/// 分组卡片的表头高度。
const GROUP_HEAD_H: f32 = 34.0;

/// 分组卡片里一行版本的高度。
///
/// 比独立卡片（54）矮两像素：行与行之间靠细线分隔，不再各留一圈边距。
const GROUP_ROW_H: f32 = 52.0;

/// 展开的分组底部留的那点白。
const GROUP_PAD_H: f32 = 4.0;

/// 一个游戏版本分组的整张卡片：表头 `▸ 1.21.1　12 个版本` 加（展开时的）若干行。
///
/// `open` 传进来是当前状态，点一下就变成 `!open`；调用方拿返回值去写回
/// `DetailView::toggled`（不能在里面改，那个 `HashMap` 属于 `view`，
/// 而这里正借着它读版本列表）。
///
/// 返回 `(表头被点了吗, 点了下载的是第几行)`。
fn group_card(
    ui: &mut egui::Ui,
    game: &str,
    versions: &[&ModVersion],
    open: bool,
    can_install: bool,
) -> (bool, Option<usize>) {
    let width = ui.available_width();
    let shown = if open { versions.len() } else { 0 };
    let height = GROUP_HEAD_H
        + GROUP_ROW_H * shown as f32
        + if open { GROUP_PAD_H } else { 0.0 };
    let (slot, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let rect = slot.shrink2(Vec2::new(0.0, 1.0));
    ui.painter()
        .rect_filled(rect, CornerRadius::same(10), theme::card_fill());
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );

    // ---- 表头：整条都能点，点了收起 / 展开 ----
    let head_rect = egui::Rect::from_min_size(rect.min, Vec2::new(rect.width(), GROUP_HEAD_H));
    let response = ui.interact(
        head_rect,
        ui.scope_id().with(("res-group", game)),
        Sense::click(),
    );
    if response.hovered() {
        // 只亮表头那一块：整张卡片跟着高亮会让人分不清点的是表头还是某一行。
        ui.painter().rect_filled(
            head_rect.shrink(1.0),
            CornerRadius {
                nw: 9,
                ne: 9,
                sw: 0,
                se: 0,
            },
            theme::BG_BOTTOM,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let inner = head_rect.shrink2(Vec2::new(12.0, 0.0));
    // 三角用画的不塞字符：`▶`/`▸` 在中文字体里有没有全看运气
    let tri = egui::pos2(inner.left() + 4.0, head_rect.center().y);
    let color = theme::TEXT_WEAK;
    let points = if open {
        vec![
            egui::pos2(tri.x - 4.0, tri.y - 2.5),
            egui::pos2(tri.x + 4.0, tri.y - 2.5),
            egui::pos2(tri.x, tri.y + 3.0),
        ]
    } else {
        vec![
            egui::pos2(tri.x - 2.5, tri.y - 4.0),
            egui::pos2(tri.x - 2.5, tri.y + 4.0),
            egui::pos2(tri.x + 3.0, tri.y),
        ]
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(points, color, Stroke::NONE));

    let text_left = inner.left() + 16.0;
    ui.painter().text(
        egui::pos2(text_left, head_rect.center().y),
        Align2::LEFT_CENTER,
        game,
        FontId::new(12.5, egui::FontFamily::Proportional),
        theme::TEXT,
    );
    let galley = ui.painter().layout_no_wrap(
        game.to_string(),
        FontId::new(12.5, egui::FontFamily::Proportional),
        theme::TEXT,
    );
    ui.painter().text(
        egui::pos2(text_left + galley.size().x + 8.0, head_rect.center().y),
        Align2::LEFT_CENTER,
        format!("{} 个版本", versions.len()),
        FontId::proportional(10.5),
        theme::TEXT_FAINT,
    );

    // ---- 展开的那些行：同一张卡片里，行与行之间只隔一条细线 ----
    let mut picked = None;
    if open {
        let line = Stroke::new(1.0, theme::BORDER);
        for (index, version) in versions.iter().enumerate() {
            let top = head_rect.max.y + GROUP_ROW_H * index as f32;
            let row = egui::Rect::from_min_size(
                egui::pos2(rect.left(), top),
                Vec2::new(rect.width(), GROUP_ROW_H),
            );
            // 表头下面那条线由第一条自己的上沿画，免得两处各画一条、叠成两像素
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left() + 12.0, top),
                    egui::pos2(rect.max.x - 12.0, top),
                ],
                line,
            );
            // 分组本身已经说明了游戏版本，行里不再重复打标记
            if draw_version_row(
                ui,
                row.shrink2(Vec2::new(0.0, 1.0)),
                None,
                version,
                can_install,
                &Marks::default(),
            ) {
                picked = Some(index);
            }
        }
    }

    (response.clicked(), picked)
}

/// 一行版本要打的标记。
///
/// `matched_game` 只用来决定卡片描边（能装进当前实例的那几行亮一圈主题色），
/// 不再在标题旁边打「支持 xxx」——置顶那一行和分组标题都已经说清楚了，
/// 同一句话写三遍只是噪音。
#[derive(Default)]
struct Marks {
    matched_game: Option<String>,
    /// 是不是置顶的那一条（标题旁打「适配当前版本」）。
    pinned: bool,
}

/// 置顶那条的标记文案。
const PINNED_LABEL: &str = "适配当前版本";

/// 标记之间、以及标记与版本号之间的间隙。
const BADGE_GAP: f32 = 6.0;

/// 一个标记要多宽（文字宽 + 左右内边距）。
fn badge_width(ui: &egui::Ui, text: &str) -> f32 {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::proportional(10.0),
        theme::TEXT_WEAK,
    );
    galley.size().x + 14.0
}

/// 在 `x` 处画一个小标记，纵向以 `center_y` 为中线，返回它占的宽度。
///
/// `center_y` 要传**标题那一行的视觉中线**（`inner.top() + 行高 / 2`），
/// 不能拿标题的顶端当基准——那样标记会整体浮在字上面，看着像歪了。
///
/// 右边留的宽度不够时干脆不画：挤到「下载」按钮上比没有更难看。
fn badge(
    ui: &egui::Ui,
    x: f32,
    center_y: f32,
    text: &str,
    fill: Color32,
    color: Color32,
    limit_x: f32,
) -> f32 {
    let width = badge_width(ui, text);
    if x + width > limit_x {
        return 0.0;
    }
    let rect = egui::Rect::from_center_size(
        egui::pos2(x + width / 2.0, center_y),
        Vec2::new(width, 18.0),
    );
    ui.painter().rect_filled(rect, CornerRadius::same(9), fill);
    // 中文字体整体上移了 6%（`install_cjk_font` 的 `y_offset_factor`），
    // 小号字在这个高度上偏得比较明显，这里补回半个像素。
    ui.painter().text(
        rect.center() + Vec2::new(0.0, 0.5),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(10.0),
        color,
    );
    width
}

/// 版本列表里一个独立的行（置顶那一条）：自己是一张卡片。点「下载」返回 `true`。
fn version_row(ui: &mut egui::Ui, version: &ModVersion, can_install: bool, marks: &Marks) -> bool {
    let (slot, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 54.0), Sense::click());
    let rect = slot.shrink2(Vec2::new(0.0, 1.0));
    draw_version_row(ui, rect, Some(&response), version, can_install, marks)
}

/// 在给定矩形里画一行版本。
///
/// `hover` 传 `Some` 时按「自己占一整张卡片」的画法（悬停换底色、四周描边）；
/// 分组卡片里的行传 `None`：它们共用外层那一张卡片，悬停只让文字那一块亮一下，
/// 描边交给分组去画——每行各描一圈会把一张卡片切成一摞卡片。
fn draw_version_row(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    hover: Option<&egui::Response>,
    version: &ModVersion,
    can_install: bool,
    marks: &Marks,
) -> bool {
    let response = match hover {
        Some(response) => response.clone(),
        None => ui.interact(rect, ui.scope_id().with(("res-line", &version.id)), Sense::click()),
    };
    // 能装进当前实例的那几行描边用主题色，扫一眼就知道该点哪几行
    let border = if marks.matched_game.is_some() {
        theme::accent_fill()
    } else {
        theme::BORDER
    };
    if hover.is_some() {
        let fill = if response.hovered() {
            theme::CARD_HOVER
        } else {
            theme::card_fill()
        };
        ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(10),
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
    } else if response.hovered() {
        // 分组里的行：整张卡片已经铺好了白底，这里只把指针下那一行点亮。
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), theme::CARD_HOVER);
        if marks.matched_game.is_some() {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(8),
                Stroke::new(1.0, border),
                StrokeKind::Inside,
            );
        }
    } else if marks.matched_game.is_some() {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
    }
    let inner = rect.shrink2(Vec2::new(14.0, 8.0));
    let left_width = (inner.width() - 92.0).max(60.0);

    // 标记先算出来，标题的可用宽度得把它们扣掉——
    // 否则 Sodium 那种长版本号会把标记直接顶出卡片
    let mut badges: Vec<(String, Color32, Color32)> = Vec::new();
    if marks.pinned {
        badges.push((
            PINNED_LABEL.to_string(),
            theme::accent_soft(),
            theme::accent_strong(),
        ));
    }
    let badge_total: f32 = badges
        .iter()
        .map(|(text, _, _)| badge_width(ui, text) + BADGE_GAP)
        .sum();

    // 第一行：版本号（作者起的名字常为空，有就放后面）
    let title = if version.name.trim().is_empty() {
        version.version_number.clone()
    } else {
        format!("{}　{}", version.version_number, version.name.trim())
    };
    let galley = elide(
        ui,
        &title,
        FontId::new(12.5, egui::FontFamily::Proportional),
        theme::TEXT,
        (left_width - badge_total).max(40.0),
    );
    let title_width = galley.size().x;
    let title_center_y = inner.top() + galley.size().y / 2.0;
    ui.painter()
        .galley(egui::pos2(inner.left(), inner.top()), galley, theme::TEXT);

    // 标记跟在标题右边、与它垂直居中
    if !badges.is_empty() {
        let mut x = inner.left() + title_width + BADGE_GAP;
        for (text, fill, color) in badges {
            let width = badge(
                ui,
                x,
                title_center_y,
                &text,
                fill,
                color,
                inner.right() - 92.0,
            );
            if width == 0.0 {
                break;
            }
            x += width + BADGE_GAP;
        }
    }

    // 第二行：游戏版本 + 加载器 + 日期 + 下载量
    let mut meta = Vec::new();
    if let Some(first) = version.game_versions.first() {
        let more = version.game_versions.len();
        meta.push(if more > 1 {
            format!("{} 等 {} 个版本", first, more)
        } else {
            first.clone()
        });
    }
    for l in &version.loaders {
        if let Some(kind) = LoaderKind::from_id(l) {
            meta.push(kind.label().to_string());
        }
    }
    if !version.date.is_empty() {
        meta.push(version.date.clone());
    }
    meta.push(format!(
        "↓ {}",
        aicraft_core::modrinth::count_text(version.downloads)
    ));
    let galley = elide(
        ui,
        &meta.join("　·　"),
        FontId::proportional(10.5),
        theme::TEXT_FAINT,
        left_width,
    );
    ui.painter().galley(
        egui::pos2(inner.left(), inner.top() + 20.0),
        galley,
        theme::TEXT_FAINT,
    );

    // 右侧按钮
    let enabled = can_install && version.file.is_some();
    let button_rect = egui::Rect::from_min_size(
        egui::pos2(inner.right() - 78.0, rect.center().y - 13.0),
        Vec2::new(78.0, 26.0),
    );
    let button_resp = ui.interact(
        button_rect,
        ui.scope_id().with(("res-version", &version.id)),
        Sense::click(),
    );
    let hovered = button_resp.hovered() && enabled;
    let fill = if !enabled {
        theme::BORDER
    } else if hovered {
        theme::accent_strong()
    } else {
        theme::accent_fill()
    };
    ui.painter()
        .rect_filled(button_rect, CornerRadius::same(6), fill);
    ui.painter().text(
        button_rect.center(),
        Align2::CENTER_CENTER,
        if version.file.is_some() {
            "下载"
        } else {
            "无文件"
        },
        FontId::proportional(11.5),
        if enabled {
            theme::accent_ink()
        } else {
            theme::TEXT_FAINT
        },
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    button_resp.clicked() && enabled
}

/// 只排一行、超宽用 `…` 收尾的文字。
///
/// `painter().text(...)` 是「有多少画多少」，长描述会直接冲出卡片。
/// 这里走 `LayoutJob` 的 `max_rows = 1`，让排版自己把尾巴吃掉。
fn elide(
    ui: &egui::Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// 一个结果卡片。整张卡片可点，点了就进详情页——装哪个版本由详情页决定，
/// 列表里只有「一长串版本，猜哪个能装」的信息量，放手让玩家在那儿选。
fn row(ui: &mut egui::Ui, hit: &ModHit, icons: &mut icons::Cache) -> bool {
    let (slot, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
    let rect = slot.shrink2(Vec2::new(0.0, 1.5));
    let fill = if response.hovered() {
        theme::CARD_HOVER
    } else {
        theme::card_fill()
    };
    ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );

    let inner = rect.shrink2(Vec2::new(14.0, 10.0));

    // 左边的图标。没下回来的先画一块灰底，别留黑洞——
    // 空一块会让整行看起来像没加载完，灰块至少说明「这里会有图」。
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(inner.left(), inner.center().y - ICON_SIZE / 2.0),
        Vec2::splat(ICON_SIZE),
    );
    match icons.texture(&hit.id) {
        Some(texture) => {
            // 走 `Image` 而不是 `painter().image`：只有前者会按 `corner_radius`
            // 把四角裁圆（内部是把 uv 之外的像素丢弃，不是真的加个遮罩）。
            egui::Image::from_texture(texture)
                .corner_radius(ICON_ROUNDING)
                .paint_at(ui, icon_rect);
        }
        None => {
            ui.painter().rect_filled(
                icon_rect,
                CornerRadius::same(ICON_ROUNDING),
                theme::BG_BOTTOM,
            );
            ui.painter().rect_stroke(
                icon_rect,
                CornerRadius::same(ICON_ROUNDING),
                Stroke::new(1.0, theme::BORDER),
                StrokeKind::Inside,
            );
        }
    }
    // 登记这张图（已经有的不会再排一次）
    icons.want(&hit.id, hit.icon_url.as_deref());

    // 文字从图标右边开始
    let inner =
        egui::Rect::from_min_max(egui::pos2(icon_rect.right() + 12.0, inner.top()), inner.max);

    // 没有右下角按钮了，整行宽度都留给文字；只留一点右边距
    let text_width = (inner.width() - 8.0).max(40.0);

    // 标题行：名字 + 作者
    //
    // 名字太长（有模组就叫一长串）会把作者顶出卡片右边，所以同样只给一行，
    // 超了就用 `…` 收尾。
    let title_font = FontId::new(13.5, egui::FontFamily::Proportional);
    let title = elide(ui, &hit.title, title_font.clone(), theme::TEXT, text_width);
    let title_width = title.size().x;
    ui.painter().galley(inner.left_top(), title, theme::TEXT);
    if !hit.author.is_empty() && title_width + 8.0 < text_width {
        let room = text_width - title_width - 8.0;
        let author = elide(
            ui,
            &format!("by {}", hit.author),
            FontId::proportional(11.0),
            theme::TEXT_FAINT,
            room,
        );
        ui.painter().galley(
            egui::pos2(inner.left() + title_width + 8.0, inner.top() + 1.0),
            author,
            theme::TEXT_FAINT,
        );
    }

    // 描述：一行，超宽用 `…` 收尾
    //
    // `layout_no_wrap` 不换行也不截断，长描述会直接画出卡片外、压到右边的
    // 「下载」按钮下面。这里改成限宽 + 只排一行。
    let description = elide(
        ui,
        &hit.description,
        FontId::proportional(11.5),
        theme::TEXT_WEAK,
        text_width,
    );
    ui.painter().galley(
        egui::pos2(inner.left(), inner.top() + 21.0),
        description,
        theme::TEXT_WEAK,
    );

    // 底行：加载器标签 + 下载量 + 支持的最新版本
    let mut x = inner.left();
    let bottom = inner.bottom() - 4.0;
    for loader in &hit.loaders {
        if let Some(kind) = LoaderKind::from_id(loader) {
            // 只有字：底排常常要并排三四个标签，8px 的徽记比字还抢眼，
            // 一列扫下来像一串小印章。颜色照样能把它们分开。
            let style = theme::loader_chip(Some(kind));
            let font = FontId::new(10.0, egui::FontFamily::Proportional);
            let galley = ui
                .painter()
                .layout_no_wrap(kind.label().to_string(), font, style.text);
            let size = galley.size() + Vec2::new(10.0, 2.0);
            let chip_rect = egui::Rect::from_min_size(egui::pos2(x, bottom - size.y), size);
            ui.painter()
                .rect_filled(chip_rect, CornerRadius::same(5), style.fill);
            ui.painter().galley(
                egui::pos2(
                    chip_rect.center().x - galley.size().x / 2.0,
                    bottom - size.y + 1.0,
                ),
                galley,
                style.text,
            );
            x += size.x + 4.0;
        }
    }
    let text = format!(
        "↓ {}   支持 {}",
        hit.downloads_text(),
        hit.versions
            .first()
            .cloned()
            .unwrap_or_else(|| "?".to_string())
    );
    // 这一行也可能长过卡片（下载量 + 版本号），同样只留一行、画到按钮前为止
    let room = (inner.left() + text_width - x - 4.0).max(0.0);
    if room > 24.0 {
        let galley = elide(
            ui,
            &text,
            FontId::proportional(10.5),
            theme::TEXT_FAINT,
            room,
        );
        let height = galley.size().y;
        ui.painter().galley(
            egui::pos2(x + 4.0, bottom - height),
            galley,
            theme::TEXT_FAINT,
        );
    }

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

/// 「版本筛选」下拉里能选的游戏版本。
///
/// 只列本机已经装过的那些：玩家想给哪个实例加模组，那个版本的模组才用得上。
/// 让他从 300 多个官方版本里挑一个自己没装的，选完还得先回来装游戏。
fn game_version_choices(instances: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in instances {
        // 实例名常带加载器后缀（`1.21.1-Fabric`），把前面那段版本号取出来
        let version = name.split(['-', ' ']).next().unwrap_or(name).to_string();
        if version.chars().next().is_some_and(|c| c.is_ascii_digit()) && !out.contains(&version) {
            out.push(version);
        }
    }
    out.sort_by_key(|v| std::cmp::Reverse(version_key(v)));
    out
}

/// 版本号排序用的键：`1.21.1` 要比 `1.9.4` 大。
fn version_key(text: &str) -> Vec<u32> {
    text.split(['.', '-', '_'])
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_yield_their_game_version() {
        let instances = vec![
            "1.21.1".to_string(),
            "1.21.1-Fabric".to_string(),
            "1.20.1-Forge".to_string(),
            "随便起的名字".to_string(),
        ];
        let choices = game_version_choices(&instances);
        assert_eq!(choices, vec!["1.21.1".to_string(), "1.20.1".to_string()]);
    }

    #[test]
    fn newer_versions_sort_first() {
        let instances = vec![
            "1.9.4".to_string(),
            "1.21.1".to_string(),
            "1.20.1".to_string(),
        ];
        assert_eq!(
            game_version_choices(&instances),
            vec![
                "1.21.1".to_string(),
                "1.20.1".to_string(),
                "1.9.4".to_string()
            ]
        );
    }

    #[test]
    fn target_falls_back_to_the_first_instance() {
        let mut state = State::default();
        let instances = vec!["a".to_string(), "b".to_string()];
        state.pick_target(&instances, None);
        assert_eq!(state.target.as_deref(), Some("a"));

        // 选过的实例还在，就不该被换掉
        state.target = Some("b".to_string());
        state.pick_target(&instances, None);
        assert_eq!(state.target.as_deref(), Some("b"));

        // 实例没了才回退
        state.pick_target(&[], None);
        assert_eq!(state.target, None);
    }

    /// 安装目标跟着主页走：主页换版本，这里跟着换。
    #[test]
    fn target_follows_the_version_chosen_at_home() {
        let mut state = State::default();
        let instances = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        state.pick_target(&instances, Some("c"));
        assert_eq!(state.target.as_deref(), Some("c"), "该落到主页选中的版本");

        // 主页换了版本，安装目标跟着换
        state.pick_target(&instances, Some("a"));
        assert_eq!(state.target.as_deref(), Some("a"), "主页换了就得跟着换");

        // 主页没换时，玩家在下拉里自己挑的要留着
        state.target = Some("b".to_string());
        state.pick_target(&instances, Some("a"));
        assert_eq!(state.target.as_deref(), Some("b"), "主页没动就别顶掉手选");

        // 主页选中的那个没装成实例（或者已经被删）就退回第一个
        let mut fresh = State::default();
        fresh.pick_target(&instances, Some("missing"));
        assert_eq!(fresh.target.as_deref(), Some("a"));

        // 压根没有已装实例时保持空
        let mut empty = State::default();
        empty.pick_target(&[], Some("c"));
        assert_eq!(empty.target, None);
    }

    fn version(number: &str, loaders: &[&str], gv: &[&str]) -> ModVersion {
        ModVersion {
            id: format!("id-{}", number),
            name: String::new(),
            version_number: number.to_string(),
            game_versions: gv.iter().map(|v| v.to_string()).collect(),
            loaders: loaders.iter().map(|l| l.to_string()).collect(),
            downloads: 0,
            date: String::new(),
            file: None,
        }
    }

    #[test]
    fn markdown_badges_become_plain_text() {
        // Sodium 简介的真实样子：图片套链接、再来一段普通链接
        let text = concat!(
            "**Note** this is [great](https://example.com).

",
            "[![Buy me a coffee](https://cdn.modrinth.com/a.png)](https://ko-fi.com/x)
",
            "`code` and # heading



end",
        );
        let out = plain_markdown(text);
        assert_eq!(
            out,
            "Note this is great.

code and heading

end",
            "实际得到: {out:?}"
        );
    }

    #[test]
    fn raw_html_is_dropped() {
        let text = "<center>
<img src=\"https://cdn.modrinth.com/a.png\" width=\"1\">
</center>
谢谢";
        assert_eq!(plain_markdown(text), "谢谢");
        // 落单的 `<` 不能把后面的正文一起吞掉
        assert_eq!(plain_markdown("1 < 2 是对的"), "1  2 是对的");
    }

    #[test]
    fn markdown_without_markup_is_untouched() {
        assert_eq!(
            plain_markdown(
                "  hello
world  "
            ),
            "hello
world"
        );
        assert_eq!(plain_markdown(""), "");
        // 落单的方括号原样留着，不要吞掉用户看得见的字
        assert_eq!(plain_markdown("a [b"), "a [b");
    }

    #[test]
    fn detail_filters_are_applied_locally() {
        let mut view = DetailView::new("p".to_string(), "Sodium".to_string());
        view.versions = Some(vec![
            version("1.0", &["fabric"], &["1.21.1"]),
            version("0.9", &["forge"], &["1.20.1"]),
            version("0.8", &["fabric", "forge"], &["1.21.1", "1.20.1"]),
        ]);
        assert_eq!(view.visible_versions().len(), 3);

        view.loader = Some(LoaderKind::Fabric);
        assert_eq!(view.visible_versions().len(), 2);

        view.game_version = Some("1.21.1".to_string());
        assert_eq!(view.visible_versions().len(), 2);

        view.loader = Some(LoaderKind::NeoForge);
        assert!(view.visible_versions().is_empty());

        // 版本列表还没回来时不该报错，也不该显示「没有版本」
        let empty = DetailView::new("p".to_string(), "x".to_string());
        assert!(empty.visible_versions().is_empty());
    }

    #[test]
    fn each_kind_links_to_its_own_modrinth_section() {
        assert_eq!(project_page_url(ProjectKind::Mod, "sodium"), "https://modrinth.com/mod/sodium");
        assert_eq!(project_page_url(ProjectKind::ResourcePack, "x"), "https://modrinth.com/resourcepack/x");
        assert_eq!(project_page_url(ProjectKind::Shader, "y"), "https://modrinth.com/shader/y");
    }

    #[test]
    fn a_wiki_entry_becomes_a_class_page() {
        assert_eq!(mcmod_page_url("2021"), "https://www.mcmod.cn/class/2021.html");
    }

    #[test]
    fn the_pinned_one_matches_the_instance_game_and_loader() {
        let mut view = DetailView::new("p".to_string(), "Sodium".to_string());
        view.versions = Some(vec![
            version("4.0", &["fabric"], &["1.21.4"]),
            version("3.9", &["fabric"], &["1.21.1"]),
            version("3.8", &["forge"], &["1.21.1"]),
            version("3.7", &["fabric"], &["1.21.1"]),
        ]);
        view.game_hint = Some("1.21.1".to_string());
        view.loader_hint = Some(LoaderKind::Fabric);

        // 置顶的不是「整表最新」（4.0 是别的游戏版本），而是
        // 「这个游戏版本 + 这个加载器」里最新的那一版
        assert_eq!(
            view.pinned_version().map(|v| v.version_number.as_str()),
            Some("3.9")
        );

        // 原版实例没有加载器偏好：这个游戏版本下最新的一版就行，
        // 不管它是给哪个加载器做的
        view.loader_hint = None;
        assert_eq!(
            view.pinned_version().map(|v| v.version_number.as_str()),
            Some("3.9")
        );

        // Forge 实例就得置顶 Forge 版
        view.loader_hint = Some(LoaderKind::Forge);
        assert_eq!(
            view.pinned_version().map(|v| v.version_number.as_str()),
            Some("3.8")
        );

        view.game_hint = Some("1.21.1".to_string());
        view.loader_hint = Some(LoaderKind::Fabric);
        // 手选的筛选比实例的提示优先
        view.loader = Some(LoaderKind::Fabric);
        assert_eq!(
            view.pinned_version().map(|v| v.version_number.as_str()),
            Some("3.9")
        );

        // 置顶那一版不会再出现在分组里
        let (pinned, groups) = view.grouped_versions();
        assert_eq!(pinned.map(|v| v.version_number.as_str()), Some("3.9"));
        assert!(!groups
            .iter()
            .any(|g| g.versions.iter().any(|v| v.version_number == "3.9")));
    }

    #[test]
    fn nothing_is_pinned_when_no_visible_version_fits() {
        let mut view = DetailView::new("p".to_string(), "Sodium".to_string());
        view.versions = Some(vec![
            version("4.0", &["fabric"], &["1.21.4"]),
            version("3.9", &["fabric"], &["1.21.1"]),
        ]);
        // 筛到 1.20.1：两个版本都不适配，一个都不该置顶
        view.game_version = Some("1.20.1".to_string());
        assert!(view.pinned_version().is_none());
        let (pinned, groups) = view.grouped_versions();
        assert!(pinned.is_none());
        assert!(groups.is_empty());
    }

    #[test]
    fn the_latest_one_is_pinned_and_the_rest_group_by_game() {
        let mut view = DetailView::new("p".to_string(), "Sodium".to_string());
        view.versions = Some(vec![
            version("3.0", &["fabric"], &["1.21.4"]),
            version("2.9", &["fabric"], &["1.21.1"]),
            version("2.8", &["fabric"], &["1.21.4"]),
            version("2.7", &["fabric"], &["1.20.1"]),
        ]);

        let (pinned, groups) = view.grouped_versions();
        // 只有一个置顶：最新那一版，且不再出现在分组里
        assert_eq!(pinned.map(|v| v.version_number.as_str()), Some("3.0"));
        let shape: Vec<(&str, usize)> = groups
            .iter()
            .map(|g| (g.game.as_str(), g.versions.len()))
            .collect();
        assert_eq!(shape, vec![("1.21.4", 1), ("1.21.1", 1), ("1.20.1", 1)]);
        // 组内保持 API 的时间倒序
        assert_eq!(groups[0].versions[0].version_number, "2.8");

        // 默认只展开当前实例那个游戏版本
        view.game_hint = Some("1.21.1".to_string());
        assert!(view.group_open("1.21.1"));
        assert!(!view.group_open("1.21.4"));
        // 手动收起的会被记住
        view.toggled.insert("1.21.1".to_string(), false);
        assert!(!view.group_open("1.21.1"));
    }

    #[test]
    fn a_filtered_out_version_is_never_pinned() {
        let mut view = DetailView::new("p".to_string(), "Sodium".to_string());
        view.versions = Some(vec![
            version("3.0", &["fabric"], &["1.21.4"]),
            version("2.9", &["fabric"], &["1.21.1"]),
        ]);
        // 筛到 1.21.1：置顶的必须是筛完还在的那个，3.0 不许越界冒出来
        view.game_version = Some("1.21.1".to_string());
        let (pinned, groups) = view.grouped_versions();
        assert_eq!(pinned.map(|v| v.version_number.as_str()), Some("2.9"));
        assert!(groups.is_empty());
    }

    #[test]
    fn switching_kind_clears_the_previous_ones_state() {
        let mut state = State {
            kind: ProjectKind::Mod,
            ..State::default()
        };
        state.category = 5;
        state.loader = Some(LoaderKind::Fabric);
        state.hits = vec![ModHit::default()];
        state.total = 12;
        state.offset = 40;
        state.error = Some("boom".to_string());

        state.switch_kind(ProjectKind::Shader);
        assert_eq!(state.kind, ProjectKind::Shader);
        // 分类下标得回 0：模组表有 15 项，光影表也有自己的第 5 项，
        // 留着旧下标会指到一个完全无关的分类
        assert_eq!(state.category, 0);
        assert!(state.hits.is_empty());
        assert_eq!(state.total, 0);
        assert_eq!(state.offset, 0);
        assert_eq!(state.error, None);
        // 加载器只对模组有意义
        assert_eq!(state.loader, None);
    }

    #[test]
    fn switching_back_to_mods_keeps_the_tab_usable() {
        let mut state = State {
            loader: Some(LoaderKind::Forge),
            ..State::default()
        };
        state.switch_kind(ProjectKind::ResourcePack);
        assert_eq!(state.loader, None, "切走后加载器要清掉");
        state.switch_kind(ProjectKind::Mod);
        assert_eq!(state.kind, ProjectKind::Mod);
        // 换回模组不会自动恢复上次的加载器——那是一次性的筛选，不是记忆
        assert_eq!(state.loader, None);
    }

    #[test]
    fn the_world_falls_back_but_keeps_a_manual_pick() {
        let mut state = State::default();
        assert_eq!(state.world, None, "还没挑过就是空的");

        state.pick_world(&["world".to_string(), "second".to_string()]);
        assert_eq!(state.world.as_deref(), Some("world"), "默认挑第一个");

        state.world = Some("second".to_string());
        state.pick_world(&["world".to_string(), "second".to_string()]);
        assert_eq!(state.world.as_deref(), Some("second"), "手挑的要留着");

        state.pick_world(&["fresh".to_string()]);
        assert_eq!(state.world.as_deref(), Some("fresh"), "存档没了就退回第一个");

        state.pick_world(&[]);
        assert_eq!(state.world, None, "一个存档都没有时清空");
    }

    #[test]
    fn datapacks_need_a_world_before_they_can_be_installed() {
        for kind in ProjectKind::ALL {
            let state = State {
                kind,
                ..State::default()
            };
            // 只有数据包要求先挑存档
            assert_eq!(
                kind.installs_into_world(),
                kind == ProjectKind::Datapack,
                "{:?}",
                kind
            );
            assert_eq!(state.world, None, "{:?} 一开始都没有存档", kind);
        }
    }

    #[test]
    fn each_kind_queries_its_own_project_type() {
        for kind in ProjectKind::ALL {
            let state = State {
                kind,
                ..State::default()
            };
            let spec = state.query_spec();
            assert_eq!(spec.kind, kind);
            assert_eq!(
                spec.category.as_deref(),
                Some(""),
                "{:?} 默认应该是全部分类",
                kind
            );
        }
    }

    #[test]
    fn the_filter_line_only_mentions_the_loader_for_mods() {
        let mut state = State {
            kind: ProjectKind::Mod,
            ..State::default()
        };
        assert!(state.filter_text().contains("加载器"));
        state.kind = ProjectKind::Shader;
        assert!(
            !state.filter_text().contains("加载器"),
            "光影包没有加载器这一维: {}",
            state.filter_text()
        );
        // 但得说清楚现在是哪一类
        assert!(state.filter_text().contains("光影包下载"));
    }

    #[test]
    fn an_empty_result_is_not_refetched_forever() {
        // 搜过了但一条没命中：`hits` 也是空的，不能因此每帧重搜
        let mut state = State {
            open: true,
            ..State::default()
        };
        state.searched = Some(ProjectKind::Mod);
        assert!(!state.needs_fetch(), "搜空之后不该再自动搜");
        // 换一类，就得重新搜
        state.switch_kind(ProjectKind::Shader);
        assert!(state.needs_fetch());
    }

    /// 每页条数只在候选表里取值；表外的（旧状态、手改的配置）退回默认。
    #[test]
    fn the_page_size_stays_on_the_menu() {
        let mut state = State::default();
        assert_eq!(state.page_size, 20);
        for size in [5, 10, 15, 20, 50, 100] {
            state.page_size = size;
            assert_eq!(state.page_limit(), size, "{} 该原样用", size);
            assert_eq!(state.query_spec().limit, size);
        }
        state.page_size = 7;
        assert_eq!(state.page_limit(), 20, "表外的条数退回默认");
        state.page_size = 0;
        assert_eq!(state.page_limit(), 20);
    }

    #[test]
    fn the_modpack_tab_does_not_ask_which_instance_to_install_into() {
        let mut state = State {
            kind: ProjectKind::Modpack,
            ..State::default()
        };
        // 整合包自己长成一个实例，这一维对它没有意义
        assert!(!state.kind.installs_into_instance());
        assert!(!state.filter_text().contains("加载器"));
        assert!(state.filter_text().contains("整合包下载"));
        // 从模组切到整合包时，上一类的加载器条件要清掉，别带过去
        state.kind = ProjectKind::Mod;
        state.loader = Some(LoaderKind::Fabric);
        state.switch_kind(ProjectKind::Modpack);
        assert_eq!(state.loader, None);
    }

    #[test]
    fn a_modpack_version_is_a_download_not_an_install() {
        // 三类东西里只有整合包走「下载 → 导入」这条路
        assert!(ProjectKind::Mod.installs_into_instance());
        assert!(ProjectKind::ResourcePack.installs_into_instance());
        assert!(ProjectKind::Shader.installs_into_instance());
        assert!(!ProjectKind::Modpack.installs_into_instance());
    }

    #[test]
    fn queries_start_on_the_first_page() {
        let state = State::default();
        let spec = state.query_spec();
        assert_eq!(spec.offset, 0);
        assert_eq!(spec.limit, DEFAULT_PAGE, "默认每页 20 条");
        assert_eq!(spec.category.as_deref(), Some(""));
        assert_eq!(spec.loader, None);
        assert_eq!(spec.game_version, None);
    }
}
