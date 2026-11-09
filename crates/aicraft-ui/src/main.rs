#![windows_subsystem = "windows"]
//! aicraft-ui —— 图形界面。
//!
//! 结构上刻意保持「UI 只做呈现，逻辑全在 core」：这里不出现版本解析、
//! classpath 拼装之类的代码，那些都在 `aicraft-core` 里测过了。
//!
//! 界面分几页，左侧窄导航切换：
//! - 主页：选版本、按启动，其余什么都不放
//! - 版本管理（可展开分组）
//!   - 目录列表：官方目录 / 当前目录 / 自己加过的目录，切换与添加
//!   - 版本列表：本地版本列表、详情，校验 / 补全 / 删除
//!   - 下载版本：官方清单，搜索、筛选、安装
//! - 设置：版本下载源、下载限速、内存、JVM、Java、日志
//! - 账户：标题栏右上角那个按钮，离线名字与正版登录都在里面
//!
//! 状态机很浅：`Idle` / `Busy`。启动与校验都放到后台线程，
//! 主线程只轮询消息——因为「等待游戏退出」会阻塞，不能卡住 UI。
//!
//! `#![windows_subsystem = "windows"]` 让程序编译成 GUI 子系统：双击时
//! 不弹黑色控制台窗口。代价是 stdout/stderr 没有控制台可写，所以启动
//! 阶段的提示走下面的 `note()`，稍后并进界面日志。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Mutex;
use std::time::Duration;

use egui::{RichText, Vec2};

mod dirs;
mod flow;
mod icons;
mod manage;
mod packui;
mod resources;
mod serverui;
mod settings;
mod theme;
mod versions;
mod wizard;

use aicraft_core::auth::store::{self, AccountStore, StoredAccount};
use aicraft_core::auth::{self, msa};
use aicraft_core::java;
use aicraft_core::launch::{self, Account, LaunchConfig};
use aicraft_core::logging::{self, Level};
use aicraft_core::modrinth::ProjectKind;
use aicraft_core::resolve;
use aicraft_core::rules::RuleContext;
use aicraft_core::server::{CoreKind, Server};
use aicraft_core::settings::{Mirror, Settings};
use aicraft_core::version::parse_version;
use versions::Action;

/// 启动阶段（字体加载等）攒下的提示，等界面建好后并进日志。
///
/// GUI 子系统下没有控制台，`println!` 写 stdout 会因句柄无效而 panic，
/// 所以这里先存内存，不直接打印。
static STARTUP_NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// 「新建服务端」能选哪些游戏版本（只留正式版）。
///
/// 一次就够，但拉清单要联网，所以放后台线程写到这个槽里，
/// 界面每帧顺手取一次——不阻塞绘制。
static SERVER_VERSIONS: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// 正版（微软账号）登录的界面开关。
///
/// Mojang 还没批准这个 Azure 应用调用 Minecraft 服务的权限
/// （`POST /authentication/login_with_xbox` 返回 403
/// `Invalid app registration, see https://aka.ms/AppRegInfo`），
/// 审批通过前不适合把入口摆给用户看。这里只关界面入口：
/// `auth` 模块、CLI 的 `login` 子命令、已保存账号的自动续期都原样保留，
/// 审批下来改成 `true` 就恢复，不用再改别的代码。
const SHOW_MSA_LOGIN: bool = false;

/// 「选加载器」那张卡片最少要占的高度。
///
/// 它现在整块顶掉版本清单，所以不靠列表让位；这个数只在窗口被拉得很矮、
/// 卡片内容挤不下时用，保证卡片自己至少有这么高（内容超了它仍然会长）。
const WIZARD_CARD_MIN_H: f32 = 320.0;

fn note(msg: String) {
    if let Ok(mut notes) = STARTUP_NOTES.lock() {
        notes.push(msg);
    }
}

fn take_startup_notes() -> Vec<String> {
    STARTUP_NOTES
        .lock()
        .map(|mut notes| std::mem::take(&mut *notes))
        .unwrap_or_default()
}

/// 应用图标（任务栏、Alt-Tab 用的那一枚，64×64）。
///
/// 直接内嵌裸 RGBA 而不是 PNG/ICO：egui 的 `IconData` 本来就是 RGBA 缓冲，
/// 这样不必为解码图片引入 image/png 依赖。文件由 `target/make_logo.py`
/// 生成，和 exe 里那份 `assets/icon.ico` 同源。
static APP_ICON: &[u8] = include_bytes!("../assets/icon-64.rgba");

/// 标题栏里那枚小标记（24×24）。
///
/// 不用 64 那份缩下去：18 逻辑像素上，64 图里的涟漪细环和面与面的接缝
/// 会糊成噪声。这份是生成脚本单独按「简版」画的，去掉细环、放大立方体、
/// 拉开三个面的明度差，缩到 18px 反而更清楚。
static APP_MARK: &[u8] = include_bytes!("../assets/icon-24.rgba");

/// 把一块裸 RGBA 缓冲转成 egui 的窗口图标。
fn icon_data(bytes: &[u8], side: u32) -> Option<egui::IconData> {
    // 尺寸对不上就当没有图标：宁可少一枚图标，也不要画出一片乱码。
    if bytes.len() != (side * side * 4) as usize {
        return None;
    }
    Some(egui::IconData {
        rgba: bytes.to_vec(),
        width: side,
        height: side,
    })
}

fn app_icon() -> Option<egui::IconData> {
    icon_data(APP_ICON, 64)
}

/// 把标题栏那枚标记上传成 egui 纹理。
///
/// 只在启动时上传一次：每帧 `load_texture` 会往显存里堆无数份副本。
fn app_icon_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    const SIDE: usize = 24;
    if APP_MARK.len() != SIDE * SIDE * 4 {
        return None;
    }
    let pixels = egui::ColorImage::from_rgba_unmultiplied([SIDE, SIDE], APP_MARK);
    Some(ctx.load_texture("aicraft-mark", pixels, egui::TextureOptions::LINEAR))
}

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([900.0, 620.0])
        .with_min_inner_size([720.0, 480.0])
        .with_title("AICraft Launcher")
        // 去掉系统标题栏：自己画一条更贴合配色的，拖动/最大化/关闭都自己接
        .with_decorations(false);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        // 创建窗口时就居中（eframe 在窗口创建阶段按主显示器尺寸算），
        // 而不是等第一帧再挪：那样会先在系统默认位置闪一下、再跳到中间。
        centered: true,
        // 不让 eframe 记住上次的窗口位置/大小：窗口尺寸与位置由程序决定，
        // 每次启动都落在屏幕正中，而不是「上次随手拖到哪」。
        persist_window: false,
        ..Default::default()
    };

    install_panic_log();

    eframe::run_native(
        "AICraft Launcher",
        options,
        Box::new(|cc| {
            install_fonts(&cc.egui_ctx);
            theme::install(&cc.egui_ctx);
            Ok(Box::new(App::new()) as Box<dyn eframe::App>)
        }),
    )
}

/// GUI 子系统没有控制台，panic 默认只会让进程悄悄消失。
/// 装个钩子把崩溃信息写到日志文件，出问题时至少有迹可循。
fn install_panic_log() {
    std::panic::set_hook(Box::new(|info| {
        // 崩溃记录跟其它日志放一起，不进账号目录：那是给「换机器要不要
        // 一起带走」做决定的地方，塞个崩溃堆栈进去只会让人以为坏了。
        let path = log_file_path().with_file_name("panic.log");
        let _ = std::fs::write(&path, format!("{}\n", info));
        logging::error(format!("启动器崩溃: {}", info));
    }));
}

/// 候选 CJK 字体，按优先级排列。
///
/// 全部取自 Windows 自带字体，不随程序分发（避免把 18 MB 字体塞进 exe，
/// 也避免字体授权问题）。运行时从磁盘读取；读不到就退回默认字体 ——
/// 此时界面可用但中文会显示为方块。
///
/// 顺序上把「微软雅黑」放最前：它是 Vista 之后 Windows 的默认中文 UI 字体，
/// 字形和 hinting 在屏幕上最好看。黑体（simhei）作为兜底，因为它几乎
/// 所有 Windows 版本都有。
const CJK_FONT_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\msyh.ttf",
    r"C:\Windows\Fonts\simhei.ttf",
    r"C:\Windows\Fonts\Deng.ttf",
    r"C:\Windows\Fonts\simsun.ttc",
];

/// 候选等宽字体（日志、JVM 参数这类要对齐的正文）。
///
/// Consolas 从 Vista 起就在，字形宽度一致、容易分辨 `0`/`O` 和 `1`/`l`。
const MONO_FONT_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\consola.ttf",
    r"C:\Windows\Fonts\lucon.ttf",
];

/// 候选符号字体，排在最后兜底。
///
/// 界面里用到 `→`、`…`、`⟲`（egui 的重置按钮）以及模组名里的 emoji
/// （如 `Jade 🔍`）。实测微软雅黑**没有** U+23F7/U+27F2/U+1F50D 这些码位，
/// 而 `seguisym.ttf` 全都有 —— 它从 Win7 起就是系统组件，可以放心依赖。
const SYMBOL_FONT_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\seguisym.ttf",
    r"C:\Windows\Fonts\seguiemj.ttf",
];

/// 字体在 egui 里的名字（`FontDefinitions::font_data` 的键）。
const FONT_CJK: &str = "cjk";
const FONT_MONO: &str = "mono";
const FONT_SYMBOLS: &str = "symbols";

/// 搭一套自己的字体链。
///
/// **为什么不用 egui 自带的 `default_fonts`**：它把四个跨平台的演示字体
/// （约 1.42 MB）直接编进 exe，而启动器本来就得从系统读中文字体。自己搭
/// 同样的功能只花几百字节的代码，而且挑的都是 Windows 自带字体。
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::empty();

    // 微软雅黑这类 CJK 方块字在 egui 默认的行高里会微微下沉，一行中文
    // 看着比框的中心低一点。给 CJK 字体加一点**向上**的偏移把它提回来：
    // `y_offset_factor` 正值是往下，所以要往上就得用负数。
    // （之前试 0.1 往下压，tooltip 反而更靠下，方向搞反了。）
    let cjk = load_first(CJK_FONT_CANDIDATES).map(|(path, bytes)| {
        fonts.font_data.insert(
            FONT_CJK.to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes).tweak(egui::FontTweak {
                y_offset_factor: -0.06,
                ..Default::default()
            })),
        );
        path
    });

    // 等宽字体不做偏移：它要跟边框、缩进对齐，动一下整块日志就歪了。
    let mono = load_first(MONO_FONT_CANDIDATES).map(|(path, bytes)| {
        fonts.font_data.insert(
            FONT_MONO.to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        path
    });

    let symbols = load_first(SYMBOL_FONT_CANDIDATES).map(|(path, bytes)| {
        fonts.font_data.insert(
            FONT_SYMBOLS.to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        path
    });

    // 回退顺序就是这里的先后：先用写正文的那一族，缺字再往后找。
    // 等宽族把 CJK 排在 mono 之后 —— 日志里中文和英文混着写，
    // 中文只能落到 cjk，英文必须留在 mono 才有对齐感。
    let proportional: [&str; 2] = [FONT_CJK, FONT_SYMBOLS];
    let monospace: [&str; 3] = [FONT_MONO, FONT_CJK, FONT_SYMBOLS];
    let chains: [(egui::FontFamily, &[&str]); 2] = [
        (egui::FontFamily::Proportional, &proportional),
        (egui::FontFamily::Monospace, &monospace),
    ];
    for (family, chain) in chains {
        let list: Vec<String> = chain
            .iter()
            .filter(|name| fonts.font_data.contains_key(**name))
            .map(|name| (*name).to_owned())
            .collect();
        if !list.is_empty() {
            fonts.families.insert(family, list);
        }
    }

    if fonts.families.is_empty() {
        // 一个字体都没读到：装一套空的只会让界面彻底空白，不如留着
        // egui 的默认（没有 default_fonts 时是空的），并明确告诉玩家。
        note("警告：未找到可用的中文字体，界面中的中文可能显示为方块".to_string());
        return;
    }

    ctx.set_fonts(fonts);
    note(format!(
        "字体已就绪 —— 中文: {}，等宽: {}，符号: {}",
        cjk.as_deref().unwrap_or("未找到"),
        mono.as_deref().unwrap_or("未找到"),
        symbols.as_deref().unwrap_or("未找到"),
    ));
}

/// 读取候选列表里第一个能用的字体文件。
fn load_first(candidates: &[&str]) -> Option<(String, Vec<u8>)> {
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            // 简单校验：字体文件不会小于几 KB，空文件说明读取失败
            if bytes.len() > 4096 {
                return Some(((*path).to_string(), bytes));
            }
        }
    }
    None
}

/// 侧栏用的版本摘要。只存展示需要的字段，避免持有完整清单。
#[derive(Clone)]
struct VersionEntry {
    name: String,
    java_major: Option<u32>,
    main_class: String,
    library_count: usize,
    /// 挂的加载器；纯原版为 None。
    ///
    /// 扫描时就算好：界面每帧都要画那枚标签，
    /// 每帧去读一次版本 json 是白花的磁盘寻道。
    loader: Option<aicraft_core::loader::LoaderKind>,
}

/// 后台任务回传的消息。
enum TaskMsg {
    Log(String),
    /// 下载进度：已完成数 / 总数，以及累计字节
    Progress {
        done: usize,
        total: usize,
        bytes: u64,
    },
    /// 加载器阶段能算出比例的进度（预置库、补下缺的库）
    LoaderTick {
        done: usize,
        total: usize,
        bytes: u64,
    },
    /// 加载器阶段说不清进度的一句话（跑 java 安装器）
    LoaderPhase(String),
    /// 启动游戏走到第几步了：(第几步, 共几步, 说明)
    LaunchPhase {
        step: usize,
        total: usize,
        label: String,
    },
    /// 向导里某个加载器的可用版本列表已取到
    LoaderVersions {
        game_version: String,
        kind: aicraft_core::loader::LoaderKind,
        list: Vec<aicraft_core::loader::LoaderVersion>,
    },
    /// 向导里某个加载器版本列表拉取失败
    LoaderFailed(String),
    /// 某个版本安装完成，需要刷新本地列表
    Installed,
    /// 一个模组装完了（Ok 带成功说明，Err 是失败原因）
    ModInstalled(Result<String, String>),
    /// 模组更新检查的结果（已筛成「确实有新版的那些」）
    ModUpdates(Vec<aicraft_core::modupdate::Check>),
    /// 一次（或一批）模组更新的结果：(成功几个, 失败几句)
    ModUpdated {
        done: usize,
        failed: Vec<String>,
    },
    /// 校验跑完了，带上发现的问题数（0 表示全好，不必打扰用户）
    Verified {
        version: String,
        problems: usize,
    },
    /// 启动前自动续期成功，界面要同步成新账号
    Account(Account),
    /// 第三方登录有结果了
    ThirdParty(Result<aicraft_core::auth::yggdrasil::Session, String>),
    /// 游戏进程起来了，带上它的 pid（启动器可能按设置退出去）
    GameRunning(u32),
    /// 等游戏窗口的结果：`shown` 为假表示进程先没了、或者等到超时
    GameWindow {
        pid: u32,
        shown: bool,
    },
    Finished(Result<(), String>),
}

/// 登录后台线程回传的消息。
enum LoginMsg {
    /// 设备码：去哪个网址、抄哪个码、微软附带的整句提示
    DeviceCode {
        uri: String,
        code: String,
        message: String,
    },
    /// 浏览器登录已开始，把授权链接给用户（自动打开失败时能手动点）
    BrowserUrl(String),
    /// 中间阶段提示（比如自动打开浏览器失败）
    Status(String),
    /// 微软登录走到第几跳了（换令牌的那四步）
    Phase(aicraft_core::auth::LoginPhase),
    Done(Result<aicraft_core::MinecraftAccount, String>),
}

/// 登录对话框的界面状态。`App::login` 为 None 表示对话框没开。
struct LoginState {
    busy: bool,
    /// 设备码信息：(verification_uri, user_code)
    device_code: Option<(String, String)>,
    device_message: String,
    browser_url: Option<String>,
    status: String,
    error: Option<String>,
    /// 现在走到哪一跳了（换 Xbox Live → XSTS → Minecraft → 档案）。
    /// 微软登录要跨好几次网络往返，没有它界面就只剩一句不动的话。
    phase: Option<aicraft_core::auth::LoginPhase>,
    rx: Option<Receiver<LoginMsg>>,
}

/// 账户页左侧的两个分区。
#[derive(Clone, Copy, PartialEq, Eq)]
enum AccountSection {
    /// 账号列表：已经加过的那些，点一行就切过去
    List,
    /// 离线账户：起个名字就能玩
    Offline,
    /// 正版账户：微软登录
    Official,
    /// 第三方账户：皮肤站（authlib-injector）
    ThirdParty,
}

impl AccountSection {
    /// 左栏里平级排开的几个页签。`List` 不在里面：它现在挂在
    /// 左栏顶部那块「当前账户」上，点一下就到。
    const ALL: [AccountSection; 3] = [
        AccountSection::Offline,
        AccountSection::Official,
        AccountSection::ThirdParty,
    ];

    fn label(self) -> &'static str {
        match self {
            AccountSection::List => "账号",
            AccountSection::Offline => "离线账户",
            AccountSection::Official => "正版账户",
            AccountSection::ThirdParty => "第三方登录",
        }
    }
}

/// 账户页里的按钮动作。页面闭包里不能直接调 `&mut self` 的方法，
/// 先记下来，出闭包再执行。
enum AccountAction {
    /// 切换当前使用的账号（存的是 UUID）
    Switch(String),
    /// 从列表里删掉一个账号（存的是 UUID）；真正删除前要再问一次
    Remove(String),
    SaveName,
    Logout,
    AddThirdParty,
    Device,
    Browser,
    ReopenBrowser,
    Close,
}

/// 左侧导航选中的页面。
///
/// 「版本管理」不是页面，是左侧导航里的一层目录：进了它，导航栏整体换成
/// 「目录列表」（`Dirs`）、「版本列表」（`Versions`）和「下载版本」（`Download`）
/// 三项，各自画在右边那块正文里。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Dirs,
    Versions,
    Download,
    Resources,
    Settings,
    /// 更多功能 → 服务端管理（「更多功能」本身不是页面）
    Servers,
}

impl Page {
    /// 这一页是否属于「版本管理」那一组（用来给分组标题上色）。
    fn in_versions_group(self) -> bool {
        matches!(self, Page::Dirs | Page::Versions | Page::Download)
    }

    /// 这一页是否属于「资源下载」那一组。
    ///
    /// 资源下载也不是单页，而是模组 / 资源包 / 光影包三页共用一条导航，
    /// 和「版本管理」一样：进去之后整栏换成那三项。
    fn in_resources_group(self) -> bool {
        self == Page::Resources
    }

    /// 这一页是否属于「更多功能」那一组。
    fn in_more_group(self) -> bool {
        self == Page::Servers
    }
}

/// 后台取头像回传的一条结果：账号 UUID 与那张脸（失败时带着原因）。
type SkinResult = (String, Result<Vec<u8>, String>);

/// 后台取回一个模组图标：项目 id 与那张图（解码好的，失败时带着原因）。
/// `bool` 是「这张脸属于管理页那一列吗」：两个页面各有一份缓存，
/// 结果回来得分清往哪边放。
type IconResult = (bool, String, Result<egui::ColorImage, String>);

/// 后台挑 Java 的结果。
///
/// 三层套娃：外层是「对话框本身成不成」（起不来 PowerShell 之类），
/// 中层是「玩家取消了没有」，内层是「这个文件是不是能用的 Java」。
/// 三种失败给玩家的说法完全不同，所以一路分明地传回来。
type JavaPick = Result<Option<Result<aicraft_core::java::JavaRuntime, String>>, String>;

/// 后台搜 Modrinth 回传的一条结果：命中的模组与总数。
type ModSearchRx = Receiver<Result<(Vec<aicraft_core::modrinth::ModHit>, u64), String>>;

/// 详情页后台线程回传的两类结果（详情的、版本列表的）。
enum DetailMsg {
    // 详情结构有十几个字段，塞进枚举里会让每个 `DetailMsg` 都按它的大小走；
    // 装箱之后枚举本身只有一个指针那么大。
    Info(Box<Result<aicraft_core::modrinth::ModDetail, String>>),
    Versions(Result<Vec<aicraft_core::modrinth::ModVersion>, String>),
}

/// 服务端那边一条后台结果。
enum ServerMsg {
    /// 某个 (核心, 游戏版本) 的加载器版本列表
    Loaders {
        core: CoreKind,
        mc_version: String,
        list: Vec<String>,
    },
    LoaderFailed(String),
    /// 装的过程中换一句说明
    Stage(String),
    /// 装完了（失败时带原因）
    Created(Box<Result<Server, String>>),
}

struct App {
    /// 当前页面：主页只放启动，其余功能都在另外两页
    page: Page,
    game_dir: String,
    versions: Vec<VersionEntry>,
    selected: Option<usize>,
    busy: Option<String>,
    /// 主界面日志的显示阈值（比它更啰嗦的等级不画出来）
    log_view: Level,
    /// 离线账户的名字。空串 = 还没配置过（旧版本会给个 "Steve"，那是
    /// 替玩家做了决定，现在留白，让标题栏那个按钮自己说「未配置」）。
    username: String,
    /// 「账户」页开着没有（开着时整页盖住主界面）
    /// 游戏已经起来了、启动器该按设置退出去了。真正关窗口要在 ui() 里发命令。
    close_after_launch: bool,
    account_open: bool,
    /// 待确认删除的账号：UUID 与名字。Some 时弹出确认框。
    pending_account_remove: Option<(String, String)>,
    /// 账户页里当前看的分区
    account_section: AccountSection,
    /// 离线名字的草稿：点「保存」才写回 `username`
    account_draft: String,
    /// 第三方登录表单：认证服务器地址、账号、密码
    third_root: String,
    third_user: String,
    third_pass: String,
    /// 第三方登录失败的那句话
    third_error: Option<String>,
    /// 磁盘上的账号列表（多份离线名字、多份正版、多份第三方都在这儿）。
    ///
    /// 缓存一份而不是每帧读文件：左栏要按它画列表，而 egui 每帧都会重绘。
    accounts: AccountStore,
    /// 皮肤头像纹理：账号 UUID → 32×32 的那张脸。
    ///
    /// 离线名字没有皮肤，不会进这张表；界面那边查不到就画首字母。
    skin_tex: std::collections::HashMap<String, egui::TextureHandle>,
    /// 后台取头像的进度。一次只跑一个线程，按顺序把结果送回来。
    skin_rx: Option<std::sync::mpsc::Receiver<SkinResult>>,
    /// 后台下模组图标的进度。一次只开一个线程，按顺序往回送。
    icon_rx: Option<std::sync::mpsc::Receiver<IconResult>>,
    /// 已经发过请求的 UUID（不管成没成）。
    ///
    /// 没有这个集合的话，取失败的头像会在每一帧被重新排进队列，
    /// 玩家看到的就是一直在敲网络、日志一直刷。
    skin_tried: std::collections::HashSet<String>,
    /// 通用设置（版本下载源、并发、下载限速、内存、JVM 参数、Java 路径）
    settings: Settings,
    /// 版本页里等待二次确认的删除目标
    /// 正在等用户确认删除的版本名（弹窗的开关，不是「已经删了」）
    pending_remove: Option<String>,
    /// 校验完发现缺失，等用户确认要不要补全：(版本名, 问题数)
    verify_prompt: Option<(String, usize)>,
    /// 「恢复默认设置」的二次确认开关
    reset_confirm: bool,
    /// 「管理」页：Some 就说明现在整页盖住了主界面
    manage: Option<manage::State>,
    /// 管理页里模组那一列的图标（从 jar 里挖出来的那张）。
    ///
    /// 和资源下载页的 `resources.icons` 分开存：两边的键域不同（那边是
    /// Modrinth 项目 id，这边是本地路径），混在一起只会互相覆盖。
    manage_icons: icons::Cache,
    /// 启动/校验时发现父版本不在盘上：(实例名, 缺的那个版本号)。
    ///
    /// 只在界面里问一句「要不要现在装上」，不自动替人下几百 MB。
    parent_prompt: Option<(String, String)>,
    /// 正在换加载器时记下（实例名, 目标版本），换完要刷新管理页
    manage_switch: Option<(String, String)>,
    rx: Option<Receiver<TaskMsg>>,
    /// 拉官方清单的后台结果：Ok 是版本列表，Err 是失败原因。
    ///
    /// 单独一条通道，不走 `rx`：见 `start_fetch_manifest` 上的说明。
    manifest_rx: Option<Receiver<Result<Vec<versions::Entry>, String>>>,
    /// 「资源下载」页的搜索请求：单独一条通道，和拉官方清单互不干扰。
    ///
    /// 搜索是玩家会连点的事（换筛选、翻页），塞进全局 `busy` 会让
    /// 「启动游戏」按钮跟着变灰——那是两件事。
    modrinth_rx: Option<ModSearchRx>,
    /// 详情页的两路请求（详情 + 版本列表）
    detail_rx: Option<Receiver<DetailMsg>>,
    /// 「资源下载」页的界面状态
    resources: resources::State,
    /// 整合包拖拽 / 导入确认的界面状态
    pack: packui::State,
    /// 后台读整合包清单的那条通道
    pack_rx: Option<Receiver<(PathBuf, Result<aicraft_core::modpack::PackManifest, String>)>>,
    /// 本机 Java 探测的后台状态（首次进设置页要几秒，不能在绘制里做）
    java_scan: settings::JavaScan,
    /// 「更多功能 → 服务端管理」的界面状态
    servers: serverui::State,
    /// 正在装服务端时那条通道（和 `rx` 分开：装服务端不该锁住整个启动器）
    server_rx: Option<Receiver<ServerMsg>>,
    /// 拉官方正式版清单给「新建服务端」的下拉用（一次就够）
    server_versions: Vec<String>,
    server_versions_loading: bool,
    last_scan_ms: u128,
    /// 当前下载进度，None 表示不在下载
    download_progress: Option<(usize, usize, u64)>,
    /// 本次安装的取消令牌：点「取消下载」时点掉它，工作线程会自己收手。
    cancel_token: Option<aicraft_core::cancel::CancelToken>,
    /// 「确定要取消吗」那一层确认。
    cancel_confirm: bool,
    /// 已经点了确定取消（界面据此显示「正在停止…」）。
    cancel_requested: bool,
    /// 本次安装新建的实例目录名，取消之后要照着删。
    install_created: Vec<String>,
    /// 下载速度（字节/秒），由两次进度事件之间的差算出来。
    /// 加载器阶段那句「说不清进度」的说明；为 Some 时进度条改成来回滚动
    loader_phase: Option<String>,
    /// 启动游戏走到第几步了：(第几步, 共几步, 说明)。
    ///
    /// 读清单、挑 Java、拼参数、解压 natives、起进程、等窗口——这几步都有确定的
    /// 先后，能算出比例，所以给一条真进度条，而不是又一个转圈。
    launch_phase: Option<(usize, usize, String)>,
    /// 这次启动的游戏进程 pid。等窗口、安排重开都靠它。
    launching_pid: Option<u32>,
    speed_bps: f64,
    last_tick_at: Option<std::time::Instant>,
    last_tick_bytes: u64,
    /// 官方清单里可安装的版本 + 版本管理窗口的状态
    version_mgr: versions::State,
    /// 装版本的向导（选原版 → 起名 → 选加载器）；None 表示没在向导里
    wizard: Option<wizard::Wizard>,
    /// 已登录的正版账号；None 表示离线模式
    account: Option<Account>,
    /// 登录对话框
    login: Option<LoginState>,
    /// 文件夹选择框的后台结果通道：Ok(Some(路径)) 选中、Ok(None) 取消
    folder_rx: Option<Receiver<Result<Option<PathBuf>, String>>>,
    /// 同一个选择框，但这次是给服务端挑目录。
    ///
    /// 和 `folder_rx` 分开而不是加个字段区分：两者都是「开一个对话框、
    /// 回一个路径」，共用一个通道的话，回调里还得再判断这次点的是谁——
    /// 而对话框可能同时只开一个，多一个字段就少一处歧义。
    server_folder_rx: Option<Receiver<Result<Option<PathBuf>, String>>>,
    /// 后台挑 Java 的结果。
    java_rx: Option<Receiver<JavaPick>>,
    /// 「另存为」对话框的后台结果通道
    export_rx: Option<Receiver<Result<Option<PathBuf>, String>>>,
    /// 标题栏左侧那枚小标记的纹理，启动时上传一次
    icon_tex: Option<egui::TextureHandle>,
    /// 首页版本菜单是否被点开了（悬停是临时的，点一下才钉住）
    version_menu_open: bool,
    /// 上一帧版本菜单占的矩形：指针移进去时要继续算「开着」
    version_menu_rect: Option<egui::Rect>,
    mirror_probe_rx: Option<std::sync::mpsc::Receiver<()>>,
}

impl App {
    fn new() -> Self {
        logging::init(Some(log_file_path()));
        let settings = Settings::load_or_default(&settings_path());
        logging::debug(format!(
            "数据目录：{}",
            aicraft_core::settings::data_dir().display()
        ));
        logging::info(format!(
            "账号目录：{}",
            aicraft_core::settings::accounts_dir().display()
        ));
        let mut app = App {
            page: Page::Home,
            game_dir: initial_game_dir(&settings),
            versions: Vec::new(),
            selected: None,
            busy: None,
            log_view: Level::Info,
            username: initial_username(&settings),
            close_after_launch: false,
            account_open: false,
            pending_account_remove: None,
            account_section: AccountSection::List,
            account_draft: String::new(),
            third_root: String::new(),
            third_user: String::new(),
            third_pass: String::new(),
            third_error: None,
            accounts: AccountStore::default(),
            skin_tex: std::collections::HashMap::new(),
            skin_rx: None,
            skin_tried: std::collections::HashSet::new(),
            icon_rx: None,
            settings,
            pending_remove: None,
            verify_prompt: None,
            reset_confirm: false,
            parent_prompt: None,
            manage: None,
            manage_icons: icons::Cache::default(),
            manage_switch: None,
            rx: None,
            manifest_rx: None,
            modrinth_rx: None,
            detail_rx: None,
            resources: resources::State::default(),
            pack: packui::State::default(),
            pack_rx: None,
            java_scan: settings::JavaScan::default(),
            servers: serverui::State::default(),
            server_rx: None,
            server_versions: Vec::new(),
            server_versions_loading: false,
            last_scan_ms: 0,
            download_progress: None,
            cancel_token: None,
            cancel_confirm: false,
            cancel_requested: false,
            install_created: Vec::new(),
            loader_phase: None,
            launch_phase: None,
            launching_pid: None,
            speed_bps: 0.0,
            last_tick_at: None,
            last_tick_bytes: 0,
            version_mgr: versions::State::default(),
            wizard: None,
            account: None,
            login: None,
            folder_rx: None,
            server_folder_rx: None,
            java_rx: None,
            export_rx: None,
            icon_tex: None,
            version_menu_open: false,
            version_menu_rect: None,
            mirror_probe_rx: None,
        };
        // 调试用：AICRAFT_PAGE=dirs|versions|download|resources|servers|settings 时启动就停在那页（截图/排查方便）
        if let Ok(page) = std::env::var("AICRAFT_PAGE") {
            app.page = match page.as_str() {
                "dirs" => Page::Dirs,
                "versions" => Page::Versions,
                "download" => Page::Download,
                "resources" => Page::Resources,
                "settings" => Page::Settings,
                "servers" => Page::Servers,
                _ => Page::Home,
            };
            if app.page == Page::Servers {
                // 直接落在这一页时也要走一遍真正的入口，否则列表与版本清单都是空的。
                app.open_servers();
            }
        }
        app.rescan();
        for msg in take_startup_notes() {
            logging::info(msg);
        }
        app.restore_saved_account();
        app.start_mirror_probe();
        // 内存快照要起一个 PowerShell（约 0.4 秒）。趁玩家还在看主页的时候
        // 就把它焐热，进设置页时 `cached()` 直接有数，不会卡那一帧。
        aicraft_core::meminfo::prefetch();

        app
    }

    /// 启动时把上次登录的正版账号找回。
    ///
    /// 令牌过期也不要紧：真正过期是在点「启动」时用 refresh token 续，
    /// 这里只是让界面一打开就显示「正版：名字」而不是假装离线。
    /// 把还没有头像的账号排进后台队列。
    ///
    /// 每帧都会被调用，所以第一件事就是把处理过的挑出去 —— 只有真的缺
    /// 东西时才会开线程；`skin_tried` 让取失败的那些不会每帧重排一遍。
    fn kick_off_skins(&mut self, ctx: &egui::Context) {
        // 头像缓存跟着账号走：换台电脑（或者就在这台机器上删掉启动器目录）
        // 时，不该因为缓存位置变了而重新敲一遍皮肤服务器。
        let data_dir = aicraft_core::settings::accounts_dir();
        let mut wanted: Vec<(String, Option<String>)> = Vec::new();
        for account in &self.accounts.accounts {
            if self.skin_tex.contains_key(&account.uuid) {
                continue;
            }
            if !self.skin_tried.insert(account.uuid.clone()) {
                continue;
            }
            // 离线名字背后没有皮肤服务器，但原版照样有张脸：按 UUID 在
            // Steve 与 Alex 之间挑一张。这一步不联网，直接就能画出来。
            if account.offline {
                let head = aicraft_core::skin::default_head(&account.uuid);
                if let Some(texture) = skin_texture(ctx, &account.uuid, &head) {
                    self.skin_tex.insert(account.uuid.clone(), texture);
                }
                continue;
            }
            // 先看磁盘上的缓存：上次取到过就不用再敲一次网络
            match aicraft_core::skin::head_from_cache(&data_dir, &account.uuid) {
                Some(head) => {
                    if let Some(texture) = skin_texture(ctx, &account.uuid, &head) {
                        self.skin_tex.insert(account.uuid.clone(), texture);
                    }
                }
                None => wanted.push((account.uuid.clone(), account.api_root.clone())),
            }
        }
        if wanted.is_empty() || self.skin_rx.is_some() {
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel();
        self.skin_rx = Some(rx);
        std::thread::spawn(move || {
            let http = aicraft_core::auth::CurlHttp;
            for (uuid, api_root) in wanted {
                let head =
                    aicraft_core::skin::head_for(&http, api_root.as_deref(), &uuid, &data_dir);
                if tx.send((uuid, head)).is_err() {
                    break;
                }
            }
        });
    }

    /// 收后台取回来的头像，变成纹理。
    fn poll_skins(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.skin_rx.take() else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok((uuid, head)) => match head {
                    Ok(head) => {
                        if let Some(texture) = skin_texture(ctx, &uuid, &head) {
                            self.skin_tex.insert(uuid, texture);
                        }
                    }
                    Err(message) => {
                        // 取不到就退回原版默认皮肤，而不是留一块空白：
                        // 游戏里这会儿显示的也正是 Steve 或 Alex。
                        logging::debug(format!("{} 的皮肤没取到：{}", uuid, message));
                        let head = aicraft_core::skin::default_head(&uuid);
                        if let Some(texture) = skin_texture(ctx, &uuid, &head) {
                            self.skin_tex.insert(uuid, texture);
                        }
                    }
                },
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.skin_rx = Some(rx);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    /// 开一个线程，把「资源下载」页缺的图标挨个取回来。
    ///
    /// 一次只跑一个线程：图标是小图，但列表一屏就有 20 个，并发二十条
    /// 连接对 CDN 不客气，对我们也没好处——反正先到的先画。
    fn kick_off_mod_icons(&mut self, ctx: &egui::Context) {
        if self.icon_rx.is_some() {
            // 线程还在跑，等它结束再说
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
            return;
        }
        let mut requests: Vec<(bool, icons::Request)> = self
            .resources
            .icons
            .take_pending()
            .into_iter()
            .map(|r| (false, r))
            .collect();
        requests.extend(
            self.manage_icons
                .take_pending()
                .into_iter()
                .map(|r| (true, r)),
        );
        if requests.is_empty() {
            return;
        }
        logging::debug(format!("去取 {} 个模组图标", requests.len()));
        let (tx, rx) = std::sync::mpsc::channel();
        self.icon_rx = Some(rx);
        std::thread::spawn(move || {
            for (manage, (id, source)) in requests {
                // `jar+local://` 的图标压在模组 jar 里，不走网络。
                let bytes = match source.strip_prefix(icons::JAR_SCHEME) {
                    Some(path) => aicraft_core::modicon::from_jar(std::path::Path::new(path))
                        .map_err(|e| format!("读模组图标失败: {}", e))
                        .and_then(|found| found.ok_or_else(|| "这个模组没带图标".to_string())),
                    None => aicraft_core::transport::fetch_bytes(&source)
                        .map_err(|e| format!("取图标失败: {}", e)),
                };
                let result = bytes.and_then(|bytes| {
                    icons::decode(&bytes).ok_or_else(|| "图标格式不认识".to_string())
                });
                if tx.send((manage, id, result)).is_err() {
                    break;
                }
            }
        });
        // 线程不会自己叫醒界面，得让它隔一会儿回来收一次
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
    }

    /// 收图标：成功就上传成纹理，失败就记下来别再试。
    fn poll_mod_icons(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.icon_rx.take() else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok((manage, id, Ok(image))) => {
                    let cache = if manage {
                        &mut self.manage_icons
                    } else {
                        &mut self.resources.icons
                    };
                    cache.put(ctx, &id, &image);
                }
                Ok((manage, id, Err(message))) => {
                    logging::debug(format!("{} 的图标没取到：{}", id, message));
                    let cache = if manage {
                        &mut self.manage_icons
                    } else {
                        &mut self.resources.icons
                    };
                    cache.mark_failed(&id);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.icon_rx = Some(rx);
                    ctx.request_repaint_after(std::time::Duration::from_millis(150));
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.resources.icons.abandon_requests();
                    self.manage_icons.abandon_requests();
                    break;
                }
            }
        }
    }

    fn restore_saved_account(&mut self) {
        let path = store::default_path();
        self.accounts = match AccountStore::load(&path) {
            Ok(store) => store,
            Err(message) => {
                // 文件在、但读不开：多半是凭证跟的是另一台电脑（或文件被改过）。
                // 这种情况玩家该看到一句原因，而不是账号悄悄消失。
                if path.is_file() {
                    logging::warn(format!("账号文件读不开：{}", message));
                }
                AccountStore::default()
            }
        };
        let Some(stored) = self.accounts.current().cloned() else {
            return;
        };
        self.username = stored.name.clone();
        self.account = Some(stored.to_account());
        if stored.offline {
            logging::info(format!("当前账号：离线 {}", stored.name));
        } else if stored.is_third_party() {
            logging::info(format!("已找回第三方账号 {}", stored.name));
        } else {
            logging::info(format!("已找回正版账号 {}", stored.name));
        }
    }

    /// 写回账号文件（列表增删改之后都要调一次）。
    fn save_accounts(&mut self) {
        let path = store::default_path();
        if let Err(e) = self.accounts.save(&path) {
            logging::warn(format!("保存账号列表失败: {}", e));
        }
    }

    /// 切到另一个账号：改当前账号、改界面上的名字与登录态。
    fn switch_account(&mut self, uuid: &str) {
        let to_offline = self
            .accounts
            .accounts
            .iter()
            .find(|a| a.uuid == uuid)
            .map(|a| a.offline)
            .unwrap_or(false);
        if to_offline && !self.offline_allowed() {
            logging::warn("离线账户现在用不了：先登录一次正版账户。");
            return;
        }
        if !self.accounts.select(uuid) {
            return;
        }
        self.save_accounts();
        if let Some(stored) = self.accounts.current().cloned() {
            logging::info(format!(
                "切到{}账号 {}",
                if stored.offline {
                    "离线"
                } else if stored.is_third_party() {
                    "第三方"
                } else {
                    "正版"
                },
                stored.name
            ));
            self.username = stored.name.clone();
            self.settings.offline_name = stored.name.clone();
            self.account_draft = stored.name.clone();
            self.account = Some(stored.to_account());
            self.persist_settings_quietly();
        }
    }

    /// 从账号列表里删掉一个账号。
    ///
    /// 删的可能正是当前账号：那就退回到列表里剩下那一个；一个都不剩就把
    /// 界面上的名字一起清掉（右上角会回到「未配置」，点启动会先让你配账户）。
    fn remove_account(&mut self, uuid: &str) {
        let Some(found) = self.accounts.accounts.iter().find(|a| a.uuid == uuid) else {
            return;
        };
        let name = found.name.clone();
        let kind = account_kind(found);
        let was_current = self.account.as_ref().map(|a| a.uuid.as_str()) == Some(uuid);

        if !self.accounts.remove(uuid) {
            return;
        }
        self.save_accounts();
        logging::info(format!("已删掉{}账号 {}", kind, name));

        if was_current {
            self.account = None;
            self.apply_current_account();
        }
    }

    /// 按 `AccountStore` 里记着的当前账号，刷新界面上的名字与登录态。
    fn apply_current_account(&mut self) {
        match self.accounts.current().cloned() {
            Some(next) => {
                self.username = next.name.clone();
                self.settings.offline_name = next.name.clone();
                self.account_draft = next.name.clone();
                self.account = Some(next.to_account());
            }
            None => {
                self.username.clear();
                self.settings.offline_name.clear();
                self.account_draft.clear();
                self.account = None;
            }
        }
        self.persist_settings_quietly();
    }

    /// 左栏「当前账户」那块显示谁：永远是**启动游戏真正会用的**那个号。
    ///
    /// 它不跟着右边页签走。理由很实在：这块的头衔是「当前账户」，
    /// 玩家看它是为了确认「点了启动会以谁的身份进去」。以前它跟着页签
    /// 换成「离线 / 正版 / 第三方」里各自那一条，切到「正版账户」页
    /// 就会显示一个其实没在用的号 —— 那是会误导人的。
    ///
    /// 优先读账号列表里记着的当前账号；一个都没存时退回界面上那个
    /// 手输的离线名字（`username`），这时候还没进列表，但它就是会用的号。
    fn current_account_label(&self) -> Option<(String, &'static str)> {
        if let Some(stored) = self.accounts.current() {
            return Some((stored.name.clone(), account_kind(stored)));
        }
        if !self.username.trim().is_empty() {
            // 还没入库的离线名字：它照样会被填进 `--username`。
            return Some((self.username.clone(), "离线"));
        }
        None
    }

    /// 账号列表里有没有一个真正的正版账户（第三方与离线都不算）。
    fn has_premium_account(&self) -> bool {
        self.accounts
            .accounts
            .iter()
            .any(|a| !a.offline && a.api_root.is_none())
    }

    /// 现在能不能用离线身份进游戏。
    fn offline_allowed(&self) -> bool {
        self.settings.allow_offline || self.has_premium_account()
    }

    /// 这次启动会不会以离线身份进场（没有账号，或者当前账号就是离线的）。
    fn launches_offline(&self) -> bool {
        match &self.account {
            Some(account) => account.user_type == "legacy" && account.api_root.is_none(),
            None => true,
        }
    }

    /// 弹出系统文件夹选择框挑游戏目录。
    ///
    /// 对话框会阻塞它所在的线程，所以扔到后台线程里开；
    /// 选中的路径经通道回到主线程，再改 `game_dir` 并重新扫描。
    fn pick_game_dir(&mut self) {
        if self.folder_rx.is_some() {
            return;
        }
        let initial = self.game_dir.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = pick_folder_blocking(&initial);
            let _ = tx.send(result);
        });
        self.folder_rx = Some(rx);
    }

    /// 挑一个已有的服务端文件夹，认出来是什么就加进列表。
    ///
    /// 玩家手上多半已经开着一个服了——可能是半年前从别处下的、也可能
    /// 是 HMCL 之类别的启动器建的。让他重装一遍不现实，所以这里只读：
    /// 认核心、认版本，认不出来就当没这个目录。
    fn pick_server_folder(&mut self) {
        if self.server_folder_rx.is_some() {
            return;
        }
        let initial = self
            .settings
            .server_dirs
            .first()
            .cloned()
            .unwrap_or_else(|| self.game_dir.clone());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(pick_folder_blocking(&initial));
        });
        self.server_folder_rx = Some(rx);
    }

    /// 收「挑服务端文件夹」的结果。
    fn poll_server_folder(&mut self) {
        let Some(rx) = self.server_folder_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(Some(path))) => self.adopt_server_folder(path),
            Ok(Ok(None)) => {}
            Ok(Err(message)) => self.servers.notice = Some((message, true)),
            Err(TryRecvError::Empty) => self.server_folder_rx = Some(rx),
            Err(TryRecvError::Disconnected) => {}
        }
    }

    /// 玩家挑了一个文件夹之后：能认出核心就加进列表，认不出就开新装。
    fn adopt_server_folder(&mut self, path: PathBuf) {
        let picked = path.display().to_string();
        let key = picked.to_lowercase();
        if self
            .settings
            .server_dirs
            .iter()
            .any(|dir| dir.to_lowercase() == key)
        {
            self.servers.notice = Some(("这个文件夹已经在列表里了。".to_string(), false));
            return;
        }
        match aicraft_core::server::probe(&path) {
            Some(found) => {
                aicraft_core::audit::action(format!(
                    "服务端：把 {} 接进列表（认出 {}）",
                    picked,
                    found.summary()
                ));
                self.settings.server_dirs.push(picked);
                self.persist_settings_quietly();
                self.servers.set_extra(&self.settings.server_dirs);
                self.servers.notice = Some((format!("认出：{}", found.summary()), false));
            }
            None => {
                // 没认出核心，但玩家是有意选这个文件夹的——那就进全新安装
                // 流程，装完正好落在这个目录里。
                aicraft_core::audit::action(format!(
                    "服务端：{} 里没有服务端核心，开始新建",
                    picked
                ));
                self.servers.form = serverui::Form {
                    open: true,
                    target: Some(path),
                    memory_mb: self.settings.memory_mb(),
                    ..serverui::Form::default()
                };
                self.servers.notice = Some((
                    "这个文件夹里没有服务端核心，那就装一个新的进去。".to_string(),
                    false,
                ));
            }
        }
    }

    /// 弹文件选择框挑一个 java 可执行文件。
    ///
    /// 对话框会阻塞它所在的线程，所以扔到后台；挑完之后在那里就地跑一次
    /// `java -version` 确认它真是个能用的 Java，再回传解析好的运行时 ——
    /// 主线程不做任何磁盘/进程操作。
    fn pick_java(&mut self) {
        if self.java_rx.is_some() {
            return;
        }
        let initial = self
            .settings
            .java_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| {
                aicraft_core::settings::official_game_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            });
        let (tx, rx) = mpsc::channel::<JavaPick>();
        std::thread::spawn(move || {
            let result = pick_java_blocking(&initial).map(|picked| {
                picked.map(|path| {
                    // 探一次：不是 java.exe、或者跑不起来，在这里就发现，
                    // 而不是等到点「启动游戏」才报「找不到运行时」。
                    match aicraft_core::java::probe_java(&path) {
                        Some(rt) => Ok(rt),
                        None => Err(format!("这个文件不像是能用的 Java：{}", path.display())),
                    }
                })
            });
            let _ = tx.send(result);
        });
        self.java_rx = Some(rx);
    }

    /// 收 Java 选择框的结果。
    fn poll_java_pick(&mut self) {
        let Some(rx) = self.java_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(Some(Ok(rt)))) => {
                let major = rt.major_version;
                let version = rt.version_string.clone();
                if self.java_scan.add(rt) {
                    logging::info(format!("已添加 Java {}（{}）", major, version));
                } else {
                    logging::info(format!("Java {} 已经在列表里了", major));
                }
            }
            Ok(Ok(Some(Err(message)))) => {
                logging::warn(message.clone());
                self.java_scan.error = Some(message);
            }
            Ok(Ok(None)) => {}
            Ok(Err(message)) => {
                logging::warn(message.clone());
                self.java_scan.error = Some(message);
            }
            Err(TryRecvError::Empty) => self.java_rx = Some(rx),
            Err(TryRecvError::Disconnected) => {
                self.java_scan.error = Some("选择 Java 时线程意外结束".to_string());
            }
        }
    }

    /// 收文件夹选择框的结果。取消（Ok(None)）时什么都不改。
    ///
    /// 选中的目录同时进「目录列表」，这样用户从列表里还能切回去；官方目录
    /// 与当前目录不进（前者现算、后者本来就有一行）。
    fn poll_folder_pick(&mut self) {
        let Some(rx) = self.folder_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(Some(path))) => {
                let picked = path.display().to_string();
                let official = aicraft_core::settings::official_game_dir()
                    .map(|p| p.display().to_string().to_lowercase())
                    .unwrap_or_default();
                let key = picked.to_lowercase();
                // 官方目录那一行被用户移掉时，官方路径就不再由那行代表——
                // 这时把它当普通目录记进列表，否则它既没有行、也点不到。
                let official_is_hidden_row = self.settings.hide_official_dir && key == official;
                if !key.is_empty()
                    && (key != official || official_is_hidden_row)
                    && !self
                        .settings
                        .game_dirs
                        .iter()
                        .any(|d| d.to_lowercase() == key)
                {
                    self.settings.game_dirs.push(picked.clone());
                }
                self.set_game_dir(picked);
            }
            Ok(Ok(None)) => {}
            Ok(Err(message)) => logging::info(message),
            // 还没选完，把接收端放回去等下一帧
            Err(TryRecvError::Empty) => self.folder_rx = Some(rx),
            // 线程已经结束却没发结果，按取消处理
            Err(TryRecvError::Disconnected) => {}
        }
    }

    /// 扫描版本目录。界面上唯一碰磁盘的地方，必须快。
    fn rescan(&mut self) {
        let started = std::time::Instant::now();
        self.versions.clear();
        self.selected = None;

        let dir = PathBuf::from(&self.game_dir).join("versions");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            logging::warn(format!("无法读取版本目录: {}", dir.display()));
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let json_path = path.join(format!("{}.json", name));
            if !json_path.is_file() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&json_path) else {
                continue;
            };
            let Ok(raw) = parse_version(&text) else {
                continue;
            };
            self.versions.push(VersionEntry {
                name: name.to_string(),
                java_major: raw.java_version.major_version,
                main_class: raw.main_class.unwrap_or_default(),
                library_count: raw.libraries.len(),
                loader: aicraft_core::loader::detect_in_text(&text, name).map(|(kind, _)| kind),
            });
        }

        self.versions.sort_by_key(|entry| entry.name.to_lowercase());
        self.last_scan_ms = started.elapsed().as_millis();
        if !self.versions.is_empty() {
            // 优先回到上次选中的那个版本；它被删掉了才退回第一个。
            // 每次开启动器都重置成第一个，等于让玩家每天重新点一遍。
            let remembered = &self.settings.selected_version;
            self.selected = self
                .versions
                .iter()
                .position(|entry| &entry.name == remembered)
                .or(Some(0));
        }
        let count = self.versions.len();
        let ms = self.last_scan_ms;
        logging::info(format!("发现 {} 个版本（{} ms）", count, ms));
    }

    /// 换一个选中的版本，并且把它记进设置。
    ///
    /// 选版本是玩家的一个决定，不是临时高亮 —— 下次打开启动器还该是它。
    fn select_version(&mut self, index: usize) {
        self.selected = Some(index);
        let name = self.versions.get(index).map(|entry| entry.name.clone());
        if let Some(name) = name {
            if self.settings.selected_version != name {
                self.settings.selected_version = name;
                self.persist_settings_quietly();
            }
        }
    }

    fn selected_version(&self) -> Option<&VersionEntry> {
        self.selected.and_then(|i| self.versions.get(i))
    }

    /// 起一个后台任务，统一处理状态切换。
    fn spawn_task<F>(&mut self, label: String, job: F)
    where
        F: FnOnce(Sender<TaskMsg>) -> Result<(), String> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.busy = Some(label);

        let tx2 = tx.clone();
        std::thread::spawn(move || {
            let result = job(tx2.clone());
            let _ = tx2.send(TaskMsg::Finished(result));
        });
    }

    /// 打开「账户」页：默认停在离线这一分区 —— 大多数人第一件事是想个名字
    /// 进游戏，而不是去登微软账号。
    fn open_account(&mut self) {
        self.account_draft = self.username.clone();
        self.account_section = AccountSection::List;
        self.account_open = true;
    }

    /// 确保登录状态对象在（账号窗口里点「登录」时用）。客户端 ID 用内置常量。
    fn open_login(&mut self) {
        if self.login.is_some() {
            return;
        }
        self.login = Some(LoginState {
            busy: false,
            device_code: None,
            device_message: String::new(),
            browser_url: None,
            status: String::new(),
            error: None,
            phase: None,
            rx: None,
        });
    }

    /// 起一个登录后台线程。两种流程（浏览器/设备码）的最后几步相同，
    /// 所以闭包的返回值统一是 `MinecraftAccount`。
    fn spawn_login<F>(&mut self, job: F)
    where
        F: FnOnce(Sender<LoginMsg>) -> Result<aicraft_core::MinecraftAccount, String>
            + Send
            + 'static,
    {
        let Some(login) = self.login.as_mut() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        login.rx = Some(rx);
        login.busy = true;
        login.error = None;
        login.device_code = None;
        login.device_message.clear();
        login.browser_url = None;
        login.status = "正在连接微软…".to_string();
        login.phase = None;

        std::thread::spawn(move || {
            let result = job(tx.clone());
            let _ = tx.send(LoginMsg::Done(result));
        });
    }

    /// 浏览器授权码 + PKCE：最接近玩家平时的「点登录跳浏览器」体验。
    fn start_login_browser(&mut self) {
        if self.login.is_none() {
            return;
        }
        let client_id = auth::DEFAULT_CLIENT_ID;

        self.spawn_login(move |tx| {
            let http = auth::CurlHttp;
            let login = msa::BrowserLogin::start(client_id).map_err(describe_auth_error)?;
            let url = login.url().to_string();
            let _ = tx.send(LoginMsg::BrowserUrl(url.clone()));
            if let Err(e) = msa::open_browser(&url) {
                let _ = tx.send(LoginMsg::Status(format!(
                    "自动打开浏览器失败：{}。请手动复制上面的链接。",
                    e
                )));
            }
            let tokens = login
                .wait(&http, client_id, Duration::from_secs(300))
                .map_err(describe_auth_error)?;
            finish_login(&http, &tokens, &tx)
        });
    }

    /// 设备码：让玩家在任意设备上输入 8 位码，适合浏览器打不开回环地址的机器。
    fn start_login_device(&mut self) {
        if self.login.is_none() {
            return;
        }
        let client_id = auth::DEFAULT_CLIENT_ID;

        self.spawn_login(move |tx| {
            let http = auth::CurlHttp;
            let code = msa::request_device_code(&http, client_id).map_err(describe_auth_error)?;
            let _ = tx.send(LoginMsg::DeviceCode {
                uri: code.verification_uri.clone(),
                code: code.user_code.clone(),
                message: code.message.clone(),
            });
            let tokens =
                msa::wait_for_device_code(&http, client_id, &code).map_err(describe_auth_error)?;
            finish_login(&http, &tokens, &tx)
        });
    }

    /// 轮询登录线程。
    fn poll_login(&mut self) {
        let mut done = None;
        {
            let Some(login) = self.login.as_mut() else {
                return;
            };
            let Some(rx) = &login.rx else { return };

            while let Ok(msg) = rx.try_recv() {
                match msg {
                    LoginMsg::DeviceCode { uri, code, message } => {
                        login.device_code = Some((uri, code));
                        login.device_message = message;
                        login.status = "等你在浏览器里完成授权…".to_string();
                    }
                    LoginMsg::BrowserUrl(url) => {
                        login.browser_url = Some(url);
                        login.status = "等你在浏览器里完成授权…".to_string();
                    }
                    LoginMsg::Status(text) => login.status = text,
                    LoginMsg::Phase(phase) => {
                        login.phase = Some(phase);
                        login.status = phase.label().to_string();
                    }
                    LoginMsg::Done(result) => done = Some(result),
                }
            }
        }

        let Some(result) = done else { return };
        if let Some(login) = self.login.as_mut() {
            login.busy = false;
            login.rx = None;
        }

        match result {
            Ok(account) => {
                let name = account.username.clone();
                self.username = name.clone();
                self.account = Some(account.to_account());
                logging::info(format!("已登录正版账号 {}", name));
                self.login = None;
            }
            Err(e) => {
                if let Some(login) = self.login.as_mut() {
                    login.error = Some(e);
                    login.status.clear();
                }
            }
        }
    }

    /// 账户页。
    ///
    /// 一个入口管两件事：离线名字与正版登录。左边两个分区，右边是内容；
    /// 和「管理」一样整页盖住主界面（含主导航与标题栏以下的全部）。
    fn account_page(&mut self, ui: &mut egui::Ui, busy: bool) {
        let mut action: Option<AccountAction> = None;
        let mut copy: Option<String> = None;

        // 当前账号如果不是离线（正版 / 第三方），离线分区底部要提一句：
        // 光说「已经登录正版」会把皮肤站账号也说成正版，玩家会以为
        // 令牌丢了或者登录串了。
        let current_elsewhere = self.account.as_ref().and_then(|a| {
            if a.access_token == "0".repeat(32) {
                return None;
            }
            Some((
                a.username.clone(),
                if a.api_root.is_some() {
                    "第三方"
                } else {
                    "正版"
                },
            ))
        });

        // 「正版账户」页只认微软账号：离线账号的 api_root 也是 None，
        // 拿它当正版会在这一页显示一个「退出登录」按钮，很怪。
        let official = self
            .accounts
            .current()
            .filter(|a| !a.offline && a.api_root.is_none())
            .map(|a| a.name.clone());

        egui::Panel::left("account-nav")
            .exact_size(178.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(12, 14)),
            )
            .show(ui, |ui| {
                if ui
                    .add(egui::Button::new(
                        RichText::new("‹ 返回").size(12.5).color(theme::TEXT_WEAK),
                    ))
                    .on_hover_text("回到主界面")
                    .clicked()
                {
                    aicraft_core::audit::action("账户：返回主界面");
                    action = Some(AccountAction::Close);
                }
                ui.add_space(10.0);
                ui.label(
                    RichText::new("当前账户")
                        .size(10.5)
                        .color(theme::TEXT_FAINT),
                );

                // 这块既是状态也是入口：一直是启动游戏真正会用的那个号，
                // 不跟随下面的页签；点一下回到账号列表 —— 原来那个「账号」
                // 页签就省了。
                let (shown_name, shown_kind) = match self.current_account_label() {
                    Some((name, kind)) => (name, kind.to_string()),
                    None => ("未配置".to_string(), String::new()),
                };
                // 停在这一页时也穿绿底：它和下面几个页签是同一层关系，
                // 只有它光秃秃的反而像漏了状态。
                let on_list = self.account_section == AccountSection::List;
                if theme::text_button(ui, &shown_name, on_list).clicked() {
                    self.account_section = AccountSection::List;
                    aicraft_core::audit::detail("账户：切到账户列表");
                }
                if !shown_kind.is_empty() {
                    ui.label(
                        RichText::new(&shown_kind)
                            .size(10.5)
                            .color(theme::TEXT_WEAK),
                    );
                }
                ui.add_space(14.0);

                // 三个页签平级排开；账号列表不在左栏，在主窗口里。
                for section in AccountSection::ALL {
                    let selected = self.account_section == section;
                    if theme::nav_item(ui, section.label(), selected).clicked() {
                        self.account_section = section;
                        aicraft_core::audit::detail(format!("账户：切到「{}」", section.label()));
                    }
                    ui.add_space(2.0);
                }
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(theme::BG_BOTTOM)
                    .inner_margin(egui::Margin::symmetric(22, 20)),
            )
            .show(ui, |ui| {
                match self.account_section {
                    AccountSection::List => {
                        ui.label(RichText::new("账号").size(17.0).strong());
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "点一行就切过去；右边的「×」把它从这台电脑上删掉。登录凭证跟着这台电脑走。",
                            )
                            .size(11.5)
                            .color(theme::TEXT_WEAK),
                        );
                        ui.add_space(12.0);

                        if self.accounts.accounts.is_empty() {
                            let offline_ok = self.offline_allowed();
                            theme::card().show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.label(RichText::new("还没有账号").size(13.0).strong());
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(if offline_ok {
                                        "左边「离线账户」起个名字就能玩；有正版或皮肤站账号的话，那边也能登录。"
                                    } else {
                                        "左边「正版账户」那页登录一次，之后离线账户也能用；皮肤站账号在那之后同样可登录。"
                                    })
                                    .size(11.0)
                                    .color(theme::TEXT_WEAK),
                                );
                            });
                        } else {
                            // 纹理先克隆出来：`TextureHandle` 内部是 Arc，
                            // 克隆很便宜，但能让闭包里同时读 `skin_tex` 和改 `action`。
                            let rows: Vec<(String, String, &'static str, Option<egui::TextureHandle>)> = self
                                .accounts
                                .accounts
                                .iter()
                                .map(|a| {
                                    (
                                        a.uuid.clone(),
                                        a.name.clone(),
                                        account_kind(a),
                                        self.skin_tex.get(&a.uuid).cloned(),
                                    )
                                })
                                .collect();
                            let current_uuid = self.account.as_ref().map(|a| a.uuid.clone());
                            theme::card().show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                for (uuid, name, kind, skin) in rows {
                                    let selected =
                                        current_uuid.as_deref() == Some(uuid.as_str());
                                    let line = theme::account_row(
                                        ui,
                                        &name,
                                        kind,
                                        selected,
                                        skin.as_ref(),
                                    );
                                    // 「×」优先：它和整行重叠，先问它有没有被点
                                    if line.delete.clicked() {
                                        aicraft_core::audit::action(format!(
                                            "账户：删除 {}",
                                            name
                                        ));
                                        action = Some(AccountAction::Remove(uuid));
                                    } else if line.row.clicked() && !selected {
                                        aicraft_core::audit::action(format!(
                                            "账户：切换到 {}",
                                            name
                                        ));
                                        action = Some(AccountAction::Switch(uuid));
                                    }
                                    ui.add_space(2.0);
                                }
                            });
                        }
                    }
                    AccountSection::Offline => {
                        let offline_ok = self.offline_allowed();
                        ui.label(RichText::new("离线账户").size(17.0).strong());
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(if offline_ok {
                                "只在本机生效，不用密码。起个名字就能进单人游戏与离线服务器。"
                            } else {
                                "只在本机生效，不用密码。要先登录一次正版账户，这一页才能用。"
                            })
                            .size(11.5)
                            .color(theme::TEXT_WEAK),
                        );
                        ui.add_space(12.0);

                        if !offline_ok {
                            theme::card().show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.label(
                                    RichText::new("还没有登录过正版账户")
                                        .size(13.0)
                                        .color(theme::TEXT),
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(
                                        "登录正版之后，这里就能起离线名字了。左边「正版账户」那页可以登录。",
                                    )
                                    .size(11.0)
                                    .color(theme::TEXT_WEAK),
                                );
                            });
                            return;
                        }

                        theme::card().show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(RichText::new("玩家名").size(12.0).color(theme::TEXT_WEAK));
                            ui.add_space(4.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut self.account_draft)
                                    .hint_text("起个名字，进游戏就用它")
                                    .desired_width(f32::INFINITY),
                            );
                            ui.add_space(6.0);

                            if let Some(warning) = offline_name_warning(&self.account_draft) {
                                ui.label(
                                    RichText::new(warning).size(11.0).color(theme::WARN),
                                );
                            } else if self.account_draft.trim().is_empty() {
                                ui.label(
                                    RichText::new("还没有名字")
                                        .size(11.0)
                                        .color(theme::TEXT_FAINT),
                                );
                            } else {
                                ui.label(
                                    RichText::new("名字存下来之后，启动游戏就用它。")
                                        .size(11.0)
                                        .color(theme::TEXT_FAINT),
                                );
                            }

                            ui.add_space(10.0);
                            if theme::dialog_primary_button(
                                ui,
                                "保存",
                                offline_name_usable(&self.account_draft) && !busy,
                                92.0,
                            )
                            .clicked()
                            {
                                aicraft_core::audit::action("账户：保存离线名字");
                                action = Some(AccountAction::SaveName);
                            }
                        });

                        if let Some((name, kind)) = &current_elsewhere {
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(format!(
                                    "现在用的是{}账号 {}，启动游戏会优先用它；离线名字留着备用。",
                                    kind, name
                                ))
                                .size(11.0)
                                .color(theme::TEXT_WEAK),
                            );
                        }
                    }
                    AccountSection::ThirdParty => {
                        ui.label(RichText::new("第三方登录").size(17.0).strong());
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "皮肤站（LittleSkin、ElyBy 之类）用这套。启动时会自动挂上 \
                                 authlib-injector，皮肤和披风都走那台服务器。",
                            )
                            .size(11.5)
                            .color(theme::TEXT_WEAK),
                        );
                        ui.add_space(12.0);

                        // 已经挂着第三方账号时，先让用户看清楚现在是谁
                        if let Some(account) = &self.account {
                            if let Some(root) = &account.api_root {
                                theme::card().show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        theme::status_dot(ui, theme::ACCENT);
                                        ui.label(
                                            RichText::new(account.username.clone())
                                                .size(13.5)
                                                .strong()
                                                .color(theme::TEXT),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if theme::dialog_danger_button(
                                                    ui,
                                                    "退出登录",
                                                    !busy,
                                                    96.0,
                                                )
                                                .clicked()
                                                {
                                                    aicraft_core::audit::action(
                                                        "账户：退出登录",
                                                    );
                                                    aicraft_core::audit::action(
                                                    "账户：退出登录",
                                                );
                                                action = Some(AccountAction::Logout);
                                                }
                                            },
                                        );
                                    });
                                    ui.add_space(2.0);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(root.clone())
                                                .size(11.0)
                                                .color(theme::TEXT_WEAK),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(root.clone());
                                });
                                ui.add_space(10.0);
                            }
                        }

                        theme::card().show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(
                                RichText::new("认证服务器")
                                    .size(12.0)
                                    .color(theme::TEXT_WEAK),
                            );
                            ui.add_space(4.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut self.third_root)
                                    .hint_text("https://littleskin.cn/api/yggdrasil")
                                    .desired_width(f32::INFINITY),
                            );
                            ui.add_space(2.0);
                            ui.label(
                                RichText::new(
                                    "填站点给的 Yggdrasil 地址；只写域名也行，会自动补 /api/yggdrasil。",
                                )
                                .size(10.5)
                                .color(theme::TEXT_FAINT),
                            );

                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("账号").size(12.0).color(theme::TEXT_WEAK),
                            );
                            ui.add_space(4.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut self.third_user)
                                    .hint_text("邮箱或用户名")
                                    .desired_width(f32::INFINITY),
                            );

                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("密码").size(12.0).color(theme::TEXT_WEAK),
                            );
                            ui.add_space(4.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut self.third_pass)
                                    .password(true)
                                    .hint_text("只用于这一次登录，不会存下来")
                                    .desired_width(f32::INFINITY),
                            );

                            if let Some(err) = &self.third_error {
                                ui.add_space(6.0);
                                ui.label(RichText::new(err.clone()).size(11.0).color(theme::DANGER));
                            }

                            ui.add_space(10.0);
                            let ready = !self.third_root.trim().is_empty()
                                && !self.third_user.trim().is_empty()
                                && !self.third_pass.is_empty()
                                && !busy;
                            if theme::dialog_primary_button(ui, "登录", ready, 92.0).clicked() {
                                aicraft_core::audit::action(format!(
                                    "账户：用 {} 登录外置账号 {}",
                                    self.third_root.trim(),
                                    self.third_user.trim()
                                ));
                                action = Some(AccountAction::AddThirdParty);
                            }
                        });
                    }
                    AccountSection::Official => {
                        ui.label(RichText::new("正版账户").size(17.0).strong());
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("皮肤、Realms、开了验证的服务器都要它。")
                                .size(11.5)
                                .color(theme::TEXT_WEAK),
                        );
                        ui.add_space(12.0);

                        // 两条路互不排斥：账号本来就是一个列表，谁在用只是
                        // 一个指针。第三方账号在用时登录正版，会把它留在列表里
                        // 并切过去，不需要先删掉谁。
                        if let Some(root) = self.account.as_ref().and_then(|a| a.api_root.as_ref())
                        {
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(format!(
                                    "现在用的是第三方账号（{}）。登录正版只是把它留在列表里、切过去，随时能切回来。",
                                    root
                                ))
                                .size(11.0)
                                .color(theme::TEXT_WEAK),
                            );
                        }

                        theme::card().show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            if let Some(name) = &official {
                                ui.horizontal(|ui| {
                                    theme::status_dot(ui, theme::ACCENT);
                                    ui.label(
                                        RichText::new(name.clone())
                                            .size(13.5)
                                            .strong()
                                            .color(theme::TEXT),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if theme::dialog_danger_button(
                                                ui,
                                                "退出登录",
                                                !busy,
                                                96.0,
                                            )
                                            .clicked()
                                            {
                                                aicraft_core::audit::action(
                                                    "账户：退出登录",
                                                );
                                                action = Some(AccountAction::Logout);
                                            }
                                        },
                                    );
                                });
                                return;
                            }

                            if !SHOW_MSA_LOGIN {
                                ui.label(
                                    RichText::new("正版登录暂时关着（应用注册还在等 Mojang 批准）。")
                                        .size(11.5)
                                        .color(theme::TEXT_FAINT),
                                );
                                return;
                            }

                            let running = self.login.as_ref().map(|l| l.busy).unwrap_or(false);

                            if let Some(login) = self.login.as_mut() {
                                if !login.status.is_empty() {
                                    ui.horizontal(|ui| {
                                        if login.busy {
                                            ui.spinner();
                                        }
                                        ui.label(RichText::new(login.status.clone()).size(12.0));
                                    });
                                }
                                // 换令牌这四跳是有确定顺序的，能算比例就别转圈：
                                // 玩家看得见「第 2 / 4 步」，才知道它是慢还是卡住了。
                                if let Some(phase) = login.phase {
                                    ui.add_space(4.0);
                                    ui.add(
                                        egui::ProgressBar::new(
                                            phase.index() as f32
                                                / aicraft_core::auth::LoginPhase::TOTAL as f32,
                                        )
                                        .desired_width(ui.available_width())
                                        .text(format!(
                                            "第 {} / {} 步",
                                            phase.index(),
                                            aicraft_core::auth::LoginPhase::TOTAL
                                        )),
                                    );
                                }
                                if let Some((uri, code)) = &login.device_code {
                                    ui.add_space(6.0);
                                    ui.label(
                                        RichText::new("1. 在浏览器打开")
                                            .size(11.0)
                                            .color(theme::TEXT_WEAK),
                                    );
                                    if ui.link(uri.clone()).clicked() {
                                        let _ = msa::open_browser(uri);
                                    }
                                    ui.add_space(3.0);
                                    ui.label(
                                        RichText::new("2. 输入下面的代码")
                                            .size(11.0)
                                            .color(theme::TEXT_WEAK),
                                    );
                                    theme::inset().show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new(code.clone())
                                                    .monospace()
                                                    .size(22.0)
                                                    .strong()
                                                    .color(theme::ACCENT),
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if ui.button("复制").clicked() {
                                                        copy = Some(code.clone());
                                                    }
                                                },
                                            );
                                        });
                                    });
                                    if !login.device_message.is_empty() {
                                        ui.label(
                                            RichText::new(login.device_message.clone())
                                                .size(10.5)
                                                .color(theme::TEXT_WEAK),
                                        );
                                    }
                                }
                                if login.browser_url.is_some()
                                    && ui.link("浏览器没自动打开？点这里重开").clicked()
                                {
                                    action = Some(AccountAction::ReopenBrowser);
                                }
                                if let Some(err) = &login.error {
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(err.clone()).size(11.0).color(theme::DANGER),
                                    );
                                }
                            }

                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                if theme::dialog_primary_button(
                                    ui,
                                    "浏览器登录",
                                    !running && !busy,
                                    108.0,
                                )
                                .clicked()
                                {
                                    aicraft_core::audit::action("账户：正版登录（浏览器）");
                                    action = Some(AccountAction::Browser);
                                }
                                if theme::dialog_button(ui, "设备码登录", !running && !busy, 108.0)
                                    .clicked()
                                {
                                    aicraft_core::audit::action("账户：正版登录（设备码）");
                                    action = Some(AccountAction::Device);
                                }
                            });
                        });
                    }
                }
            });

        if let Some(text) = copy {
            ui.ctx().copy_text(text);
        }

        match action {
            Some(AccountAction::Switch(uuid)) => {
                self.switch_account(&uuid);
            }
            Some(AccountAction::Remove(uuid)) => {
                if let Some(found) = self.accounts.accounts.iter().find(|a| a.uuid == uuid) {
                    self.pending_account_remove = Some((found.uuid.clone(), found.name.clone()));
                }
            }
            Some(AccountAction::SaveName) => {
                let name = self.account_draft.trim().to_string();
                if !self.offline_allowed() {
                    logging::warn("离线账户现在用不了：先登录一次正版账户。");
                    return;
                }
                if offline_name_usable(&name) {
                    // 离线名字也是一条账号：同一个名字算出同一个 UUID，
                    // 所以反复保存不会堆出一串重名的条目。
                    let stored = StoredAccount::offline(&name);
                    logging::info(format!("离线账户 {}", name));
                    self.username = stored.name.clone();
                    self.settings.offline_name = stored.name.clone();
                    self.accounts.upsert(stored.clone());
                    self.save_accounts();
                    self.account = Some(stored.to_account());
                    self.persist_settings_quietly();
                    self.account_open = false;
                }
            }
            Some(AccountAction::Logout) => {
                // 与列表里那个「×」是同一件事，所以走同一个确认框：
                // 同一条破坏性动作留两条路径，迟早有一条忘了问。
                if let Some(current) = self.account.as_ref() {
                    self.pending_account_remove =
                        Some((current.uuid.clone(), current.username.clone()));
                }
            }
            Some(AccountAction::AddThirdParty) => {
                let root = self.third_root.trim().to_string();
                let user = self.third_user.trim().to_string();
                let pass = std::mem::take(&mut self.third_pass);
                self.third_error = None;
                let mirror = self
                    .settings
                    .mirror
                    .resolved(aicraft_core::settings::ProbeKind::Version)
                    == aicraft_core::Mirror::Bmclapi;
                self.spawn_task("正在登录第三方账户…".to_string(), move |tx| {
                    let result = (|| -> Result<aicraft_core::auth::yggdrasil::Session, String> {
                        let root = aicraft_core::auth::yggdrasil::normalize_api_root(&root)?;
                        // 先把 authlib-injector 下好：登录成功却发现少了它，
                        // 玩家点启动时才报错，比现在报更难受。
                        aicraft_core::auth::yggdrasil::ensure_injector(
                            &aicraft_core::settings::accounts_dir(),
                            mirror,
                        )?;
                        let session = aicraft_core::auth::yggdrasil::authenticate(
                            &aicraft_core::auth::CurlHttp,
                            &root,
                            &user,
                            &pass,
                        )
                        .map_err(describe_auth_error)?;
                        Ok(session)
                    })();
                    let _ = tx.send(TaskMsg::ThirdParty(result));
                    Ok(())
                });
            }
            Some(AccountAction::Browser) => {
                self.account_section = AccountSection::Official;
                self.open_login();
                self.start_login_browser();
            }
            Some(AccountAction::Device) => {
                self.account_section = AccountSection::Official;
                self.open_login();
                self.start_login_device();
            }
            Some(AccountAction::ReopenBrowser) => {
                if let Some(url) = self.login.as_ref().and_then(|l| l.browser_url.clone()) {
                    let _ = msa::open_browser(&url);
                }
            }
            Some(AccountAction::Close) => {
                self.account_open = false;
                self.login = None;
            }
            None => {}
        }
    }

    /// 主界面上直接改的内存值也顺手落盘，免得重启后变回去。
    fn persist_settings_quietly(&mut self) {
        self.settings.clamp();
        if let Err(e) = self.settings.save(&settings_path()) {
            logging::warn(format!("保存设置失败: {}", e));
        }
    }

    /// 导出日志：先让用户挑保存位置，挑好了才把内容写过去。
    ///
    /// 保存框会阻塞它所在的线程，所以和选游戏目录一样扔到后台线程；
    /// 结果经 `export_rx` 回到主线程再落盘。
    fn export_log(&mut self) {
        if self.export_rx.is_some() {
            return;
        }
        let (stamp, _) = now_local();
        let name = format!("launcher-{}.log", stamp);
        let initial = aicraft_core::settings::suggested_export_dir();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = pick_save_blocking(&initial, &name);
            let _ = tx.send(result);
        });
        self.export_rx = Some(rx);
    }

    /// 收「另存为」对话框的结果，选中了就写文件。
    fn poll_export_pick(&mut self) {
        let Some(rx) = self.export_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(Some(path))) => self.save_log_to(&path),
            Ok(Ok(None)) => {}
            Ok(Err(message)) => logging::warn(format!("导出日志失败: {}", message)),
            Err(TryRecvError::Empty) => self.export_rx = Some(rx),
            Err(TryRecvError::Disconnected) => {}
        }
    }

    /// 把日志写进用户选定的文件，并在资源管理器里选中它。
    fn save_log_to(&mut self, path: &Path) {
        let version = self.selected_version().map(|v| v.name.clone());
        let text = log_text(
            &logging::entries(None),
            &self.settings,
            &self.game_dir,
            version.as_deref(),
        );
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                logging::warn(format!("导出日志失败: 创建目录失败: {}", e));
                return;
            }
        }
        match std::fs::write(path, &text) {
            Ok(()) => {
                let _ = aicraft_core::spawn::silent("explorer.exe")
                    .arg(format!("/select,{}", path.display()))
                    .spawn();
                logging::info(format!("日志已导出到 {}", path.display()));
            }
            Err(e) => logging::warn(format!("导出日志失败: {}（{}）", path.display(), e)),
        }
    }

    /// 首页只干一件事：选版本、按下启动。
    ///
    /// 版本列表不占版面——右下角启动按钮右边那颗三条杠按钮，鼠标上去就向上
    /// 弹出一列版本。这样首页左侧只剩「现在选的是谁、账号是谁」，一眼看完。
    fn home_page(&mut self, ui: &mut egui::Ui, busy: bool) {
        let page_size = ui.available_size();

        // 整个中央区被一张半透明白色大卡片包住：左上角写「当前版本」，
        // 右上角列这一份版本的主类 / 库 / Java，底部右对齐摆启动按钮。
        // 卡片撑满可用区域（`set_min_size` 扣掉它自己的内边距）。
        theme::glass_card().show(ui, |ui| {
            ui.set_min_size(egui::vec2(
                (page_size.x - 40.0).max(200.0),
                (page_size.y - 36.0).max(160.0),
            ));

            // 上半部分：左边当前版本，右边版本信息，各占一列。
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.set_width((ui.available_width() * 0.52).max(180.0));

                    let current = self
                        .selected_timeout()
                        .map(|i| self.versions[i].name.clone())
                        .unwrap_or_else(|| "（还没有选择版本）".to_string());

                    ui.label(RichText::new("当前版本").size(12.0).color(theme::TEXT_WEAK));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(current)
                            .size(26.0)
                            .strong()
                            .color(theme::TEXT),
                    );

                    ui.add_space(10.0);

                    // 账户状态不在这儿：标题栏右上角那个按钮就是入口。

                    if self.selected.is_none() {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(if self.versions.is_empty() {
                                "先去「版本管理 → 下载版本」装一个"
                            } else {
                                "点右下角那三条横线，选一个版本"
                            })
                            .size(11.5)
                            .color(theme::TEXT_WEAK),
                        );
                    }
                });
            });

            // 右下角：先把剩余高度让出来，再在底部一条 52px 的横带里右对齐放按钮。
            // 不能直接用 Layout::BOTTOM 把按钮丢进剩余空间——那样按钮会被撑高，
            // 文字跟着底边走，看着是偏下的（实测偏移约 11px）。
            // `right_to_left` 里先加的在最右，所以三条杠排在启动按钮右边。
            let ready = !busy && self.selected.is_some();
            let band = 52.0;
            let remaining = ui.available_height();
            if remaining > band {
                ui.add_space(remaining - band);
            }
            let menu_anchor = ui
                .allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), band),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let menu = theme::menu_button(ui, self.version_menu_open);
                        ui.add_space(8.0);
                        if theme::launch_button(ui, ready, 264.0).clicked() {
                            aicraft_core::audit::action("首页：启动游戏");
                            self.start_launch();
                        }
                        menu
                    },
                )
                .inner;
            // 点一下钉住（这样不悬停也能翻列表），再点一下收回
            if menu_anchor.clicked() {
                self.version_menu_open = !self.version_menu_open;
            }
            self.version_menu(ui, &menu_anchor);
        });
    }

    /// 三条杠按钮上方的版本列表：鼠标停在按钮（或列表）上就开着，松手即收。
    ///
    /// 不用 `Popup::menu`，因为那是「点击才开」的语义；这里要的是悬停即展开。
    /// 收起判定没法只看按钮 `hovered()`——指针一移进列表，按钮就不 hovered 了，
    /// 列表会瞬间消失，所以额外记住上一帧列表占的矩形，指针在里面也算开着。
    fn version_menu(&mut self, ui: &mut egui::Ui, anchor: &egui::Response) {
        let pointer = ui.ctx().pointer_hover_pos();
        let in_popup = match (self.version_menu_rect, pointer) {
            (Some(rect), Some(pos)) => rect.expand(8.0).contains(pos),
            _ => false,
        };
        // 点开了之后（click 钉住）要能靠点别处 / Esc 收掉
        if self.version_menu_open && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.version_menu_open = false;
        }
        let clicked_elsewhere = self.version_menu_open
            && !anchor.hovered()
            && !in_popup
            && ui.input(|i| i.pointer.any_click());
        if clicked_elsewhere {
            self.version_menu_open = false;
        }

        let open = anchor.hovered() || in_popup || self.version_menu_open;
        if !open {
            self.version_menu_rect = None;
            return;
        }

        let popup_frame = egui::Frame::NONE
            .fill(theme::CARD)
            .stroke(egui::Stroke::new(1.0, theme::BORDER))
            .corner_radius(egui::CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(8, 8))
            .shadow(egui::epaint::Shadow {
                offset: [0, 2],
                blur: 10,
                spread: 0,
                color: egui::Color32::from_black_alpha(14),
            });

        let shown = egui::Popup::from_response(anchor)
            .open(true)
            .align(egui::RectAlign::TOP_END)
            .gap(6.0)
            .width(248.0)
            .frame(popup_frame)
            .show(|ui| {
                ui.set_min_width(232.0);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(
                    RichText::new(format!("共 {} 个版本", self.versions.len()))
                        .size(11.0)
                        .color(theme::TEXT_WEAK),
                );
                ui.add_space(4.0);
                if self.versions.is_empty() {
                    ui.label(
                        RichText::new("这个目录里还没有版本")
                            .size(12.0)
                            .color(theme::TEXT_WEAK),
                    );
                    return;
                }
                // 循环里正借着 `self.versions`，不能顺手改 `self` 的别的字段，
                // 先记下点的是第几行，退出闭包再统一处理。
                let mut clicked: Option<usize> = None;
                egui::ScrollArea::vertical()
                    .id_salt("home-version-menu")
                    .max_height(280.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (i, v) in self.versions.iter().enumerate() {
                            let picked = self.selected == Some(i);
                            // 每行铺满：`selectable_label` 默认只占文字宽，
                            // 高亮色块就一小条，得手动把可用宽度全给它。
                            let resp = ui.add_sized(
                                [ui.available_width(), 0.0],
                                egui::Button::selectable(
                                    picked,
                                    RichText::new(v.name.clone()).size(13.0),
                                ),
                            );
                            if resp.clicked() {
                                clicked = Some(i);
                            }
                        }
                    });
                if let Some(i) = clicked {
                    if let Some(name) = self.versions.get(i).map(|v| v.name.clone()) {
                        aicraft_core::audit::action(format!("首页：切换到版本 {}", name));
                    }
                    self.select_version(i);
                    self.version_menu_open = false;
                }
            });
        self.version_menu_rect = shown.map(|r| r.response.rect);
    }

    /// 某个版本启动时会不会走隔离布局。
    ///
    /// 设置里开着就一律隔离；关掉之后再看它目录里有没有用户数据——
    /// 有的（options.txt / saves / mods …）仍然按隔离启动，否则改个开关
    /// 就会让玩家「看不见自己的世界」。
    fn launch_isolation(&self, name: &str) -> bool {
        if self.settings.isolate_instances {
            return true;
        }
        aicraft_core::detect_mode(&PathBuf::from(&self.game_dir), name)
            == aicraft_core::InstanceMode::Isolated
    }

    /// 版本页：本地列表 + 选中详情 + 校验 / 补全 / 删除。
    fn versions_page(&mut self, ui: &mut egui::Ui, busy: bool) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("版本列表").size(17.0).strong());
            ui.add_space(4.0);
            if theme::refresh_button(ui, !busy)
                .on_hover_text("点击刷新列表")
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                self.rescan();
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "{} 个 · {} ms",
                    self.versions.len(),
                    self.last_scan_ms
                ))
                .size(11.0)
                .color(theme::TEXT_WEAK),
            );
        });
        ui.add_space(10.0);

        if self.versions.is_empty() {
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("这个游戏目录里没有找到版本")
                        .size(13.0)
                        .color(theme::TEXT_WEAK),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("点左边「版本管理 → 下载版本」装一个，或者去「版本管理 → 目录列表」换个游戏目录。")
                        .size(11.5)
                        .color(theme::TEXT_WEAK),
                );
            });
            return;
        }

        // 详情卡片**浮在列表上面**，不是占掉列表下面那一块。
        //
        // 先在整块可用高度里画列表（它会自己在底下滚），然后把卡片当作浮层
        // 叠上去、贴住页面底边。这样列表能一直滚到窗口最下面，
        // 视觉上就是详情卡片压在列表上——列表在它下面继续延伸。
        //
        // 列表内容末尾要补一段空白（`CARD_PEEK`），否则滚到底时最后几行
        // 会被这张浮层卡片永久挡住，点不到。
        const CARD_PEEK: f32 = 170.0;
        let mut clicked: Option<usize> = None;
        let page_rect = ui.available_rect_before_wrap();
        egui::ScrollArea::vertical()
            .id_salt("versions-page-list")
            .auto_shrink([false, false])
            .max_height(page_rect.height())
            .show(ui, |ui| {
                // 每行本身就是一张卡片，行间留一条缝，卡片才分得开。
                ui.spacing_mut().item_spacing.y = 6.0;
                for (i, v) in self.versions.iter().enumerate() {
                    // 纯原版也标一枚：详情卡片那边就写着「原版」，列表里空着会让人
                    // 以为「没读到」；而且这一枚正好把「魔改过的」和「没魔改的」分开。
                    if theme::version_item(
                        ui,
                        &v.name,
                        v.java_major,
                        v.loader,
                        self.selected == Some(i),
                    )
                    .clicked()
                    {
                        clicked = Some(i);
                    }
                }
                // 给浮层卡片让出一条能滚过去的空间。
                ui.add_space(CARD_PEEK);
            });
        if let Some(i) = clicked {
            if let Some(name) = self.versions.get(i).map(|v| v.name.clone()) {
                aicraft_core::audit::detail(format!("版本列表：选中 {}", name));
            }
            self.select_version(i);
            self.pending_remove = None;
        }

        let Some(index) = self.selected.filter(|i| *i < self.versions.len()) else {
            return;
        };
        let name = self.versions[index].name.clone();
        let main_class = self.versions[index].main_class.clone();
        let libs = self.versions[index].library_count;
        let java_major = self.versions[index].java_major;
        let isolated = self.launch_isolation(&name);
        let mode_text = if isolated {
            "隔离实例"
        } else {
            "共享目录"
        };

        // 日志与崩溃报告的位置跟启动时一致：隔离实例在 versions/<名字>/，
        // 共享模式在 .minecraft 根目录。算出来的就是游戏真的会写进去的地方。
        let run_dir = game_run_dir(Path::new(&self.game_dir), &name, isolated);
        let log_dir = game_log_dir(&run_dir);
        let crash_dir = game_crash_dir(&run_dir);
        // 这一份挂的是什么加载器：原版就是纯原版，其余按标记/清单认。
        // 结论在 rescan 时就算好了，这里只是取一下。
        let loader = self.versions[index].loader;
        let loader_label = match loader {
            Some(kind) => kind.label().to_string(),
            None => "原版".to_string(),
        };

        // 列表已经占掉了整块高度（它会自己滚），这里把卡片当浮层叠到底边。
        //
        // 用 `Area` 而不是 `scope_builder`：`Area` 的尺寸由内容自己决定，
        // 再用 `pivot(BOTTOM_CENTER)` + `anchor(BOTTOM_CENTER)` 把它的**底边**
        // 钉在页面底边上——不用估卡片高度，也不受外层光标影响。
        // `scope_builder` 那条路试过：`max_rect` 给多高它就往下长多少，
        // 卡片永远停不到真正的底边。
        let page_bottom = ui.max_rect().bottom();
        egui::Area::new(egui::Id::unique("versions-detail-card"))
            .order(egui::Order::Middle)
            .fixed_pos(egui::pos2(page_rect.left(), page_bottom))
            .pivot(egui::Align2::LEFT_BOTTOM)
            .show(ui.ctx(), |ui| {
                ui.set_width(page_rect.width());
                theme::card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&name).size(19.0).strong());
                        // 加载器紧跟着版本名：它是「这是什么版本」的一部分，
                        // 跟 Java 号、库数量那种属性不同。
                        ui.add_space(6.0);
                        theme::chip_loader(ui, loader, &loader_label);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            theme::chip(
                                ui,
                                &match java_major {
                                    Some(j) => format!("Java {}", j),
                                    None => "Java 未声明".to_string(),
                                },
                            );
                            theme::chip(ui, &format!("{} 个库", libs));
                            theme::chip(ui, mode_text);
                        });
                    });
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(format!("主类  {}", main_class))
                            .size(11.5)
                            .color(theme::TEXT_WEAK),
                    );

                    // 隔离与否只看上面那枚 chip：一句「隔离实例 / 共享目录」已经够用，
                    // 再补一段解释会把这一页越堆越长。要改去「设置 → 实例隔离」。

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        // 左边只放「删除」这类破坏性动作：它离得开一点，误点的概率就小一点，
                        // 而且标红之后一眼能看出这一侧和右边那组不是一类事。
                        if ui
                            .add_enabled(
                                !busy,
                                egui::Button::new(RichText::new("删除版本").color(theme::DANGER)),
                            )
                            .clicked()
                        {
                            aicraft_core::audit::action(format!(
                                "版本列表：删除 {}（等确认）",
                                name
                            ));
                            self.pending_remove = Some(name.clone());
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::primary_button(ui, "启动游戏", !busy).clicked() {
                                aicraft_core::audit::action(format!("版本列表：启动 {}", name));
                                self.start_launch();
                            }
                            ui.add_space(6.0);
                            if ui
                                .add_enabled(!busy, egui::Button::new("校验资源"))
                                .clicked()
                            {
                                aicraft_core::audit::action(format!(
                                    "版本列表：校验 {} 的资源",
                                    name
                                ));
                                self.start_verify();
                            }
                            ui.add_space(6.0);
                            if ui.add_enabled(!busy, egui::Button::new("管理")).clicked() {
                                aicraft_core::audit::action(format!(
                                    "版本列表：打开 {} 的管理页",
                                    name
                                ));
                                self.open_manage(&name);
                            }
                            ui.add_space(6.0);
                            // 日志与崩溃报告：没跑过就没有目录，按钮直接灰掉，
                            // 免得点开一个空文件夹还得自己找。
                            if ui
                                .add_enabled(log_dir.is_some(), egui::Button::new("日志"))
                                .clicked()
                            {
                                if let Some(dir) = &log_dir {
                                    aicraft_core::audit::detail(format!(
                                        "版本列表：打开日志目录 {}",
                                        dir.display()
                                    ));
                                    open_in_explorer(dir);
                                }
                            }
                            ui.add_space(6.0);
                            if ui
                                .add_enabled(crash_dir.is_some(), egui::Button::new("崩溃日志"))
                                .clicked()
                            {
                                if let Some(dir) = &crash_dir {
                                    aicraft_core::audit::detail(format!(
                                        "版本列表：打开崩溃日志目录 {}",
                                        dir.display()
                                    ));
                                    open_in_explorer(dir);
                                }
                            }
                        });
                    });
                });
            });
    }

    /// 进「服务端管理」：第一次进要扫一遍磁盘，还要给「新游戏版本」下拉备一份清单。
    fn open_servers(&mut self) {
        self.page = Page::Servers;
        if !self.servers.loaded {
            self.servers.set_extra(&self.settings.server_dirs);
        }
        if self.server_versions.is_empty() && !self.server_versions_loading {
            self.server_versions_loading = true;
            let mirror = self.settings.mirror;
            std::thread::spawn(move || {
                // 服务端只吃正式版，快照装了也跑不起来；顺手把快照滤掉。
                let result = aicraft_core::transport::fetch_text(
                    &mirror.rewrite(aicraft_core::install::VERSION_MANIFEST_URL),
                )
                .map_err(|e| e.to_string())
                .and_then(|text| aicraft_core::install::parse_manifest(&text));
                let ids = match result {
                    Ok(manifest) => manifest
                        .versions
                        .into_iter()
                        .filter(|version| version.version_type == "release")
                        .map(|version| version.id)
                        .collect(),
                    Err(e) => {
                        logging::warn(format!("取服务端可用的游戏版本失败: {}", e));
                        Vec::new()
                    }
                };
                SERVER_VERSIONS.lock().map(|mut slot| *slot = Some(ids)).ok();
            });
        }
        if let Ok(slot) = SERVER_VERSIONS.lock() {
            if let Some(ids) = slot.as_ref() {
                self.server_versions = ids.clone();
            }
        }
    }

    fn servers_page(&mut self, ui: &mut egui::Ui, busy: bool) {
        if let Ok(slot) = SERVER_VERSIONS.lock() {
            if let Some(ids) = slot.as_ref() {
                if self.server_versions.len() != ids.len() {
                    self.server_versions = ids.clone();
                    self.server_versions_loading = false;
                    ui.ctx().request_repaint();
                }
            }
        } else {
            ui.ctx().request_repaint_after(Duration::from_millis(120));
        }
        if self.server_versions_loading && self.server_versions.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(120));
        }

        // 正在装的时候每帧都重绘，进度才会动。
        if self.server_rx.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(120));
        }

        let action = serverui::page(
            ui,
            &mut self.servers,
            &self.server_versions,
            busy || self.server_rx.is_some(),
        );
        if let Some(action) = action {
            self.handle_server(action);
        }
    }

    fn handle_server(&mut self, action: serverui::Action) {
        use serverui::Action;
        match action {
            Action::Reload => {
                aicraft_core::audit::action("服务端：重新扫描");
                self.servers.reload();
            }
            Action::PickFolder => {
                aicraft_core::audit::action("服务端：挑一个已有的服务端文件夹");
                self.pick_server_folder();
            }
            Action::Forget(path) => {
                aicraft_core::audit::action(format!("服务端：把 {} 移出列表", path));
                let key = path.to_lowercase();
                self.settings
                    .server_dirs
                    .retain(|dir| dir.to_lowercase() != key);
                self.persist_settings_quietly();
                self.servers.set_extra(&self.settings.server_dirs);
                self.servers.notice = Some((
                    "已移出列表，磁盘上的文件没动。".to_string(),
                    false,
                ));
            }
            Action::LoadLoaders { core, mc_version } => {
                aicraft_core::audit::action(format!(
                    "服务端：查 {} {} 的核心版本",
                    core.label(),
                    mc_version
                ));
                let (tx, rx) = mpsc::channel();
                self.server_rx = Some(rx);
                let mirror = match core {
                    CoreKind::Fabric => self.settings.fabric_mirror,
                    CoreKind::Forge => self.settings.forge_mirror,
                    CoreKind::NeoForge => self.settings.neoforge_mirror,
                    // 代理端不装，走不到这里；给个默认值把 match 补全。
                    CoreKind::Vanilla | CoreKind::Velocity => self.settings.mirror,
                };
                std::thread::spawn(move || {
                    let result = match core {
                        CoreKind::Vanilla | CoreKind::Velocity => Ok(Vec::new()),
                        core => aicraft_core::loader::list_versions(
                            core.loader_kind(),
                            &mc_version,
                            mirror,
                        )
                        .map(|list| list.into_iter().map(|v| v.version).collect()),
                    };
                    let message = match result {
                        Ok(list) => ServerMsg::Loaders {
                            core,
                            mc_version,
                            list,
                        },
                        Err(e) => ServerMsg::LoaderFailed(e),
                    };
                    let _ = tx.send(message);
                });
            }
            Action::Create(request) => {
                let mut plan = request.plan;
                let target = request.target;
                plan.mirror = self.settings.mirror;
                plan.limit_kbps = self.settings.download_limit_kbps;
                plan.memory_mb = plan.memory_mb.max(512);
                let where_to = match &target {
                    Some(dir) => dir.display().to_string(),
                    None => format!("servers/{}", plan.name),
                };
                aicraft_core::audit::action(format!(
                    "服务端：新建 {}（{} {}）",
                    where_to,
                    plan.core.label(),
                    plan.mc_version
                ));
                let name = plan.name.clone();
                self.servers.installing = Some(name);
                self.servers.progress = Some("准备中…".to_string());

                let (tx, rx) = mpsc::channel();
                self.server_rx = Some(rx);
                let cancel = aicraft_core::cancel::CancelToken::new();
                let progress_tx = tx.clone();
                std::thread::spawn(move || {
                    let on_progress = move |step: aicraft_core::server::Progress| {
                        let aicraft_core::server::Progress::Stage(text) = step;
                        let _ = progress_tx.send(ServerMsg::Stage(text));
                    };
                    let result = match &target {
                        Some(dir) => aicraft_core::server::create_at(
                            &plan,
                            dir,
                            &cancel,
                            &on_progress,
                            |line| logging::info(line),
                        ),
                        None => aicraft_core::server::create(
                            &plan,
                            &cancel,
                            &on_progress,
                            |line| logging::info(line),
                        ),
                    };
                    let _ = tx.send(ServerMsg::Created(Box::new(result)));
                });
            }
            Action::OpenFolder(dir) => {
                open_in_explorer(&dir);
            }
            Action::Remove(name) => {
                let Some(server) = self.servers.servers.iter().find(|s| s.name == name) else {
                    return;
                };
                aicraft_core::audit::action(format!("服务端：删除 {}", name));
                match aicraft_core::server::remove(server) {
                    Ok(()) => {
                        self.servers.reload();
                        self.servers.notice = Some((format!("servers/{} 已删除", name), false));
                    }
                    Err(e) => self.servers.notice = Some((e, true)),
                }
            }
            Action::SetEula { name, accepted } => {
                let Some(server) = self.servers.servers.iter().find(|s| s.name == name) else {
                    return;
                };
                aicraft_core::audit::action(format!(
                    "服务端：{} 的 EULA {}",
                    name,
                    if accepted { "同意" } else { "撤回" }
                ));
                match aicraft_core::server::set_eula(&server.dir, accepted) {
                    Ok(()) => self.servers.reload(),
                    Err(e) => self.servers.notice = Some((e, true)),
                }
            }
            Action::OpenConfig(name) => {
                aicraft_core::audit::action(format!("服务端：打开 {} 的配置", name));
                self.servers.open_config(&name);
            }
            Action::CloseConfig => {
                self.servers.editing = None;
            }
            Action::SaveConfig { name, props } => {
                aicraft_core::audit::action(format!("服务端：保存 {} 的配置", name));
                let Some(server) = self.servers.servers.iter().find(|s| s.name == name) else {
                    return;
                };
                match props.save(&server.properties_path()) {
                    Ok(()) => {
                        serverui::config_saved(&mut self.servers);
                        self.servers.reload();
                    }
                    Err(e) => serverui::config_failed(&mut self.servers, e),
                }
            }
        }
    }

    /// 收服务端那条通道的消息。
    fn poll_server(&mut self) {
        let Some(rx) = self.server_rx.take() else {
            return;
        };
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(ServerMsg::Loaders {
                    core,
                    mc_version,
                    list,
                }) => self.servers.set_loader_versions(core, mc_version, list),
                Ok(ServerMsg::LoaderFailed(message)) => {
                    self.servers.fail_loader_versions(message);
                    keep = false;
                }
                Ok(ServerMsg::Stage(text)) => self.servers.progress = Some(text),
                Ok(ServerMsg::Created(result)) => {
                    match *result {
                        Ok(server) => {
                            logging::info(format!("服务端 {} 装好了", server.name));
                            // 装进玩家自己挑的目录时，这个目录还不认识我们
                            // （没有 aicraft-server.json 之外任何线索也没关系，
                            // 但列表是按目录找的），得记进设置里才留得住。
                            if !server.managed
                                || !server.dir.starts_with(aicraft_core::server::root())
                            {
                                let path = server.dir.display().to_string();
                                if !self
                                    .settings
                                    .server_dirs
                                    .iter()
                                    .any(|dir| dir.to_lowercase() == path.to_lowercase())
                                {
                                    self.settings.server_dirs.push(path);
                                    self.persist_settings_quietly();
                                }
                                self.servers.set_extra(&self.settings.server_dirs);
                            }
                            self.servers.created(&server.name);
                        }
                        Err(e) => {
                            logging::error(format!("服务端安装失败: {}", e));
                            self.servers.failed(e);
                        }
                    }
                    keep = false;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.servers.installing.is_some() {
                        self.servers.failed("安装线程意外结束了".to_string());
                    }
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.server_rx = Some(rx);
        }
    }

    /// 设置页：把原来散在首页的开关都收进来。
    fn settings_page(&mut self, ui: &mut egui::Ui, busy: bool) {
        let before = self.settings.clone();

        ui.horizontal(|ui| {
            ui.label(RichText::new("设置").size(17.0).strong());
            ui.add_space(4.0);
            // 和「下载版本」页的刷新键长得一样、位置也一样：标题右边那个小图标。
            // 恢复默认会丢掉一整套偏好，所以点它不是立刻生效，而是先问一句。
            if theme::reset_button(ui, !busy)
                .on_hover_text("恢复默认设置")
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                self.reset_confirm = true;
            }
        });
        ui.add_space(8.0);

        let action = {
            // 按「显示」那一档筛过再交给设置页：`entries` 的 `min` 是
            // 「至少这么严重」，选「错误」就只剩错误行，选「详细」才是全部。
            let entries = logging::entries(Some(self.log_view));
            egui::ScrollArea::vertical()
                .id_salt("settings-page")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    settings::page(
                        ui,
                        &mut self.settings,
                        &mut self.log_view,
                        &entries,
                        &mut self.java_scan,
                        busy,
                    )
                })
                .inner
        };

        if let Some(action) = action {
            aicraft_core::audit::action(action.describe());
            match action {
                settings::Action::ExportLog => self.export_log(),
                settings::Action::ClearLog => {
                    logging::clear();
                    logging::info("日志已清空");
                }
                settings::Action::AddJava => self.pick_java(),
            }
        }

        if self.settings != before {
            // 设置控件直接改字段（见 `settings::page` 的注释），没有统一的
            // “提交”时刻，所以每帧比一次快照，把「哪一项从什么变成了什么」记下来。
            aicraft_core::audit::log_settings_changes(&before, &self.settings);
            self.persist_settings_quietly();
        }
    }

    /// 选中下标越界时当作没选（删除版本之后索引会变）。
    fn selected_timeout(&self) -> Option<usize> {
        self.selected.filter(|i| *i < self.versions.len())
    }

    /// 目录列表页：官方目录、当前目录、自己加过的目录，以及添加/切换/移除。
    fn dirs_page(&mut self, ui: &mut egui::Ui, busy: bool) {
        let current = self.game_dir.clone();
        let extra = self.settings.game_dirs.clone();
        let show_official = !self.settings.hide_official_dir;
        let action = dirs::page(ui, &current, &extra, show_official, busy);

        if let Some(action) = &action {
            aicraft_core::audit::action(action.describe());
        }
        match action {
            Some(dirs::Action::Switch(path)) => self.set_game_dir(path),
            Some(dirs::Action::Add) => self.pick_game_dir(),
            Some(dirs::Action::Remove(path)) => {
                self.settings
                    .game_dirs
                    .retain(|d| !d.eq_ignore_ascii_case(&path));
                self.persist_settings_quietly();
                logging::info(format!("已从目录列表移除 {}", path));
            }
            Some(dirs::Action::HideOfficial) => {
                self.settings.hide_official_dir = true;
                self.persist_settings_quietly();
                logging::info("已把官方目录从列表里移掉（磁盘上的东西没动）");
            }
            Some(dirs::Action::ShowOfficial) => {
                self.settings.hide_official_dir = false;
                self.persist_settings_quietly();
                logging::info("已把官方目录重新显示在列表里");
            }
            Some(dirs::Action::Rescan) => self.rescan(),
            None => {}
        }
    }

    /// 换到某个游戏目录：改 App 字段、存进设置、重新扫描。
    fn set_game_dir(&mut self, path: String) {
        if path == self.game_dir {
            return;
        }
        logging::info(format!("游戏目录切换为 {}", path));
        self.game_dir = path;
        self.settings.game_dir = self.game_dir.clone();
        self.persist_settings_quietly();
        self.rescan();
    }

    /// 「下载版本」页：和「版本列表」平级画在正文里。
    fn download_page(&mut self, ui: &mut egui::Ui, busy: bool) {
        // 第一次进这一页（包括用 AICRAFT_PAGE=download 直接落进来）就顺手拉清单
        if !self.version_mgr.open {
            self.version_mgr.open = true;
            if self.version_mgr.needs_fetch() && self.busy.is_none() {
                self.start_fetch_manifest();
            }
        }
        let busy_hint = self.busy.clone();
        let busy_arg = if busy { busy_hint.as_deref() } else { None };

        // 标题和筛选永远都在：哪怕是选加载器那一步，玩家也该看得见
        // 自己挑的是哪个版本、还能换个关键词找别的。
        let mut action = versions::header(ui, &mut self.version_mgr, busy_arg);

        // 第二步（选加载器）整块顶掉版本清单。不是接在清单底下——那样两边
        // 都挤，玩家还得把清单滚开才看得全卡片。清单这会儿也用不上：
        // 版本在第一步就挑定了。
        if self.wizard.as_ref().is_some_and(wizard::wants_inline) {
            self.wizard_card(ui, busy);
        } else if let Some(from_list) = versions::list(ui, &mut self.version_mgr, busy_arg) {
            action = action.or(Some(from_list));
        }

        if let Some(action) = &action {
            aicraft_core::audit::action(action.describe());
        }
        match action {
            Some(Action::Refresh) => self.start_fetch_manifest(),
            Some(Action::Install(official_id)) => self.open_wizard(official_id),
            Some(Action::ImportPack) => self.pick_pack(),
            None => {}
        }
    }

    /// 让玩家挑一个整合包文件。
    ///
    /// 对话框会阻塞主线程几十毫秒（PowerShell 起进程），和「导出日志」那边
    /// 是同一套做法——不值得为它再引入一个异步通道。
    fn pick_pack(&mut self) {
        if self.busy.is_some() {
            return;
        }
        let start = PathBuf::from(&self.game_dir);
        match pick_pack_blocking(&start) {
            Ok(Some(path)) => self.take_pack_file(path),
            Ok(None) => {}
            Err(e) => {
                logging::warn(format!("打开整合包选择框失败：{}", e));
                self.pack.fail(start, e);
            }
        }
    }

    /// 打开装版本的向导：先起名，再决定装不装加载器。
    fn open_wizard(&mut self, official_id: String) {
        if self.busy.is_some() {
            return;
        }
        aicraft_core::audit::action(format!("下载版本：开始安装 {}", official_id));
        self.wizard = Some(wizard::Wizard::new(&official_id));
    }

    /// 画向导，并把它的动作接上后台任务。
    ///
    /// 只有第一步（起名）走弹窗；第二步（加载器）是嵌在「下载版本」页里的
    /// 一张卡片，由 `download_page` 负责画，这里直接跳过。
    fn show_wizard(&mut self, ctx: &egui::Context, busy: bool) {
        let Some(mut state) = self.wizard.take() else {
            return;
        };
        if wizard::wants_inline(&state) {
            // 第二步交给页面自己画，别在这里把它变回弹窗。
            self.wizard = Some(state);
            return;
        }
        let game_dir = PathBuf::from(&self.game_dir);
        let outcome = wizard::show(ctx, &mut state, &game_dir, busy);
        match outcome {
            wizard::Outcome::Idle => self.wizard = Some(state),
            wizard::Outcome::Cancel => {
                logging::info("取消安装");
            }
            wizard::Outcome::Act(wizard::Action::Load { game_version, kind }) => {
                state.loading = true;
                state.load_error = None;
                self.start_fetch_loader_versions(game_version, kind);
                self.wizard = Some(state);
            }
            wizard::Outcome::Act(wizard::Action::Start {
                official_id,
                instance,
                loader,
            }) => {
                self.start_install(official_id, instance, loader);
            }
        }
    }

    /// 把向导第二步（加载器）的那张卡片画进下载页，并把动作接上。
    fn wizard_card(&mut self, ui: &mut egui::Ui, busy: bool) {
        let Some(mut state) = self.wizard.take() else {
            return;
        };
        if !wizard::wants_inline(&state) {
            self.wizard = Some(state);
            return;
        }
        ui.add_space(12.0);
        // 卡片把清单那一整块接手过来，就跟着撑满剩下的高度——不然它贴着
        // 标题浮在上半页，底下空一大片灰，看着像没画完。
        // 减去 12（上面的空）+ 30（底部呼吸），再扣掉卡片自己 14×2 的内边距。
        let page_h = ui.available_height();
        let outcome = theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height((page_h - 56.0).max(WIZARD_CARD_MIN_H));
            wizard::show_loader_card(ui, &mut state, busy)
        });
        let outcome = outcome.inner;
        match outcome {
            wizard::Outcome::Idle => self.wizard = Some(state),
            wizard::Outcome::Cancel => {
                logging::info("取消安装");
            }
            wizard::Outcome::Act(wizard::Action::Load { game_version, kind }) => {
                state.loading = true;
                state.load_error = None;
                self.start_fetch_loader_versions(game_version, kind);
                self.wizard = Some(state);
            }
            wizard::Outcome::Act(wizard::Action::Start {
                official_id,
                instance,
                loader,
            }) => {
                self.start_install(official_id, instance, loader);
            }
        }
    }

    /// 启动游戏。正版账号优先；没登录就用离线名字。
    fn start_launch(&mut self) {
        let Some(entry) = self.selected_version().cloned() else {
            return;
        };
        // 父版本（原版）不在盘上就别急着启动：链读不出来，报出来的只会是
        // 一句「系统找不到指定的路径」。先问一句「缺的是 26.3，装吗」。
        if self.blocked_by_missing_parent(&entry.name) {
            return;
        }
        // 没账号也没离线名字：直接开会把空名字塞给游戏，身份是空的。
        // 与其在日志里留一句看不懂的报错，不如把账户窗口递到眼前。
        if self.account.is_none() && self.username.trim().is_empty() {
            logging::warn("还没有配置账户：先设一个离线名字，或者登录正版。");
            self.open_account();
            return;
        }
        if self.launches_offline() && !self.offline_allowed() {
            logging::warn("还没有登录正版账户，离线账户现在用不了。");
            self.open_account();
            return;
        }
        let game_dir = PathBuf::from(&self.game_dir);
        let opts = LaunchOptions {
            // 自动模式下这里现算：设置页显示多少，启动时就给多少。
            // 用 `memory_mb()` 而不是 `max_memory_mb`，否则界面上写着
            // 「自动分配 4.0 GiB」、真跑起来还是旧的手动值。
            max_memory_mb: self.settings.memory_mb(),
            min_memory_mb: self.settings.min_memory_mb,
            extra_jvm_args: self.settings.extra_jvm_args(),
            java_path: self.settings.java_path.clone(),
        };
        let current = self.account.clone();
        let fallback_name = self.username.clone();
        let launcher_mode = self.settings.launcher_mode;
        let client_id = auth::DEFAULT_CLIENT_ID;

        // 设置里开着就一律隔离；关掉之后退回按目录内容判断，
        // 这样已经有自己存档的版本不会因为改了个开关就「看不见世界」。
        let detected = aicraft_core::detect_mode(&game_dir, &entry.name);
        let mode = if self.settings.isolate_instances {
            aicraft_core::InstanceMode::Isolated
        } else {
            detected
        };

        if mode == aicraft_core::InstanceMode::Isolated {
            // 把目录结构建出来，让用户立刻能看到它在哪
            let _ = aicraft_core::ensure_instance(&game_dir, &entry.name);
            if detected == aicraft_core::InstanceMode::Shared && game_dir.join("saves").is_dir() {
                // 这个版本以前是共享的。隔离之后它看不到根目录的存档——
                // 东西没被删，只是换了个地方找，得说一声。
                logging::info(format!(
                    "{} 这次按隔离模式启动：以前共享的存档还在 {}/saves 里，需要的话手动搬进 versions/{}/",
                    entry.name,
                    game_dir.display(),
                    entry.name
                ));
            }
        }

        self.spawn_task(format!("正在启动 {}", entry.name), move |tx| {
            // 正版会话过期就在后台续一次，别让过期令牌进游戏。
            // 续期要连微软，慢的时候能好几秒——先报一步，别让面板空转。
            report_launch_step(&tx, 1, "检查正版会话…");
            let (account, note, renewed) =
                prepare_launch_account(current.as_ref(), &fallback_name, client_id);
            if let Some(note) = note {
                let _ = tx.send(TaskMsg::Log(note));
            }
            if let Some(account) = renewed {
                let _ = tx.send(TaskMsg::Account(account));
            }
            do_launch(&game_dir, &entry, &account, &opts, mode, launcher_mode, &tx)
        });
    }

    fn start_verify(&mut self) {
        let Some(entry) = self.selected_version().cloned() else {
            return;
        };
        if self.blocked_by_missing_parent(&entry.name) {
            return;
        }
        let game_dir = PathBuf::from(&self.game_dir);

        self.spawn_task(format!("正在校验 {}", entry.name), move |tx| {
            do_verify(&game_dir, &entry, &tx)
        });
    }

    /// 这个版本缺父版本吗？缺就记下来、摆出「要不要现在装上」，并返回 true。
    ///
    /// 直接拿去启动只会得到一句「读取 …26.3.json 失败：系统找不到指定的路径」，
    /// 玩家还以为是自己装坏了。缺什么就说缺什么，顺手给一个按钮。
    fn blocked_by_missing_parent(&mut self, instance: &str) -> bool {
        let game_dir = PathBuf::from(&self.game_dir);
        let Some(parent) = aicraft_core::version::missing_parent(&game_dir, instance) else {
            return false;
        };
        logging::warn(format!(
            "{} 挂在 {} 上，但 {} 没装：先装上父版本才能启动",
            instance, parent, parent
        ));
        self.parent_prompt = Some((instance.to_string(), parent));
        true
    }

    /// 把缺的那个父版本（原版）装上。
    fn install_missing_parent(&mut self, parent: String) {
        if self.busy.is_some() {
            return;
        }
        let game_dir = PathBuf::from(&self.game_dir);
        let mirror = self.settings.mirror;
        let concurrency = self.settings.concurrency;
        let limit_kbps = self.settings.download_limit_kbps;

        self.download_progress = Some((0, 0, 0));
        self.speed_bps = 0.0;
        self.last_tick_at = None;
        self.last_tick_bytes = 0;
        let cancel = aicraft_core::cancel::CancelToken::new();
        self.cancel_token = Some(cancel.clone());
        self.cancel_confirm = false;
        self.cancel_requested = false;
        // 取消时把这次装出来的目录一起删掉，别留半份原版。
        self.install_created = vec![parent.clone()];

        let label = format!("正在安装 {}", parent);
        self.spawn_task(label, move |tx| {
            do_install(
                &game_dir,
                &parent,
                &parent,
                &mirror,
                concurrency,
                limit_kbps,
                &cancel,
                &tx,
            )?;
            // `do_install` 末尾自己会发 `Installed`，这里补一句人话：
            // 刚才是为了修「缺底座」才装的，装完该说一声现在能启动了。
            let _ = tx.send(TaskMsg::Log(format!(
                "{} 装好了，回到主页再点一次「启动游戏」就能进。",
                parent
            )));
            Ok(())
        });
    }

    /// 缺父版本时的询问：要不要现在把它装上。
    fn missing_parent_modal(&mut self, ctx: &egui::Context, busy: bool) {
        let Some((instance, parent)) = self.parent_prompt.clone() else {
            return;
        };
        let mut confirmed = false;
        let mut dismissed = false;

        let response = egui::Modal::new(egui::Id::unique("aicraft-missing-parent"))
            .frame(theme::dialog_frame())
            .show(ctx, |ui| {
                ui.set_width(400.0);
                ui.label(RichText::new("这个版本缺了它的底座").size(15.0).strong());
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "{} 是装在 {} 上面的一层加载器，启动要靠 {} 自己的清单、库和素材。",
                        instance, parent, parent
                    ))
                    .size(11.5)
                    .color(theme::TEXT_WEAK),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "现在 versions/{}/ 里没有这份清单，所以只能走到一半就停下。",
                        parent
                    ))
                    .size(11.5)
                    .color(theme::WARN),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if theme::dialog_button(ui, "以后再说", !busy, 92.0).clicked() {
                        dismissed = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::dialog_primary_button(
                            ui,
                            &format!("装上 {}", parent),
                            !busy,
                            120.0,
                        )
                        .clicked()
                        {
                            confirmed = true;
                        }
                    });
                });
            });

        if response.should_close() {
            dismissed = true;
        }
        if confirmed {
            aicraft_core::audit::action(format!("缺底座：确认安装 {}", parent));
            self.parent_prompt = None;
            self.install_missing_parent(parent);
        } else if dismissed {
            self.parent_prompt = None;
        }
    }

    /// 校验并补全缺失文件（下载）。
    ///
    /// 复用 `do_verify` 的校验逻辑，再对发现的问题调用 core 的下载器。
    /// 进度通过 channel 回传，主线程据此刷新进度条。
    fn start_repair(&mut self) {
        let Some(entry) = self.selected_version().cloned() else {
            return;
        };
        let game_dir = PathBuf::from(&self.game_dir);
        let mirror = self.settings.mirror;
        let concurrency = self.settings.concurrency;
        let limit_kbps = self.settings.download_limit_kbps;
        self.download_progress = Some((0, 0, 0));

        self.spawn_task(format!("正在补全 {}", entry.name), move |tx| {
            do_repair(&game_dir, &entry, &mirror, concurrency, limit_kbps, &tx)
        });
    }

    /// 拉取官方版本清单（可安装的版本）。
    ///
    /// 一次拿全量（正式版 + 快照 + 远古版，约一千条），交给这一页按需筛选；
    /// 只保留界面要展示的字段，别把整份 JSON 留在内存里。
    ///
    /// **这条通道刻意不走 `spawn_task`。** `spawn_task` 会占住全局 `busy`，
    /// 而界面里所有动作按钮（启动游戏、校验资源、管理……）都是拿 `!busy` 决定
    /// 灰不灰的 —— 拉清单既不写盘也不碰游戏目录，凭什么让整台启动器停手。
    /// 它自己的进行中状态由 `version_mgr.fetching` 表示，只影响这一页。
    fn start_fetch_manifest(&mut self) {
        if self.version_mgr.fetching {
            return;
        }
        self.version_mgr.begin_fetch();
        let mirror = self.settings.mirror;
        let (tx, rx) = mpsc::channel();
        self.manifest_rx = Some(rx);
        std::thread::spawn(move || {
            let result = aicraft_core::transport::fetch_text(
                &mirror.rewrite(aicraft_core::install::VERSION_MANIFEST_URL),
            )
            .map_err(|e| format!("获取版本清单失败: {}", e))
            .and_then(|text| {
                aicraft_core::install::parse_manifest(&text)
                    .map_err(|e| format!("解析版本清单失败: {}", e))
            })
            .map(|manifest| {
                (
                    manifest
                        .versions
                        .iter()
                        .map(|v| versions::Entry {
                            id: v.id.clone(),
                            kind: v.version_type.clone(),
                            time: v.release_time.clone(),
                        })
                        .collect::<Vec<_>>(),
                    manifest.latest_release.clone(),
                )
            });
            let _ = tx.send(match result {
                Ok((releases, latest)) => {
                    let latest = latest.unwrap_or_else(|| "?".to_string());
                    logging::info(format!(
                        "可安装版本 {} 个（最新正式版 {}）",
                        releases.len(),
                        latest
                    ));
                    Ok(releases)
                }
                Err(e) => {
                    logging::warn(e.clone());
                    Err(e)
                }
            });
        });
    }

    /// 收拉清单的结果。成功就换上新列表，失败就记日志并把「获取中」收回来。
    fn poll_manifest(&mut self) {
        let Some(rx) = self.manifest_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(releases)) => {
                self.version_mgr.set_entries(releases);
                self.manifest_rx = None;
            }
            Ok(Err(e)) => {
                logging::warn(e);
                self.version_mgr.fail_fetch();
                self.manifest_rx = None;
            }
            Err(TryRecvError::Empty) => self.manifest_rx = Some(rx),
            Err(TryRecvError::Disconnected) => {
                // 线程没送回任何东西就没了（panic）。别让「获取中」永远转下去。
                logging::warn("获取版本清单的后台线程意外结束");
                self.version_mgr.fail_fetch();
                self.manifest_rx = None;
            }
        }
    }

    /// 向 Modrinth 搜一次模组。
    ///
    /// 和拉官方清单一样单开一条通道：这个请求被玩家连点（换筛选、翻页），
    /// 挂到全局 `busy` 上会让「启动游戏」跟着变灰。
    fn start_search_mods(&mut self) {
        if self.modrinth_rx.is_some() {
            return;
        }
        self.resources.loading = true;
        self.resources.error = None;
        let spec = self.resources.query_spec();
        self.resources.searching = Some(spec.kind);
        let (tx, rx) = mpsc::channel();
        self.modrinth_rx = Some(rx);
        std::thread::spawn(move || {
            let result = aicraft_core::transport::fetch_text(&spec.url())
                .map_err(|e| format!("连不上 Modrinth: {}", e))
                .and_then(|text| aicraft_core::modrinth::parse_search(&text))
                .inspect_err(|e| logging::warn(e.clone()));
            let _ = tx.send(result);
        });
    }

    /// 收搜索的结果。
    fn poll_modrinth(&mut self) {
        let Some(rx) = self.modrinth_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok((hits, total))) => {
                self.resources.loading = false;
                // 迟到的旧结果：中途换了看的东西（模组 → 光影包），这一份
                // 已经不该显示了。丢掉并立刻按新条件重搜一次。
                if self.resources.searching != Some(self.resources.kind) {
                    self.resources.searching = None;
                    if self.resources.needs_fetch() {
                        self.start_search_mods();
                    }
                    return;
                }
                logging::info(format!(
                    "Modrinth 命中 {} 个{}",
                    total,
                    self.resources.kind.unit()
                ));
                self.resources.searched = self.resources.searching;
                self.resources.hits = hits;
                self.resources.total = total;
                self.resources.error = None;
            }
            Ok(Err(e)) => {
                self.resources.loading = false;
                // 出错也算「这一轮结束了」，否则每帧都会自动重试，
                // 断网时等于对着服务端连打
                self.resources.searched = self.resources.searching;
                self.resources.hits.clear();
                self.resources.total = 0;
                self.resources.error = Some(e);
            }
            Err(TryRecvError::Empty) => self.modrinth_rx = Some(rx),
            Err(TryRecvError::Disconnected) => {
                logging::warn("搜索模组的后台线程意外结束");
                self.resources.loading = false;
                self.resources.error = Some("搜索中断了，点刷新再试一次".to_string());
            }
        }
    }

    /// 打开某个模组的详情页：详情与版本分两路取，谁先到先显示谁。
    fn open_mod_detail(&mut self, project: String, title: String) {
        self.resources.detail = Some(resources::DetailView::new(project.clone(), title));
        let (tx, rx) = mpsc::channel();
        self.detail_rx = Some(rx);
        // 版本列表要按「现在看的是哪一类」过扩展名：模组收 jar，
        // 资源包与光影包收 zip。
        let kind = self.resources.kind;
        std::thread::spawn(move || {
            let detail = aicraft_core::modrinth::fetch_detail(&project)
                .and_then(|text| aicraft_core::modrinth::parse_detail(&text))
                .inspect_err(|e| logging::warn(e.clone()));
            let _ = tx.send(DetailMsg::Info(Box::new(detail)));

            let versions = aicraft_core::modrinth::fetch_versions(&project)
                .and_then(|text| aicraft_core::modrinth::parse_versions(&text, kind))
                .inspect_err(|e| logging::warn(e.clone()));
            let _ = tx.send(DetailMsg::Versions(versions));
        });
    }

    /// 收详情页的两路结果。
    fn poll_mod_detail(&mut self) {
        let Some(rx) = self.detail_rx.take() else {
            return;
        };
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(DetailMsg::Info(inner)) => match *inner {
                    Ok(detail) => {
                        if let Some(view) = self.resources.detail.as_mut() {
                            if view.project == detail.id {
                                view.detail = Some(detail);
                            }
                        }
                    }
                    Err(e) => {
                        if let Some(view) = self.resources.detail.as_mut() {
                            view.error = Some(e);
                        }
                    }
                },
                Ok(DetailMsg::Versions(Ok(versions))) => {
                    if let Some(view) = self.resources.detail.as_mut() {
                        view.versions = Some(versions);
                    }
                }
                Ok(DetailMsg::Versions(Err(e))) => {
                    if let Some(view) = self.resources.detail.as_mut() {
                        view.error = Some(e);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.detail_rx = Some(rx);
        } else {
            // 线程没了还没送到，别让详情页永远转圈
            if let Some(view) = self.resources.detail.as_mut() {
                if view.detail.is_none() {
                    view.error = Some("取模组详情时连接断了，返回再进一次".to_string());
                }
            }
        }
    }

    /// 装详情页里选中的那一版（版本由玩家指定，不再按加载器猜）。
    fn start_install_file(&mut self, file: aicraft_core::modrinth::ModFile) {
        if self.busy.is_some() {
            return;
        }
        let Some(target) = self.resources.target.clone() else {
            self.resources.notice = Some(("还没有可用的实例，先把游戏版本装好".to_string(), false));
            return;
        };
        let game_dir = PathBuf::from(&self.game_dir);
        let filename = file.filename.clone();
        let kind = self.resources.kind;
        let label = format!("正在下载{} {}", kind.unit(), filename);
        aicraft_core::audit::action(format!(
            "资源下载：把 {} 装进 {} 的 {}",
            filename,
            target,
            kind.dir()
        ));
        self.spawn_task(label, move |tx| {
            let _ = tx.send(TaskMsg::Log(format!(
                "从 Modrinth 下载 {} 到 {}…",
                filename, target
            )));
            match aicraft_core::modrinth::install_file(&game_dir, &target, kind, &file) {
                Ok(dest) => {
                    let _ = tx.send(TaskMsg::Log(format!("已放进 {}", dest.display())));
                    let _ = tx.send(TaskMsg::ModInstalled(Ok(format!(
                        "{} 装好了",
                        dest.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("mod.jar")
                    ))));
                }
                Err(e) => {
                    let _ = tx.send(TaskMsg::Log(e.clone()));
                    let _ = tx.send(TaskMsg::ModInstalled(Err(e)));
                }
            }
            Ok(())
        });
    }

    /// 收整合包清单的读取结果。
    fn poll_pack(&mut self) {
        let Some(rx) = self.pack_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok((path, Ok(manifest))) => {
                logging::info(format!("整合包清单：{}", manifest.summary()));
                self.pack.ready(path, manifest);
            }
            Ok((path, Err(error))) => {
                logging::warn(format!("读整合包失败：{}", error));
                self.pack.fail(path, error);
            }
            Err(TryRecvError::Empty) => self.pack_rx = Some(rx),
            Err(TryRecvError::Disconnected) => {
                logging::warn("读整合包的线程意外结束");
                if let Some(path) = self.pack.reading.clone() {
                    self.pack.fail(path, "读整合包时后台线程退出了".to_string());
                }
            }
        }
    }

    /// 收到一个要导入的文件：非整合包就当场说清楚，是的话起线程读清单。
    ///
    /// 读 zip 要几百毫秒，放主线程会让窗口僵一下——而拖拽正好是「松手之后
    /// 期待马上有反应」的动作，卡住的感觉最明显。
    fn take_pack_file(&mut self, path: PathBuf) {
        if self.busy.is_some() {
            self.pack
                .fail(path, "现在有别的事在做，等它跑完再导入".to_string());
            return;
        }
        if !packui::looks_like_pack(&path) {
            self.pack.fail(
                path,
                "只认 .mrpack 和 .zip。别的文件即使改了后缀也装不了。".to_string(),
            );
            return;
        }
        if !self.pack.begin(path.clone()) {
            return;
        }
        aicraft_core::audit::action(format!(
            "整合包：读取 {}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("整合包")
        ));
        self.pack_rx = Some(packui::spawn_read(path));
    }

    /// 整合包确认之后真正开装。
    fn start_pack_import(&mut self, instance: String) {
        let Some(packui::Stage::Ready(ready)) = self.pack.stage.take() else {
            return;
        };
        let packui::Ready {
            archive, manifest, ..
        } = *ready;
        if self.busy.is_some() {
            self.pack.close();
            return;
        }

        let game_dir = PathBuf::from(&self.game_dir);
        let mirror = self.settings.mirror;
        let loader_mirror = match manifest.loader.as_ref().map(|(kind, _)| *kind) {
            Some(aicraft_core::loader::LoaderKind::Fabric) => self.settings.fabric_mirror,
            Some(aicraft_core::loader::LoaderKind::Forge) => self.settings.forge_mirror,
            Some(aicraft_core::loader::LoaderKind::NeoForge) => self.settings.neoforge_mirror,
            None => mirror,
        };
        let concurrency = self.settings.concurrency;
        let limit_kbps = self.settings.download_limit_kbps;
        let cancel = aicraft_core::cancel::CancelToken::new();
        self.cancel_token = Some(cancel.clone());
        self.cancel_confirm = false;
        self.cancel_requested = false;
        self.install_created = vec![instance.clone()];
        self.download_progress = None;
        self.loader_phase = None;
        self.launch_phase = None;
        self.speed_bps = 0.0;
        self.last_tick_at = None;
        self.last_tick_bytes = 0;

        let summary = manifest.summary();
        aicraft_core::audit::action(format!("整合包：开始导入 {} → {}", summary, instance));
        let label = format!("正在导入整合包 {}", manifest.name);
        self.spawn_task(label, move |tx| {
            let send_line = |line: &str| {
                let _ = tx.send(TaskMsg::Log(line.to_string()));
            };
            send_line(&format!("导入整合包：{}", summary));
            send_line(&format!("装成实例 {}", instance));

            let tx_progress = tx.clone();
            let on_progress = move |event: aicraft_core::modpack::PackProgress| {
                let msg = match event {
                    aicraft_core::modpack::PackProgress::Stage(text) => TaskMsg::LoaderPhase(text),
                    aicraft_core::modpack::PackProgress::Install { done, total, bytes } => {
                        TaskMsg::Progress { done, total, bytes }
                    }
                    aicraft_core::modpack::PackProgress::Pack { done, total } => {
                        TaskMsg::Progress {
                            done,
                            total,
                            bytes: 0,
                        }
                    }
                    aicraft_core::modpack::PackProgress::Loader(event) => match event {
                        aicraft_core::loader::LoaderProgress::Tick { done, total, bytes } => {
                            TaskMsg::LoaderTick { done, total, bytes }
                        }
                        aicraft_core::loader::LoaderProgress::Working(text) => {
                            TaskMsg::LoaderPhase(text)
                        }
                    },
                };
                let _ = tx_progress.send(msg);
            };

            let config = aicraft_core::modpack::ImportConfig {
                game_dir: &game_dir,
                instance: &instance,
                mirror,
                loader_mirror,
                concurrency,
                limit_kbps,
                cancel: &cancel,
            };
            let send = send_line;
            let report = aicraft_core::modpack::import_pack(
                &archive,
                &config,
                &on_progress,
                |line: &str| send(line),
            )?;

            if report.cancelled {
                send("导入被取消了");
                let _ = tx.send(TaskMsg::Finished(Ok(())));
                return Ok(());
            }
            send(&format!(
                "完成：{} 个文件在包里，{} 个从网上下载",
                report.overrides, report.downloaded
            ));
            for (path, reason) in &report.failures {
                send(&format!("没下成 {}：{}", path, reason));
            }
            let _ = tx.send(TaskMsg::Installed);
            Ok(())
        });
    }

    /// 「资源下载」页。
    fn resources_page(&mut self, ui: &mut egui::Ui, busy: bool) {
        if !self.resources.open {
            self.resources.open = true;
            if self.resources.needs_fetch() {
                self.start_search_mods();
            }
        }
        // 二级目录里切了类别之后，新的一类是空的，进来立刻补一次
        if self.resources.needs_fetch() {
            self.start_search_mods();
        }
        let game_dir = PathBuf::from(&self.game_dir);
        let instances = aicraft_core::install::installed_instances(&game_dir);
        // 详情页要知道「要装进去的那个实例是什么游戏版本」，好把能装的版本
        // 排到前面打上标记。target 要到第一次 `pick_target` 之后才定下来，
        // 所以在这里补，而不是开详情页那一刻。
        if let Some(detail) = self.resources.detail.as_mut() {
            if detail.game_hint.is_none() {
                if let Some(target) = self.resources.target.clone() {
                    detail.game_hint =
                        aicraft_core::install::instance_game_version(&game_dir, &target);
                    // 置顶要「游戏版本 + 加载器」都对得上：Fabric 实例别把
                    // Forge 版顶到最上面。原版实例没有加载器，留 `None`
                    // 让 `matches_loader` 一视同仁。
                    //
                    // 资源包与光影包不挂加载器——它们不是「给哪个加载器用的」，
                    // 把实例的加载器套上去会把所有版本都筛掉，一条也置顶不了。
                    detail.loader_hint = if self.resources.kind.has_loader() {
                        aicraft_core::loader::detect_installed(&game_dir, &target)
                            .map(|(kind, _)| kind)
                    } else {
                        None
                    };
                }
            }
        }
        let busy_hint = self.busy.clone();
        let busy_arg = if busy { busy_hint.as_deref() } else { None };
        let action = resources::page(ui, &mut self.resources, &instances, busy_arg);
        if let Some(action) = &action {
            aicraft_core::audit::action(action.describe());
        }
        match action {
            Some(resources::Action::Search(_)) => self.start_search_mods(),
            Some(resources::Action::OpenDetail(project, title)) => {
                self.open_mod_detail(project, title)
            }
            Some(resources::Action::CloseDetail) => {
                self.resources.detail = None;
            }
            Some(resources::Action::InstallFile(file)) => self.start_install_file(file),
            Some(resources::Action::OpenUrl(url)) => open_external_url(&url),
            None => {}
        }
        // 这一帧刚登记了缺图标：下一帧 `logic` 才去取，得先把界面叫醒。
        if self.resources.icons.has_pending() {
            ui.ctx().request_repaint();
        }
    }

    /// 下载并安装指定官方版本。
    ///
    /// 「下载版本」页只给官方版本号，落盘目录名在这里现算：同名目录已经
    /// 存在就自动往后排（`26.2` → `26.2-2` …），所以同一版本可以装很多份，
    /// 而界面上不需要为此多一个输入框。
    fn start_install(
        &mut self,
        official_id: String,
        instance: String,
        loader: Option<wizard::Choice>,
    ) {
        if self.busy.is_some() {
            return;
        }
        let game_dir = PathBuf::from(&self.game_dir);
        let mirror = self.settings.mirror;
        let concurrency = self.settings.concurrency;
        let limit_kbps = self.settings.download_limit_kbps;
        let loader_mirror = loader.as_ref().map(|c| self.loader_mirror(c.kind));
        aicraft_core::audit::action(match &loader {
            Some(c) => format!(
                "安装：{} 装成 {}（{} {}）",
                official_id,
                instance,
                c.kind.label(),
                c.version
            ),
            None => format!("安装：{} 装成 {}", official_id, instance),
        });

        // 选了加载器时，原版必须以官方名装好：Forge/NeoForge 的清单里写着
        // `inheritsFrom: 1.20.1`，父版本不在就启动不了；Fabric 同理。
        // 玩家起的名字留给加载器那一层。
        let base_instance = if loader.is_some() {
            official_id.clone()
        } else {
            instance.clone()
        };

        self.download_progress = Some((0, 0, 0));
        self.version_mgr.installing = Some(instance.clone());

        // 进度弹窗与取消都靠这一组字段。令牌先建好，闭包里拿一份克隆。
        let cancel = aicraft_core::cancel::CancelToken::new();
        self.cancel_token = Some(cancel.clone());
        self.cancel_confirm = false;
        self.cancel_requested = false;
        self.speed_bps = 0.0;
        self.last_tick_at = None;
        self.last_tick_bytes = 0;
        self.install_created.clear();
        let needs_base = !aicraft_core::install::is_installed(&game_dir, &base_instance);
        if needs_base {
            self.install_created.push(base_instance.clone());
        }

        let label = match &loader {
            Some(choice) => format!(
                "正在安装 {}（{} {}）",
                instance,
                choice.kind.label(),
                choice.version
            ),
            None if instance == official_id => format!("正在安装 {}", official_id),
            None => format!("正在安装 {}（官方 {}）", instance, official_id),
        };

        self.spawn_task(label, move |tx| {
            let send = |msg: String| {
                let _ = tx.send(TaskMsg::Log(msg));
            };
            // 加载器阶段的进度走另一条通道：它有「能算比例」和「算不出来」
            // 两种状态，混进 TaskMsg::Progress 会让底部进度条一直卡在 100%。
            let send_progress = |ev: aicraft_core::loader::LoaderProgress| {
                let msg = match ev {
                    aicraft_core::loader::LoaderProgress::Tick { done, total, bytes } => {
                        TaskMsg::LoaderTick { done, total, bytes }
                    }
                    aicraft_core::loader::LoaderProgress::Working(text) => {
                        TaskMsg::LoaderPhase(text)
                    }
                };
                let _ = tx.send(msg);
            };
            let mut send_line = |line: &str| {
                let _ = tx.send(TaskMsg::Log(line.to_string()));
            };

            // 第一步：原版。选了加载器时它已经装过就不必再下 600 MB。
            if needs_base {
                do_install(
                    &game_dir,
                    &official_id,
                    &base_instance,
                    &mirror,
                    concurrency,
                    limit_kbps,
                    &cancel,
                    &tx,
                )?;
            } else {
                send(format!("原版 {} 已经就位，跳过", base_instance));
            }

            // 第二步：加载器（可选）
            if let Some(choice) = loader {
                let lm = loader_mirror.unwrap_or(mirror);
                send(format!(
                    "开始装 {} {}（源：{}）",
                    choice.kind.label(),
                    choice.version,
                    lm.label()
                ));
                let outcome = match choice.kind {
                    aicraft_core::loader::LoaderKind::Fabric => {
                        aicraft_core::loader::install_fabric(
                            &game_dir,
                            &official_id,
                            &choice.version,
                            &lm,
                            concurrency,
                            Some(&instance),
                            &cancel,
                            &send_progress,
                            &mut send_line,
                        )
                    }
                    aicraft_core::loader::LoaderKind::Forge => aicraft_core::loader::install_forge(
                        &game_dir,
                        &official_id,
                        &choice.version,
                        lm,
                        Some(&instance),
                        &cancel,
                        &send_progress,
                        &mut send_line,
                    ),
                    aicraft_core::loader::LoaderKind::NeoForge => {
                        aicraft_core::loader::install_neoforge(
                            &game_dir,
                            &choice.version,
                            lm,
                            Some(&instance),
                            &cancel,
                            &send_progress,
                            &mut send_line,
                        )
                    }
                };
                let outcome = outcome?;
                send(format!(
                    "{} 装好了：versions/{}",
                    choice.kind.label(),
                    outcome.instance
                ));

                if choice.with_fabric_api && choice.kind == aicraft_core::loader::LoaderKind::Fabric
                {
                    match aicraft_core::loader::install_fabric_api(
                        &game_dir,
                        &outcome.instance,
                        &official_id,
                        &mut send_line,
                    ) {
                        Ok(path) => send(format!("Fabric API 也放好了：{}", path.display())),
                        Err(e) => send(format!(
                            "Fabric API 没装上，不影响启动，之后可以手动放进 mods/：{}",
                            e
                        )),
                    }
                }
            }

            let _ = tx.send(TaskMsg::Installed);
            Ok(())
        });
    }

    /// 某个加载器该用哪个源。
    ///
    /// 三种加载器的源是分开选的：它们的端点、镜像覆盖度、失败方式都不一样，
    /// 用一个全局开关绑在一起反而会互相拖累（比如 Fabric 镜像很稳，NeoForge
    /// 的 maven 镜像时不时抽风）。
    fn loader_mirror(&self, kind: aicraft_core::loader::LoaderKind) -> aicraft_core::Mirror {
        match kind {
            aicraft_core::loader::LoaderKind::Fabric => self.settings.fabric_mirror,
            aicraft_core::loader::LoaderKind::Forge => self.settings.forge_mirror,
            aicraft_core::loader::LoaderKind::NeoForge => self.settings.neoforge_mirror,
        }
    }

    /// 拉某个加载器对某个游戏版本的可用版本列表。
    fn start_fetch_loader_versions(
        &mut self,
        game_version: String,
        kind: aicraft_core::loader::LoaderKind,
    ) {
        if self.busy.is_some() {
            return;
        }
        aicraft_core::audit::action(format!(
            "安装向导：给 {} 挑 {} 版本",
            game_version,
            kind.label()
        ));
        let gv = game_version.clone();
        let loader_mirror = self.loader_mirror(kind);
        self.spawn_task(
            format!("正在获取 {} 的 {} 版本", game_version, kind.label()),
            move |tx| {
                match aicraft_core::loader::list_versions(kind, &gv, loader_mirror) {
                    Ok(list) => {
                        let _ = tx.send(TaskMsg::Log(format!(
                            "{} 有 {} 个 {} 版本",
                            gv,
                            list.len(),
                            kind.label()
                        )));
                        let _ = tx.send(TaskMsg::LoaderVersions {
                            game_version: gv,
                            kind,
                            list,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(TaskMsg::LoaderFailed(e));
                    }
                }
                Ok(())
            },
        );
    }

    /// 切到「下载版本」页；清单还没拉过就顺手拉一次。
    ///
    /// 以前这里是打开一个独立窗口，现在改成页面：和「版本列表」共用右边那片区域，
    /// 切换时视线不用跟着窗口跑。
    fn open_versions(&mut self) {
        self.page = Page::Download;
        self.version_mgr.open = true;
        if self.version_mgr.needs_fetch() && self.busy.is_none() {
            self.start_fetch_manifest();
        }
    }

    /// 删除一个已安装版本。
    ///
    /// 删的是 `versions/<id>` 这一整层。隔离实例的存档就在里面，所以确认按钮上
    /// 会写明白「存档会一起走」；但不再一刀切禁止删除——默认开隔离之后，任何
    /// 版本跑过一次就带上实例标记，禁止删除等于让「删除版本」形同虚设。
    fn delete_version(&mut self, version_id: String) {
        let game_dir = PathBuf::from(&self.game_dir);
        let isolated = aicraft_core::detect_mode(&game_dir, &version_id)
            == aicraft_core::InstanceMode::Isolated;
        let dir = aicraft_core::instance::version_dir(&game_dir, &version_id);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                if isolated {
                    logging::warn(format!(
                        "已删除 {}（隔离实例，versions/{}/ 里的存档、模组、配置也一起删了）",
                        version_id, version_id
                    ));
                } else {
                    logging::info(format!("已删除 {}（{}）", version_id, dir.display()));
                }
                self.rescan();
            }
            Err(e) => logging::warn(format!("删除 {} 失败: {}", version_id, e)),
        }
    }

    /// 取消之后把这次安装留下的东西清掉。
    ///
    /// 只删「这一次产生的」：本次新建的实例目录，以及下载途中留下的 `.part`
    /// 半截文件。已经下完整的库与资源留着 —— 它们按内容寻址，下次装同一个
    /// 版本会直接跳过，删了反而白下。
    fn drop_install_leftovers(&mut self) {
        let game_dir = PathBuf::from(self.game_dir.trim());
        let mut removed = 0usize;
        for name in std::mem::take(&mut self.install_created) {
            let dir = game_dir.join("versions").join(&name);
            if dir.is_dir() && std::fs::remove_dir_all(&dir).is_ok() {
                removed += 1;
            }
        }
        removed += remove_part_files(&game_dir);
        if removed > 0 {
            logging::info(format!("取消安装：删掉了 {} 处这次下载留下的东西", removed));
        } else {
            logging::info("取消安装：没有找到需要清理的文件");
        }
    }

    /// 进入「管理」页。
    fn open_manage(&mut self, version: &str) {
        if self.busy.is_some() {
            return;
        }
        self.manage = Some(manage::State::new(Path::new(&self.game_dir), version));
        logging::info(format!("打开 {} 的管理页", version));
    }

    /// 管理页交回来的动作。
    /// 按文件指纹去 Modrinth 问一遍哪些模组有新版本。
    ///
    /// 这一步只读、不改盘，所以可以放心跑；结果收进 `manage.updates`，
    /// 界面上每一行才决定要不要亮「更新」按钮。
    fn start_check_updates(&mut self) {
        let Some(state) = self.manage.as_mut() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let files: Vec<PathBuf> = state.mods.iter().map(|m| m.path.clone()).collect();
        if files.is_empty() {
            return;
        }
        // 只问「这份实例能装的那些」：Fabric 实例不该被建议装 Forge 版
        let loaders: Vec<String> = state
            .loader
            .as_ref()
            .map(|(kind, _)| vec![kind.id().to_string()])
            .unwrap_or_default();
        let game_version = state.mc_version.clone();
        state.update_busy = Some("正在检查更新…".to_string());
        state.update_notice = None;
        logging::info(format!(
            "检查 {} 个模组的更新（{} / {}）",
            files.len(),
            game_version,
            loaders.join(",")
        ));
        self.spawn_task("检查模组更新".to_string(), move |tx| {
            let checks =
                aicraft_core::modupdate::scan(&files, &loaders, &game_version, |_, _, _| {});
            let _ = tx.send(TaskMsg::ModUpdates(checks));
            Ok(())
        });
    }

    /// 更新一个（`Some`）或者全部（`None`）。
    fn start_update_mods(&mut self, only: Option<PathBuf>) {
        let Some(state) = self.manage.as_mut() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let picks: Vec<aicraft_core::modupdate::Check> = match &only {
            Some(path) => state
                .updates
                .iter()
                .filter(|c| &c.path == path)
                .cloned()
                .collect(),
            None => state.updates.clone(),
        };
        if picks.is_empty() {
            return;
        }
        let total = picks.len();
        state.update_busy = Some(if total == 1 {
            "正在更新 1 个模组…".to_string()
        } else {
            format!("正在更新 {} 个模组…", total)
        });
        state.update_notice = None;
        self.spawn_task(format!("更新 {} 个模组", total), move |tx| {
            let mut done = 0usize;
            let mut failed: Vec<String> = Vec::new();
            for check in picks {
                let Some(latest) = check.latest.as_ref() else {
                    continue;
                };
                match aicraft_core::modupdate::replace(&check.path, latest) {
                    Ok(dest) => {
                        done += 1;
                        let _ = tx.send(TaskMsg::Log(format!(
                            "{} → {}",
                            check.filename,
                            dest.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                        )));
                    }
                    Err(e) => failed.push(format!("{}: {}", check.filename, e)),
                }
            }
            let _ = tx.send(TaskMsg::ModUpdated { done, failed });
            Ok(())
        });
    }

    fn handle_manage(&mut self, action: manage::Action) {
        aicraft_core::audit::action(action.describe());
        match action {
            manage::Action::Close => {
                self.manage = None;
            }
            manage::Action::Reload => {
                if let Some(state) = self.manage.as_mut() {
                    state.reload(Path::new(&self.game_dir));
                }
            }
            manage::Action::OpenFolder(path) => open_in_explorer(&path),
            manage::Action::Flatten => {
                let Some(state) = self.manage.as_ref() else {
                    return;
                };
                let instance = state.version.clone();
                let game_dir = PathBuf::from(&self.game_dir);
                match aicraft_core::flatten::rewrite_instance(&game_dir, &instance) {
                    Ok(0) => logging::info(format!("{} 本来就是自包含的，不用动", instance)),
                    Ok(bytes) => {
                        logging::info(format!("{} 的清单压平了（{} 字节）", instance, bytes))
                    }
                    Err(e) => logging::warn(format!("{} 压平失败: {}", instance, e)),
                }
                if let Some(state) = self.manage.as_mut() {
                    state.refresh_identity(&game_dir);
                }
            }
            manage::Action::Delete(path) => {
                let shown = path.display().to_string();
                match aicraft_core::instance::remove_item(&path) {
                    Ok(()) => {
                        logging::info(format!("删掉了 {}", shown));
                        if let Some(state) = self.manage.as_mut() {
                            state.reload(Path::new(&self.game_dir));
                        }
                    }
                    Err(e) => logging::warn(format!("删除 {} 失败: {}", shown, e)),
                }
            }
            manage::Action::CheckUpdates => self.start_check_updates(),
            manage::Action::UpdateMod(path) => self.start_update_mods(Some(path)),
            manage::Action::UpdateAll => self.start_update_mods(None),
            manage::Action::LoadLoaderVersions(kind) => {
                // 「在加载」由页面在发动作前置好，这里只管发请求。
                let Some(state) = self.manage.as_ref() else {
                    return;
                };
                let mc_version = state.mc_version.clone();
                let mirror = self.loader_mirror(kind);
                self.spawn_task(
                    format!("正在获取 {} 的 {} 版本", mc_version, kind.label()),
                    move |tx| match aicraft_core::loader::list_versions(kind, &mc_version, mirror) {
                        Ok(list) => {
                            let _ = tx.send(TaskMsg::LoaderVersions {
                                game_version: mc_version,
                                kind,
                                list,
                            });
                            Ok(())
                        }
                        Err(e) => {
                            let _ = tx.send(TaskMsg::LoaderFailed(e));
                            Ok(())
                        }
                    },
                );
            }
            manage::Action::SwitchLoader { kind, version } => {
                let Some(state) = self.manage.as_ref() else {
                    return;
                };
                let instance = state.version.clone();
                let mc_version = state.mc_version.clone();
                let game_dir = PathBuf::from(&self.game_dir);
                let mirror = self.loader_mirror(kind);
                let concurrency = self.settings.concurrency;
                let label = format!("{} {}", kind.label(), version);
                // 换加载器同样会往这一层写 `inheritsFrom`：换完要靠父版本
                // 提供主类、库和素材。父版本不在盘上就别白换一遍——
                // 26.3-Fabric 当初就是换完之后才发现底座没了。
                if !aicraft_core::install::is_installed(&game_dir, &mc_version) {
                    logging::warn(format!(
                        "{} 的家底在 {} 里，但 {} 没装：先装上它再换加载器",
                        instance, mc_version, mc_version
                    ));
                    self.parent_prompt = Some((instance.clone(), mc_version.clone()));
                    return;
                }
                self.manage_switch = Some((instance.clone(), label.clone()));
                // 换加载器一样要能看见进度：Forge/NeoForge 这条路上要跑安装器，
                // 没有进度的话界面就只剩一个转圈，玩家分不清是在干活还是卡死。
                self.download_progress = Some((0, 0, 0));
                self.loader_phase = None;
                self.speed_bps = 0.0;
                self.last_tick_at = None;
                self.last_tick_bytes = 0;
                self.spawn_task(format!("正在装 {}", label), move |tx| {
                    let mut send = |line: &str| {
                        let _ = tx.send(TaskMsg::Log(line.to_string()));
                    };
                    let send_progress = |ev: aicraft_core::loader::LoaderProgress| {
                        let msg = match ev {
                            aicraft_core::loader::LoaderProgress::Tick { done, total, bytes } => {
                                TaskMsg::LoaderTick { done, total, bytes }
                            }
                            aicraft_core::loader::LoaderProgress::Working(text) => {
                                TaskMsg::LoaderPhase(text)
                            }
                        };
                        let _ = tx.send(msg);
                    };
                    let cancel = aicraft_core::cancel::CancelToken::new();
                    aicraft_core::loader::switch_loader(
                        &game_dir,
                        &instance,
                        &mc_version,
                        kind,
                        &version,
                        &mirror,
                        concurrency,
                        &cancel,
                        &send_progress,
                        &mut send,
                    )?;
                    Ok(())
                });
            }
        }
    }

    /// 删除版本的确认弹窗。
    ///
    /// 破坏性动作一律再问一次：删除删的是 `versions/<版本>/` 这一整层，
    /// 隔离实例的存档就在里面，没有回收站可退。
    /// 删账号前的确认。
    ///
    /// 账号删掉之后要重新登录才能回来（正版/第三方还得再走一遍浏览器或
    /// 密码），所以这里不学「点了就删」那种利落——多问一句，比让玩家
    /// 在日志里翻「我刚才是不是手滑了」划算。
    fn account_remove_modal(&mut self, ctx: &egui::Context, busy: bool) {
        let Some((uuid, name)) = self.pending_account_remove.clone() else {
            return;
        };
        let is_current = self.account.as_ref().map(|a| a.uuid.as_str()) == Some(uuid.as_str());
        let mut confirmed = false;
        let mut dismissed = false;

        let response = egui::Modal::new(egui::Id::unique("aicraft-remove-account"))
            .frame(theme::dialog_frame())
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(RichText::new("从列表里删掉这个账号？").size(15.0).strong());
                ui.add_space(6.0);
                ui.label(
                    RichText::new(&name)
                        .size(12.5)
                        .color(theme::TEXT)
                        .strong(),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(if is_current {
                        "它是当前账号，删掉之后启动器会退回「未配置」。登录凭证一起删掉，要重新登录才能用回来。"
                    } else {
                        "登录凭证会一起删掉。之后想用它得重新登录一次。"
                    })
                    .size(11.0)
                    .color(theme::WARN),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if theme::dialog_button(ui, "取消", !busy, 92.0).clicked() {
                        dismissed = true;
                    }
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if theme::dialog_danger_button(ui, "删除", !busy, 92.0).clicked() {
                                confirmed = true;
                            }
                        },
                    );
                });
            });

        if response.should_close() {
            dismissed = true;
        }
        if confirmed {
            aicraft_core::audit::action(format!("账户：确认删除 {}", name));
            self.remove_account(&uuid);
            self.third_error = None;
            self.pending_account_remove = None;
        } else if dismissed {
            self.pending_account_remove = None;
        }
    }

    fn delete_confirm_modal(&mut self, ctx: &egui::Context, busy: bool) {
        let Some(name) = self.pending_remove.clone() else {
            return;
        };
        let isolated = self.launch_isolation(&name);
        let mut confirmed = false;
        let mut dismissed = false;

        let response = egui::Modal::new(egui::Id::unique("aicraft-delete-version"))
            .frame(theme::dialog_frame())
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(RichText::new("删除这个版本？").size(15.0).strong());
                ui.add_space(6.0);
                ui.label(RichText::new(&name).size(12.5).color(theme::TEXT).strong());
                ui.add_space(6.0);
                ui.label(
                    RichText::new(if isolated {
                        "存档、模组、配置都在 versions/ 里，会一起删掉——删了找不回来。"
                    } else {
                        "只删 versions/ 下这一层；共用目录里的存档和模组不受影响。"
                    })
                    .size(11.0)
                    .color(if isolated {
                        theme::WARN
                    } else {
                        theme::TEXT_WEAK
                    }),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if theme::dialog_button(ui, "取消", !busy, 92.0).clicked() {
                        dismissed = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::dialog_danger_button(ui, "确认删除", !busy, 92.0).clicked() {
                            confirmed = true;
                        }
                    });
                });
            });

        if response.should_close() {
            dismissed = true;
        }
        if confirmed {
            aicraft_core::audit::action(format!("版本列表：确认删除 {}", name));
            self.delete_version(name);
            self.pending_remove = None;
        } else if dismissed {
            self.pending_remove = None;
        }
    }

    /// 「恢复默认设置」的二次确认。
    ///
    /// 这一步不能省：恢复默认会一次抹掉下载源、并发、限速、内存、JVM 参数、
    /// 界面偏好 —— 玩家可能是误点了标题旁边那个小图标，代价却是一整套配置。
    fn reset_confirm_modal(&mut self, ctx: &egui::Context, busy: bool) {
        if !self.reset_confirm {
            return;
        }
        let mut confirmed = false;
        let mut dismissed = false;

        let response = egui::Modal::new(egui::Id::unique("aicraft-reset-settings"))
            .frame(theme::dialog_frame())
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(RichText::new("恢复默认设置？").size(15.0).strong());
                ui.add_space(6.0);
                ui.label(
                    RichText::new("下载源、并发、限速、内存、JVM 参数和界面偏好都会回到出厂状态。")
                        .size(11.5)
                        .color(theme::TEXT_WEAK),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("游戏目录、账号和已装好的版本不受影响。")
                        .size(11.5)
                        .color(theme::TEXT_WEAK),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if theme::dialog_button(ui, "取消", !busy, 92.0).clicked() {
                        dismissed = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::dialog_primary_button(ui, "恢复默认", !busy, 108.0).clicked()
                        {
                            confirmed = true;
                        }
                    });
                });
            });

        if response.should_close() {
            dismissed = true;
        }
        if confirmed {
            aicraft_core::audit::action("设置：恢复默认设置");
            self.settings = Settings::default();
            logging::info("已恢复默认设置");
            self.reset_confirm = false;
        } else if dismissed {
            self.reset_confirm = false;
        }
    }

    /// 校验完发现缺失时的询问：要不要现在补全。
    fn repair_confirm_modal(&mut self, ctx: &egui::Context, busy: bool) {
        let Some((name, problems)) = self.verify_prompt.clone() else {
            return;
        };
        let mut confirmed = false;
        let mut dismissed = false;

        let response = egui::Modal::new(egui::Id::unique("aicraft-repair-version"))
            .frame(theme::dialog_frame())
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(RichText::new("发现缺失的文件").size(15.0).strong());
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("{} 有 {} 个文件缺失或损坏。", name, problems))
                        .size(11.5)
                        .color(theme::TEXT_WEAK),
                );
                ui.label(
                    RichText::new("要现在把它们下载回来吗？明细已经写在日志里了。")
                        .size(11.5)
                        .color(theme::TEXT_WEAK),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if theme::dialog_button(ui, "以后再说", !busy, 92.0).clicked() {
                        dismissed = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::dialog_primary_button(ui, "补全", !busy, 92.0).clicked() {
                            confirmed = true;
                        }
                    });
                });
            });

        if response.should_close() {
            dismissed = true;
        }
        if confirmed {
            aicraft_core::audit::action(format!("校验：确认补全 {} 的 {} 处缺失", name, problems));
            self.verify_prompt = None;
            // 补全走的是「当前选中版本」，先把选中项指回去，免得用户中途点过别的。
            if let Some(i) = self.versions.iter().position(|v| v.name == name) {
                self.selected = Some(i);
            }
            self.start_repair();
        } else if dismissed {
            self.verify_prompt = None;
        }
    }

    /// 下载时贴在窗口底部的进度条：进度、速度、取消。
    ///
    /// 用 `Area` 而不是 `Modal`：它要浮在正文之上、贴着底边，
    /// 同时不挡住上面正在看的东西，也不吃背景点击。
    fn progress_panel(&mut self, ctx: &egui::Context) {
        // 三种形态都画这张卡片：
        // - 有 `download_progress`：下载，能算出百分比，走普通进度条；
        // - 有 `launch_phase`：启动游戏那几步（读清单 / 挑 Java / 拼参数 /
        //   解压 natives / 起进程），步骤数是固定的，也走百分比；
        // - 只有 `loader_phase`：跑 java 安装器那种说不清进度的阶段，
        //   换成来回滚动的动画条，绝不假装知道进度。
        let Some(busy) = self.busy.clone() else {
            return;
        };
        if self.download_progress.is_none()
            && self.loader_phase.is_none()
            && self.launch_phase.is_none()
        {
            return;
        }
        let phase = self.loader_phase.clone();
        let launching = self.launch_phase.clone();
        let (done, total, bytes) = self.download_progress.unwrap_or((0, 0, 0));

        // 启动阶段和下载阶段都算得出比例，只是刻度不同。
        let frac = if let Some((step, steps, _)) = &launching {
            if *steps > 0 {
                *step as f32 / *steps as f32
            } else {
                0.0
            }
        } else if total > 0 {
            done as f32 / total as f32
        } else {
            0.0
        };
        let speed_text = if self.speed_bps >= 1.0 {
            format!("{}/s", human_size(self.speed_bps))
        } else {
            String::new()
        };
        let confirm = self.cancel_confirm;
        let requested = self.cancel_requested;

        let mut cancel_clicked = false;
        let mut confirm_clicked = false;
        let mut resume_clicked = false;

        egui::Area::new(egui::Id::unique("aicraft-progress-panel"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -16.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(theme::CARD)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER))
                    .corner_radius(12.0)
                    .inner_margin(egui::Margin::symmetric(14, 12))
                    .shadow(egui::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(26),
                    })
                    .show(ui, |ui| {
                        ui.set_width(540.0);

                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(busy.clone())
                                    .size(12.5)
                                    .color(theme::TEXT_WEAK),
                            );
                            if !speed_text.is_empty() {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(speed_text.clone())
                                                .size(12.0)
                                                .color(theme::ACCENT_STRONG),
                                        );
                                    },
                                );
                            }
                        });
                        if let Some(phase) = &phase {
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(phase.clone())
                                    .size(11.5)
                                    .color(theme::TEXT_WEAK),
                            );
                        }
                        if let Some((step, steps, label)) = &launching {
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "第 {} / {} 步：{}",
                                    step, steps, label
                                ))
                                .size(11.5)
                                .color(theme::TEXT_WEAK),
                            );
                        }
                        ui.add_space(6.0);

                        if phase.is_some() {
                            // 说明文字不往条里塞：ProgressBar 的文字是画在条中间的，
                            // 长句子会把条撑成半透明色块、还会被裁掉一截，
                            // 已经在上面的标题行说清了，这里只留一条干净的动画。
                            indeterminate_bar(ui, 18.0);
                        } else if launching.is_some() {
                            // 启动这几步没有文件计数，条里只写百分比。
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .desired_width(ui.available_width())
                                    .text(format!("{:.0}%", frac * 100.0)),
                            );
                        } else {
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .desired_width(ui.available_width())
                                    .text(format!("{} / {}（{:.0}%）", done, total, frac * 100.0)),
                            );
                        }
                        ui.add_space(8.0);

                        // 启动这几步没有下载量，也没法中途叫停（进程还没起来，
                        // 取消要么装没发生、要么留下半截 dll），所以不摆按钮。
                        if launching.is_some() {
                            ui.label(
                                egui::RichText::new("游戏窗口出现后这里会自动收起。")
                                    .size(12.0)
                                    .color(theme::TEXT_WEAK),
                            );
                        } else if confirm {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("取消之后，这一份已经下好的文件会被删掉。")
                                        .size(12.0)
                                        .color(theme::DANGER),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .button(
                                                egui::RichText::new("确定取消")
                                                    .color(theme::DANGER),
                                            )
                                            .clicked()
                                        {
                                            confirm_clicked = true;
                                        }
                                        if ui.button("继续下载").clicked() {
                                            resume_clicked = true;
                                        }
                                    },
                                );
                            });
                        } else {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(if phase.is_some() {
                                        "这一步没有百分比".to_string()
                                    } else {
                                        format!("已下载 {}", human_size(bytes as f64))
                                    })
                                    .size(12.0)
                                    .color(theme::TEXT_WEAK),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let label = if requested {
                                            "正在停止…"
                                        } else {
                                            "取消下载"
                                        };
                                        if ui
                                            .add_enabled(
                                                !requested,
                                                egui::Button::new(label)
                                                    .min_size(egui::vec2(78.0, 24.0)),
                                            )
                                            .clicked()
                                        {
                                            cancel_clicked = true;
                                        }
                                    },
                                );
                            });
                        }
                    });
            });

        if cancel_clicked {
            aicraft_core::audit::action("下载：点了取消（等确认）");
            self.cancel_confirm = true;
        }
        if resume_clicked {
            aicraft_core::audit::action("下载：继续下载");
            self.cancel_confirm = false;
        }
        if confirm_clicked {
            aicraft_core::audit::action("下载：确认取消");
            self.cancel_confirm = false;
            self.cancel_requested = true;
            if let Some(token) = &self.cancel_token {
                token.cancel();
            }
            logging::info("取消下载：正在让下载器停下来…");
        }
    }

    /// 轮询后台消息。
    fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut logs = Vec::new();
        let mut finished = false;
        let mut installed = false;
        // `rx` 借用着 `self`，所以「删文件」不能写在循环里，先记个旗标。
        let mut cancel_cleanup = false;

        while let Ok(msg) = rx.try_recv() {
            match msg {
                TaskMsg::Log(line) => logs.push((line, Level::Info)),
                TaskMsg::LoaderVersions {
                    game_version,
                    kind,
                    list,
                } => {
                    if let Some(state) = self.manage.as_mut() {
                        if state.loader_open == Some(kind) {
                            state.set_loader_versions(kind, game_version.clone(), list.clone());
                        }
                    }
                    if let Some(wizard) = self.wizard.as_mut() {
                        wizard.set_versions(game_version, kind, list);
                    }
                }
                TaskMsg::LoaderFailed(message) => {
                    if let Some(state) = self.manage.as_mut() {
                        state.fail_loader_versions(message.clone());
                    }
                    if let Some(wizard) = self.wizard.as_mut() {
                        wizard.fail_loading(message);
                    }
                }
                // 游戏进程起来之后，启动器按设置决定自己退不退出。
                // 这个分支只记旗标：真正关窗口得在 ui() 里发 ViewportCommand。
                TaskMsg::GameRunning(pid) => {
                    self.launch_phase =
                        Some((LAUNCH_STEPS, LAUNCH_STEPS, "等游戏窗口出现…".to_string()));
                    // 记下 pid：「等窗口」回来的时候要拿它安排重开。
                    self.launching_pid = Some(pid);
                }
                TaskMsg::GameWindow { pid, shown } => {
                    // 窗口出来了（或者等超了），面板到此为止。
                    // 留着它会一直停在最后一步，好像还卡在那里。
                    // 但 busy 不动：还在等游戏退出，不能让人再点一次启动。
                    self.launch_phase = None;
                    self.launching_pid = None;
                    let mode = self.settings.launcher_mode;
                    if shown && mode != aicraft_core::LauncherMode::KeepOpen {
                        if mode == aicraft_core::LauncherMode::Reopen {
                            spawn_reopen_watcher(pid);
                        }
                        self.close_after_launch = true;
                    }
                }
                TaskMsg::Verified { version, problems } => {
                    if problems > 0 {
                        self.verify_prompt = Some((version, problems));
                    }
                }
                TaskMsg::Installed => {
                    installed = true;
                    self.version_mgr.installing = None;
                }
                TaskMsg::ModInstalled(result) => match result {
                    Ok(text) => self.resources.notice = Some((text, true)),
                    Err(e) => self.resources.notice = Some((e, false)),
                },
                TaskMsg::ModUpdates(list) => {
                    if let Some(state) = self.manage.as_mut() {
                        state.set_updates(list);
                    }
                }
                TaskMsg::ModUpdated { done, failed } => {
                    if let Some(state) = self.manage.as_mut() {
                        state.update_busy = None;
                        state.update_notice = Some(if failed.is_empty() {
                            (format!("更新了 {} 个模组", done), true)
                        } else {
                            (
                                format!("更新了 {} 个，{} 个失败（看日志）", done, failed.len()),
                                false,
                            )
                        });
                        state.updates.clear();
                        state.reload(Path::new(&self.game_dir));
                    }
                    for line in failed {
                        logging::warn(line);
                    }
                }
                TaskMsg::Account(account) => {
                    // 正版登录成功：账号文件已经被 finish_login 写好，
                    // 这里重新读一遍是为了让左栏列表立刻多出这一条。
                    self.accounts = AccountStore::load_or_default(&store::default_path());
                    self.username = account.username.clone();
                    self.account = Some(account);
                }
                TaskMsg::ThirdParty(result) => match result {
                    Ok(session) => {
                        let stored = store::StoredAccount::from_yggdrasil(&session);
                        let path = store::default_path();
                        let mut saved = AccountStore::load_or_default(&path);
                        saved.upsert(stored.clone());
                        match saved.save(&path) {
                            Ok(()) => logging::info(format!(
                                "已登录第三方账户 {}（{}）",
                                stored.name, session.api_root
                            )),
                            Err(e) => {
                                logging::warn(format!("第三方账户登录成功，但保存账号失败：{}", e))
                            }
                        }
                        // 缓存同步一份：左栏列表下一帧就要显示这条新账号
                        self.accounts = saved;
                        self.username = stored.name.clone();
                        self.settings.offline_name = self.username.clone();
                        // 这里不能调 `self.persist_settings_quietly()`：整份 `&mut self`
                        // 会和循环开头借出去的 `rx` 打架。字段各写各的就不会。
                        self.settings.clamp();
                        if let Err(e) = self.settings.save(&settings_path()) {
                            logging::warn(format!("保存设置失败: {}", e));
                        }
                        self.account = Some(stored.to_account());
                        self.account_draft = self.username.clone();
                        self.third_pass.clear();
                        self.third_error = None;
                        self.account_open = false;
                    }
                    Err(e) => {
                        logging::warn(format!("第三方登录失败：{}", e));
                        self.third_error = Some(e);
                        self.third_pass.clear();
                    }
                },
                TaskMsg::LoaderTick { done, total, bytes } => {
                    // 加载器这一段的计数是它自己的一批文件，和原版那 3xxx 个
                    // 不是一套刻度，所以直接整份替换，不做「只增不减」的保护。
                    self.download_progress = Some((done, total, bytes));
                    self.loader_phase = None;
                    let now = std::time::Instant::now();
                    if let (Some(at), sample) = (self.last_tick_at, self.last_tick_bytes) {
                        let secs = now.duration_since(at).as_secs_f64();
                        let delta = bytes.saturating_sub(sample);
                        if secs >= 0.25 && delta > 0 {
                            let instant = delta as f64 / secs;
                            self.speed_bps = if self.speed_bps > 0.0 {
                                self.speed_bps * 0.6 + instant * 0.4
                            } else {
                                instant
                            };
                            self.last_tick_at = Some(now);
                            self.last_tick_bytes = bytes;
                        }
                    } else {
                        self.last_tick_at = Some(now);
                        self.last_tick_bytes = bytes;
                    }
                }
                TaskMsg::LaunchPhase { step, total, label } => {
                    self.launch_phase = Some((step, total, label));
                    // 启动这几步都不是下载，别让「已下载 0 B」留在下面
                    self.download_progress = None;
                    self.loader_phase = None;
                    self.speed_bps = 0.0;
                }
                TaskMsg::LoaderPhase(text) => {
                    self.loader_phase = Some(text);
                    // 说不清进度的时候别留一个停在 97% 的条，清掉，
                    // 让面板显示滚动的动画条。
                    self.download_progress = None;
                    self.speed_bps = 0.0;
                }
                TaskMsg::Progress { done, total, bytes } => {
                    // `done` 事件只带字节数时为 0，不能让它覆盖已完成计数，
                    // 否则进度条会来回跳。
                    let prev_done = self.download_progress.map(|(d, _, _)| d).unwrap_or(0);
                    let prev_bytes = self.download_progress.map(|(_, _, b)| b).unwrap_or(0);
                    let new_done = if done > 0 { done } else { prev_done };
                    let new_bytes = bytes.max(prev_bytes);
                    self.download_progress = Some((new_done, total, new_bytes));

                    // 速度只能自己算：核心只报累计字节。取样间隔太短会让数字
                    // 抖得没法看，所以至少隔 0.25 秒才更新一次，并且做一点平滑。
                    let now = std::time::Instant::now();
                    if let (Some(at), sample) = (self.last_tick_at, self.last_tick_bytes) {
                        let secs = now.duration_since(at).as_secs_f64();
                        let delta = new_bytes.saturating_sub(sample);
                        if secs >= 0.25 && delta > 0 {
                            let instant = delta as f64 / secs;
                            self.speed_bps = if self.speed_bps > 0.0 {
                                self.speed_bps * 0.6 + instant * 0.4
                            } else {
                                instant
                            };
                            self.last_tick_at = Some(now);
                            self.last_tick_bytes = new_bytes;
                        }
                    } else {
                        self.last_tick_at = Some(now);
                        self.last_tick_bytes = new_bytes;
                    }
                }
                TaskMsg::Finished(result) => {
                    if let Some((instance, what)) = self.manage_switch.take() {
                        let game_dir = PathBuf::from(&self.game_dir);
                        let ok = matches!(result, Ok(()));
                        if let Some(state) = self.manage.as_mut() {
                            state.refresh_identity(&game_dir);
                            state.loader_versions.clear();
                            state.loader_for = None;
                            state.reload(&game_dir);
                        }
                        if ok {
                            logging::info(format!("{} 现在用 {}", instance, what));
                            // 加载器换了，版本列表里那枚标签也得跟着变。
                            // 只改这一个字段，不用整盘重扫（重扫会清掉选中项）。
                            refresh_loader_tag(&mut self.versions, &game_dir, &instance);
                        }
                    }
                    if self.cancel_requested {
                        // 用户自己停的，不该报成失败。
                        logs.push(("已取消安装".to_string(), Level::Warn));
                        cancel_cleanup = true;
                    } else {
                        match result {
                            Ok(()) => logs.push(("完成".to_string(), Level::Info)),
                            Err(e) => logs.push((format!("失败: {}", e), Level::Error)),
                        }
                    }
                    finished = true;
                }
            }
        }

        for (line, level) in logs {
            logging::log(level, line);
        }
        if cancel_cleanup {
            self.drop_install_leftovers();
            installed = true;
        }
        if finished {
            self.rx = None;
            self.busy = None;
            self.download_progress = None;
            self.cancel_token = None;
            self.cancel_confirm = false;
            self.cancel_requested = false;
            self.speed_bps = 0.0;
            self.last_tick_at = None;
            self.last_tick_bytes = 0;
            self.launch_phase = None;
            // 装失败时把「正在安装 X」收回来。清单那边不归这里管：
            // 拉清单走自己的 `manifest_rx`，这条任务结束跟它没关系。
            self.version_mgr.installing = None;
        }
        // 装完立刻刷新本地列表，让新版本马上可选
        if installed {
            self.rescan();
        }
    }

    /// 给「自动」下载源量一次延迟。
    ///
    /// 四类（版本清单 / Fabric / Forge / NeoForge）并行测，各测官方与镜像
    /// 两条，谁快用谁。这件事必须放在后台线程里：探测最坏要等 8 秒超时，
    /// 卡在启动的第一帧就等于启动器打不开。
    fn start_mirror_probe(&mut self) {
        if self.mirror_probe_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.mirror_probe_rx = Some(rx);
        std::thread::spawn(move || {
            aicraft_core::settings::Mirror::probe_all();
            let _ = tx.send(());
        });
    }
}

/// 重新认一遍某个版本挂的加载器，只更新那一条，不动别的。
///
/// 换完加载器不能整盘 rescan：那会清掉选中项、把滚动位置也弄回去，
/// 玩家刚点的那个版本会「跳走」。这枚标签只跟一份 json 有关，
/// 单独重算就够了。
fn refresh_loader_tag(versions: &mut [VersionEntry], game_dir: &Path, instance: &str) {
    let Some(entry) = versions.iter_mut().find(|v| v.name == instance) else {
        return;
    };
    let path =
        aicraft_core::instance::version_dir(game_dir, instance).join(format!("{}.json", instance));
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    entry.loader = aicraft_core::loader::detect_in_text(&text, instance).map(|(kind, _)| kind);
}

/// 把字节数写成给人看的单位。
/// 在资源管理器里打开一个目录，或者定位一个文件。
///
/// 子进程一律走 `spawn::silent`：这里只是扔个窗口出来，不需要等它，
/// 更不该因为 shell 弹一下黑框。
/// 游戏运行目录：隔离实例是 `versions/<名字>`，共享模式是 `.minecraft` 根。
///
/// 跟启动时 `LaunchConfig::with_instance_mode` 用的是同一个判断，
/// 所以这里算出来的目录，就是游戏真的会往里写 `logs/` 的那个。
fn game_run_dir(game_dir: &Path, version: &str, isolated: bool) -> PathBuf {
    let mode = if isolated {
        aicraft_core::InstanceMode::Isolated
    } else {
        aicraft_core::InstanceMode::Shared
    };
    aicraft_core::instance::game_dir_for(game_dir, version, mode)
}

/// 这个实例的游戏日志目录。
///
/// 优先 `logs/`（`latest.log` 一类运行日志），退回实例目录本身
/// ——老版本（1.6 以前）把日志直接写在游戏根目录。
fn game_log_dir(run_dir: &Path) -> Option<PathBuf> {
    let logs = run_dir.join("logs");
    if logs.is_dir() {
        return Some(logs);
    }
    if run_dir.is_dir() {
        return Some(run_dir.to_path_buf());
    }
    None
}

/// 这个实例的崩溃报告目录。
///
/// 1.14 之后在 `crash-reports/`；更早的版本直接扔在游戏根目录，
/// 文件名形如 `crash-2026-10-30_21.55.12-client.txt`。两个都看一遍，
/// 有崩溃报告的那个优先。
fn game_crash_dir(run_dir: &Path) -> Option<PathBuf> {
    let reports = run_dir.join("crash-reports");
    if has_crash_files(&reports) {
        return Some(reports);
    }
    if has_crash_files(run_dir) {
        return Some(run_dir.to_path_buf());
    }
    if reports.is_dir() {
        return Some(reports);
    }
    None
}

fn has_crash_files(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .map(|name| name.starts_with("crash-") && name.ends_with(".txt"))
            .unwrap_or(false)
    })
}

fn open_in_explorer(path: &Path) {
    let mut cmd = aicraft_core::spawn::silent("explorer.exe");
    if path.is_dir() {
        cmd.arg(path);
    } else {
        cmd.arg(format!("/select,{}", path.display()));
    }
    let _ = cmd.spawn();
}

/// 用系统默认浏览器打开一个网址。
///
/// 不拼 `cmd /c start`：那个要过一层 `cmd` 的引号规则，网址里带 `&` 就会被
/// 截断；`explorer.exe <url>` 直接把整串交给 shell 关联程序，干净。
fn open_external_url(url: &str) {
    let mut cmd = aicraft_core::spawn::silent("explorer.exe");
    cmd.arg(url);
    let _ = cmd.spawn();
}

/// 等游戏窗口最多等多久（秒）。模组多的整合包，十几秒不进游戏很正常，
/// 给足余量；等超了也不杀游戏，只是启动器先留下。
const GAME_WINDOW_TIMEOUT_SECS: u32 = 180;

/// 等游戏窗口真正出现在屏幕上。`true` 表示等到了。
///
/// 为什么不用「进程起来了」当信号：点完启动到看见窗口之间要读几百个模组，
/// 十几秒是常态。启动器要是这时候已经退了，屏幕上一片安静，玩家会以为自己
/// 没点中，于是再点一次、开出第二个游戏进程 —— 存档就危险了。
/// 等窗口来再退，那一下才有明确的因果。
///
/// 判断手段是 `Get-Process` 的 `MainWindowHandle`：javaw 的窗口由 JVM 自己
/// 创建，窗口一出现这个整数就不再是 0，比盯日志里某一行稳。
/// 启动器这边 `unsafe_code = "forbid"`，不能直接调 `EnumWindows`，所以借
/// 一趟 PowerShell；它只起一个进程、在里面自己循环，比重开几十次便宜。
fn wait_for_game_window(pid: u32) -> bool {
    // 整条命令拼成一行、只用分号分隔。下面那几个反斜杠是 Rust 的字符串续行，
    // 编译期就被吃掉了，送到 PowerShell 手里的始终是完整的一行。
    // （别把它改成真·多行文本：PowerShell 不认反斜杠续行，那样每一行都会
    // 被当成独立命令，报一串 CommandNotFoundException，最后落到 `exit 3`。）
    let script = format!(
        "$d=(Get-Date).AddSeconds({timeout}); while((Get-Date) -lt $d) \
         {{ $p=Get-Process -Id {pid} -ErrorAction SilentlyContinue; \
         if(-not $p){{exit 2}}; if($p.MainWindowHandle -ne 0){{exit 0}}; \
         Start-Sleep -Milliseconds 250 }}; exit 3",
        timeout = GAME_WINDOW_TIMEOUT_SECS,
        pid = pid
    );

    let mut cmd = aicraft_core::spawn::silent("powershell.exe");
    // 不要加 `-WindowStyle Hidden`：`spawn::silent` 已经带了
    // `CREATE_NO_WINDOW`，两者叠在一起时这台机器上的 PowerShell 会把
    // 退出码改成 -1（4294967295），那个 0 就永远等不到了。
    cmd.arg("-NoProfile").arg("-Command").arg(&script);
    match cmd.status() {
        Ok(status) if status.code() == Some(0) => true,
        Ok(_) => false,
        Err(e) => {
            // 盯不上就别拖着玩家：按老样子直接退，至少不比以前差。
            logging::warn(format!("没能盯着游戏窗口（{}），仍按原样退出", e));
            true
        }
    }
}

/// 游戏退出之后，把启动器自己再拉起来。
///
/// 为什么用 PowerShell 而不是原地等：启动器马上要退出了，它自己的线程会跟着
/// 一起消失，所以「等游戏结束」这件事必须交给另一个进程去做。
/// `Wait-Process` 会阻塞到那个 pid 消失，然后 `Start-Process` 把启动器拉回来。
fn spawn_reopen_watcher(pid: u32) {
    let Ok(exe) = std::env::current_exe() else {
        logging::warn("记不住启动器自己的路径，游戏结束后没法自己回来");
        return;
    };
    // PowerShell 单引号字符串里，单引号要写两遍
    let path = exe.display().to_string().replace('\'', "''");
    let script = format!(
        "Wait-Process -Id {} -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500; \
         Start-Process -FilePath '{}'",
        pid, path
    );
    let mut cmd = aicraft_core::spawn::silent("powershell.exe");
    cmd.arg("-NoProfile")
        .arg("-WindowStyle")
        .arg("Hidden")
        .arg("-Command")
        .arg(script);
    if let Err(e) = cmd.spawn() {
        logging::warn(format!("没能安排游戏结束后重开启动器: {}", e));
    }
}

/// 不知道百分比时的进度条：一条来回扫的光带。
///
/// 为什么不用 `egui::ProgressBar::animate(true)`：那个动画只是在填充条右端
/// 画一个跳动的圆弧，进度为 0 时整个填充宽度只有 2px，那个圆弧就缩在左上角
/// 一小团，看着像渲染坏了。这里自己画一条在轨道里来回走的带子，意思清楚得多
/// ——「还在动，但别问百分比」。
fn indeterminate_bar(ui: &mut egui::Ui, height: f32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter();

    // 轨道
    painter.rect_filled(rect, egui::CornerRadius::same(9), theme::BG_BOTTOM);

    // 光带：宽度是轨道的 34%，位置用时间的正弦来回摆，周期约 1.6 秒。
    let time = ui.input(|i| i.time);
    let phase = (time * 1.28).sin() * 0.5 + 0.5; // 0..1 平滑往复
    let band_w = (rect.width() * 0.34).max(48.0);
    let travel = (rect.width() - band_w).max(0.0);
    let band_x = rect.left() + travel * phase as f32;
    let band = egui::Rect::from_min_size(
        egui::pos2(band_x, rect.top()),
        egui::vec2(band_w, rect.height()),
    );
    let clipped = painter.with_clip_rect(rect.intersect(ui.clip_rect()));
    clipped.rect_filled(band, egui::CornerRadius::same(9), theme::ACCENT_SOFT);
    clipped.rect_filled(
        band.shrink2(egui::vec2(0.0, 3.0)),
        egui::CornerRadius::same(6),
        theme::ACCENT,
    );

    // 动画得自己催帧，否则界面不动它就不重绘。
    ui.ctx().request_repaint();
}

/// 头像的圆角比例：占边长的多少。26px 的头像对应约 4px 圆角。
///
/// 只求「方角被磨掉一点」的观感：再圆一档就变成圆脸，皮肤边缘的像素
/// 会被啃掉，头像就不像方块人了。
pub(crate) const AVATAR_ROUNDING: f32 = 0.14;

/// 把方形头像四周削成圆角，边缘按覆盖度做一点抗锯齿。
fn round_corners(pixels: &mut [u8], side: usize, radius: f32) {
    let r = radius.max(0.0);
    let edge = side as f32;
    for y in 0..side {
        for x in 0..side {
            // 每个像素取 4×4 子采样：像素艺术放大之后边缘本来就有台阶，
            // 再叠一层硬切会看到很明显的锯齿。
            let mut inside = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f32 + (sx as f32 + 0.5) / 4.0;
                    let py = y as f32 + (sy as f32 + 0.5) / 4.0;
                    let cx = px.clamp(r, edge - r);
                    let cy = py.clamp(r, edge - r);
                    let dx = px - cx;
                    let dy = py - cy;
                    if dx * dx + dy * dy <= r * r {
                        inside += 1.0;
                    }
                }
            }
            let alpha = (y * side + x) * 4 + 3;
            let cover = inside / 16.0;
            pixels[alpha] = (pixels[alpha] as f32 * cover).round() as u8;
        }
    }
}

/// 把 32×32 的 RGBA 头像变成一张纹理。
///
/// 用 `NEAREST` 而不是线性过滤：皮肤是像素画，放大时糊掉就没法认了。
fn skin_texture(ctx: &egui::Context, uuid: &str, head: &[u8]) -> Option<egui::TextureHandle> {
    // 边长不固定：纯 2D 是 32，带 3D 层的是 40（脸 32 铺在中间，四周留一圈给壳探出去）。
    // 画的时候按边长把整个方块放大，脸于是和 2D 头像一样大。
    let cells = head.len() / 4;
    let side = (cells as f64).sqrt() as usize;
    if side * side * 4 != head.len() || side < 8 {
        return None;
    }
    // 圆角在这儿削，而不是画的时候再裁：`Painter::image` 只认矩形，
    // 想在绘制阶段抠出圆角就得自己上遮罩，还不如一次做好让纹理本身是圆的。
    let mut pixels = head.to_vec();
    round_corners(&mut pixels, side, side as f32 * AVATAR_ROUNDING);
    let image = egui::ColorImage::from_rgba_unmultiplied([side, side], &pixels);
    Some(ctx.load_texture(
        format!("skin-{}", uuid),
        image,
        egui::TextureOptions::NEAREST,
    ))
}

pub(crate) fn human_size(bytes: f64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes.max(0.0);
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{:.0} {}", value, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

/// 删掉 `dir` 下面所有 `.part`（下载中断留下的半截文件）。
///
/// 只往可能与下载有关的子目录里走，不把整个游戏目录翻一遍。
fn remove_part_files(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if matches!(name, "libraries" | "assets" | "versions" | "cache") {
                    removed += remove_part_files(&path);
                }
            }
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("part")
            && std::fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// 安装一个官方版本。
///
/// `official_id` 是官方清单里的版本号，`instance` 是落盘的实例名
/// （也就是 `versions/<instance>/` 的目录名）。同一官方版本装多份时
/// 实例名不同即可；版本 JSON 里的 `id` 会被改写成实例名，官方版本号
/// 留在私有字段里，供「已装几份」这类统计使用。
// 参数多是安装这件事本身决定的（目标、源、并发、限速、取消、回报通道），
// 再拆一层结构体只是把同样的东西换个地方写。
#[allow(clippy::too_many_arguments)]
fn do_install(
    game_dir: &Path,
    official_id: &str,
    instance: &str,
    mirror: &Mirror,
    concurrency: usize,
    limit_kbps: Option<u32>,
    cancel: &aicraft_core::cancel::CancelToken,
    tx: &Sender<TaskMsg>,
) -> Result<(), String> {
    let send = |s: String| {
        let _ = tx.send(TaskMsg::Log(s));
    };

    if !aicraft_core::transport::curl_available() {
        return Err("找不到 curl.exe，无法下载".to_string());
    }

    // 先探一下游戏目录能不能写。不探的话要等清单、资源索引、几百 MB 都走完
    // 才会在「创建版本目录」那一步失败，白等一场。
    if !aicraft_core::settings::is_writable_dir(game_dir) {
        return Err(format!(
            "游戏目录写不进去：{}\n原因可能是：这个启动器所在的文件夹带了低完整性标记（沙箱或安全策略），进程被降权，写不进别的位置。\n可以试试：把启动器换个文件夹再启动、用「以管理员身份运行」、或在「版本管理 → 目录列表」里换一个能写的游戏目录。",
            game_dir.display()
        ));
    }

    send("获取版本清单…".to_string());
    let manifest_text = aicraft_core::transport::fetch_text(
        &mirror.rewrite(aicraft_core::install::VERSION_MANIFEST_URL),
    )
    .map_err(|e| format!("获取版本清单失败: {}", e))?;
    let manifest = aicraft_core::install::parse_manifest(&manifest_text)
        .map_err(|e| format!("解析清单失败: {}", e))?;

    let summary = manifest
        .find(official_id)
        .ok_or_else(|| format!("官方清单里没有版本 {}", official_id))?;
    let version_url = summary.url.clone();

    send(format!("下载 {} 的清单…", official_id));
    let version_json = aicraft_core::transport::fetch_text(&mirror.rewrite(&version_url))
        .map_err(|e| format!("下载版本 JSON 失败: {}", e))?;

    let parsed = parse_version(&version_json).map_err(|e| format!("解析版本 JSON 失败: {}", e))?;

    let index_text = if let Some(url) = &parsed.asset_index.url {
        send("下载资源索引…".to_string());
        Some(
            aicraft_core::transport::fetch_text(&mirror.rewrite(url))
                .map_err(|e| format!("下载资源索引失败: {}", e))?,
        )
    } else {
        None
    };

    let ctx = RuleContext::current();
    let plan = aicraft_core::install::plan_install_as(
        game_dir,
        &version_json,
        index_text.as_deref(),
        &ctx,
        Some(instance),
    )?;

    if plan.version_id != plan.official_id {
        send(format!(
            "装成新实例 {}（官方 {}）",
            plan.version_id, plan.official_id
        ));
    }

    send(format!(
        "需要下载 {} 个文件，约 {:.1} MB",
        plan.items.len(),
        plan.total_bytes as f64 / 1_048_576.0
    ));

    let tx_for_progress = tx.clone();
    let total = plan.items.len();
    let budget = aicraft_core::speed::Budget::new(limit_kbps);
    if let Some(kbps) = limit_kbps {
        send(format!(
            "下载限速 {}（总带宽，分给 {} 路并发）",
            aicraft_core::speed::describe(Some(kbps)),
            concurrency
        ));
    }
    let report = aicraft_core::install::execute_install_live(
        &plan,
        concurrency,
        mirror,
        &budget,
        cancel,
        &|ev| {
            if let aicraft_core::install::InstallProgress::Tick { done, bytes, .. } = ev {
                let _ = tx_for_progress.send(TaskMsg::Progress { done, total, bytes });
            }
        },
    )?;

    if report.cancelled {
        send("已取消，剩下的文件不下了".to_string());
        return Ok(());
    }

    send(format!(
        "完成：下载 {}，跳过 {}，失败 {}，共 {:.1} MB",
        report.downloaded,
        report.skipped,
        report.failed,
        report.bytes as f64 / 1_048_576.0
    ));

    if report.failed > 0 {
        for (path, msg) in report.failures.iter().take(5) {
            send(format!("  失败 {} — {}", path.display(), msg));
        }
        return Err(format!("{} 个文件下载失败", report.failed));
    }

    let _ = tx.send(TaskMsg::Installed);
    Ok(())
}

/// 把日志连同环境信息拼成一份可分享的文本。
///
/// 落盘位置由用户自己在「另存为」对话框里定，这里只负责内容 ——
/// 也就不再需要猜「哪个目录能写」。
fn log_text(
    log: &[logging::Entry],
    settings: &Settings,
    game_dir: &str,
    version: Option<&str>,
) -> String {
    let (_, pretty) = now_local();

    let mut text = String::new();
    text.push_str("AICraft Launcher 日志\n");
    text.push_str(&format!("版本: {}\n", env!("CARGO_PKG_VERSION")));
    text.push_str(&format!("导出时间: {} (UTC)\n", pretty));
    text.push_str(&format!("系统: {}\n", std::env::consts::OS));
    text.push_str(&format!("游戏目录: {}\n", game_dir));
    text.push_str(&format!("当前版本: {}\n", version.unwrap_or("(未选择)")));
    text.push_str(&format!("版本下载源: {}\n", settings.mirror.id()));
    text.push_str(&format!("下载并发: {}\n", settings.concurrency));
    text.push_str(&format!(
        "下载限速: {}\n",
        aicraft_core::speed::describe(settings.download_limit_kbps)
    ));
    text.push_str(&format!(
        "内存: 最大 {} MB（{}），最小 {}\n",
        settings.memory_mb(),
        if settings.auto_memory {
            "自动"
        } else {
            "手动"
        },
        settings
            .min_memory_mb
            .map(|m| format!("{} MB", m))
            .unwrap_or_else(|| "自动".to_string())
    ));
    text.push_str(&format!(
        "Java 路径: {}\n",
        settings
            .java_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "自动检测".to_string())
    ));
    if !settings.jvm_args.trim().is_empty() {
        text.push_str(&format!("JVM 参数: {}\n", settings.jvm_args.trim()));
    }
    text.push_str("\n----- 日志 -----\n");
    if log.is_empty() {
        text.push_str("(空)\n");
    }
    for entry in log {
        text.push_str(&format!(
            "{} {:<5} {}\n",
            logging::format_local(entry.at),
            entry.level.label(),
            entry.message
        ));
    }
    text
}

/// 日志显示阈值在界面上怎么写。
fn level_filter_text(level: Level) -> &'static str {
    level.text()
}

/// 日志文件常驻位置：`<数据目录>/logs/launcher.log`。
fn log_file_path() -> PathBuf {
    aicraft_core::settings::data_dir()
        .join("logs")
        .join("launcher.log")
}

/// 当前本地时间：（文件名用的 `20261027-233500`，可读的 `2026-10-27 23:35:00`）。
///
/// 文件名和正文都用本地时间：导出之后给人看的是「我什么时候导的」，
/// 而不是「UTC 的几点」，和日志正文里的时间戳也对得上。
fn now_local() -> (String, String) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) =
        aicraft_core::logging::parts_with_offset(secs, aicraft_core::logging::local_offset_secs());
    (
        format!("{:04}{:02}{:02}-{:02}{:02}{:02}", y, mo, d, h, mi, s),
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s),
    )
}

/// 设置文件路径（`<exe 旁>/AICraftData/settings.json`，退到 `%APPDATA%\aicraft`）。
fn settings_path() -> PathBuf {
    aicraft_core::settings::default_path()
}

/// 启动时从设置里快照出来的参数，避免后台线程读到改到一半的设置。
struct LaunchOptions {
    max_memory_mb: u32,
    min_memory_mb: Option<u32>,
    extra_jvm_args: Vec<String>,
    java_path: Option<PathBuf>,
}

/// 启动前校准账号：返回 (要用的账号, 提示语, 续期后的账号)。
///
/// 正版会话过期时用 refresh token 续一次，成功就把新账号回传给界面；
/// 续不了则明确退回离线——带着过期令牌进服，服务器只会报一句和本启动器
/// 毫无关系的「验证失败」，比直接说明更折磨人。
fn prepare_launch_account(
    current: Option<&Account>,
    fallback_name: &str,
    client_id: &str,
) -> (Account, Option<String>, Option<Account>) {
    let Some(current) = current else {
        return (Account::offline(fallback_name), None, None);
    };

    let path = store::default_path();
    let mut saved = AccountStore::load_or_default(&path);
    let Some(mut stored) = saved.find(&current.uuid).cloned() else {
        return (current.clone(), None, None);
    };

    if !stored.is_expired(store::now_unix()) {
        return (current.clone(), None, None);
    }

    if client_id.trim().is_empty() || !stored.can_refresh() {
        return (
            Account::offline(fallback_name),
            Some(format!(
                "{} 的正版会话已过期，又缺 client id 或 refresh token，没法续期；本次用离线模式",
                stored.name
            )),
            None,
        );
    }

    match stored.refresh(&auth::CurlHttp, client_id) {
        Ok(account) => {
            saved.upsert(stored);
            let note = match saved.save(&path) {
                Ok(()) => format!("正版会话已过期，已自动续期（{}）", account.username),
                Err(e) => format!("续期成功，但保存账号失败：{}", e),
            };
            let account = account.to_account();
            (account.clone(), Some(note), Some(account))
        }
        Err(e) => (
            Account::offline(fallback_name),
            Some(format!(
                "自动续期失败：{}；本次用离线模式",
                describe_auth_error(e)
            )),
            None,
        ),
    }
}

/// 启动一共分几步。界面按它算比例，所以这里是唯一的刻度来源。
///
/// 第一步在 `start_launch` 里发（续正版会话），剩下几步在 `do_launch` 里。
const LAUNCH_STEPS: usize = 6;

/// 报告「启动走到第几步」。步骤编号从 1 开始。
fn report_launch_step(tx: &Sender<TaskMsg>, step: usize, label: &str) {
    let _ = tx.send(TaskMsg::LaunchPhase {
        step,
        total: LAUNCH_STEPS,
        label: label.to_string(),
    });
}

/// 执行启动。返回游戏是否正常退出。
fn do_launch(
    game_dir: &Path,
    entry: &VersionEntry,
    account: &Account,
    opts: &LaunchOptions,
    mode: aicraft_core::InstanceMode,
    launcher_mode: aicraft_core::LauncherMode,
    tx: &Sender<TaskMsg>,
) -> Result<(), String> {
    let log = |s: String| {
        let _ = tx.send(TaskMsg::Log(s));
    };

    log(format!("加载 {}", entry.name));
    report_launch_step(tx, 2, "读取版本清单…");
    // 走继承链：Forge / NeoForge / Fabric 的实例只声明加载器差异，
    // 主类、库、资源索引都在父版本（原版）里。
    let chain = aicraft_core::version::load_chain_from_dir(game_dir, &entry.name)
        .map_err(|e| format!("读取版本链失败: {}", e))?;
    let legacy = chain.last().and_then(|raw| raw.minecraft_arguments.clone());
    let resolved = resolve::resolve(&chain)
        .map_err(|e| format!("合并失败: {}", e))?
        .with_legacy_arguments(legacy);

    report_launch_step(tx, 3, "挑选 Java 运行时…");
    // Java：设置里指定了就用它，否则按版本需要的 Java 版本自动挑
    let java_path = match &opts.java_path {
        Some(path) if path.is_file() => {
            log(format!("使用设置里指定的 Java：{}", path.display()));
            path.clone()
        }
        Some(path) => {
            return Err(format!("设置里指定的 Java 不存在：{}", path.display()));
        }
        None => {
            let needed = resolved.java_version.major_version.unwrap_or(17);
            let runtimes = java::detect_all();
            let (runtime, choice) = java::pick_java_choice(&runtimes, needed)
                .ok_or_else(|| format!("找不到满足 Java {} 的运行时", needed))?;
            if let Some(notice) = choice.notice() {
                log(notice);
            }
            log(format!(
                "使用 Java {} ({})",
                runtime.major_version,
                runtime.path.display()
            ));
            runtime.path.clone()
        }
    };

    // 客户端 jar
    let jar_id = resolved
        .jar_id
        .clone()
        .unwrap_or_else(|| resolved.id.clone());
    let candidate = game_dir
        .join("versions")
        .join(&jar_id)
        .join(format!("{}.jar", jar_id));
    let client_jar = if candidate.is_file() {
        Some(candidate)
    } else {
        None
    };

    let mut cfg = LaunchConfig::new(game_dir);
    cfg = cfg.with_instance_mode(game_dir, &entry.name, mode);
    // 第三方账号必须挂 authlib-injector，否则游戏会拿这台皮肤站的令牌
    // 去问 Mojang，得到一句和本启动器毫无关系的「验证失败」。
    if let Some(root) = &account.api_root {
        // injector 的位置要和下载它时用的目录一致（登录流程里用账号目录），
        // 否则下载完这边还是找不到。
        let injector =
            aicraft_core::auth::yggdrasil::injector_path(&aicraft_core::settings::accounts_dir());
        if !injector.is_file() {
            return Err(format!(
                "缺失 authlib-injector：{}。去「账户 → 第三方登录」重新登录一次会自动下好。",
                injector.display()
            ));
        }
        log(format!("第三方认证：{}", root));
        cfg.injected_jvm_args
            .push(aicraft_core::auth::yggdrasil::agent_argument(
                &injector, root,
            ));
    }
    cfg.client_jar = client_jar;
    cfg.max_memory_mb = Some(opts.max_memory_mb);
    cfg.min_memory_mb = opts.min_memory_mb;
    cfg.extra_jvm_args = opts.extra_jvm_args.clone();
    cfg.natives_dir = game_dir
        .join("versions")
        .join(&entry.name)
        .join("natives-lib");

    log(format!(
        "实例模式: {}（游戏目录 {}）",
        mode.as_str(),
        cfg.game_dir.display()
    ));

    report_launch_step(tx, 4, "拼装启动参数…");
    let plan = launch::build_plan(&resolved, account, &cfg);

    // natives
    report_launch_step(tx, 5, "解压游戏原生库…");
    let ctx = RuleContext::current();
    let natives = launch::natives_to_extract(&resolved, &ctx, &cfg);
    if !natives.is_empty() {
        // 落点听计划的：26.2+ 的清单把 java.library.path 写成
        // `${natives_directory}/java`，dll 就得放进那一层。
        log(format!("解压 natives -> {}", plan.natives_dir.display()));
        aicraft_core::extract_natives(&natives, &plan.natives_dir)
            .map_err(|e| format!("natives 解压失败: {}", e))?;
    }

    report_launch_step(tx, 6, "拉起游戏进程…");
    log("启动中…".to_string());
    let mut cmd = aicraft_core::spawn::silent(java_path);
    for a in plan.to_argv() {
        cmd.arg(a);
    }
    // 进程工作目录跟着实例走，避免模组里的相对路径写到根目录
    cmd.current_dir(&cfg.game_dir);

    let mut child = cmd.spawn().map_err(|e| format!("启动失败: {}", e))?;
    let pid = child.id();

    // 先把 pid 交出去：按设置，启动器可能马上就要退出，
    // 而「等游戏结束再回来」得靠这个 pid 交给别的进程去做。
    let _ = tx.send(TaskMsg::GameRunning(pid));

    if launcher_mode != aicraft_core::LauncherMode::KeepOpen {
        // 等窗口真的出来再让界面关。进程起来只是一瞬间的事，
        // 玩家眼里「还没反应」的那十几秒全在窗口出现之前。
        let shown = wait_for_game_window(pid);
        if shown {
            log("游戏窗口已经出来了，启动器按设置退出".to_string());
        } else {
            log("一直没等到游戏窗口，启动器先留着".to_string());
        }
        let _ = tx.send(TaskMsg::GameWindow { pid, shown });
        // 不等了。丢下的子进程不会跟着启动器一起退出。
        return Ok(());
    }

    let status = child.wait().map_err(|e| format!("等待进程失败: {}", e))?;

    if status.success() {
        log("游戏正常退出".to_string());
        Ok(())
    } else {
        Err(format!("游戏异常退出: {}", status))
    }
}

/// 把版本 JSON 解析成可校验的描述。
///
/// `do_verify` / `do_repair` / `do_launch` 都需要这一步，
/// 抽出来避免三处重复同一段解析。
///
/// 走的是**继承链**：装完 Forge / NeoForge / Fabric 之后，子版本的清单里
/// 只有加载器那点差异，主类、库、资源索引全在父版本（原版）里。
/// 只解析子版本会得到一份缺 jar、缺 assetIndex 的半成品。
fn load_resolved(
    game_dir: &Path,
    entry: &VersionEntry,
) -> Result<aicraft_core::ResolvedVersion, String> {
    let chain = aicraft_core::version::load_chain_from_dir(game_dir, &entry.name)
        .map_err(|e| format!("读取版本链失败: {}", e))?;
    // 旧格式的 `minecraftArguments` 只取链尾（用户实际点的那个版本），
    // 父版本的原版清单里通常也有，但那属于「合并后被覆盖」的字段。
    let legacy = chain.last().and_then(|raw| raw.minecraft_arguments.clone());
    resolve::resolve(&chain)
        .map_err(|e| format!("合并失败: {}", e))
        .map(|v| v.with_legacy_arguments(legacy))
}

/// 执行资源校验。
fn do_verify(game_dir: &Path, entry: &VersionEntry, tx: &Sender<TaskMsg>) -> Result<(), String> {
    let send = |s: String| {
        let _ = tx.send(TaskMsg::Log(s));
    };

    let resolved = load_resolved(game_dir, entry)?;

    let ctx = RuleContext::current();
    let report = aicraft_core::verify_version(
        game_dir,
        &resolved,
        &ctx,
        aicraft_core::VerifyMode::Existence,
    );

    send(format!(
        "检查 {} 个文件，assets 正常 {}，库正常 {}",
        report.checked, report.assets_ok, report.libs_ok
    ));

    if report.is_clean() {
        send("✅ 全部完整".to_string());
    } else {
        send(format!("⚠ 发现 {} 个问题", report.total_problems()));
        for item in report.problems.iter().take(10) {
            send(format!(
                "  {} {}",
                item.reason.as_str(),
                item.path.display()
            ));
        }
        if report.problems.len() > 10 {
            send(format!("  … 其余 {} 项", report.problems.len() - 10));
        }
    }

    // 数目回给界面：有缺失就弹一句「要不要补全」，没有就什么也别做。
    let _ = tx.send(TaskMsg::Verified {
        version: entry.name.clone(),
        problems: report.total_problems(),
    });
    Ok(())
}

/// 校验并补全缺失文件。
fn do_repair(
    game_dir: &Path,
    entry: &VersionEntry,
    mirror: &Mirror,
    concurrency: usize,
    limit_kbps: Option<u32>,
    tx: &Sender<TaskMsg>,
) -> Result<(), String> {
    let send = |s: String| {
        let _ = tx.send(TaskMsg::Log(s));
    };

    let resolved = load_resolved(game_dir, entry)?;

    // 先确认 curl 可用：它是唯一的传输后端，缺了就不必白跑一遍校验
    if !aicraft_core::transport::curl_available() {
        return Err("找不到 curl.exe，无法下载。Windows 10 1803 及以上应自带。".to_string());
    }

    send("正在校验…".to_string());
    let ctx = RuleContext::current();
    let report = aicraft_core::verify_version(
        game_dir,
        &resolved,
        &ctx,
        aicraft_core::VerifyMode::Existence,
    );

    if report.is_clean() {
        send("✅ 全部完整，无需下载".to_string());
        return Ok(());
    }

    send(format!("发现 {} 个问题，开始下载", report.total_problems()));

    // 下载前先清掉损坏文件：拿坏文件做续传只会拼出更坏的结果
    for item in &report.problems {
        let _ = aicraft_core::repair::prepare_for_download(item);
    }

    let total = report.problems.len();
    let tx_for_progress = tx.clone();
    let progress_cb = move |p: aicraft_core::Progress| match p {
        aicraft_core::Progress::Start { .. } => {}
        aicraft_core::Progress::ItemDone {
            index,
            outcome,
            path,
            ..
        } => {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string();
            let desc = match &outcome {
                aicraft_core::RepairOutcome::Downloaded { bytes } => {
                    format!("完成 {} KB", bytes / 1024)
                }
                aicraft_core::RepairOutcome::Skipped => "已存在".to_string(),
                aicraft_core::RepairOutcome::NoUrl => "无下载地址".to_string(),
                aicraft_core::RepairOutcome::Failed { message } => {
                    format!("失败: {}", message)
                }
            };
            let _ = tx_for_progress.send(TaskMsg::Log(format!("  {} {}", name, desc)));
            let _ = tx_for_progress.send(TaskMsg::Progress {
                done: index + 1,
                total,
                bytes: 0,
            });
        }
        aicraft_core::Progress::Bytes { total: bytes } => {
            let _ = tx_for_progress.send(TaskMsg::Progress {
                done: 0,
                total,
                bytes,
            });
        }
    };

    let config = aicraft_core::RepairConfig {
        concurrency,
        dry_run: false,
        mirror: *mirror,
        limit_kbps,
    };
    if let Some(kbps) = limit_kbps {
        send(format!(
            "下载限速 {}（总带宽，分给 {} 路并发）",
            aicraft_core::speed::describe(Some(kbps)),
            concurrency
        ));
    }
    let result = aicraft_core::repair_with_progress(&report.problems, &config, progress_cb);

    send(format!(
        "下载完成：成功 {}，跳过 {}，失败 {}，共 {:.1} MB",
        result.downloaded,
        result.skipped,
        result.failed,
        result.bytes as f64 / 1_048_576.0
    ));

    if result.failed > 0 {
        send("失败明细:".to_string());
        for r in &result.results {
            if let aicraft_core::RepairOutcome::Failed { message } = &r.outcome {
                send(format!("  {} — {}", r.item.path.display(), message));
            }
        }
        return Err(format!("{} 个文件下载失败", result.failed));
    }

    Ok(())
}

/// 登录成功后统一收尾：过完 Xbox → Minecraft 三跳、拿到档案、写进账号文件。
/// 用微软令牌换出游戏账户，并把每跳报给界面。
///
/// 换 Xbox Live → 换 XSTS → 登录 Minecraft → 读档案这四步都是网络往返，
/// 加起来能有几秒；一声不吭的话界面就只剩一句「正在连接微软…」。
fn finish_login(
    http: &dyn auth::Http,
    tokens: &msa::MsaTokens,
    tx: &Sender<LoginMsg>,
) -> Result<aicraft_core::MinecraftAccount, String> {
    let account = auth::complete_login_with(http, tokens, |phase| {
        let _ = tx.send(LoginMsg::Phase(phase));
    })
    .map_err(describe_auth_error)?;

    let path = store::default_path();
    let mut accounts = AccountStore::load_or_default(&path);
    accounts.upsert(StoredAccount::from_login(&account, tokens));
    accounts
        .save(&path)
        .map_err(|e| format!("保存账号失败: {}", e))?;

    Ok(account)
}

/// 把登录错误翻成玩家能照做的话。技术细节仍保留在原文里，方便排查。
fn describe_auth_error(error: auth::AuthError) -> String {
    use auth::AuthErrorKind;

    let hint = match error.kind {
        AuthErrorKind::ClientNotPublic => {
            "去 Azure 应用 → 身份验证，勾上「允许公共客户端流」"
        }
        AuthErrorKind::NoXboxProfile => "先打开 https://www.xbox.com 登录一次，把 Xbox 档案建出来",
        AuthErrorKind::NoXboxRegion => "需要把微软账号的地区改到支持 Xbox Live 的地方",
        AuthErrorKind::ChildAccount => "让家长在 account.microsoft.com/family 里同意",
        AuthErrorKind::NotEntitled => "需要先在 Microsoft Store 或 minecraft.net 买一份 Java 版",
        AuthErrorKind::AppNotApproved => {
            "Mojang 还没批准这个 Azure 应用调用 Minecraft 服务，审批通过前请用离线模式（https://aka.ms/AppRegInfo）"
        }
        AuthErrorKind::Network => "检查网络；启动器走系统 curl，代理也要跟着设",
        AuthErrorKind::Timeout | AuthErrorKind::Expired => "这次授权超时了，再点一次登录就好",
        AuthErrorKind::Cancelled => "你在授权页点了拒绝",
        _ => "",
    };

    if hint.is_empty() {
        error.to_string()
    } else {
        format!("{}（{}）", error, hint)
    }
}

/// 猜一个默认 .minecraft 路径。
/// 弹出系统自带的文件夹选择框，返回 Ok(Some(路径)) 表示选中、Ok(None) 表示取消。
///
/// 为什么不直接引一个对话框 crate：本机连 crates.io 都拉不到，仓库要能就地编译，
/// 而 egui 没有内置的原生文件对话框。借 PowerShell 的 `FolderBrowserDialog`
/// 来干活，和 core 里借系统 curl 干 TLS 是同一种取舍。
#[cfg(windows)]
fn pick_folder_blocking(initial: &str) -> Result<Option<PathBuf>, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    // 别让对话框顺带闪出一个控制台窗口
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // PowerShell 单引号字符串里，单引号本身要写成两个
    let initial = initial.replace('\'', "''");
    let preselect = if initial.is_empty() {
        String::new()
    } else {
        format!(
            "if (Test-Path -LiteralPath '{initial}' -PathType Container) {{ $dlg.SelectedPath = '{initial}' }}"
        )
    };
    let script = format!(
        "[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false\n\
         Add-Type -AssemblyName System.Windows.Forms\n\
         $dlg = New-Object System.Windows.Forms.FolderBrowserDialog\n\
         $dlg.Description = '选择 Minecraft 游戏目录'\n\
         $dlg.ShowNewFolderButton = $true\n\
         {preselect}\n\
         if ($dlg.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ [Console]::Out.Write($dlg.SelectedPath) }}"
    );

    let output = aicraft_core::spawn::silent("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("无法启动文件夹选择框: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("文件夹选择框异常退出（代码 {:?}）", output.status.code())
        } else {
            format!("文件夹选择框出错: {}", stderr)
        });
    }

    let picked = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if picked.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(picked)))
    }
}

#[cfg(not(windows))]
fn pick_folder_blocking(_initial: &str) -> Result<Option<PathBuf>, String> {
    Err("系统文件夹选择框目前只在 Windows 上实现".to_string())
}

/// 弹系统「另存为」对话框，让用户自己挑日志存哪。
///
/// 和选目录一样走 PowerShell + WinForms（没有额外依赖），对话框阻塞的
/// 是后台线程，所以调用方必须从 `std::thread::spawn` 里调。
#[cfg(windows)]
/// 挑一个整合包文件。
///
/// 过滤器和 `packui::looks_like_pack` 保持一致：`.mrpack` 与 `.zip`。
/// 别的格式（比如 CurseForge 的 `.zip` 我们其实读得了清单，但装不出正确的
/// 结果）不在这一版的范围里。
fn pick_pack_blocking(initial_dir: &Path) -> Result<Option<PathBuf>, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let initial_dir = initial_dir.display().to_string().replace('\'', "''");
    let script = format!(
        "[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false\n\
         Add-Type -AssemblyName System.Windows.Forms\n\
         $dlg = New-Object System.Windows.Forms.OpenFileDialog\n\
         $dlg.Title = '选择整合包'\n\
         $dlg.Filter = '整合包 (*.mrpack;*.zip)|*.mrpack;*.zip|所有文件 (*.*)|*.*'\n\
         $dlg.CheckFileExists = $true\n\
         if (Test-Path -LiteralPath '{initial_dir}' -PathType Container) {{ $dlg.InitialDirectory = '{initial_dir}' }}\n\
         if ($dlg.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ [Console]::Out.Write($dlg.FileName) }}"
    );

    let output = aicraft_core::spawn::silent("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("无法启动文件选择对话框: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("文件选择对话框异常退出（代码 {:?}）", output.status.code())
        } else {
            format!("文件选择对话框出错: {}", stderr)
        });
    }

    let picked = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if picked.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(picked)))
    }
}

#[cfg(not(windows))]
fn pick_pack_blocking(_initial_dir: &Path) -> Result<Option<PathBuf>, String> {
    Err("系统文件选择框目前只在 Windows 上实现".to_string())
}

/// 挑一个 java / javaw 可执行文件。
///
/// 只认 `java.exe` / `javaw.exe`：这两个才是启动器能 exec 的东西。
/// 玩家手头的 JDK 目录里 `bin\` 下就这两个，让他进一级比记路径轻松。
fn pick_java_blocking(initial_dir: &str) -> Result<Option<PathBuf>, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let initial_dir = initial_dir.replace('\'', "''");
    let script = format!(
        "[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false\n\
         Add-Type -AssemblyName System.Windows.Forms\n\
         $dlg = New-Object System.Windows.Forms.OpenFileDialog\n\
         $dlg.Title = '选择 Java 可执行文件（JDK 安装目录下的 bin\\javaw.exe）'\n\
         $dlg.Filter = 'Java 可执行文件 (java.exe;javaw.exe)|java.exe;javaw.exe|所有文件 (*.*)|*.*'\n\
         $dlg.CheckFileExists = $true\n\
         if (Test-Path -LiteralPath '{initial_dir}' -PathType Container) {{ $dlg.InitialDirectory = '{initial_dir}' }}\n\
         if ($dlg.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ [Console]::Out.Write($dlg.FileName) }}"
    );

    let output = aicraft_core::spawn::silent("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("无法启动文件选择对话框: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("文件选择对话框异常退出（代码 {:?}）", output.status.code())
        } else {
            format!("文件选择对话框出错: {}", stderr)
        });
    }

    let picked = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if picked.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(picked)))
    }
}

#[cfg(not(windows))]
fn pick_java_blocking(_initial_dir: &str) -> Result<Option<PathBuf>, String> {
    Err("系统文件选择框目前只在 Windows 上实现".to_string())
}

fn pick_save_blocking(initial_dir: &Path, file_name: &str) -> Result<Option<PathBuf>, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // PowerShell 单引号字符串里，单引号本身要写成两个
    let initial_dir = initial_dir.display().to_string().replace('\'', "''");
    let file_name = file_name.replace('\'', "''");
    let script = format!(
        "[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false\n\
         Add-Type -AssemblyName System.Windows.Forms\n\
         $dlg = New-Object System.Windows.Forms.SaveFileDialog\n\
         $dlg.Title = '导出日志'\n\
         $dlg.Filter = '日志文件 (*.log)|*.log|文本文件 (*.txt)|*.txt|所有文件 (*.*)|*.*'\n\
         $dlg.DefaultExt = 'log'\n\
         $dlg.AddExtension = $true\n\
         $dlg.OverwritePrompt = $true\n\
         $dlg.FileName = '{file_name}'\n\
         if (Test-Path -LiteralPath '{initial_dir}' -PathType Container) {{ $dlg.InitialDirectory = '{initial_dir}' }}\n\
         if ($dlg.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ [Console]::Out.Write($dlg.FileName) }}"
    );

    let output = aicraft_core::spawn::silent("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("无法启动保存对话框: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("保存对话框异常退出（代码 {:?}）", output.status.code())
        } else {
            format!("保存对话框出错: {}", stderr)
        });
    }

    let picked = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if picked.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(picked)))
    }
}

#[cfg(not(windows))]
fn pick_save_blocking(_initial_dir: &Path, _file_name: &str) -> Result<Option<PathBuf>, String> {
    Err("系统保存对话框目前只在 Windows 上实现".to_string())
}

fn default_game_dir() -> String {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let p = PathBuf::from(appdata).join(".minecraft");
        if p.is_dir() {
            return p.display().to_string();
        }
    }
    "E:\\Minecraft\\.minecraft".to_string()
}

/// 启动时用哪个游戏目录：设置里存过的优先，没存过才猜默认位置。
///
/// 之前这里直接调 `default_game_dir()`，导致用户在设置页选好的目录
/// 每次重启都被忘掉——目录没有跟着设置一起存，自然也就无从读回。
fn initial_game_dir(settings: &Settings) -> String {
    let saved = settings.game_dir.trim();
    if !saved.is_empty() {
        return saved.to_string();
    }
    default_game_dir()
}

/// 离线名字：存过就用存的，没存过就是空 —— 不替玩家编一个。
fn initial_username(settings: &Settings) -> String {
    settings.offline_name.trim().to_string()
}

/// 名字能不能存。只拦一件事：空的。名字是空的，游戏里就没有身份。
///
/// 格式不对不拦 —— 玩家想叫什么就叫什么，我们只负责提醒他可能遇到的麻烦。
fn offline_name_usable(name: &str) -> bool {
    !name.trim().is_empty()
}

/// 账号是哪一类：列表里那行小字与日志共用同一套说法。
/// 左侧导航的一项：收起时只画一个字并挂悬停提示，展开时是整行文字。
///
/// 抽出来是因为导航现在有两层（一级四页、版本管理下面三项），
/// 四段一模一样的「点没点、要不要悬停提示」没有必要各写一遍。
fn nav_entry(ui: &mut egui::Ui, symbol: &str, text: &str, selected: bool, collapsed: bool) -> bool {
    let resp = if collapsed {
        theme::nav_item_icon(ui, symbol, selected)
    } else {
        theme::nav_item(ui, text, selected)
    };
    let resp = if collapsed {
        resp.on_hover_text(text)
    } else {
        resp
    };
    let clicked = resp.clicked();
    ui.add_space(2.0);
    clicked
}

fn account_kind(stored: &StoredAccount) -> &'static str {
    if stored.offline {
        "离线"
    } else if stored.is_third_party() {
        "第三方"
    } else {
        "正版"
    }
}

/// 名字不像 Minecraft 玩家名时给一句提醒，但不阻止保存。
///
/// Minecraft 的玩家名只认 `[A-Za-z0-9_]`、3~16 个字符。离线名字不走 Mojang
/// 校验，写成什么样都能进单人游戏，但皮肤加载、部分服务器的白名单、命令里的
/// 选择器都可能认不出来 —— 所以只提醒，不强制改。
fn offline_name_warning(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if name.chars().count() > 16 {
        return Some(format!(
            "{} 个字符，超过 16 个了。Minecraft 的玩家名最长 16 个字符。",
            name.chars().count()
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Some(
            "Minecraft 的玩家名只用字母、数字和下划线。这样也能进单人游戏，但皮肤、\
             部分服务器和命令可能认不出这个名字。"
                .to_string(),
        );
    }
    None
}

/// 无边框窗口没有系统给的拖拽边，自己在四周铺一圈热区：
/// 指针靠近边缘时换缩放光标，按下就交给系统开始缩放。
fn handle_window_resize(ctx: &egui::Context) {
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
        return;
    }
    let Some(pointer) = ctx.pointer_latest_pos() else {
        return;
    };
    // egui 0.37 把 screen_rect 改名成了 content_rect
    let screen = ctx.input(|i| i.content_rect());
    const EDGE: f32 = 7.0;

    let near_left = pointer.x <= screen.left() + EDGE;
    let near_right = pointer.x >= screen.right() - EDGE;
    let near_top = pointer.y <= screen.top() + EDGE;
    let near_bottom = pointer.y >= screen.bottom() - EDGE;

    use egui::viewport::ResizeDirection as Dir;
    let (cursor, direction) = match (near_left, near_right, near_top, near_bottom) {
        (true, _, true, _) => (egui::CursorIcon::ResizeNwSe, Dir::NorthWest),
        (_, true, true, _) => (egui::CursorIcon::ResizeNeSw, Dir::NorthEast),
        (true, _, _, true) => (egui::CursorIcon::ResizeNeSw, Dir::SouthWest),
        (_, true, _, true) => (egui::CursorIcon::ResizeNwSe, Dir::SouthEast),
        (true, _, _, _) => (egui::CursorIcon::ResizeHorizontal, Dir::West),
        (_, true, _, _) => (egui::CursorIcon::ResizeHorizontal, Dir::East),
        (_, _, true, _) => (egui::CursorIcon::ResizeVertical, Dir::North),
        (_, _, _, true) => (egui::CursorIcon::ResizeVertical, Dir::South),
        _ => return,
    };

    ctx.set_cursor_icon(cursor);
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
    }
}

/// App 自己的辅助方法（不属于 eframe::App trait）。
impl App {
    /// 自绘标题栏：左边是牌子，右边是账号状态与窗口按钮。
    /// 空白处可拖动窗口。
    fn title_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let bar_rect = ui.max_rect();
        let drag_id = ui.scope_id().with("titlebar-drag");

        // 先铺一整条拖拽热区；按钮在那之后加入，同一层里更靠上，
        // 点按钮时不会误触拖动。
        let drag = ui.interact(bar_rect, drag_id, egui::Sense::click_and_drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        // 纹理句柄是 Arc，clone 一下就解开了对 self 的借用
        let icon_tex = self.icon_tex.clone();

        ui.horizontal_centered(|ui| {
            // 左端那枚标记就是应用图标本身：窗口里外看起来才是同一个东西。
            // 万一纹理没传上来（尺寸对不上），退回一个小圆点，标题栏不至于空着。
            match &icon_tex {
                Some(tex) => {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                    ui.painter().image(
                        tex.id(),
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
                None => {
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.5, theme::ACCENT);
                }
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new("AICraft")
                    .size(13.5)
                    .strong()
                    .color(theme::ACCENT_STRONG),
            );
            ui.label(
                RichText::new("Launcher")
                    .size(13.5)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new("Minecraft Java")
                    .size(10.5)
                    .color(theme::TEXT_WEAK),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // 从右往左：关闭、最小化
                if theme::window_button(ui, theme::WindowIcon::Close, true).clicked() {
                    aicraft_core::audit::action("标题栏：关闭启动器");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if theme::window_button(ui, theme::WindowIcon::Minimize, false).clicked() {
                    aicraft_core::audit::detail("标题栏：最小化");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }

                ui.add_space(12.0);

                // 账户入口：一个胶囊，点开就是一个整页的账户设置。
                // 没配置过就说「未配置」，不假装自己有个 Steve。
                //
                // 只在主页出现。进了账号页再看右上角那个名字是多余的：
                // 那一页左边已经写着「当前账户」，点开的又正是它自己。
                if !self.account_open {
                    let (label, muted) = match &self.account {
                        Some(account) => (account.username.clone(), false),
                        None => {
                            let name = self.username.trim();
                            if name.is_empty() {
                                ("未配置".to_string(), true)
                            } else {
                                (name.to_string(), false)
                            }
                        }
                    };
                    // 纹理要在这之前取好：闭包里还要调 `open_account`，
                    // 不能一边借着 `skin_tex` 一边改 `self`。
                    let avatar_skin = self
                        .account
                        .as_ref()
                        .and_then(|account| self.skin_tex.get(&account.uuid))
                        .cloned();
                    if theme::account_chip(ui, &label, muted, avatar_skin.as_ref()).clicked() {
                        aicraft_core::audit::action("右上角：打开账户");
                        self.open_account();
                    }
                }
            });
        });

        // 一条极细的分隔线，替代系统标题栏的下边框
        ui.painter().hline(
            bar_rect.x_range(),
            bar_rect.bottom(),
            egui::Stroke::new(1.0, theme::BORDER),
        );
    }
}

impl eframe::App for App {
    // eframe 0.37 的 App trait 只要求实现 `ui`；
    // 需要每帧先跑的准备工作放在 `logic` 里（它是可选的）。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 测速回来自动重画一次，设置页里的「正在测速…」才能变成实际结果
        if let Some(rx) = &self.mirror_probe_rx {
            if matches!(
                rx.try_recv(),
                Ok(()) | Err(std::sync::mpsc::TryRecvError::Disconnected)
            ) {
                self.mirror_probe_rx = None;
                ctx.request_repaint();
            }
        }
        // 无边框窗口没有系统给的拖拽边，自己处理贴边缩放
        handle_window_resize(ctx);

        self.poll();
        self.poll_manifest();
        self.poll_server();
        self.poll_mod_detail();
        self.poll_modrinth();
        self.poll_pack();
        self.poll_skins(ctx);
        self.kick_off_skins(ctx);
        self.poll_mod_icons(ctx);
        self.kick_off_mod_icons(ctx);
        // 扫完 Java 要立刻重画一次，不然「正在扫描…」会一直停在屏幕上
        // 等到下一次有别的重绘才消失。
        if self.java_scan.poll() {
            ctx.request_repaint();
        }
        // 游戏已经起来了、设置里又选了「关掉启动器」：在这儿发关闭命令。
        if self.close_after_launch {
            self.close_after_launch = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.poll_login();
        self.poll_folder_pick();
        self.poll_server_folder();
        self.poll_java_pick();
        self.poll_export_pick();
        // 有后台任务、登录在等授权、或选择框开着时保持重绘
        let waiting = self.rx.is_some()
            || self.manifest_rx.is_some()
            || self.modrinth_rx.is_some()
            || self.detail_rx.is_some()
            || self.pack_rx.is_some()
            || self.java_scan.is_running()
            || self.login.as_ref().map(|l| l.busy).unwrap_or(false)
            || self.folder_rx.is_some()
            || self.server_folder_rx.is_some()
            || self.java_rx.is_some()
            || self.export_rx.is_some()
            || self.mirror_probe_rx.is_some();
        if waiting {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let busy = self.busy.is_some();

        // 图标纹理会用到 painter，只能等第一帧的 ctx 就绪后再传；
        // 传过之后 `icon_tex` 就是 Some，不会重复上传。
        if self.icon_tex.is_none() {
            self.icon_tex = app_icon_texture(ui.ctx());
        }

        // 自绘标题栏：系统栏被关掉了，拖动、最小化、关闭都在这里接。
        egui::Panel::top("titlebar")
            .exact_size(38.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(12, 0)),
            )
            .show(ui, |ui| {
                self.title_bar(ui);
            });

        // 拖进来的文件当场接住：`dropped_files` 只在松手那一帧有值。
        for path in packui::dropped(ui.ctx()) {
            self.take_pack_file(path);
        }
        if let Some(instance) =
            packui::modal(ui.ctx(), &mut self.pack, Path::new(&self.game_dir), busy)
        {
            self.start_pack_import(instance);
        }

        self.show_wizard(ui.ctx(), busy);
        self.delete_confirm_modal(ui.ctx(), self.busy.is_some());
        self.repair_confirm_modal(ui.ctx(), self.busy.is_some());
        self.reset_confirm_modal(ui.ctx(), self.busy.is_some());
        self.missing_parent_modal(ui.ctx(), self.busy.is_some());
        self.account_remove_modal(ui.ctx(), self.busy.is_some());
        // 进度弹窗画在最后面（Foreground 层），压住正文但只占底部一条。
        self.progress_panel(ui.ctx());

        // 「账户」页同样盖住标题栏以下的全部，包括主导航。
        // 它排在最前面：管理与账户不会同时开着，但先画谁决定了视觉层次。
        if self.account_open {
            self.account_page(ui, busy);
        }

        // 「管理」是一层盖住整个窗口（含左侧主导航）的视图：
        // 进来之后视线只在这一份实例上，返回才回到版本列表。
        if self.manage.is_some() {
            let mut state = self.manage.take().expect("上面刚判断过");
            let action = manage::page(
                ui,
                &mut state,
                Path::new(&self.game_dir),
                &mut self.manage_icons,
                busy,
            );
            self.manage = Some(state);
            if let Some(action) = action {
                self.handle_manage(action);
            }
            // 这一帧刚登记了要取的模组图标：下一帧 `logic` 才去挖 jar，
            // 得先把界面叫醒——列表静止时 egui 会直接睡，图标就永远是灰块。
            if self.manage_icons.has_pending() {
                ui.ctx().request_repaint();
            }
        }

        if self.manage.is_none() && !self.account_open {
            // 左侧导航：主页 / 版本管理（可展开）/ 设置。
            // 收起后只剩图标条，正文可以更宽——状态存在设置里，重启还记得。
            let collapsed = self.settings.nav_collapsed;
            egui::Panel::left("nav")
                .exact_size(if collapsed { 52.0 } else { 146.0 })
                .show_separator_line(false)
                .frame(
                    egui::Frame::NONE
                        .fill(theme::PANEL)
                        .inner_margin(egui::Margin::symmetric(if collapsed { 6 } else { 10 }, 10)),
                )
                .show(ui, |ui| {
                    // 一级：主页 / 版本管理 / 资源下载 / 设置。
                    // 「版本管理」不是可展开的分组，而是跳进它自己的二级目录：
                    // 进了目录列表 / 版本列表 / 下载版本 任意一页，这一栏整体换成
                    // 那三项，顶部一个「‹ 版本管理」回到一级。
                    //
                    // 收起成 52px 图标条时不分层——一行只放得下一个字的地方再分层，
                    // 玩家看到的就只剩两次点击。
                    let in_versions = self.page.in_versions_group();
                    let in_resources = self.page.in_resources_group();
                    let in_more = self.page.in_more_group();
                    let level_two = (in_versions || in_resources || in_more) && !collapsed;

                    // 收起时只画一个字：用汉字而不是 emoji，免得字体缺字变方框
                    if level_two {
                        // 两套二级目录共用一个画法：一个返回行 + 三项。
                        // 「资源下载」那三项不是页面而是同一页的三种内容，
                        // 所以下标存进 `resources.kind` 而不是 `Page`。
                        if in_more {
                            if theme::nav_back(ui, "更多功能").clicked() {
                                aicraft_core::audit::detail("导航：离开「更多功能」目录");
                                self.page = Page::Home;
                            }
                            ui.add_space(6.0);
                            if theme::nav_item_sub(ui, "服务端管理", self.page == Page::Servers)
                                .clicked()
                                && self.page != Page::Servers
                            {
                                aicraft_core::audit::detail("导航：切到「服务端管理」");
                                self.open_servers();
                            }
                        } else if in_resources {
                            if theme::nav_back(ui, "资源下载").clicked() {
                                aicraft_core::audit::detail("导航：离开「资源下载」目录");
                                self.page = Page::Home;
                            }
                            ui.add_space(6.0);
                            for kind in ProjectKind::ALL {
                                let picked = self.resources.kind == kind;
                                let clicked =
                                    theme::nav_item_sub(ui, kind.label(), picked).clicked();
                                ui.add_space(2.0);
                                if clicked && !picked {
                                    aicraft_core::audit::detail(format!(
                                        "导航：切到「{}」",
                                        kind.label()
                                    ));
                                    self.resources.switch_kind(kind);
                                    self.start_search_mods();
                                }
                            }
                        } else {
                            if theme::nav_back(ui, "版本管理").clicked() {
                                aicraft_core::audit::detail("导航：离开「版本管理」目录");
                                self.page = Page::Home;
                            }
                            ui.add_space(6.0);
                            for (page, text) in [
                                (Page::Dirs, "目录列表"),
                                (Page::Versions, "版本列表"),
                                (Page::Download, "下载版本"),
                            ] {
                                let clicked =
                                    theme::nav_item_sub(ui, text, self.page == page).clicked();
                                ui.add_space(2.0);
                                if clicked {
                                    aicraft_core::audit::detail(format!("导航：切到「{}」", text));
                                    // 「下载版本」还要顺手拉一次清单，走 open_versions
                                    if page == Page::Download {
                                        self.open_versions();
                                    } else {
                                        self.page = page;
                                    }
                                }
                            }
                        }
                    } else {
                        if nav_entry(ui, "首", "主页", self.page == Page::Home, collapsed) {
                            aicraft_core::audit::detail("导航：切到「主页」");
                            self.page = Page::Home;
                        }

                        // 收起态没有地方退回二级目录，点它就直接落到版本列表。
                        if nav_entry(ui, "版", "版本管理", in_versions, collapsed) {
                            aicraft_core::audit::detail("导航：进入「版本管理」目录");
                            self.page = Page::Versions;
                        }

                        if nav_entry(ui, "资", "资源下载", in_resources, collapsed) {
                            aicraft_core::audit::detail("导航：进入「资源下载」目录");
                            self.page = Page::Resources;
                            self.resources.switch_kind(ProjectKind::Mod);
                            if self.resources.needs_fetch() {
                                self.start_search_mods();
                            }
                        }

                        if nav_entry(ui, "设", "设置", self.page == Page::Settings, collapsed) {
                            aicraft_core::audit::detail("导航：切到「设置」");
                            self.page = Page::Settings;
                        }

                        if nav_entry(ui, "多", "更多功能", in_more, collapsed) {
                            aicraft_core::audit::detail("导航：进入「更多功能」目录");
                            self.open_servers();
                        }
                    }

                    // 底部（自下而上）：折叠开关 → 忙碌提示 → 下载进度。
                    // 开关贴最下沿，因为它管的是这条导航自己，不该和页面入口抢位置。
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        let toggle = theme::nav_toggle(ui, collapsed);
                        let hint = if collapsed {
                            "展开导航"
                        } else {
                            "收起导航"
                        };
                        if toggle.on_hover_text(hint).clicked() {
                            self.settings.nav_collapsed = !collapsed;
                            self.persist_settings_quietly();
                        }
                    });
                });

            egui::CentralPanel::default()
                .frame(
                    egui::Frame::NONE
                        .fill(theme::BG_BOTTOM)
                        .inner_margin(egui::Margin::symmetric(22, 18)),
                )
                .show(ui, |ui| {
                    theme::paint_background(ui);
                    match self.page {
                        Page::Home => self.home_page(ui, busy),
                        Page::Dirs => self.dirs_page(ui, busy),
                        Page::Versions => self.versions_page(ui, busy),
                        Page::Download => self.download_page(ui, busy),
                        Page::Resources => self.resources_page(ui, busy),
                        Page::Settings => self.settings_page(ui, busy),
                        Page::Servers => self.servers_page(ui, busy),
                    }
                });
        }

        // 拖拽悬停的全屏提示画在最后：它要盖住正文、侧栏和标题栏，
        // 只留一圈虚线告诉玩家「松手就行」。
        packui::drop_overlay(ui.ctx());

        // 无系统边框，自己描一圈 1px 浅灰，免得白窗和浅色壁纸糊在一起
        let frame_rect = ui.max_rect();
        ui.painter().rect_stroke(
            frame_rect,
            0.0,
            egui::Stroke::new(1.0, theme::BORDER),
            egui::StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        account_kind, game_crash_dir, game_log_dir, game_run_dir, log_text, now_local,
        offline_name_usable, offline_name_warning,
    };
    use aicraft_core::auth::store::StoredAccount;

    #[test]
    fn every_account_says_what_kind_it_is() {
        assert_eq!(account_kind(&StoredAccount::offline("星星")), "离线");
        let mut third = StoredAccount::offline("星星");
        third.offline = false;
        third.api_root = Some("https://littleskin.cn/api/yggdrasil".to_string());
        assert_eq!(account_kind(&third), "第三方");
        let mut microsoft = StoredAccount::offline("星星");
        microsoft.offline = false;
        microsoft.api_root = None;
        assert_eq!(account_kind(&microsoft), "正版");
    }

    #[test]
    fn only_an_empty_offline_name_is_refused() {
        assert!(!offline_name_usable(""));
        assert!(!offline_name_usable("   "));
        assert!(offline_name_usable("Steve"));
        assert!(offline_name_usable("苦力怕爱好者"));
    }

    #[test]
    fn unusual_names_get_a_warning_instead_of_a_refusal() {
        // 标准格式：不啰嗦
        assert_eq!(offline_name_warning("Steve"), None);
        assert_eq!(offline_name_warning("Steve_123"), None);
        assert_eq!(offline_name_warning(""), None);

        // 非 ASCII：提醒，但仍然可以存
        let chinese = offline_name_warning("苦力怕爱好者").expect("中文名应该给提醒");
        assert!(chinese.contains("字母、数字和下划线"));
        assert!(offline_name_usable("苦力怕爱好者"));

        // 太长：提醒里带上实际长度
        let long = offline_name_warning(&"a".repeat(17)).expect("超长应该给提醒");
        assert!(long.contains("17"));
        assert!(long.contains("16"));
    }

    #[test]
    fn local_stamp_is_sortable() {
        let (stamp, pretty) = now_local();
        assert_eq!(stamp.len(), 15, "{}", stamp);
        assert_eq!(&stamp[8..9], "-");
        assert_eq!(pretty.len(), 19, "{}", pretty);
        assert_eq!(&pretty[4..5], "-");
    }

    #[test]
    fn log_text_carries_header_and_every_entry() {
        let settings = aicraft_core::settings::Settings::default();
        let text = log_text(&[], &settings, "E:\\game", Some("1.20.1"));
        assert!(text.starts_with("AICraft Launcher 日志\n"));
        assert!(text.contains("游戏目录: E:\\game"));
        assert!(text.contains("当前版本: 1.20.1"));
        assert!(text.contains("----- 日志 -----"));
        // 空日志也要有交代，别只留一个光秃秃的分节标题
        assert!(text.contains("(空)"));

        // 未选版本时也写得出来
        let text = log_text(&[], &settings, "E:\\game", None);
        assert!(text.contains("当前版本: (未选择)"));
    }

    #[test]
    fn isolated_instances_read_logs_from_their_own_folder() {
        let tmp = aicraft_core::testutil::fresh_dir("ui-logs");
        let game = tmp.join("game");
        std::fs::create_dir_all(game.join("versions").join("26.3-生存").join("logs")).unwrap();
        let run = game_run_dir(&game, "26.3-生存", true);
        assert_eq!(run, game.join("versions").join("26.3-生存"));
        assert_eq!(game_log_dir(&run), Some(run.join("logs")));
        // 隔离实例的崩溃报告只认自己那一份
        assert_eq!(game_crash_dir(&run), None);
    }

    #[test]
    fn shared_instances_read_logs_from_the_game_root() {
        let tmp = aicraft_core::testutil::fresh_dir("ui-logs-shared");
        let game = tmp.join("game");
        std::fs::create_dir_all(game.join("logs")).unwrap();
        let run = game_run_dir(&game, "1.20.1", false);
        assert_eq!(run, game);
        assert_eq!(game_log_dir(&run), Some(game.join("logs")));
    }

    #[test]
    fn crash_reports_are_looked_for_in_both_places() {
        let tmp = aicraft_core::testutil::fresh_dir("ui-crash");
        let run = tmp.to_path_buf();
        // 新版：crash-reports/ 里有报告
        std::fs::create_dir_all(run.join("crash-reports")).unwrap();
        assert_eq!(game_crash_dir(&run), Some(run.join("crash-reports")));
        std::fs::write(
            run.join("crash-reports")
                .join("crash-2026-10-30_21.55.12-client.txt"),
            "boom",
        )
        .unwrap();
        assert_eq!(game_crash_dir(&run), Some(run.join("crash-reports")));

        // 老版本：直接扔在游戏根目录，这时要优先认出根目录
        let old = tmp.join("old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("crash-2013-01-01_00.00.00-client.txt"), "boom").unwrap();
        assert_eq!(game_crash_dir(&old), Some(old.clone()));

        // 别的 txt 不算崩溃报告
        let clean = tmp.join("clean");
        std::fs::create_dir_all(&clean).unwrap();
        std::fs::write(clean.join("options.txt"), "fov:70").unwrap();
        assert_eq!(game_crash_dir(&clean), None);
    }

    #[test]
    fn a_missing_game_folder_has_no_logs() {
        let tmp = aicraft_core::testutil::fresh_dir("ui-logs-missing");
        let run = tmp.join("nowhere");
        assert_eq!(game_log_dir(&run), None);
        assert_eq!(game_crash_dir(&run), None);
    }
}
