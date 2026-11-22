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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{RichText, Vec2};

mod blur;
mod dirs;
mod flow;
mod icons;
mod launchview;
mod manage;
mod packui;
mod resources;
mod serverui;
mod settings;
mod theme;
mod versions;
mod wallpaper;
mod wardrobe;
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

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([900.0, 620.0])
        .with_min_inner_size([720.0, 480.0])
        .with_title("AICraft Launcher")
        // 去掉系统标题栏：自己画一条更贴合配色的，拖动/关闭都自己接
        .with_decorations(false)
        // 也不给「可最大化」这个窗口属性。
        //
        // 这一条影响的是真正的窗口样式位（winit 的 `WS_MAXIMIZEBOX`），
        // 而不只是画不画那颗按钮——Windows 的 Aero Snap 正是看它来决定
        // 「拖到屏幕顶边要不要自动铺满」。启动器没有最大化的界面（自绘
        // 标题栏只有最小化与关闭），那这个手势就只剩误触：手一抖甩到顶上，
        // 窗口突然占满整块屏幕，还得再拖回来。
        .with_maximize_button(false);
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
            Ok(Box::new(App::new(&cc.egui_ctx)) as Box<dyn eframe::App>)
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
    /// 整合包下载完了：Ok 带落到盘上的路径，接下来交给导入流程
    PackDownloaded(Result<PathBuf, String>),
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
    /// 启动时发现缺某个主版本的 Java（`major` 是版本要求的那个）
    JavaMissing {
        major: u32,
    },
    /// 设置里钉死的 Java 和这个版本要的对不上（`actual` 是问出来的主版本，问不出为 None）
    JavaMismatch {
        major: u32,
        path: PathBuf,
        actual: Option<u32>,
    },
    /// 自动下载的 Java 装好了（Ok 带装好的运行时，Err 是原因）
    JavaInstalled(Result<aicraft_core::java::JavaRuntime, String>),
    /// 游戏进程起来了，带上它的 pid（启动器可能按设置退出去）
    GameRunning(u32),
    /// 等游戏窗口的结果：`shown` 为假表示进程先没了、或者等到超时
    GameWindow {
        pid: u32,
        shown: bool,
    },
    /// 导出整合包跑完了：Ok 带落到盘上的路径与统计，Err 是失败原因。
    Exported(Result<aicraft_core::packexport::ExportReport, String>),
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

/// 「Java 不对」那张弹窗的状态。
///
/// 两种来历共用一套字段：
/// - 机器上根本没有这个主版本（`path` 为 None）—— 可以问一句「要不要我下给你」；
/// - 设置里钉死了一个 Java，但主版本和这个版本要的不一样（`path` 有值）。
struct JavaPrompt {
    /// 这个版本要的 Java 主版本。
    major: u32,
    /// 玩家自己在设置里钉死的那个 Java；None 表示「本机压根没这个版本」。
    path: Option<PathBuf>,
    /// 钉死那份 Java 探测出来的主版本。None 表示探测不到（不是能跑 `-version` 的 Java）。
    actual: Option<u32>,
    /// 点过「下载安装」之后，卡片上那句进度话（也用来把按钮变成灰的）。
    progress: Option<String>,
    /// 装失败了的那句话。Some 的时候不再转圈，让玩家能重试。
    error: Option<String>,
}

/// 一次导出整合包的参数：挑好保存位置之后，后台任务照着它打包。
struct ExportJob {
    instance: String,
    format: aicraft_core::packexport::ExportFormat,
    name: String,
    version: String,
    /// 玩家在导出对话框里勾中的顶层条目。
    include: Vec<String>,
}

/// 手动钉死的 Java 与这个版本要的主版本之间的关系。
///
/// 只看主版本：Minecraft 的版本清单里也只声明到这一层（`javaVersion.majorVersion`）。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum JavaVerdict {
    /// 正好是要的那个，什么都别说。
    Match,
    /// 比要的老：字节码版本对不上，直接起不来。
    Older,
    /// 比要的新：一般能跑（字节码向后兼容），个别模组会挑。
    Newer,
    /// 探测不到版本 —— 路径在，但不是一份能跑的 Java。
    Unknown,
}

/// 主版本对不对得上。
fn java_verdict(required: u32, actual: Option<u32>) -> JavaVerdict {
    match actual {
        None => JavaVerdict::Unknown,
        Some(major) if major == required => JavaVerdict::Match,
        Some(major) if major < required => JavaVerdict::Older,
        Some(_) => JavaVerdict::Newer,
    }
}

/// 登录对话框的界面状态。`App::login` 为 None 表示对话框没开。
struct LoginState {
    busy: bool,
    /// 设备码信息：(verification_uri, user_code)
    device_code: Option<(String, String)>,
    /// 设备码是否已经放进系统剪贴板（决定那句提示是「直接粘贴」还是「自己输入」）。
    device_copied: bool,
    /// 设备码登录时浏览器有没有自动打开。
    device_opened: bool,
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
    /// 微软账户：微软登录
    Official,
    /// 第三方账户：皮肤站（authlib-injector）
    ThirdParty,
}

impl AccountSection {
    /// 左栏里平级排开的几个页签。`List` 不在里面：它现在挂在
    /// 左栏顶部那块「当前账户」上，点一下就到。
    const ALL: [AccountSection; 3] = [
        AccountSection::Official,
        AccountSection::Offline,
        AccountSection::ThirdParty,
    ];

    fn label(self) -> &'static str {
        match self {
            AccountSection::List => "账号",
            AccountSection::Offline => "离线账户",
            AccountSection::Official => "微软账户",
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
    /// 给这个账号续一次期（存的是 UUID）；令牌过期了才有意义
    Refresh(String),
    SaveName,
    AddThirdParty,
    Device,
    Browser,
    /// 打开官方购买页（Minecraft: Java & Bedrock Edition）
    Buy,
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
    /// 个性化：目前只有「要不要播放换页动画」这一条，做成独立一页
    /// 而不是塞进设置里，是给以后的外观偏好留位置。
    Personalize,
    /// 拓展功能 → 服务端管理（「拓展功能」本身不是页面）
    Servers,
}

/// 换页过渡的时长（秒）。
///
/// 再长一点就能看出「慢」，再短一点肉眼又分不清是淡入还是直接跳过去。
const PAGE_FADE_SECS: f32 = 0.2;

/// 换页过渡的透明度曲线：开头快、收尾慢，内容先浮出来、再稳住。
fn fade_alpha_at(progress: f32) -> f32 {
    let t = progress.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// 侧栏在「一级 / 二级目录」之间换层时的过渡时长（秒）。
///
/// 比换页短一点：导航栏只是换了几行字，拖太久会显得整栏在抖。
const NAV_SWITCH_SECS: f32 = 0.16;

/// 左侧导航的宽度（图标 + 文字）。
const NAV_WIDTH: f32 = 146.0;

/// 左侧导航的左右内边距。
const NAV_MARGIN: i8 = 10;

/// 账户页左栏的宽度。和主导航一样固定，右边那条线不该被拖动。
const ACCOUNT_NAV_W: f32 = 178.0;

/// 退出管理页时该落到哪一页。
///
/// `None` 表示「别动页号」——从版本列表进来的，侧栏本来就停在版本列表上。
/// 从首页那颗齿轮进来的则必须显式落回 [`Page::Home`]：管理页盖住整个窗口时
/// 侧栏是藏着的，看不出页号，来路得靠管理页自己记着（`manage::State::from_home`）。
fn page_after_manage_close(from_home: bool) -> Option<Page> {
    from_home.then_some(Page::Home)
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

    /// 这一页是否属于「拓展功能」那一组。
    fn in_more_group(self) -> bool {
        self == Page::Servers
    }

    /// 导航栏此刻该显示哪一组内容。
    ///
    /// 0 是一级那六项，其余各是一套二级目录。换组时导航整块重画，
    /// 过渡动画就靠这个值判断「内容换过了」。
    fn nav_group(self) -> u8 {
        if self.in_versions_group() {
            1
        } else if self.in_resources_group() {
            2
        } else if self.in_more_group() {
            3
        } else {
            0
        }
    }
}

/// 后台取头像回传的一条结果：账号 UUID 与那张脸（失败时带着原因）。
type SkinResult = (String, Result<Vec<u8>, String>);

/// 后台给一个账号续期的结果：UUID、新名字（成功）或一句话（失败）。
///
/// 成功也要带名字回来：皮肤站那边玩家可能改过名，续期是唯一会顺带发现
/// 这件事的时机。
type RefreshResult = (String, Result<String, String>);

/// 后台解背景图回传的一条结果：文件记号（长度:修改时间），以及解出来的像素。
///
/// 记号一起带回来是必要的：解码要几百毫秒，这期间玩家可能又换了一张，
/// 回来时得能认出自己解的是不是当前这张。
type WallpaperResult = (String, Result<egui::ColorImage, String>);

/// 后台取回一个模组图标：项目 id 与那张图（解码好的，失败时带着原因）。
/// `bool` 是「这张脸属于管理页那一列吗」：两个页面各有一份缓存，
/// 结果回来得分清往哪边放。
type IconResult = (bool, String, Result<icons::Decoded, String>);

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
    /// 装的过程中换一句说明（说不清进度的阶段）
    Stage(String),
    /// 正在下载：已下 / 一共（0 表示不知道总量）
    Bytes { got: u64, total: u64 },
    /// 预置安装器依赖的库：已下 / 一共（其实是这两数，但只有文件数）
    Libraries { done: u64, total: u64 },
    /// 装完了（失败时带原因）
    Created(Box<Result<Server, String>>),
    /// 探完本机的 Python / MCDR
    ///
    /// `manage` 说明这份结果是管理页要的、不是新建流程要的 —— 两边可能
    /// 同时开着（流程盖在管理页上），写错地方就会串味。
    Env {
        manage: bool,
        env: aicraft_core::mcdr::Env,
    },
    /// 问到了 PyPI 上 MCDR 的最新版本（或者没问到）
    McdrUpdate {
        manage: bool,
        result: Result<String, String>,
    },
}

struct App {
    /// 当前页面：主页只放启动，其余功能都在另外两页
    page: Page,
    /// 上一帧画的是哪一页，用来判断「这一帧是不是刚换页」。
    ///
    /// 在这里比对，而不是在每个 `self.page = …` 旁边各记一次：换页入口
    /// 有七八处，漏掉一个，那一页就会突然不淡入了。
    fade_mark: Page,
    /// 本次淡入的起始时刻；播完置回 None。
    fade_start: Option<std::time::Instant>,
    /// 版本列表开始「逐行进场」的时刻；播完置回 None。
    ///
    /// 和 fade_start 分开记：换页淡入是整块正文的事，而逐行进场只跟
    /// 「列表什么时候换了一批」有关——刷新列表时页面没换，也要重播。
    rows_reveal_at: Option<std::time::Instant>,
    /// 目录列表页各行开始依次进场的时刻；播完置回 None。
    ///
    /// 单独记一份，不复用 rows_reveal_at：那一个是版本列表的，
    /// 刷新版本时会被 rescan 重设，跟「目录列表换了一批行」不是一件事。
    dirs_reveal_at: Option<std::time::Instant>,
    /// 「版本管理」那三页的标题行开始从右侧滑进来的时刻；播完置回 None。
    ///
    /// 和 rows_reveal_at 一起起跑：标题先到位，下面的列表再依次跟上。
    title_slide_at: Option<std::time::Instant>,
    /// 账户页页头开始从右侧滑进来的时刻；播完置回 None。
    ///
    /// 账户页不是 `Page` 里的一页（它整块盖在正文上），换页那套
    /// 时间戳轮不到它，只好单独记一份。
    account_slide_at: Option<std::time::Instant>,
    /// 账户页各张卡片开始依次进场的时刻；播完置回 None。
    ///
    /// 和 `account_slide_at` 分开：换分区时页头不动，只重播这一批卡片。
    account_reveal_at: Option<std::time::Instant>,
    /// 账户页左栏滑进／滑出的起点；播完置回 None。
    ///
    /// 进页面时它从左沿外面滑进来，点「返回」时原路滑出去 —— 一个是
    /// 0 → 1、一个是 1 → 0，共用这一个时间戳，由 `account_closing` 定方向。
    account_nav_at: Option<std::time::Instant>,
    /// 账户页正在退场：左栏滑完才真的收起来。
    ///
    /// 少了这一小段，点「返回」会「啪」一下消失，滑出动画根本没机会播。
    account_closing: bool,

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
    /// 后台续期的进度。一次只跑一个线程，按顺序往回送。
    ///
    /// 不走 `spawn_task`：那个会占住全局 `busy` 并弹下载进度条，而续期
    /// 就是两三个 HTTPS 往返——拿进度条盖住整个界面反而吓人。
    refresh_rx: Option<std::sync::mpsc::Receiver<RefreshResult>>,
    /// 续期失败的那句话，显示在账号列表顶部
    refresh_error: Option<String>,
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
    /// 账户页里那些展开卡片的状态（皮肤档案、披风列表、联网排队）。
    ///
    /// 和头像那套分开：头像只要 32×32 一张脸，这里要整张皮肤和整份披风表，
    /// 而且只在卡片展开时才去取。
    wardrobe: wardrobe::State,
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
    /// 启动时发现缺某个主版本的 Java：Some 就开着那张「要不要下载」的弹窗。
    java_prompt: Option<JavaPrompt>,
    /// 装好的 Java 在等下一次启动时被用上：(实例名, 主版本)
    java_retry: Option<(String, u32)>,
    /// 玩家在「Java 版本不对」那张卡片上点了「仍然用它启动」：(实例名)
    ///
    /// 存实例名是为了只放行这一次启动 —— 换一个版本启动不该沾光。
    java_override: Option<String>,
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
    /// 刚下完、等着 busy 清空之后自动送去导入的整合包
    ///
    /// 下载任务结束时 busy 还没清（Finished 之后才清），而
    /// take_pack_file 见到 busy 就会拒绝。先在这里放一帧，
    /// 等 Finished 处理完再接手。
    pack_download: Option<PathBuf>,
    /// 本机 Java 探测的后台状态（首次进设置页要几秒，不能在绘制里做）
    java_scan: settings::JavaScan,
    /// 「拓展功能 → 服务端管理」的界面状态
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
    /// 启动时那张「节点进度」弹窗。有它 = 正在启动。
    ///
    /// 和 `busy` 分开记：下载、安装也占着 `busy`，但只有启动要把整屏糊掉。
    launch_view: Option<launchview::Launch>,
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
    /// 正在导出的整合包：挑好保存位置之后要接着打包，这些参数得留着。
    export_pack: Option<ExportJob>,
    /// 首页版本菜单是否被点开了（悬停是临时的，点一下才钉住）
    version_menu_open: bool,
    /// 上一帧版本菜单占的矩形：指针移进去时要继续算「开着」
    version_menu_rect: Option<egui::Rect>,
    mirror_probe_rx: Option<std::sync::mpsc::Receiver<()>>,
    /// 窗口底下那张背景图
    wallpaper: wallpaper::Wallpaper,
    /// 后台解背景图的结果通道
    wallpaper_rx: Option<Receiver<WallpaperResult>>,
    /// 挑图失败时那句提示，下次挑成功就清掉
    wallpaper_note: Option<String>,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        logging::init(Some(log_file_path()));
        let settings = Settings::load_or_default(&settings_path());
        // 样式得等读到设置再装：主题色存在设置里，而 `theme::install`
        // 是唯一给全局样式定调的地方，早一步装就只能是默认色。
        theme::install(ctx, settings.theme_color);
        theme::set_alphas(settings.ui_alphas);
        logging::debug(format!(
            "数据目录：{}",
            aicraft_core::settings::data_dir().display()
        ));
        logging::info(format!(
            "账号目录：{}",
            aicraft_core::settings::accounts_dir().display()
        ));
        // 设置文件里记着玩家手工挑过的 Java 路径，交给扫描线程一起探一遍，
        // 不然每次重启都得重新「添加 Java 路径」。
        let mut java_scan = settings::JavaScan::default();
        java_scan.seed(&settings.java_paths);
        let mut app = App {
            page: Page::Home,
            fade_mark: Page::Home,
            rows_reveal_at: None,
            dirs_reveal_at: None,
            title_slide_at: None,
            account_slide_at: None,
            account_reveal_at: None,
            account_nav_at: None,
            account_closing: false,
            fade_start: None,
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
            account_draft: initial_username(&settings),
            third_root: String::new(),
            third_user: String::new(),
            third_pass: String::new(),
            third_error: None,
            refresh_rx: None,
            refresh_error: None,
            accounts: AccountStore::default(),
            skin_tex: std::collections::HashMap::new(),
            skin_rx: None,
            skin_tried: std::collections::HashSet::new(),
            wardrobe: wardrobe::State::new(),
            icon_rx: None,
            settings,
            pending_remove: None,
            verify_prompt: None,
            reset_confirm: false,
            parent_prompt: None,
            java_prompt: None,
            java_retry: None,
            java_override: None,
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
            pack_download: None,
            java_scan,
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
            launch_view: None,
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
            export_pack: None,
            version_menu_open: false,
            version_menu_rect: None,
            mirror_probe_rx: None,
            wallpaper: wallpaper::Wallpaper::default(),
            wallpaper_rx: None,
            wallpaper_note: None,
        };
        // 调试用：AICRAFT_PAGE=dirs|versions|download|resources|servers|settings 时启动就停在那页（截图/排查方便）
        if let Ok(page) = std::env::var("AICRAFT_PAGE") {
            app.page = match page.as_str() {
                "dirs" => Page::Dirs,
                "versions" => Page::Versions,
                "download" => Page::Download,
                "resources" => Page::Resources,
                "settings" => Page::Settings,
                "personalize" => Page::Personalize,
                "servers" => Page::Servers,
                _ => Page::Home,
            };
            if app.page == Page::Servers {
                // 直接落在这一页时也要走一遍真正的入口，否则列表与版本清单都是空的。
                app.open_servers();
            }
        }

        app.rescan();
        // 壁纸在第一帧之前就要装好，否则窗口会先亮一块白底再淡进图。
        app.load_wallpaper_now(ctx);

        for msg in take_startup_notes() {
            logging::info(msg);
        }
        app.restore_saved_account();
        app.start_mirror_probe();
        // 内存快照要起一个 PowerShell（约 0.4 秒）。趁玩家还在看主页的时候
        // 就把它焐热，进设置页时 `cached()` 直接有数，不会卡那一帧。
        aicraft_core::meminfo::prefetch();
        aicraft_core::screen::prefetch();

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
                // 本机来的图标不走网络：模组的压在 jar 里，资源包的是 pack.png。
                let bytes = if let Some(path) = source.strip_prefix(icons::JAR_SCHEME) {
                    aicraft_core::modicon::from_jar(std::path::Path::new(path))
                        .map_err(|e| format!("读模组图标失败: {}", e))
                        .and_then(|found| found.ok_or_else(|| "这个模组没带图标".to_string()))
                } else if let Some(path) = source.strip_prefix(icons::PACK_SCHEME) {
                    aicraft_core::resourcepack::icon_at(std::path::Path::new(path))
                        .ok_or_else(|| "这个资源包没带图标".to_string())
                } else if let Some(path) = source.strip_prefix(icons::SAVE_SCHEME) {
                    aicraft_core::packicon::icon_at(std::path::Path::new(path))
                        .ok_or_else(|| "这个存档没留下截图".to_string())
                } else {
                    aicraft_core::transport::fetch_bytes(&source)
                        .map_err(|e| format!("取图标失败: {}", e))
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
                    logging::warn(format!("账号文件无法读取：{}", message));
                }
                AccountStore::default()
            }
        };
        if stale_offline_name(&self.settings.offline_name, &self.accounts.accounts) {
            self.settings.offline_name.clear();
            self.persist_settings_quietly();
        }

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

    /// 给一个账号续一次期。
    ///
    /// 正版和皮肤站走的是两条完全不同的路，`StoredAccount::refresh_any`
    /// 在 core 里收口了这件事；这里只负责挑线程、写回文件、清头像缓存。
    fn start_refresh(&mut self, uuid: &str) {
        let uuid = uuid.to_string();
        if self.refresh_rx.is_some() {
            self.refresh_error = Some("上一轮刷新还没结束，等它一下。".to_string());
            return;
        }
        let Some(target) = self
            .accounts
            .accounts
            .iter()
            .find(|a| a.uuid == uuid.clone())
            .cloned()
        else {
            return;
        };
        if !target.can_refresh() {
            self.refresh_error = Some("离线账号没有令牌可以续，换名字另存一份就行。".to_string());
            return;
        }
        let name = target.name.clone();
        self.refresh_error = None;
        let client_id = match auth::client_id() {
            Ok(id) => id,
            Err(e) => {
                self.refresh_error = Some(describe_auth_error(e));
                return;
            }
        };
        let path = store::default_path();
        let (tx, rx) = std::sync::mpsc::channel();
        self.refresh_rx = Some(rx);
        std::thread::spawn(move || {
            // 在后台重新读一遍文件再改：这期间玩家可能又加了一个账号。
            let mut saved = AccountStore::load_or_default(&path);
            let result = match saved.accounts.iter_mut().find(|a| a.uuid == target.uuid) {
                Some(stored) => match stored.refresh_any(&auth::CurlHttp, client_id) {
                    Ok(()) => {
                        let fresh = stored.name.clone();
                        match saved.save(&path) {
                            Ok(()) => Ok(fresh),
                            Err(e) => Err(format!("续期成功，但保存账号失败：{}", e)),
                        }
                    }
                    Err(e) => Err(describe_auth_error(e)),
                },
                None => Err("这个账号已经不在列表里了。".to_string()),
            };
            let _ = tx.send((uuid, result));
        });
        aicraft_core::audit::action(format!("账户：刷新 {}", name));
    }

    /// 收续期的结果：写回缓存、必要时清头像让人重取。
    fn poll_refresh(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.refresh_rx.take() else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok((uuid, result)) => match result {
                    Ok(name) => {
                        // 名字可能变了（皮肤站改过名），整份重读最省事。
                        self.accounts = AccountStore::load_or_default(&store::default_path());
                        if self.account.as_ref().map(|a| a.uuid.clone()).as_deref()
                            == Some(uuid.as_str())
                        {
                            self.username = name.clone();
                            self.settings.clamp();
                            if let Err(e) = self.settings.save(&settings_path()) {
                                logging::warn(format!("保存设置失败: {}", e));
                            }
                            if let Some(stored) =
                                self.accounts.accounts.iter().find(|a| a.uuid == uuid)
                            {
                                self.account = Some(stored.to_account());
                            }
                        }
                        self.refresh_error = None;
                        logging::info(format!("已刷新账号 {}", name));
                    }
                    Err(e) => {
                        logging::warn(format!("刷新账号失败：{}", e));
                        self.refresh_error = Some(e);
                    }
                },
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.refresh_rx = Some(rx);
                    ctx.request_repaint_after(std::time::Duration::from_millis(120));
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    /// 切到另一个账号：改当前账号、改界面上的名字与登录态。
    fn switch_account(&mut self, uuid: &str) {
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
                    "微软"
                },
                stored.name
            ));
            self.username = stored.name.clone();
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
        self.wardrobe.forget(uuid);
        self.skin_tex.remove(uuid);
        self.skin_tried.remove(uuid);
        logging::info(format!("已移除{}账号 {}", kind, name));

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
                self.account = Some(next.to_account());
            }
            None => {
                self.username.clear();
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
            return Some((stored.name.clone(), account_kind_title(stored)));
        }
        // 刚登录、还没存进列表的号：它确实就是这次会用的那个，不能因为
        // 「文件里没有」就显示成未配置 —— 那会让人以为登录没成功。
        if let Some(account) = &self.account
            && account.access_token != "0".repeat(32) {
            return Some((
                account.username.clone(),
                if account.api_root.is_some() {
                    "第三方"
                } else {
                    "微软"
                },
            ));
        }
        if !self.username.trim().is_empty() {
            // 还没入库的离线名字：它照样会被填进 `--username`。
            return Some((self.username.clone(), "离线"));
        }
        None
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

    /// 玩家挑了一个文件夹之后：不管认不认得出，都先让它出现在列表里。
    ///
    /// 认不出核心也照样加进去（标成「空目录」）——他刚点了「添加」，
    /// 列表里什么都不出现只会让人以为操作失败了。真要装，从那张卡片上装。
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
        match aicraft_core::server::probe_any(&path) {
            Some(found) => {
                aicraft_core::audit::action(format!(
                    "服务端：把 {} 接进列表（{}）",
                    picked,
                    if found.recognized {
                        found.summary()
                    } else {
                        "没认出核心".to_string()
                    }
                ));
                self.settings.server_dirs.push(picked);
                self.persist_settings_quietly();
                self.servers
                    .set_extra(&self.settings.server_dirs, &self.settings.hidden_server_dirs);
                // 不弹提示卡片：加进来之后列表里那一行已经把该说的都说了
                // （认出来的写核心和版本，认不出的标「空目录」）。再顶一张
                // 卡片复述一遍，等于让玩家读两遍同一句话。
                self.servers.notice = None;
            }
            None => {
                // 连目录都不是（被删掉了？），说清楚就行
                self.servers.notice = Some((format!("{} 读不了，换个文件夹吧。", picked), true));
            }
        }
    }

    /// 在某个目录里装核心：进三步流程，目标路径预设成它。
    ///
    /// `existing` 区分两种来路：空目录是「装一个新的」，管理页是「往这个
    /// 已经有东西的目录里补一个核心」。两者最后都走 `server::reinstall`
    /// （允许目录非空、失败不删目录），因为玩家可能已经往里面放过东西了。
    /// 后者允许目录里已经有东西，名字也跟着目录走、不给改。
    fn install_server_into(&mut self, path: PathBuf) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        aicraft_core::audit::action(format!("服务端：在 {} 里安装核心", path.display()));
        self.servers.form = serverui::Form {
            open: true,
            step: serverui::Step::Version,
            name,
            target: Some(path),
            existing: true,
            memory_mb: self.settings.memory_mb(),
            ..serverui::Form::default()
        };
        self.servers.notice = None;
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
                let path = rt.path.display().to_string();
                if self.java_scan.add(rt) {
                    logging::info(format!("已添加 Java {}（{}）", major, version));
                    self.java_scan.note = Some(format!("已添加 Java {}（{}）", major, version));
                    // 记进设置，下次启动就不用再挑一遍。已经在里面的不重复记。
                    // 这里直接落盘而不是等设置页比对：玩家可能压根不在设置页，
                    // 那一帧的 before/after 比对根本不会发生。
                    if !self
                        .settings
                        .java_paths
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(&path))
                    {
                        self.settings.java_paths.push(path);
                        self.persist_settings_quietly();
                    }
                } else {
                    // 列表里已经有这个 Java 了。玩家挑它的本意是「用这个」，
                    // 所以顺手切过去——总比点了半天什么都没发生要好。
                    logging::info(format!("Java {} 已在列表中，切换到它", major));
                    self.java_scan.note =
                        Some(format!("Java {}（{}）已经在列表里，已切换过去", major, version));
                    let picked = std::path::PathBuf::from(&path);
                    if self.settings.java_path.as_ref() != Some(&picked) {
                        self.settings.java_path = Some(picked);
                        self.persist_settings_quietly();
                    }
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
                    self.rows_changed();
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

    /// 目录列表换了一批行（加了、删了、官方那行显隐变了）：进场重播一次。
    ///
    /// 只重播目录列表，不动版本列表那份时间戳——两个列表在侧栏里是
    /// 两个不同的页，一处改动不该把另一页也重新演一遍。
    fn rows_changed(&mut self) {
        self.dirs_reveal_at = Some(std::time::Instant::now());
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
            // 清单仍要整份解析一遍：解析不出形状的目录本来就不该出现在列表里。
            // 解析出来的字段这一页已经不读了 —— 详情卡片撤掉之后，行上只剩下
            // 名字和加载器那枚形状。
            if parse_version(&text).is_err() {
                continue;
            }
            self.versions.push(VersionEntry {
                name: name.to_string(),
                loader: aicraft_core::loader::detect_in_text(&text, name).map(|(kind, _)| kind),
            });
        }

        self.versions.sort_by_key(|entry| entry.name.to_lowercase());
        self.last_scan_ms = started.elapsed().as_millis();
        // 换了一批行，进场重播一次。刷新按钮和首次扫描都走这里。
        self.rows_reveal_at = Some(std::time::Instant::now());
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
        if let Some(name) = name
            && self.settings.selected_version != name {
            self.settings.selected_version = name;
            self.persist_settings_quietly();
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

    /// 打开「账户」页：默认停在微软这一分区 —— 这一页现在的职责就是登录，
    /// 已经有账号的话右上角有那颗绿按钮，直接就能开。
    fn open_account(&mut self) {
        // 离线名字和「当前登录的是谁」是两回事：这里取设置里存的那个，
        // 没存过就是空的，不去借当前账号的名字。
        self.account_draft = self.settings.offline_name.trim().to_string();
        // 这一页现在的职责就是登录：进来先看「已保存账户」，已经登录过的
        // 号在那儿，点一下就能用；要新登一个，左边那两个页签就在旁边。
        self.account_section = AccountSection::List;
        self.account_open = true;
        self.account_closing = false;
        // 左栏这一趟从窗口左沿外面滑进来。换分区时不再重播（那是内容的事）。
        self.account_nav_at = Some(std::time::Instant::now());
        self.start_account_anim();
    }

    /// 关账户页：先让左栏滑走，滑完再真的收起来。
    ///
    /// 顺序反了的话面板当场消失，动画连一帧都播不出来。
    fn close_account(&mut self) {
        if !self.settings.animate_ui {
            self.account_open = false;
            self.login = None;
            return;
        }
        self.account_closing = true;
        self.account_nav_at = Some(std::time::Instant::now());
        self.start_account_anim();
    }

    /// 账户页左栏这一刻滑到哪了（0 = 整条藏在左沿外，1 = 站定）。
    ///
    /// 退场时把它倒过来用：进度从 1 掉到 0，左栏就原路滑回去。
    fn account_nav_progress(&mut self, ctx: &egui::Context) -> f32 {
        let Some(start) = self.account_nav_at else {
            return 1.0;
        };
        if !self.settings.animate_ui {
            self.account_nav_at = None;
            self.account_closing = false;
            return 1.0;
        }
        let elapsed = start.elapsed().as_secs_f32() / theme::ACCOUNT_PAGE_SLIDE_SECS;
        if elapsed >= 1.0 {
            self.account_nav_at = None;
            if self.account_closing {
                // 左栏滑完了，这一页才真的收起来。
                self.account_closing = false;
                self.account_open = false;
                self.login = None;
            }
            return 1.0;
        }
        ctx.request_repaint();
        if self.account_closing {
            1.0 - elapsed
        } else {
            elapsed
        }
    }

    /// 账户页整套进场从头播一遍：页头先滑进来，卡片再依次浮上来。
    ///
    /// 进页面、换分区都走这里 —— 换分区等于换了一批内容，只重播卡片那一段
    /// 会让页头孤零零地留在原地，看着像没换过。
    fn start_account_anim(&mut self) {
        let now = std::time::Instant::now();
        self.account_slide_at = Some(now);
        self.account_reveal_at = Some(now);
    }

    /// 确保登录状态对象在（账号窗口里点「登录」时用）。客户端 ID 用内置常量。
    fn open_login(&mut self) {
        if self.login.is_some() {
            return;
        }
        self.login = Some(LoginState {
            busy: false,
            device_code: None,
            device_copied: false,
            device_opened: false,
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
        login.device_copied = false;
        login.device_opened = false;
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
        let client_id = match auth::client_id() {
            Ok(id) => id,
            Err(e) => {
                logging::warn(describe_auth_error(e));
                return;
            }
        };

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
        let client_id = match auth::client_id() {
            Ok(id) => id,
            Err(e) => {
                logging::warn(describe_auth_error(e));
                return;
            }
        };

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
                        // 点了「设备码登录」之后，玩家本来就得自己做两件事：
                        // 打开那个网址、把码抄进去。两件都替他做了——尤其是抄码，
                        // 八位大小写混排，手输入错一次就要从头再来。
                        login.device_copied = aicraft_core::clipboard::copy(&code).is_ok();
                        login.device_opened = msa::open_browser(&uri).is_ok();
                        login.device_code = Some((uri, code));
                        login.device_message = message;
                        login.status = if login.device_opened {
                            "浏览器已打开，等你在里面确认…".to_string()
                        } else {
                            "等你在浏览器里完成授权…".to_string()
                        };
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
                // 离线名字是玩家自己填的，登录正版不替它改名。
                self.account = Some(account.to_account());
                // 账号文件是 `finish_login` 在后台线程里写的，内存里这份还是
                // 启动时读的旧列表。不重读的话，界面上虽然显示「正版：名字」，
                // 列表里却没有这一条 —— 头像队列按 `accounts` 遍历，于是
                // 正版账号永远等不到自己的脸；再删掉任意一个账号，保存时
                // 还会把这份陈旧列表整个写回去，把刚登录的账号一起抹掉。
                self.accounts = AccountStore::load_or_default(&store::default_path());
                logging::info(format!("已登录正版账号 {}", name));
                self.login = None;
                self.persist_settings_quietly();
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

        // 页头先滑进来，卡片等它落位之后再依次浮上来 —— 和版本列表、
        // 目录列表用的是同一套节奏，换页时整块界面的呼吸才对得上。
        let head = theme::head_elapsed(&mut self.account_slide_at, self.settings.animate_ui, ui.ctx());
        let slide = theme::slide_progress(head, 0.0, theme::TITLE_SLIDE_SECS);
        let reveal = theme::row_reveal_elapsed(
            &mut self.account_reveal_at,
            self.settings.animate_ui,
            ui.ctx(),
            theme::ROW_REVEAL_DELAY,
        );

        // 整页先铺一层正文底色：左栏滑出去的瞬间，它原来占的那一条会露出来，
        // 底下若还是主界面，看着就像页面上破了个洞。
        //
        // 用 available_rect 而不是 max_rect：max_rect 是整块窗口，连标题栏
        // 一起盖住，顶上那条牌子和窗口按钮会凭空消失。
        ui.painter()
            .rect_filled(ui.available_rect_before_wrap(), 0.0, theme::bg_bottom());

        // 左栏不再交给 Panel：Panel 一摆好位置就定死了，没地方塞位移。
        // 这里自己开一块，整条按进度往左挪（进页面时从外面滑进来，
        // 点「返回」时原路滑出去），挪出去的那截由裁剪框切掉。
        let nav_progress = self.account_nav_progress(ui.ctx());
        let nav_offset = theme::account_nav_offset(nav_progress, ACCOUNT_NAV_W);

        // 左右两块之间不留 item_spacing：左栏挪动时那一线空隙也会跟着抖。
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal_top(|ui| {
            let (bar, _) = ui.allocate_exact_size(
                Vec2::new(ACCOUNT_NAV_W, ui.available_height()),
                egui::Sense::hover(),
            );
            theme::account_page_nav(ui, bar, nav_offset, |ui| {
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

                let (shown_name, shown_kind) = match self.current_account_label() {
                    Some((name, kind)) => (name, kind.to_string()),
                    None => ("未配置".to_string(), String::new()),
                };
                let skin = self
                    .account
                    .as_ref()
                    .and_then(|a| self.skin_tex.get(&a.uuid).cloned());

                // 第一项就是「已保存账户」：头像和名字直接说明现在用的是哪个号，
                // 和下面三个页签并在一栏，不必再在栏底重复一遍。
                let on_list = self.account_section == AccountSection::List;
                let highlight = theme::NavHighlight::begin(ui);
                let saved = theme::account_item(ui, &shown_name, &shown_kind, skin.as_ref(), on_list);
                highlight.row(saved.rect, on_list);
                if saved.clicked() {
                    self.account_section = AccountSection::List;
                    aicraft_core::audit::detail("账户：切到已保存账户");
                    self.start_account_anim();
                }
                ui.add_space(2.0);
                for section in AccountSection::ALL {
                    let selected = self.account_section == section;
                    let response = theme::nav_item(ui, section.label(), selected, false);
                    highlight.row(response.rect, selected);
                    if response.clicked() {
                        self.account_section = section;
                        aicraft_core::audit::detail(format!("账户：切到「{}」", section.label()));
                        self.start_account_anim();
                    }
                    ui.add_space(2.0);
                }
                highlight.finish(
                    ui,
                    egui::Id::unique(egui::IdSalt::new("account-nav-highlight")),
                    self.settings.animate_ui,
                );

            });

            egui::CentralPanel::default()
            .frame(
                // 同上：这一页整块已经在外面铺过一层，再填一次就是两层。
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(22, 20)),
            )
            .show(ui, |ui| {
                match self.account_section {
                    AccountSection::List => {
                        theme::title_slide(ui, slide, |ui| {
                            ui.label(RichText::new("已保存账户").size(17.0).strong());
                            if let Some(err) = self.refresh_error.clone() {
                                ui.add_space(6.0);
                                ui.label(RichText::new(err).size(11.0).color(theme::DANGER));
                            }
                        });
                        ui.add_space(12.0);

                        if self.accounts.accounts.is_empty() {
                            theme::reveal_row(ui, 0, reveal, |ui| {
                                theme::card().show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.label(RichText::new("还没有账号").size(13.0).strong());
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(
                                            "左边「离线账户」起个名字就能玩；有微软或皮肤站账号的话，那边也能登录。",
                                        )
                                        .size(11.0)
                                        .color(theme::TEXT_WEAK),
                                    );
                                });
                            });
                        } else {
                            // 整份账号克隆一份出来：卡片要一边读账号、一边改
                            // 自己的衣柜状态，借用同一个 self 会打架。令牌也就
                            // 几百字节，几张卡片的成本不值一提。
                            let rows: Vec<(StoredAccount, Option<egui::TextureHandle>)> = self
                                .accounts
                                .accounts
                                .iter()
                                .map(|a| (a.clone(), self.skin_tex.get(&a.uuid).cloned()))
                                .collect();
                            let current_uuid = self.account.as_ref().map(|a| a.uuid.clone());
                            let ctx = ui.ctx().clone();
                            // 卡片上按下的每一颗按钮都先记下来，出了滚动区再动
                            // self —— 闭包里拿不到第二份可变的 self。
                            let mut wanted: Vec<(wardrobe::Action, StoredAccount)> = Vec::new();
                            egui::ScrollArea::vertical()
                                .id_salt("account-cards")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    for (index, (account, skin)) in rows.iter().enumerate() {
                                        let selected =
                                            current_uuid.as_deref() == Some(account.uuid.as_str());
                                        // 逐张浮上来：第一张先动，后面的依次跟上，
                                        // 和版本列表、目录列表是同一套节奏。
                                        let picked = theme::reveal_row(ui, index, reveal, |ui| {
                                            wardrobe::card(
                                                ui,
                                                &ctx,
                                                &mut self.wardrobe,
                                                wardrobe::Row {
                                                    uuid: &account.uuid,
                                                    name: &account.name,
                                                    kind: account_kind_title(account),
                                                    selected,
                                                    can_refresh: account.can_refresh(),
                                                    editable: wardrobe::not_editable(account)
                                                        .is_none(),
                                                    skin: skin.as_ref(),
                                                    offline: account.offline,
                                                },
                                                busy,
                                            )
                                        });
                                        if let Some(picked) = picked {
                                            wanted.push((picked, account.clone()));
                                        }
                                        ui.add_space(8.0);
                                    }
                                });

                            for (wanted_action, account) in wanted {
                                let uuid = account.uuid.clone();
                                match wanted_action {
                                    wardrobe::Action::Use => {
                                        aicraft_core::audit::action(format!(
                                            "账户：切换到 {}",
                                            account.name
                                        ));
                                        action = Some(AccountAction::Switch(uuid));
                                    }
                                    wardrobe::Action::Toggle => {
                                        if self.wardrobe.is_expanded(&uuid) {
                                            self.wardrobe.collapse(&uuid);
                                        } else {
                                            self.wardrobe.expand(&uuid);
                                            aicraft_core::audit::detail(format!(
                                                "账户：展开 {}",
                                                account.name
                                            ));
                                            self.wardrobe.ensure(&account);
                                        }
                                    }
                                    wardrobe::Action::Refresh => {
                                        action = Some(AccountAction::Refresh(uuid));
                                    }
                                    wardrobe::Action::Remove => {
                                        aicraft_core::audit::action(format!(
                                            "账户：删除 {}",
                                            account.name
                                        ));
                                        action = Some(AccountAction::Remove(uuid));
                                    }
                                    wardrobe::Action::Pick => {
                                        self.pick_skin_for(&account);
                                    }
                                    wardrobe::Action::Retry => {
                                        self.wardrobe.load(&account);
                                    }
                                    wardrobe::Action::ResetSkin => {
                                        aicraft_core::audit::action(format!(
                                            "账户：{} 恢复默认皮肤",
                                            account.name
                                        ));
                                        self.wardrobe.reset_skin(&account);
                                    }
                                    wardrobe::Action::Cape(cape_id) => {
                                        aicraft_core::audit::action(format!(
                                            "账户：{} 换披风",
                                            account.name
                                        ));
                                        self.wardrobe.set_cape(&account, &cape_id);
                                    }
                                }
                            }
                        }
                    }
                    AccountSection::Offline => {
                        theme::title_slide(ui, slide, |ui| {
                            ui.label(RichText::new("离线账户").size(17.0).strong());
                        });
                        ui.add_space(12.0);

                        theme::reveal_row(ui, 0, reveal, |ui| {
                            theme::card().show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.label(RichText::new("玩家名").size(12.0).color(theme::TEXT_WEAK));
                                ui.add_space(4.0);
                                let mut resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.account_draft)
                                        .hint_text("起个名字，进游戏就用它")
                                        .desired_width(f32::INFINITY),
                                );
                                theme::input_menu(&mut resp, &mut self.account_draft, false);
                                ui.add_space(6.0);
    
                                if let Some(warning) = offline_name_warning(&self.account_draft) {
                                    ui.label(
                                        RichText::new(warning).size(11.0).color(theme::WARN),
                                    );
                                } else if self.account_draft.trim().is_empty() {
                                    // 空着什么都不说：输入框自己的 hint_text 已经写着
                                    // 「起个名字，进游戏就用它」了。
                                } else if self
                                    .account
                                    .as_ref()
                                    .map(|a| {
                                        // 只有「当前真的是个离线号、名字也对得上」
                                        // 才算正在用；挂着微软号时同名也不能这么说。
                                        a.access_token == "0".repeat(32)
                                            && a.username.trim() == self.account_draft.trim()
                                    })
                                    .unwrap_or(false)
                                {
                                    // 现在用的就是这个离线名字：不用再点一次。
                                    ui.label(
                                        RichText::new("正在用这个名字，启动游戏就以它进游戏。")
                                            .size(11.0)
                                            .color(theme::accent_strong()),
                                    );
                                } else {
                                    ui.label(
                                        RichText::new("点「使用」之后，启动游戏就以这个名字进去。")
                                            .size(11.0)
                                            .color(theme::TEXT_FAINT),
                                    );
                                }
    
                                ui.add_space(10.0);
                                if theme::dialog_primary_button(
                                    ui,
                                    "使用",
                                    offline_name_usable(&self.account_draft) && !busy,
                                    92.0,
                                )
                                .clicked()
                                {
                                    aicraft_core::audit::action("账户：用这个离线名字");
                                    action = Some(AccountAction::SaveName);
                                }
                            });
                        });
                    }
                    AccountSection::ThirdParty => {
                        theme::title_slide(ui, slide, |ui| {
                            ui.label(RichText::new("第三方登录").size(17.0).strong());
                        });
                        ui.add_space(12.0);

                        theme::reveal_row(ui, 0, reveal, |ui| {
                            theme::card().show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                if let Some(root) = self.account.as_ref().and_then(|a| a.api_root.as_ref()) {
                                    ui.horizontal(|ui| {
                                        theme::status_dot(ui, theme::accent_fill());
                                        ui.label(
                                            RichText::new(
                                                self.account
                                                    .as_ref()
                                                    .map(|a| a.username.clone())
                                                    .unwrap_or_default(),
                                            )
                                            .size(13.5)
                                            .strong()
                                            .color(theme::TEXT),
                                        );
                                        ui.label(
                                            RichText::new("这次会用它启动")
                                                .size(11.0)
                                                .color(theme::TEXT_FAINT),
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
                                    ui.add_space(10.0);
                                }
                                ui.label(
                                    RichText::new("认证服务器")
                                        .size(12.0)
                                        .color(theme::TEXT_WEAK),
                                );
                                ui.add_space(4.0);
                                let mut resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.third_root)
                                        .hint_text("https://littleskin.cn/api/yggdrasil")
                                        .desired_width(f32::INFINITY),
                                );
                                theme::input_menu(&mut resp, &mut self.third_root, false);
    
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new("账号").size(12.0).color(theme::TEXT_WEAK),
                                );
                                ui.add_space(4.0);
                                let mut resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.third_user)
                                        .hint_text("邮箱或用户名")
                                        .desired_width(f32::INFINITY),
                                );
                                theme::input_menu(&mut resp, &mut self.third_user, false);
    
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new("密码").size(12.0).color(theme::TEXT_WEAK),
                                );
                                ui.add_space(4.0);
                                let mut resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.third_pass)
                                        .password(true)
                                        .hint_text("只用于这一次登录，不会存下来")
                                        .desired_width(f32::INFINITY),
                                );
                                // 密码框不给剪切/复制，和 egui 自己按键处理时的做法一致。
                                theme::input_menu(&mut resp, &mut self.third_pass, true);
    
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
                        });
                    }
                    AccountSection::Official => {
                        theme::title_slide(ui, slide, |ui| {
                            ui.label(RichText::new("微软账户").size(17.0).strong());
                        });
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

                        theme::reveal_row(ui, 0, reveal, |ui| {
                            theme::card().show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
    
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
                                        // 浏览器和剪贴板都已经自动处理过，这里只剩「告诉玩家现在该干嘛」。
                                        // 自动打开失败才退回「点这个链接」。
                                        if login.device_opened {
                                            ui.label(
                                                RichText::new("请在浏览器中输入以下代码：")
                                                    .size(11.0)
                                                    .color(theme::TEXT_WEAK),
                                            );
                                        } else {
                                            ui.label(
                                                RichText::new("浏览器未自动打开，请手动访问：")
                                                    .size(11.0)
                                                    .color(theme::TEXT_WEAK),
                                            );
                                            if ui.link(uri.clone()).clicked() {
                                                let _ = msa::open_browser(uri);
                                            }
                                        }
                                        theme::inset().show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    RichText::new(code.clone())
                                                        .monospace()
                                                        .size(22.0)
                                                        .strong()
                                                        .color(theme::accent_fill()),
                                                );
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(egui::Align::Center),
                                                    |ui| {
                                                        if ui.button("复制").clicked() {
                                                            copy = Some(code.clone());
                                                        }
                                                        if login.device_copied {
                                                            ui.label(
                                                                RichText::new("已复制，直接粘贴")
                                                                    .size(10.5)
                                                                    .color(theme::TEXT_FAINT),
                                                            );
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
                                        && ui.link("浏览器未自动打开，点此重新打开").clicked()
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
                                // 三枚并排、等宽：登录两条路 + 一个「还没有账号」的出口。
                                // 都是白底，鼠标放上去才变绿——这页一进来不该有哪个「更该点」。
                                const SLOT: f32 = 108.0;
                                ui.horizontal(|ui| {
                                    if theme::hover_green_button(
                                        ui,
                                        "浏览器登录",
                                        !running && !busy,
                                        SLOT,
                                    )
                                    .clicked()
                                    {
                                        aicraft_core::audit::action("账户：正版登录（浏览器）");
                                        action = Some(AccountAction::Browser);
                                    }
                                    if theme::hover_green_button(
                                        ui,
                                        "设备码登录",
                                        !running && !busy,
                                        SLOT,
                                    )
                                    .clicked()
                                    {
                                        aicraft_core::audit::action("账户：正版登录（设备码）");
                                        action = Some(AccountAction::Device);
                                    }
                                    if theme::hover_green_button(ui, "购买正版", !busy, SLOT).clicked() {
                                        aicraft_core::audit::action("账户：打开正版购买页");
                                        action = Some(AccountAction::Buy);
                                    }
                                });
                            });
                        });
                    }
                }
            });
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
            Some(AccountAction::Refresh(uuid)) => {
                self.start_refresh(&uuid);
            }
            Some(AccountAction::SaveName) => {
                let name = self.account_draft.trim().to_string();
                if offline_name_usable(&name) {
                    // 这一页只负责「把号登进来」：名字进来就直接进账号列表，
                    // 管理动作（删除、皮肤）都在「已保存账户」那张卡片里。
                    // 同一个名字算出同一个 UUID，反复来不会堆出重名条目。
                    let stored = StoredAccount::offline(&name);
                    logging::info(format!("离线账户 {}", name));
                    self.username = stored.name.clone();
                    self.settings.offline_name = stored.name.clone();
                    self.accounts.upsert(stored.clone());
                    self.save_accounts();
                    self.account = Some(stored.to_account());
                    self.persist_settings_quietly();
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
            Some(AccountAction::Buy) => {
                open_external_url("https://www.minecraft.net/zh-hans/store/minecraft-java-bedrock-edition-pc");
            }
            Some(AccountAction::ReopenBrowser) => {
                if let Some(url) = self.login.as_ref().and_then(|l| l.browser_url.clone()) {
                    let _ = msa::open_browser(&url);
                }
            }
            Some(AccountAction::Close) => {
                // 左栏先滑出去，滑完才真的收起来（滑出途中这一页还画着）。
                self.close_account();
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

    /// 背景图现在该长什么样：文件在哪儿、多大、什么时候改过。
    ///
    /// 没设背景就是一个空记号，主循环据此把图摘掉。用着出厂那张时是内置
    /// 记号——它不落在磁盘上，内容跟着 exe 走，exe 没换就不用重解。
    fn wallpaper_stamp(&self) -> String {
        let name = self.settings.background_image.trim();
        if name.is_empty() {
            return String::new();
        }
        if name == aicraft_core::settings::DEFAULT_BACKGROUND_NAME {
            return wallpaper::DEFAULT_KEY.to_string();
        }
        wallpaper::file_key(&aicraft_core::settings::background_path())
    }

    /// 后台解背景图。
    ///
    /// 一张手机拍的 4000×3000 照片要解几百毫秒，直接放在界面线程上会把
    /// 第一帧卡住——玩家点完「选择图片」只看见窗口僵在那儿。所以读文件、
    /// 缩小、转像素全扔后台，回来再传显存。
    fn kick_off_wallpaper(&mut self, ctx: &egui::Context) {
        if self.wallpaper_rx.is_some() {
            return;
        }
        let stamp = self.wallpaper_stamp();
        if stamp.is_empty() {
            // 图被移走、或者设置里清空了：立刻退回不透明底色。
            if self.wallpaper.has_image() {
                self.wallpaper.clear();
                theme::set_wallpaper(false);
                ctx.request_repaint();
            }
            return;
        }
        if stamp == self.wallpaper.key() {
            return;
        }
        // 解码全交给 wallpaper::prepare：它先看缓存，缓存没有才读原图，
        // 顺便把缩好的那份留成缓存，下次启动就能省掉大半时间。
        let dir = aicraft_core::settings::data_dir();
        let (tx, rx) = mpsc::channel();
        self.wallpaper_rx = Some(rx);
        std::thread::spawn(move || {
            let result = wallpaper::prepare(&dir, &stamp);
            let _ = tx.send((stamp, result));
        });
    }

    /// 启动那一下把背景图直接装好，不等后台线程。
    ///
    /// eframe 会先藏起窗口、等第一帧画完再显示，所以只要这一帧里就有图，
    /// 玩家看到的就是「打开即到位」，没有白底一闪。缓存命中时这里是
    /// 二十毫秒；第一次遇到一张大图会是几百毫秒，但只此一次。
    fn load_wallpaper_now(&mut self, ctx: &egui::Context) {
        let stamp = self.wallpaper_stamp();
        if stamp.is_empty() {
            return;
        }
        let dir = aicraft_core::settings::data_dir();
        match wallpaper::prepare(&dir, &stamp) {
            Ok(image) => {
                self.wallpaper.install(ctx, &stamp, image);
                theme::set_wallpaper(true);
            }
            Err(message) => {
                // 装不上就先让界面退回不透明底色，提示留给后台那条路去报，
                // 免得启动时一闪而过的错误横幅把人吓一跳。
                logging::warn(format!("背景图没装上：{}", message));
                self.wallpaper_note = Some(message);
                self.wallpaper.clear();
                theme::set_wallpaper(false);
            }
        }
    }

    /// 收后台解好的背景图，传上显存。
    fn poll_wallpaper(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.wallpaper_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok((stamp, result)) => {
                // 解码这几百毫秒里玩家可能又换了一张：记号对不上就丢掉这次结果，
                // 下一帧会按新的文件重新解一遍。
                if stamp == self.wallpaper_stamp() {
                    match result {
                        Ok(image) => {
                            self.wallpaper.install(ctx, &stamp, image);
                            theme::set_wallpaper(true);
                            self.wallpaper_note = None;
                        }
                        Err(message) => {
                            logging::warn(format!("背景图没装上：{}", message));
                            self.wallpaper_note = Some(message);
                            self.wallpaper.clear();
                            theme::set_wallpaper(false);
                        }
                    }
                }
                ctx.request_repaint();
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => self.wallpaper_rx = Some(rx),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
    }

    /// 挑一张图当背景：选好了就拷进数据目录，原图移走也不影响。
    ///
    /// 拷贝而不是记住原路径：玩家的图多半在桌面或下载文件夹里，过两天
    /// 就顺手删了；那时候启动器只剩一块灰底，谁也不知道是图没了还是坏了。
    fn pick_wallpaper(&mut self) {
        let start = PathBuf::from(&self.game_dir);
        let start = if start.is_dir() {
            start
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        };
        let picked = match pick_image_blocking(&start) {
            Ok(Some(path)) => path,
            Ok(None) => return,
            Err(e) => {
                logging::warn(format!("打开背景图选择框失败：{}", e));
                self.wallpaper_note = Some(format!("打不开选择框：{}", e));
                return;
            }
        };
        let target = aicraft_core::settings::background_path();
        if let Some(parent) = target.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            logging::warn(format!("建数据目录失败：{}", e));
            self.wallpaper_note = Some(format!("存不进数据目录：{}", e));
            return;
        }
        if let Err(e) = std::fs::copy(&picked, &target) {
            logging::warn(format!("拷贝背景图失败：{}", e));
            self.wallpaper_note = Some(format!("这张图拷不过来：{}", e));
            return;
        }
        let name = picked
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.settings.background_image = name;
        self.settings.clamp();
        self.persist_settings_quietly();
    }

    /// 换回出厂那张壁纸：拷进来的图删掉，名字改回默认。
    ///
    /// 图本身在 exe 里，删掉数据目录那份就够——留着它反倒会让下次启动
    /// 把玩家的旧图又读回来。
    fn restore_default_wallpaper(&mut self) {
        let path = aicraft_core::settings::background_path();
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            logging::warn(format!("删背景图失败：{}", e));
        }
        self.settings.background_image =
            aicraft_core::settings::DEFAULT_BACKGROUND_NAME.to_string();
        self.wallpaper_note = None;
        self.persist_settings_quietly();
    }

    /// 摘掉背景图：文件删掉，设置里也清空。
    fn clear_wallpaper(&mut self) {
        let path = aicraft_core::settings::background_path();
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            logging::warn(format!("删背景图失败：{}", e));
        }
        self.settings.background_image.clear();
        self.persist_settings_quietly();
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
            let result = pick_save_blocking(
                &initial,
                &name,
                "导出日志",
                "日志文件 (*.log)|*.log|文本文件 (*.txt)|*.txt|所有文件 (*.*)|*.*",
                "log",
            );
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
            Ok(Ok(Some(path))) => match self.export_pack.take() {
                Some(job) => self.start_export(job, path),
                None => self.save_log_to(&path),
            },
            Ok(Ok(None)) => {
                // 玩家在保存框上点了取消：整合包就不导了，日志导出本来也不用留话。
                if self.export_pack.take().is_some()
                    && let Some(state) = self.manage.as_mut() {
                    state.exporting = None;
                }
            }
            Ok(Err(message)) => {
                if self.export_pack.take().is_some()
                    && let Some(state) = self.manage.as_mut() {
                    state.exporting = None;
                    state.export_notice = Some((format!("导出失败: {}", message), false));
                }
                logging::warn(format!("保存对话框失败: {}", message));
            }
            Err(TryRecvError::Empty) => self.export_rx = Some(rx),
            Err(TryRecvError::Disconnected) => {}
        }
    }

    /// 导出整合包第一步：先让玩家挑存到哪，挑好了才动手打包。
    ///
    /// 顺序反过来（先打包再问存哪）也行，但那样玩家等完一长串进度才
    /// 发现位置不合适；先问位置，后面的时间都在干实事。
    fn choose_export_path(
        &mut self,
        format: aicraft_core::packexport::ExportFormat,
        name: &str,
        version: &str,
        include: &[String],
    ) {
        if self.export_rx.is_some() {
            return;
        }
        let Some(state) = self.manage.as_ref() else {
            return;
        };
        let instance = state.version.clone();
        if let Some(state) = self.manage.as_mut() {
            state.export = None;
            state.exporting = Some("选个地方存…".to_string());
            state.export_notice = None;
        }

        // 文件名先按「名字-版本」拼一份，玩家在对话框里还能改。
        let stem = if version.trim().is_empty() {
            name.to_string()
        } else {
            format!("{}-{}", name, version)
        };
        let file_name = format!(
            "{}.{}",
            sanitize_file_name(&stem, "整合包"),
            format.extension()
        );
        let initial = aicraft_core::settings::suggested_export_dir();
        let filter = format!(
            "{} (*.{})|*.{}|所有文件 (*.*)|*.*",
            format.label(),
            format.extension(),
            format.extension()
        );
        let ext = format.extension().to_string();
        let title = format!("导出{}", format.label());

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = pick_save_blocking(&initial, &file_name, &title, &filter, &ext);
            let _ = tx.send(result);
        });
        self.export_rx = Some(rx);
        self.export_pack = Some(ExportJob {
            instance,
            format,
            name: name.to_string(),
            version: version.to_string(),
            include: include.to_vec(),
        });
    }

    /// 位置定了，开一个后台任务真的打包。
    fn start_export(&mut self, job: ExportJob, dest: PathBuf) {
        let game_dir = PathBuf::from(&self.game_dir);
        if let Some(state) = self.manage.as_mut() {
            state.exporting = Some("正在打包…".to_string());
            state.export_notice = None;
        }
        let label = format!("正在导出 {}", job.name);
        // 打包几百兆要几十秒，底部那条进度面板上的「取消」得真能停下来：
        // 把这次导出的开关交给它，和下载、装加载器走同一个按钮。
        let cancel = aicraft_core::cancel::CancelToken::new();
        self.cancel_token = Some(cancel.clone());
        self.cancel_requested = false;
        self.download_progress = Some((0, 0, 0));
        self.loader_phase = None;
        self.speed_bps = 0.0;
        self.last_tick_at = None;
        self.last_tick_bytes = 0;
        self.spawn_task(label, move |tx| {
            let config = aicraft_core::packexport::ExportConfig {
                game_dir: &game_dir,
                instance: &job.instance,
                format: job.format,
                name: job.name.clone(),
                version: job.version.clone(),
                dest: &dest,
                resolve_downloads: true,
                include: &job.include,
                cancel: &cancel,
            };
            use aicraft_core::packexport::ExportProgress;
            let mut send_log = |line: &str| {
                let _ = tx.send(TaskMsg::Log(line.to_string()));
            };
            let progress = |ev: ExportProgress| {
                let msg = match ev {
                    ExportProgress::Stage(text) => TaskMsg::LoaderPhase(text),
                    // 打包没有字节进度可算，用文件数顶上去，至少让它动起来。
                    ExportProgress::File { done, total } => TaskMsg::LoaderTick {
                        done,
                        total,
                        bytes: 0,
                    },
                };
                let _ = tx.send(msg);
            };
            match aicraft_core::packexport::export_pack(&config, &progress, &mut send_log) {
                Ok(report) => {
                    let _ = tx.send(TaskMsg::Exported(Ok(report)));
                    Ok(())
                }
                Err(e) => {
                    let _ = tx.send(TaskMsg::Exported(Err(e.clone())));
                    Err(e)
                }
            }
        });
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
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent) {
            logging::warn(format!("导出日志失败: 创建目录失败: {}", e));
            return;
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

                    let picked = self.selected_timeout();
                    let current = picked
                        .map(|i| self.versions[i].name.clone())
                        .unwrap_or_else(|| "（还没有选择版本）".to_string());

                    ui.label(RichText::new("当前版本").size(12.0).color(theme::TEXT_WEAK));
                    ui.add_space(6.0);
                    // 标题右边跟着一颗齿轮：点开直接是这一份实例的管理页，
                    // 不用先去版本列表里把它找出来。没选版本时它灰着——没有实例可管。
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(RichText::new(&current).size(26.0).strong().color(theme::TEXT));
                        if let Some(index) = picked {
                            let name = self.versions[index].name.clone();
                            let gear = theme::gear_button(ui, !busy);
                            if gear.clicked() {
                                aicraft_core::audit::action(format!(
                                    "首页：打开 {} 的管理页",
                                    name
                                ));
                                self.open_manage_from(&name, true);
                            }
                            gear.on_hover_text("管理这个版本");
                        }
                    });

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
            // 启动进行中这条横带整块留空：菜单会让玩家以为自己还能换版本，
            // 换到一半的启动可没法改；「取消启动」也已经挪进弹窗里了。
            let launching = self.launch_view.is_some();
            let menu_anchor = ui
                .allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), band),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if launching {
                            return ui.allocate_exact_size(
                                Vec2::new(264.0, band),
                                egui::Sense::hover(),
                            )
                            .1;
                        }
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
            // 点一下钉住（这样不悬停也能翻列表），再点一下收回。
            // 启动中那条横带是空的，别拿它当菜单开关。
            if !launching {
                if menu_anchor.clicked() {
                    self.version_menu_open = !self.version_menu_open;
                }
                self.version_menu(ui, &menu_anchor);
            }
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

        let ctx = ui.ctx().clone();
        let area_id = anchor.id.with("home-version-menu");
        let anim_id = anchor.id.with("home-version-menu-anim");
        // 和按钮之间留一点空：卡片贴着按钮上沿会显得挤成一坨，
        // 松开几像素之后它才像浮在按钮上方、而不是按钮的一部分。
        // 间距不能超过下面 `rect.expand(8.0)` 那个悬停判定，否则指针从按钮
        // 移到卡片上时会经过一段「谁也不 hover」的死区，菜单会闪一下关掉。
        const GAP: f32 = 6.0;
        let pos = anchor.rect.right_top() - egui::Vec2::new(0.0, GAP);

        let mut clicked: Option<usize> = None;
        let layer = theme::SpringLayer::new(area_id, anim_id, open, pos)
            .pivot(egui::Align2::RIGHT_BOTTOM)
            .width(248.0)
            // 这里不能用裁剪式「拉出来」：按钮比浮层窄得多，裁剪线横在浮层底下
            // 会把它两条底边切成直边，没人挡着，看着像卡片被削掉一截。
            // 改缩放 + 淡入，四角完整，只是从按钮那个角长出来。
            .drift(5.0);
        let shown = theme::spring_layer(&ctx, layer, |ui| {
                egui::Frame::NONE
                    .fill(theme::card_fill())
                    .stroke(egui::Stroke::new(1.0, theme::BORDER))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(8, 8))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 2],
                        blur: 10,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(14),
                    })
                    .show(ui, |ui| {
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
                        // 循环里正借着 self.versions，不能顺手改 self 的别的字段，
                        // 先记下点的是第几行，退出闭包再统一处理。
                        egui::ScrollArea::vertical()
                            .id_salt("home-version-menu")
                            .max_height(280.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (i, v) in self.versions.iter().enumerate() {
                                    let picked = self.selected == Some(i);
                                    // 每行铺满：selectable_label 默认只占文字宽，
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
                    });
            },
        );
        self.version_menu_rect = shown.map(|r| r.response.rect);

        if let Some(i) = clicked {
            if let Some(name) = self.versions.get(i).map(|v| v.name.clone()) {
                aicraft_core::audit::action(format!("首页：切换到版本 {}", name));
            }
            self.select_version(i);
            self.version_menu_open = false;
        }
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
        // 标题行从右侧滑进来；下面列表的逐行进场由 reveal_row 各自管。
        let head = theme::head_elapsed(&mut self.title_slide_at, self.settings.animate_ui, ui.ctx());
        let slide = theme::slide_progress(head, 0.0, theme::TITLE_SLIDE_SECS);
        theme::title_slide(ui, slide, |ui| {
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

        // 每一行末尾带两枚图标键（管理 / 删除），列表顶到窗口最下面为止。
        //
        // 逐行进场的当前时刻；None 表示这一批早就进场完了，逐行走普通画法。
        let reveal = theme::row_reveal_elapsed(
            &mut self.rows_reveal_at,
            self.settings.animate_ui,
            ui.ctx(),
            theme::ROW_REVEAL_DELAY,
        );
        let mut clicked: Option<usize> = None;
        let mut managing: Option<usize> = None;
        let mut deleting: Option<usize> = None;
        let page_rect = ui.available_rect_before_wrap();
        let list_out = egui::ScrollArea::vertical()
            .id_salt("versions-page-list")
            .auto_shrink([false, false])
            .max_height(page_rect.height())
            .show(ui, |ui| {
                // 每行本身就是一张卡片，行间留一条缝，卡片才分得开。
                ui.spacing_mut().item_spacing.y = 6.0;
                for (i, v) in self.versions.iter().enumerate() {
                    // 纯原版也标一枚：列表里空着会让人以为「没读到」；
                    // 而且这一枚正好把「魔改过的」和「没魔改的」分开。
                    let selected = self.selected == Some(i);
                    let (row, manage, delete) = theme::reveal_row(ui, i, reveal, |ui| {
                        let row = theme::version_item(
                            ui,
                            &v.name,
                            v.loader,
                            selected,
                            theme::ROW_ICONS_RESERVE,
                        );
                        // 两枚图标键贴着行右端排：垃圾桶在最外，齿轮在它左边。
                        // 注册在这一行之后 —— 距离相等时 egui 取最后登记的那个，
                        // 指针压在图标上点到的才是图标，不是整行。
                        let side = theme::ROW_ICON_SIDE;
                        let center_y = row.rect.center().y;
                        let delete_rect = egui::Rect::from_center_size(
                            egui::Pos2::new(row.rect.right() - 14.0 - side * 0.5, center_y),
                            egui::Vec2::splat(side),
                        );
                        let manage_rect = egui::Rect::from_center_size(
                            egui::Pos2::new(
                                delete_rect.left() - theme::ROW_ICON_GAP - side * 0.5,
                                center_y,
                            ),
                            egui::Vec2::splat(side),
                        );
                        let manage = theme::row_icon_button(
                            ui,
                            manage_rect,
                            ("manage-version", i),
                            theme::NavGlyph::Settings,
                            !busy,
                            false,
                        )
                        .on_hover_text("管理");
                        let delete = theme::row_icon_button(
                            ui,
                            delete_rect,
                            ("delete-version", i),
                            theme::NavGlyph::Trash,
                            !busy,
                            true,
                        )
                        .on_hover_text("删除版本");
                        (row.clicked(), manage.clicked(), delete.clicked())
                    });
                    if row {
                        clicked = Some(i);
                    }
                    if manage {
                        managing = Some(i);
                    }
                    if delete {
                        deleting = Some(i);
                    }
                }
            });
        theme::fade_scrolled(ui, &list_out);
        if let Some(i) = clicked {
            if let Some(name) = self.versions.get(i).map(|v| v.name.clone()) {
                aicraft_core::audit::detail(format!("版本列表：选中 {}", name));
            }
            self.select_version(i);
            self.pending_remove = None;
        }
        // 行尾那两枚图标：先走图标，再回头处理整行 —— 同一帧里只会有一个人按下，
        // 但顺序仍按「离指针最近的那个」来，读起来才是对的。
        if let Some(i) = deleting
            && let Some(name) = self.versions.get(i).map(|v| v.name.clone())
        {
            aicraft_core::audit::action(format!("版本列表：删除 {}（等确认）", name));
            // 删除要对着某个具体版本确认，先把选中项挪过去：
            // 确认框里那句「隔离实例会连存档一起删」是按选中项算的。
            self.select_version(i);
            self.pending_remove = Some(name);
        }
        if let Some(i) = managing
            && let Some(name) = self.versions.get(i).map(|v| v.name.clone())
        {
            aicraft_core::audit::action(format!("版本列表：打开 {} 的管理页", name));
            self.select_version(i);
            self.open_manage(&name);
        }
    }

    /// 进「服务端管理」：第一次进要扫一遍磁盘，还要给「新游戏版本」下拉备一份清单。
    fn open_servers(&mut self) {
        self.page = Page::Servers;
        if !self.servers.loaded {
            self.servers
                    .set_extra(&self.settings.server_dirs, &self.settings.hidden_server_dirs);
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
        if let Ok(slot) = SERVER_VERSIONS.lock()
            && let Some(ids) = slot.as_ref() {
            self.server_versions = ids.clone();
        }
    }

    fn servers_page(&mut self, ui: &mut egui::Ui, busy: bool) {
        if let Ok(slot) = SERVER_VERSIONS.lock() {
            if let Some(ids) = slot.as_ref()
                && self.server_versions.len() != ids.len() {
                self.server_versions = ids.clone();
                self.server_versions_loading = false;
                ui.ctx().request_repaint();
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
                aicraft_core::audit::action("服务端：添加服务端目录");
                self.pick_server_folder();
            }
            Action::OpenManage(dir) => {
                aicraft_core::audit::action(format!("服务端：打开 {} 的管理页", dir.display()));
                self.servers.open_manage(&dir);
            }
            Action::CloseManage => {
                self.servers.managing = None;
            }
            Action::InstallCore(dir) => {
                self.install_server_into(dir);
            }
            Action::AskHide(_) => {
                // `list_view` 自己消费掉了，走不到这里。
            }
            Action::Hide(dir) => {
                aicraft_core::audit::action(format!("服务端：从列表移除 {}", dir.display()));
                let key = dir.to_string_lossy().to_lowercase();
                // 两件事同时做：外部目录从「加进来的名单」里拿掉，
                // 所有目录（含 servers/ 下我们自己装的）都记进隐藏名单。
                // 只做前者的话，servers/ 下的那一份下次扫描又冒出来。
                self.settings
                    .server_dirs
                    .retain(|dir| dir.to_lowercase() != key);
                if !self
                    .settings
                    .hidden_server_dirs
                    .iter()
                    .any(|dir| dir.to_lowercase() == key)
                {
                    self.settings.hidden_server_dirs.push(dir.display().to_string());
                }
                self.persist_settings_quietly();
                let (dirs, hidden) = (
                    self.settings.server_dirs.clone(),
                    self.settings.hidden_server_dirs.clone(),
                );
                self.servers.set_extra(&dirs, &hidden);
                if self
                    .servers
                    .managing
                    .as_ref()
                    .is_some_and(|m| m.dir == dir)
                {
                    self.servers.managing = None;
                }
                self.servers.notice = Some(("已从列表移除，磁盘上的文件没动。".to_string(), false));
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
                let existing = request.existing;
                plan.mcdr = request.mcdr;
                plan.python = request.python.clone();
                plan.mirror = self.settings.mirror;
                plan.limit_kbps = self.settings.download_limit_kbps;
                plan.memory_mb = plan.memory_mb.max(512);
                let where_to = match &target {
                    Some(dir) => dir.display().to_string(),
                    None => format!("servers/{}", plan.name),
                };
                aicraft_core::audit::action(format!(
                    "服务端：{} {}（{} {}）",
                    if existing { "装核心进" } else { "新建" },
                    where_to,
                    plan.core.label(),
                    plan.mc_version
                ));
                let name = plan.name.clone();
                self.servers.installing = Some(name);
                self.servers.progress = Some(serverui::StepProgress::text("准备中…"));

                let (tx, rx) = mpsc::channel();
                self.server_rx = Some(rx);
                let cancel = aicraft_core::cancel::CancelToken::new();
                let progress_tx = tx.clone();
                std::thread::spawn(move || {
                    let on_progress = move |step: aicraft_core::server::Progress| {
                        use aicraft_core::server::Progress;
                        let message = match step {
                            Progress::Stage(text) => ServerMsg::Stage(text),
                            Progress::Downloading { got, total } => ServerMsg::Bytes { got, total },
                            Progress::Libraries { done, total } => {
                                ServerMsg::Libraries {
                                    done: done as u64,
                                    total: total as u64,
                                }
                            }
                        };
                        let _ = progress_tx.send(message);
                    };
                    let log = |line: &str| logging::info(line);
                    let result = match (&target, existing) {
                        // 目录里已经有东西：允许非空，失败也不删目录。
                        (Some(dir), true) => aicraft_core::server::reinstall(
                            &plan,
                            dir,
                            &cancel,
                            &on_progress,
                            log,
                        ),
                        (Some(dir), false) => aicraft_core::server::create_at(
                            &plan,
                            dir,
                            &cancel,
                            &on_progress,
                            log,
                        ),
                        (None, _) => {
                            aicraft_core::server::create(&plan, &cancel, &on_progress, log)
                        }
                    };
                    let _ = tx.send(ServerMsg::Created(Box::new(result)));
                });
            }
            Action::OpenFolder(dir) => {
                open_in_explorer(&dir);
            }
            Action::OpenConfig(path) => {
                aicraft_core::audit::action(format!("服务端：打开配置 {}", path.display()));
                open_config_file(&path);
            }
            Action::SetEula { dir, accepted } => {
                aicraft_core::audit::action(format!(
                    "服务端：{} 的 EULA {}",
                    dir.display(),
                    if accepted { "同意" } else { "撤回" }
                ));
                match aicraft_core::server::set_eula(&dir, accepted) {
                    Ok(()) => {
                        self.servers.reload();
                        // 管理页里那一份也要跟着更新，否则按钮还停在旧状态。
                        if let Some(managing) = self.servers.managing.as_mut() {
                            managing.eula = accepted;
                        }
                    }
                    Err(e) => {
                        if let Some(managing) = self.servers.managing.as_mut() {
                            managing.error = Some(e);
                        } else {
                            self.servers.notice = Some((e, true));
                        }
                    }
                }
            }
            Action::SaveConfig { path, label, props } => {
                aicraft_core::audit::action(format!("服务端：保存 {} 的配置", label.display()));
                match props.save(&path) {
                    Ok(()) => serverui::config_saved(&mut self.servers),
                    Err(e) => serverui::config_failed(&mut self.servers, e),
                }
            }
            Action::ProbeEnv => {
                // 探测要起几个子进程（`py -3`、`python -m mcdreforged -V`），
                // 加起来一秒多，不能卡在绘制里。
                let (tx, rx) = mpsc::channel();
                self.server_rx = Some(rx);
                std::thread::spawn(move || {
                    let env = aicraft_core::mcdr::detect();
                    let _ = tx.send(ServerMsg::Env {
                        manage: false,
                        env,
                    });
                });
            }
            Action::CheckMcdrUpdate => {
                let (tx, rx) = mpsc::channel();
                self.server_rx = Some(rx);
                std::thread::spawn(move || {
                    let _ = tx.send(ServerMsg::McdrUpdate {
                        manage: false,
                        result: aicraft_core::mcdr::latest_version(),
                    });
                });
            }
            Action::OpenPythonDownload => {
                open_external_url("https://www.python.org/downloads/");
            }
            Action::ProbeManageEnv => {
                if let Some(managing) = self.servers.managing.as_mut() {
                    managing.probing = true;
                }
                let (tx, rx) = mpsc::channel();
                self.server_rx = Some(rx);
                std::thread::spawn(move || {
                    let env = aicraft_core::mcdr::detect();
                    let _ = tx.send(ServerMsg::Env { manage: true, env });
                });
            }
            Action::CheckManageUpdate => {
                let (tx, rx) = mpsc::channel();
                self.server_rx = Some(rx);
                std::thread::spawn(move || {
                    let _ = tx.send(ServerMsg::McdrUpdate {
                        manage: true,
                        result: aicraft_core::mcdr::latest_version(),
                    });
                });
            }
            Action::AskMcdr(dir) => {
                self.servers.confirm_mcdr = Some(dir);
            }
            Action::AdoptMcdr(dir) => {
                aicraft_core::audit::action(format!(
                    "服务端：给 {} 启用 MCDReforged（不可逆）",
                    dir.display()
                ));
                let python = self
                    .servers
                    .managing
                    .as_ref()
                    .and_then(|m| m.env.as_ref())
                    .and_then(|env| env.python.as_ref())
                    .map(|python| python.path.clone());
                let Some(python) = python else {
                    self.servers.notice = Some(("先把 MCDR 环境备齐".to_string(), true));
                    return;
                };
                let memory = self.settings.memory_mb();
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.servers.installing = Some(name);
                self.servers.progress = Some(serverui::StepProgress::text("准备中…"));
                let (tx, rx) = mpsc::channel();
                self.server_rx = Some(rx);
                let cancel = aicraft_core::cancel::CancelToken::new();
                let progress_tx = tx.clone();
                std::thread::spawn(move || {
                    let on_progress = move |step: aicraft_core::server::Progress| {
                        use aicraft_core::server::Progress;
                        let message = match step {
                            Progress::Stage(text) => ServerMsg::Stage(text),
                            Progress::Downloading { got, total } => ServerMsg::Bytes { got, total },
                            Progress::Libraries { done, total } => ServerMsg::Libraries {
                                done: done as u64,
                                total: total as u64,
                            },
                        };
                        let _ = progress_tx.send(message);
                    };
                    let log = |line: &str| logging::info(line);
                    let result = aicraft_core::server::adopt_mcdr(
                        &dir,
                        &python,
                        memory,
                        &cancel,
                        &on_progress,
                        log,
                    );
                    let _ = tx.send(ServerMsg::Created(Box::new(result)));
                });
            }
            Action::InstallMcdr(dir) => {
                aicraft_core::audit::action(format!(
                    "服务端：在 {} 里建一份带 MCDR 的",
                    dir.display()
                ));
                // 和第 3 步自己勾上那个勾选框等价，只是提前打好了。
                // 管理页刚探过的环境一并带过去：再让玩家探第二次没道理。
                let env = self
                    .servers
                    .managing
                    .as_ref()
                    .and_then(|m| m.env.clone());
                self.install_server_into(dir);
                self.servers.form.mcdr = true;
                self.servers.form.env = env;
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
                Ok(ServerMsg::Stage(text)) => {
                    // 换阶段就重开一份进度：上个阶段的速度不该留到这个阶段，
                    // 而且「查清单 / 读版本描述」这两句会连着来。
                    match self.servers.progress.as_mut() {
                        Some(step) => step.restart(text),
                        None => {
                            self.servers.progress = Some(serverui::StepProgress::text(text));
                        }
                    }
                }
                Ok(ServerMsg::Bytes { got, total }) => {
                    if let Some(step) = self.servers.progress.as_mut() {
                        step.absorb(got, total);
                    }
                }
                Ok(ServerMsg::Libraries { done, total }) => {
                    if let Some(step) = self.servers.progress.as_mut() {
                        // 预置库这一步是文件计数，不是字节数：速度对它没意义。
                        step.text = "预置依赖库".to_string();
                        step.ratio = (total > 0).then(|| (done as f32 / total as f32).min(1.0));
                        step.detail = Some(format!("{} / {} 个", done, total));
                        step.speed_bps = 0.0;
                    }
                }
                Ok(ServerMsg::Created(result)) => {
                    match *result {
                        Ok(server) => {
                            logging::info(format!("服务端 {} 已安装完成", server.name));
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
                                self.servers
                    .set_extra(&self.settings.server_dirs, &self.settings.hidden_server_dirs);
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
                Ok(ServerMsg::Env { manage, env }) => {
                    if manage {
                        if let Some(managing) = self.servers.managing.as_mut() {
                            managing.probing = false;
                            managing.env = Some(env);
                        }
                    } else {
                        self.servers.form.probing = false;
                        self.servers.form.env = Some(env);
                    }
                    keep = false;
                }
                Ok(ServerMsg::McdrUpdate { manage, result }) => {
                    match result {
                        Ok(latest) => {
                            let current = if manage {
                                self.servers
                                    .managing
                                    .as_ref()
                                    .and_then(|m| m.env.as_ref())
                                    .and_then(|env| env.mcdr_version.clone())
                            } else {
                                self.servers
                                    .form
                                    .env
                                    .as_ref()
                                    .and_then(|env| env.mcdr_version.clone())
                            }
                            .unwrap_or_default();
                            let text = if current == latest {
                                format!("MCDReforged {} 就是最新的", latest)
                            } else {
                                format!("新版是 {}（这里是 {}）", latest, current)
                            };
                            logging::info(&text);
                            self.servers.notice = Some((text, false));
                        }
                        Err(e) => {
                            logging::warn(&e);
                            self.servers.notice = Some((e, true));
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


    /// 这一帧换页过渡的进度（0 到 1）。None = 不播（动画关着、或已经播完）。
    ///
    /// 换页的判断放在这里而不是各个入口：只要 `self.page` 和上一帧画出来的
    /// 不是同一页，就重新起一次。入口有七八个，逐个加一句太容易漏。
    fn page_fade(&mut self, ctx: &egui::Context) -> Option<f32> {
        if !self.settings.animate_ui {
            // 关掉时把状态清干净：再打开时才会从零开始，而不是接着旧进度。
            self.fade_mark = self.page;
            self.fade_start = None;
            return None;
        }

        if self.fade_mark != self.page {
            self.fade_mark = self.page;
            self.fade_start = Some(std::time::Instant::now());
            // 换到一页就等于换了一批内容：标题滑入与列表的逐行进场一起重播。
            let now = std::time::Instant::now();
            self.rows_reveal_at = Some(now);
            self.dirs_reveal_at = Some(now);
            self.title_slide_at = Some(now);
            // 下载版本页的清单也要跟着重播：它自己那份时间戳平时停在 None
            // （上一批早播完了），不重设的话换页回来会直接跳出来。
            self.version_mgr.reveal_at = Some(now);
        }

        let start = self.fade_start?;
        let elapsed = start.elapsed().as_secs_f32();
        if elapsed >= PAGE_FADE_SECS {
            self.fade_start = None;
            return None;
        }

        // 过渡途中 egui 默认会睡过去，得自己把下一帧叫醒。
        ctx.request_repaint();
        Some((elapsed / PAGE_FADE_SECS).clamp(0.0, 1.0))
    }

    /// 侧栏换层（一级 ⇄ 二级目录）这一帧的进度：0 停在一级，1 停在二级。
    ///
    /// 和换页一样，判断只写一处：进二级目录有「版本管理 / 资源下载 /
    /// 拓展功能」三个入口，退回一级还有二级目录里那一行返回，
    /// 逐个在旁边记一次总会漏掉一个。交给 egui 自己补间，
    /// 中途改主意（点进去又马上退回来）也能从当前值接着走，不会跳。
    fn nav_switch(&self, ctx: &egui::Context) -> f32 {

        let target = self.page.nav_group() != 0;
        if !self.settings.animate_ui {
            // 动画关着就直接落位，不记中间值，省得下次打开时从半路接着走。
            return if target { 1.0 } else { 0.0 };
        }
        let id = egui::Id::unique(egui::IdSalt::new("nav-switch"));
        ctx.animate_bool_with_time(id, target, NAV_SWITCH_SECS)
    }

    /// 正文：按当前页画各自的内容。
    ///
    /// 单拆一个函数是因为换页过渡要把它整个包起来（先画完、再整体变换），
    /// 顺手也让 `ui` 里那个 match 只留一处。
    fn page_body(&mut self, ui: &mut egui::Ui, busy: bool) {
        match self.page {
            Page::Home => self.home_page(ui, busy),
            Page::Dirs => self.dirs_page(ui, busy),
            Page::Versions => self.versions_page(ui, busy),
            Page::Download => self.download_page(ui, busy),
            Page::Resources => self.resources_page(ui, busy),
            Page::Settings => self.settings_page(ui, busy),
            Page::Personalize => self.personalize_page(ui, busy),
            Page::Servers => self.servers_page(ui, busy),
        }
    }

    /// 「个性化」页：目前只有动画一条。
    fn personalize_page(&mut self, ui: &mut egui::Ui, _busy: bool) {
        let before = self.settings.clone();

        ui.horizontal(|ui| {
            ui.label(RichText::new("个性化").size(17.0).strong());
        });
        ui.add_space(8.0);

        // 和设置页同一套宽度算法：右让 24px，滚动条才不压着卡片边。
        let card_width = (ui.available_width() - 32.0 - 24.0).max(320.0);

        egui::ScrollArea::vertical()
            .id_salt("personalize-page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                settings::section(ui, "界面动画");
                theme::card().show(ui, |ui| {
                    ui.set_width(card_width);
                    if theme::switch(ui, "界面切换时播放过渡动画", self.settings.animate_ui, true)
                        .clicked()
                    {
                        self.settings.animate_ui = !self.settings.animate_ui;
                        // 刚打开就当场播一次，不用再换一页才知道效果。
                        self.fade_start = if self.settings.animate_ui {
                            Some(std::time::Instant::now())
                        } else {
                            None
                        };
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "关闭后页面与侧边栏的切换立即完成，不再有过渡过程。仅影响界面，不影响游戏。",
                        )
                        .size(10.5)
                        .color(theme::TEXT_WEAK),
                    );
                });

                ui.add_space(6.0);
                settings::section(ui, "背景图与透明度");
                theme::card().show(ui, |ui| {
                    ui.set_width(card_width);
                    self.wallpaper_card(ui);
                    settings::divider(ui);
                    self.alphas_card(ui);
                });

                ui.add_space(6.0);
                settings::section(ui, "界面主题色");
                theme::card().show(ui, |ui| {
                    ui.set_width(card_width);
                    let mut picked = self.settings.theme_color;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                        for color in aicraft_core::settings::ThemeColor::ALL {
                            if theme::color_swatch(ui, color, color == self.settings.theme_color)
                                .on_hover_text(color.label())
                                .clicked()
                            {
                                picked = color;
                            }
                        }
                    });
                    if picked != self.settings.theme_color {
                        self.settings.theme_color = picked;
                        // 强调色进了全局样式，改完得立刻重装一次，
                        // 否则要等下次启动才看得见新颜色。
                        theme::install(ui.ctx(), picked);
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "当前：{}。取自游戏里的十六种染料色，改的是按钮、选中项与强调文字的颜色。",
                            self.settings.theme_color.label()
                        ))
                        .size(10.5)
                        .color(theme::TEXT_WEAK),
                    );
                });
            });

        if self.settings != before {
            aicraft_core::audit::log_settings_changes(&before, &self.settings);
            self.persist_settings_quietly();
        }
    }

    /// 个性化页里的「背景图」那张卡。
    ///
    /// 图本身存在数据目录里（见 `settings::background_path`），设置里只留
    /// 一个原文件名：够在界面上说「现在用的是哪张」，也不会因为玩家把
    /// 原图挪走就整块失效。
    fn wallpaper_card(&mut self, ui: &mut egui::Ui) {
        let using_default = self.settings.background_image.trim()
            == aicraft_core::settings::DEFAULT_BACKGROUND_NAME;
        ui.horizontal(|ui| {
            if theme::dialog_button(ui, "选择图片", true, 96.0).clicked() {
                self.pick_wallpaper();
            }
            ui.add_space(8.0);
            // 换成自己的图之后，出厂那张要留一条路回去——不然只能重装。
            if theme::dialog_button(ui, "恢复默认", !using_default, 96.0).clicked() {
                self.restore_default_wallpaper();
            }
            ui.add_space(8.0);
            let has = !self.settings.background_image.trim().is_empty();
            if theme::dialog_button(ui, "移除背景", has, 96.0).clicked() {
                self.clear_wallpaper();
            }
            ui.add_space(10.0);
            let name = self.settings.background_image.trim();
            if name.is_empty() {
                ui.label(
                    RichText::new("没有背景图，整块界面回到不透明的浅色底。")
                        .size(10.5)
                        .color(theme::TEXT_WEAK),
                );
            } else if using_default {
                ui.label(
                    RichText::new("出厂壁纸")
                        .size(10.5)
                        .color(theme::TEXT_WEAK),
                );
            } else {
                ui.label(
                    RichText::new(name)
                        .size(10.5)
                        .color(theme::TEXT_WEAK),
                );
            }
        });

        ui.add_space(8.0);
        if let Some(note) = self.wallpaper_note.clone() {
            ui.add_space(4.0);
            ui.label(RichText::new(note).size(10.5).color(theme::DANGER));
        }
    }

    /// 背景卡下半截：三块底色各一根滑杆——顶栏与侧栏、中央区、卡片。
    ///
    /// 顶栏和侧栏合成一根：它们是同一圈贴着窗口边的白栏，分开调只会调出一
    /// 道不齐的边。值只在摆了背景图时看得见效果——没有图可透，各块一律是
    /// 不透明原色。
    fn alphas_card(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        changed |= Self::alpha_row(ui, "顶栏与侧边栏", &mut self.settings.ui_alphas.chrome);
        ui.add_space(2.0);
        changed |= Self::alpha_row(ui, "右侧背景", &mut self.settings.ui_alphas.background);
        ui.add_space(2.0);
        changed |= Self::alpha_row(ui, "每一张卡片", &mut self.settings.ui_alphas.card);
        if changed {
            // 当场灌进主题：底图看得见，拖的时候整块界面跟着变。
            theme::set_alphas(self.settings.ui_alphas);
        }

        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "越往左越透，底图越清楚；往右收回来则字更稳。没有背景图时这几根滑杆不产生任何变化。",
            )
            .size(10.5)
            .color(theme::TEXT_WEAK),
        );
    }

    /// 透明度的每一行：名字 + 滑杆 + 百分比。
    ///
    /// 滑杆内部是 0..=1，这里一路映射到 0..=255：最左全透、最右全不透。
    /// 百分比按 255 折算，和设置里存的原始值对得上。
    fn alpha_row(ui: &mut egui::Ui, label: &str, value: &mut u8) -> bool {
        let mut ratio = (*value as f32 / 255.0).clamp(0.0, 1.0);
        let changed = theme::slider_row(ui, label, &mut ratio, |ratio| {
            format!("{}%", (ratio * 255.0).round() as u32 * 100 / 255)
        });
        if changed {
            *value = (ratio * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        changed
    }

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
        let head = theme::head_elapsed(&mut self.title_slide_at, self.settings.animate_ui, ui.ctx());
        let slide = theme::slide_progress(head, 0.0, theme::TITLE_SLIDE_SECS);
        // 各行等标题落位之后再依次浮上来，和版本列表、下载版本用的是同一套节奏。
        let elapsed = theme::row_reveal_elapsed(
            &mut self.dirs_reveal_at,
            self.settings.animate_ui,
            ui.ctx(),
            theme::ROW_REVEAL_DELAY,
        );
        let action = dirs::page(ui, &current, &extra, show_official, busy, slide, elapsed);

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
                self.rows_changed();
                logging::info(format!("已从目录列表移除 {}", path));
            }
            Some(dirs::Action::HideOfficial) => {
                self.settings.hide_official_dir = true;
                self.persist_settings_quietly();
                self.rows_changed();
                logging::info("已将官方目录从列表中移除（磁盘内容未变动）");
            }
            Some(dirs::Action::ShowOfficial) => {
                self.settings.hide_official_dir = false;
                self.persist_settings_quietly();
                self.rows_changed();
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
        let head = theme::head_elapsed(&mut self.title_slide_at, self.settings.animate_ui, ui.ctx());
        let mut action = versions::header(ui, &mut self.version_mgr, busy_arg, head);

        // 第二步（选加载器）整块顶掉版本清单。不是接在清单底下——那样两边
        // 都挤，玩家还得把清单滚开才看得全卡片。清单这会儿也用不上：
        // 版本在第一步就挑定了。
        if self.wizard.as_ref().is_some_and(wizard::wants_inline) {
            self.wizard_card(ui, busy);
        } else if let Some(from_list) = versions::list(
            ui,
            &mut self.version_mgr,
            busy_arg,
            self.settings.animate_ui,
        ) {
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

    /// 换皮肤：先让玩家挑一张 PNG，挑到了就交给衣柜上传。
    ///
    /// 对话框和别的选择框一样直接阻塞主线程几十毫秒 —— 为它再起一个线程
    /// 加一条通道，代码量翻倍而玩家感觉不出区别。
    fn pick_skin_for(&mut self, account: &StoredAccount) {
        if !aicraft_core::skinservice::supports(account.api_root.as_deref()) {
            return;
        }
        let start = PathBuf::from(&self.game_dir);
        let start = if start.is_dir() {
            start
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        };
        match pick_skin_blocking(&start) {
            Ok(Some(path)) => match std::fs::read(&path) {
                Ok(bytes) => {
                    aicraft_core::audit::action(format!("账户：给 {} 上传皮肤", account.name));
                    self.wardrobe.upload(account, bytes);
                }
                Err(e) => {
                    logging::warn(format!("读皮肤图失败：{}", e));
                    self.wardrobe
                        .room(&account.uuid)
                        .note = Some((format!("读不了这张图：{}", e), true));
                }
            },
            Ok(None) => {}
            Err(e) => {
                logging::warn(format!("打开皮肤选择框失败：{}", e));
                self.wardrobe
                    .room(&account.uuid)
                    .note = Some((format!("打不开选择框：{}", e), true));
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
        self.wizard_outcome(state, outcome);
    }

    /// 接住向导吐出来的动作：弹窗和页内卡片走的是同一套。
    fn wizard_outcome(&mut self, mut state: wizard::Wizard, outcome: wizard::Outcome) {
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
        self.wizard_outcome(state, outcome);
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
            logging::warn("尚未配置账户：请设置离线名称或登录正版账户。");
            self.open_account();
            return;
        }
        let game_dir = PathBuf::from(&self.game_dir);
        let (window_width, window_height) = window_size(&self.settings);
        if is_fullscreen(&self.settings) {
            logging::info("游戏窗口：全屏");
        } else if let (Some(width), Some(height)) = (window_width, window_height) {
            // 「最大化」是拿屏幕工作区尺寸冒充的窗口大小，量歪了会开出一个
            // 比桌面还大的窗口 —— 记一行，出问题时能对上号。
            logging::info(format!("游戏窗口：{} × {}", width, height));
        }
        let own = aicraft_core::instancecfg::load(&game_dir, &entry.name);
        let (max_memory_mb, extra_jvm_args, java_path) =
            launch_config_for(&own, &self.settings);
        if !own.follow_global {
            logging::info(format!(
                "{} 用的是它自己的游戏设置：内存 {} MiB，Java {}",
                entry.name,
                max_memory_mb,
                match &java_path {
                    Some(path) => path.display().to_string(),
                    None => "自动选择".to_string(),
                }
            ));
        }
        // 「仍然用它启动」只对这一个实例、这一次有效，用掉就抹掉。
        let allow_java_mismatch = self
            .java_override
            .take()
            .is_some_and(|name| name == entry.name);
        let opts = LaunchOptions {
            max_memory_mb,
            min_memory_mb: self.settings.min_memory_mb,
            extra_jvm_args,
            java_path,
            window_width,
            window_height,
            fullscreen: is_fullscreen(&self.settings),
            high_performance_gpu: self.settings.high_performance_gpu,
            allow_java_mismatch,
        };
        let current = self.account.clone();
        let fallback_name = self.username.clone();
        let launcher_mode = self.settings.launcher_mode;
        // 没有 ID 时不拦启动：离线照样能玩，`prepare_launch_account`
        // 看到空 ID 会直接退回离线并说明原因。
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

        // 弹窗和后台线程共用一个开关：按钮置位，线程在下个步骤之间看见就收手。
        let launch = launchview::Launch::new(&entry.name);
        let token = launch.token.clone();
        self.launch_view = Some(launch);

        self.spawn_task(format!("正在启动 {}", entry.name), move |tx| {
            // 正版会话过期就在后台续一次，别让过期令牌进游戏。
            // 续期要连微软，慢的时候能好几秒——先报一步，别让面板空转。
            report_launch_step(&tx, 1, "检查正版会话…");
            if token.is_cancelled() {
                return Err(CANCELLED.to_string());
            }
            let (account, note, renewed) =
                prepare_launch_account(current.as_ref(), &fallback_name, client_id);
            if let Some(note) = note {
                let _ = tx.send(TaskMsg::Log(note));
            }
            if let Some(account) = renewed {
                let _ = tx.send(TaskMsg::Account(account));
            }
            do_launch(
                &game_dir,
                &entry,
                &account,
                &opts,
                mode,
                launcher_mode,
                &token,
                &tx,
            )
        });
    }

    /// 校验某一份实例的资源。
    ///
    /// 接版本名而不是「当前选中」：这个动作现在从两处来 —— 版本列表行尾的
    /// 齿轮进管理页之后，和首页的齿轮进来一样，选中项未必是眼前这一份。
    fn start_verify(&mut self, instance: &str) {
        let Some(entry) = self
            .versions
            .iter()
            .find(|v| v.name == instance)
            .cloned()
        else {
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

    /// 「缺 Java，要不要我下给你」/「这个 Java 版本对不上」。
    ///
    /// 文案刻意不复述版本号之外的东西：玩家此刻只想知道
    /// 「点这个能不能进去玩」，多说反而要他自己判断。
    ///
    /// 两种形态共用一张卡：`path` 为空是本机根本没有这个主版本
    /// （只能装），有值则是设置里指定了一份对不上的（可以先硬用）。
    fn java_prompt_modal(&mut self, ctx: &egui::Context, busy: bool) {
        let Some(prompt) = self.java_prompt.as_ref() else {
            return;
        };
        let major = prompt.major;
        let mismatch = prompt.path.is_some();
        // 「设置里那份是什么」只用来写字，别在这里动它。
        let pinned_text = match prompt.actual {
            Some(actual) => format!("Java {}", actual),
            None => "无法识别版本的 Java".to_string(),
        };
        // 比要求更老的 Java 读不了这个版本的字节码，硬用也起不来 ——
        // 那种情况就别给一个点了必然失败的按钮。
        let hopeless = mismatch && prompt.actual.is_some_and(|actual| actual < major);
        let progress = prompt.progress.clone();
        let error = prompt.error.clone();
        let running = progress.is_some();
        // 自动下载只认得 8/17/21/25。别给一个点了必然失败的按钮，
        // 那种版本只能自己去装，那就把页面递到眼前。
        let can_install = aicraft_core::javadl::SUPPORTED.contains(&major);

        let mut install = false;
        let mut open_page = false;
        let mut use_anyway = false;
        let mut dismissed = false;
        let response = egui::Modal::new(egui::Id::unique("aicraft-java-prompt"))
            .frame(theme::dialog_frame())
            .show(ctx, |ui| {
                ui.set_width(410.0);
                let title = if mismatch {
                    "指定的 Java 版本与此版本要求不符".to_string()
                } else {
                    format!("此版本需要 Java {}", major)
                };
                ui.label(RichText::new(title).size(15.0).strong());
                ui.add_space(6.0);
                let body = if mismatch {
                    // 更老的是真起不来，更高的是「能跑但别处容易翻车」——
                    // 两句话不能说成同一句。
                    if prompt.actual.is_some_and(|actual| actual < major) {
                        format!(
                            "此版本要求 Java {}，设置中指定的是 {}。低于该要求的 Java 无法加载\
                             此版本的字节码，启动将会失败。",
                            major, pinned_text
                        )
                    } else {
                        format!(
                            "此版本要求 Java {}，设置中指定的是 {}。较高的版本通常仍可运行，\
                             但部分模组与运行环境可能因此出现问题。",
                            major, pinned_text
                        )
                    }
                } else if can_install {
                    format!(
                        "本机未找到可用的 Java {}。可由启动器下载 Eclipse Temurin JRE 并安装至\
                         数据目录，下次启动时自动使用。",
                        major
                    )
                } else {
                    format!(
                        "本机未找到可用的 Java {}，且启动器不提供该版本的自动安装。",
                        major
                    )
                };
                ui.label(
                    RichText::new(body)
                        .size(11.5)
                        .color(theme::TEXT_WEAK),
                );
                ui.add_space(4.0);
                let foot = if !can_install {
                    format!(
                        "启动器仅支持自动安装 Java {}。Java {} 请自行从 Eclipse Temurin 或 \
                         Oracle 下载安装，完成后在「设置 → 游戏 → Java 路径」中指定。",
                        aicraft_core::javadl::SUPPORTED
                            .iter()
                            .map(|major| major.to_string())
                            .collect::<Vec<_>>()
                            .join(" / "),
                        major
                    )
                } else if mismatch {
                    "安装完成后，指定的 Java 会恢复为「自动选择」，以免再次遇到同样的问题。"
                        .to_string()
                } else {
                    "下载大小约 40 MB，优先使用清华大学镜像；镜像不可用时回退至官方源，速度较慢。"
                        .to_string()
                };
                ui.label(
                    RichText::new(foot)
                        .size(11.0)
                        .color(theme::TEXT_FAINT),
                );

                if let Some(line) = &progress {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(line.clone())
                                .size(11.5)
                                .color(theme::accent_strong()),
                        );
                    });
                }
                if let Some(line) = &error {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(line.clone())
                            .size(11.0)
                            .color(theme::DANGER),
                    );
                }

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if theme::dialog_button(ui, "取消", !busy && !running, 92.0).clicked() {
                        dismissed = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if error.is_some() {
                            "重试".to_string()
                        } else if can_install {
                            format!("下载并安装 Java {}", major)
                        } else {
                            "打开下载页".to_string()
                        };
                        if theme::dialog_primary_button(ui, &label, !busy && !running, 150.0)
                            .clicked()
                        {
                            if can_install {
                                install = true;
                            } else {
                                open_page = true;
                            }
                        }
                        if mismatch
                            && !hopeless
                            && theme::dialog_button(ui, "仍用此 Java 启动", !busy && !running, 132.0)
                                .clicked()
                        {
                            use_anyway = true;
                        }
                    });
                });
            });
        if response.should_close() && !running {
            dismissed = true;
        }

        if install {
            aicraft_core::audit::action(format!("Java 版本不符：确认下载安装 Java {}", major));
            if mismatch {
                // 钉子留着的话，装完再启动会再撞上同一堵墙。
                if let Some(instance) = self.selected_version().map(|v| v.name.clone()) {
                    self.forget_pinned_java(&instance);
                }
            }
            if let Some(prompt) = self.java_prompt.as_mut() {
                prompt.progress = Some("正在准备…".to_string());
                prompt.error = None;
            }
            self.start_java_install(major);
        } else if use_anyway {
            aicraft_core::audit::action("Java 版本不符：按用户选择继续使用该 Java");
            // 只放行这一次：设置不动，下次启动还问。
            self.java_override = self.selected_version().map(|v| v.name.clone());
            self.java_prompt = None;
            self.start_launch();
        } else if open_page {
            aicraft_core::audit::action(format!("Java {} 不在自动下载范围：打开下载页", major));
            open_external_url(&format!(
                "https://adoptium.net/temurin/releases/?version={}",
                major
            ));
            // 弹窗留着：玩家装完回来还能看到自己是在解哪一桩事。
        } else if dismissed {
            aicraft_core::audit::action("Java 版本不符：暂不启动");
            self.java_prompt = None;
        }
    }

    /// 撤掉「指定的 Java」这条设置，让自动选择重新做决定。
    ///
    /// 只在玩家点「下载安装」时用：装完如果钉子还在，下一次启动
    /// 会再撞上同一个「版本对不上」，白白多问一遍。
    fn forget_pinned_java(&mut self, instance: &str) {
        let game_dir = PathBuf::from(&self.game_dir);
        let mut own = aicraft_core::instancecfg::load(&game_dir, instance);
        if own.follow_global {
            if self.settings.java_path.take().is_some() {
                if let Err(e) = self.settings.save(&settings_path()) {
                    logging::warn(format!("未能保存到设置：{}", e));
                }
                logging::info("设置中指定的 Java 已恢复为「自动选择」");
            }
        } else if own.java_path.take().is_some() {
            if let Err(e) = aicraft_core::instancecfg::save(&game_dir, instance, &own) {
                logging::warn(format!("未能保存到此实例的设置：{}", e));
            }
            logging::info(format!("{} 指定的 Java 已恢复为「自动选择」", instance));
        }
    }

    /// 下并装一份 Java，装完直接接着启动。
    fn start_java_install(&mut self, major: u32) {
        if self.busy.is_some() {
            return;
        }
        if let Some(entry) = self.selected_version() {
            self.java_retry = Some((entry.name.clone(), major));
        }
        let cancel = aicraft_core::cancel::CancelToken::new();
        self.cancel_token = Some(cancel.clone());
        self.spawn_task(format!("正在安装 Java {}", major), move |tx| {
            // 进度既进底部的进度面板（`LoaderPhase`），也进那张卡片
            // （`JavaInstalled` 之前卡片上要有话可看）。
            let tick = tx.clone();
            let report = move |stage: aicraft_core::javadl::Stage| {
                let _ = tick.send(TaskMsg::LoaderPhase(stage.label()));
            };
            let log_tx = tx.clone();
            let log = move |line: String| {
                let _ = log_tx.send(TaskMsg::Log(line));
            };
            let result = aicraft_core::javadl::install(major, &cancel, Arc::new(report), &log);
            let _ = tx.send(TaskMsg::JavaInstalled(result));
            Ok(())
        });
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
                ui.label(RichText::new("缺少此版本所依赖的基础版本").size(15.0).strong());
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "{} 是安装在 {} 之上的加载器，启动时需要 {} 的清单、库与资源文件。",
                        instance, parent, parent
                    ))
                    .size(11.5)
                    .color(theme::TEXT_WEAK),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "当前 versions/{}/ 中缺少这份清单，启动将无法完成。",
                        parent
                    ))
                    .size(11.5)
                    .color(theme::WARN),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if theme::dialog_button(ui, "取消", !busy, 92.0).clicked() {
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
    fn start_repair(&mut self, instance: &str) {
        let Some(entry) = self
            .versions
            .iter()
            .find(|v| v.name == instance)
            .cloned()
        else {
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
                // 新的一批结果到了，逐行进场从头播一次。
                self.resources.reveal_at = Some(std::time::Instant::now());
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
                        if let Some(view) = self.resources.detail.as_mut()
                            && view.project == detail.id {
                            view.detail = Some(detail);
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
            if let Some(view) = self.resources.detail.as_mut()
                && view.detail.is_none() {
                view.error = Some("取模组详情时连接断了，返回再进一次".to_string());
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
            kind.dir().unwrap_or("")
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

    /// 把详情页里选中的数据包装进当前存档。
    ///
    /// 数据包和模组的落点不一样：它进的是 <实例>/saves/<存档>/datapacks/，
    /// 所以这里除了实例还要再拿一次存档名。
    fn start_install_datapack(&mut self, file: aicraft_core::modrinth::ModFile) {
        if self.busy.is_some() {
            return;
        }
        let Some(target) = self.resources.target.clone() else {
            self.resources.notice = Some(("还没有可用的实例，先把游戏版本装好".to_string(), false));
            return;
        };
        let Some(world) = self.resources.world.clone() else {
            self.resources.notice = Some(("还没有存档，先把游戏开一次让它生成".to_string(), false));
            return;
        };
        let game_dir = PathBuf::from(&self.game_dir);
        let filename = file.filename.clone();
        let label = format!("正在下载数据包 {}", filename);
        aicraft_core::audit::action(format!(
            "资源下载：把数据包 {} 装进 {} 的存档 {}",
            filename, target, world
        ));
        self.spawn_task(label, move |tx| {
            let _ = tx.send(TaskMsg::Log(format!(
                "从 Modrinth 下载数据包 {} 到 {} 的存档 {}…",
                filename, target, world
            )));
            match aicraft_core::modrinth::install_datapack(&game_dir, &target, &world, &file) {
                Ok(dest) => {
                    let _ = tx.send(TaskMsg::Log(format!("已放进 {}", dest.display())));
                    let _ = tx.send(TaskMsg::ModInstalled(Ok(format!(
                        "{} 装好了",
                        dest.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("datapack.zip")
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
    /// 下载详情页里选中的那一版整合包，下完自动送去导入。
    ///
    /// 整合包不能像模组那样「下到 mods/ 里就完事」：它是整个实例的蓝图。
    /// 所以先落到暂存目录，再由 Finished 之后的那一步交给 take_pack_file，
    /// 读清单、让玩家确认实例名，然后才真正开铺。
    fn start_download_pack(&mut self, file: aicraft_core::modrinth::ModFile) {
        if self.busy.is_some() {
            return;
        }
        let staging = aicraft_core::settings::data_dir().join("downloads");
        let filename = file.filename.clone();
        let label = format!("正在下载整合包 {}", filename);
        aicraft_core::audit::action(format!(
            "资源下载：下载整合包 {} 到暂存目录",
            filename
        ));
        self.spawn_task(label, move |tx| {
            let _ = tx.send(TaskMsg::Log(format!("从 Modrinth 下载 {}…", filename)));
            match aicraft_core::modrinth::download_pack(&staging, &file) {
                Ok(dest) => {
                    let _ = tx.send(TaskMsg::Log(format!("已存到 {}", dest.display())));
                    let _ = tx.send(TaskMsg::PackDownloaded(Ok(dest)));
                }
                Err(e) => {
                    let _ = tx.send(TaskMsg::Log(e.clone()));
                    let _ = tx.send(TaskMsg::PackDownloaded(Err(e)));
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
            // 记下「这一份是从包里来的」：整合包自己写着游戏版本与加载器，
            // 那套组合是打包的人验过的，管理页据此把这两项锁住。
            // 写在 import_pack 之后：导入失败时不该留下这个标记。
            let mut config = aicraft_core::instancecfg::load(&game_dir, &instance);
            if !config.from_pack {
                config.from_pack = true;
                if let Err(e) = aicraft_core::instancecfg::save(&game_dir, &instance, &config) {
                    send(&format!("没能记下这份实例的来历（不影响玩）：{}", e));
                }
            }
            // 装好了，暂存的那份包就没用了：它动辄几百兆，留着白占地方。
            // 只在这里删——取消或失败时要留着，玩家重试不用再下一遍。
            aicraft_core::modrinth::discard_pack(&archive);
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
        // 实例名单直接来自上次扫描的结果，不再每帧去读一遍 versions 目录：
        // `rescan()` 的过滤条件与 `installed_instances` 一致（目录 + 同名 json，按名字排），
        // 而它只在启动、安装完、手动刷新时才跑。唯一的差别是清单读不出来的实例会少一个，
        // 那种实例本来也装不进东西。
        let instances: Vec<String> = self.versions.iter().map(|e| e.name.clone()).collect();
        // 详情页要知道「要装进去的那个实例是什么游戏版本」，好把能装的版本
        // 排到前面打上标记。target 要到第一次 `pick_target` 之后才定下来，
        // 所以在这里补，而不是开详情页那一刻。
        if let Some(detail) = self.resources.detail.as_mut()
            && detail.game_hint.is_none()
            && let Some(target) = self.resources.target.clone() {
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
        let busy_hint = self.busy.clone();
        let busy_arg = if busy { busy_hint.as_deref() } else { None };
        // 主页选中的那个版本就是「我常玩的那个」，第一次进资源下载时
        // 默认装到它里面，省得每次都要在下拉里再点一遍。
        let preferred = self.selected_version().map(|entry| entry.name.clone());
        // 页头三层（标题 / 第一排筛选 / 第二排筛选）依次从右侧滑进来，
        // 和「下载版本」同一套手法，只是多一拍。
        let head = theme::head_elapsed_with(
            &mut self.title_slide_at,
            self.settings.animate_ui,
            ui.ctx(),
            theme::RES_HEAD_TOTAL_SECS,
        );
        // 数据包要挑存档：列表跟着当前安装目标那个实例的 saves/ 走。
        let worlds = match self.resources.target.as_ref() {
            Some(target) if self.resources.kind.installs_into_world() => {
                aicraft_core::modrinth::list_worlds(&PathBuf::from(&self.game_dir), target)
            }
            _ => Vec::new(),
        };
        let action = resources::page(
            ui,
            &mut self.resources,
            resources::Targets {
                instances: &instances,
                worlds: &worlds,
            },
            preferred.as_deref(),
            busy_arg,
            self.settings.animate_ui,
            head,
        );
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
            Some(resources::Action::InstallDatapack(file)) => {
                self.start_install_datapack(file)
            }
            Some(resources::Action::DownloadPack(file)) => self.start_download_pack(file),
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
            logging::info(format!("取消安装：已清理本次下载遗留的 {} 处内容", removed));
        } else {
            logging::info("取消安装：没有找到需要清理的文件");
        }
    }

    /// 进入「管理」页（从版本列表进来的，返回时回版本列表）。
    fn open_manage(&mut self, version: &str) {
        self.open_manage_from(version, false);
    }

    /// 进入「管理」页，并记下来路。
    ///
    /// 第二个参数说的是「左上角那颗返回该回哪儿」：从首页标题旁的齿轮进来
    /// 的，退出去还是首页；从版本列表点「管理」进来的，退出去回版本列表。
    /// 管理页盖住整个窗口时侧栏是藏着的，看不出页号，所以这一笔得自己记。
    fn open_manage_from(&mut self, version: &str, from_home: bool) {
        if self.busy.is_some() {
            return;
        }
        let mut state = manage::State::new(Path::new(&self.game_dir), version);
        state.from_home = from_home;
        self.manage = Some(state);
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
                // 从哪儿进来就回哪儿去。
                let from_home = self.manage.as_ref().is_some_and(|state| state.from_home);
                self.manage = None;
                if let Some(page) = page_after_manage_close(from_home) {
                    self.page = page;
                }
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
                    Ok(0) => logging::info(format!("{} 本身即为自包含结构，无需处理", instance)),
                    Ok(bytes) => {
                        logging::info(format!("{} 的清单已压平（{} 字节）", instance, bytes))
                    }
                    Err(e) => logging::warn(format!("{} 清单压平失败：{}", instance, e)),
                }
                if let Some(state) = self.manage.as_mut() {
                    state.refresh_identity(&game_dir);
                }
            }
            manage::Action::DeleteInstance => {
                // 删除的确认框住在主界面上：先退出管理页，再把待删项摆出来。
                // 管理页还开着时它会被那一层盖住，玩家会以为点了没反应。
                let Some(state) = self.manage.as_ref() else {
                    return;
                };
                let instance = state.version.clone();
                if let Some(i) = self.versions.iter().position(|v| v.name == instance) {
                    self.select_version(i);
                }
                self.manage = None;
                // 落在版本列表上：删完那一眼要看见它已经不在列表里了，
                // 回首页只会看到「当前版本」被换成了别的，反而莫名其妙。
                self.page = Page::Versions;
                self.pending_remove = Some(instance);
            }
            manage::Action::VerifyResources => {
                let Some(state) = self.manage.as_ref() else {
                    return;
                };
                let instance = state.version.clone();
                self.start_verify(&instance);
            }
            manage::Action::Delete(path) => {
                let shown = path.display().to_string();
                match aicraft_core::instance::remove_item(&path) {
                    Ok(()) => {
                        logging::info(format!("已删除 {}", shown));
                        if let Some(state) = self.manage.as_mut() {
                            state.reload(Path::new(&self.game_dir));
                        }
                    }
                    Err(e) => logging::warn(format!("删除 {} 失败: {}", shown, e)),
                }
            }
            manage::Action::SaveGameConfig(config) => {
                let Some(state) = self.manage.as_ref() else {
                    return;
                };
                let instance = state.version.clone();
                let game_dir = PathBuf::from(&self.game_dir);
                match aicraft_core::instancecfg::save(&game_dir, &instance, &config) {
                    Ok(()) => {
                        logging::info(format!("{} 的游戏设置已保存", instance));
                        if let Some(state) = self.manage.as_mut() {
                            state.game = (*config).clone();
                        }
                    }
                    Err(e) => logging::warn(format!("{} 的游戏设置保存失败：{}", instance, e)),
                }
            }
            manage::Action::ExportPack {
                format,
                name,
                version,
                include,
            } => self.choose_export_path(format, &name, &version, &include),
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
            manage::Action::ChangeGameVersion(version) => {
                let Some(state) = self.manage.as_ref() else {
                    return;
                };
                let instance = state.version.clone();
                // 换完要按原加载器重装，镜像也就该按它挑：Fabric 与 Forge
                // 走的是两套下载源，拿错了会白白慢一截。
                let kind = state
                    .loader
                    .as_ref()
                    .map(|(kind, _)| *kind)
                    .unwrap_or(aicraft_core::loader::LoaderKind::Fabric);
                let game_dir = PathBuf::from(&self.game_dir);
                let mirror = self.loader_mirror(kind);
                let base_mirror = self.settings.mirror;
                let concurrency = self.settings.concurrency;
                let limit_kbps = self.settings.download_limit_kbps;
                // 新底座还没装过就先把它装上：候选列的是官方清单里的版本，
                // 没装过的那一版得先落盘，换底座才有东西可挂。
                let needs_base = !aicraft_core::install::is_installed(&game_dir, &version);
                self.manage_switch = Some((instance.clone(), format!("Minecraft {}", version)));
                self.download_progress = Some((0, 0, 0));
                self.loader_phase = None;
                self.speed_bps = 0.0;
                self.last_tick_at = None;
                self.last_tick_bytes = 0;
                // 进度面板上的「取消」要能真的停下来：令牌挂到窗口上。
                let cancel = aicraft_core::cancel::CancelToken::new();
                self.cancel_token = Some(cancel.clone());
                self.cancel_confirm = false;
                self.cancel_requested = false;
                self.install_created.clear();
                if needs_base {
                    self.install_created.push(version.clone());
                }
                self.spawn_task(format!("正在把 {} 换到 {}", instance, version), move |tx| {
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
                    if needs_base {
                        do_install(
                            &game_dir,
                            &version,
                            &version,
                            &base_mirror,
                            concurrency,
                            limit_kbps,
                            &cancel,
                            &tx,
                        )?;
                    }
                    aicraft_core::loader::change_game_version(
                        &game_dir,
                        &instance,
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
        let lines: &[&str] = if is_current {
            &["此为当前账号。移除后启动器将回到「未配置」状态，登录凭证一并删除，需重新登录方可再次使用。"]
        } else {
            &["登录凭证将一并删除。如需再次使用，请重新登录。"]
        };
        let ask = theme::Confirm {
            id: "aicraft-remove-account",
            title: "从列表中移除此账号？",
            subject: Some(&name),
            lines,
            danger: true,
            warn: true,
            confirm: "删除",
            width: 380.0,
        };
        match theme::confirm_dialog(ctx, busy, ask) {
            theme::Answer::Confirmed => {
                aicraft_core::audit::action(format!("账户：确认删除 {}", name));
                self.remove_account(&uuid);
                self.third_error = None;
                self.pending_account_remove = None;
            }
            theme::Answer::Dismissed => self.pending_account_remove = None,
            theme::Answer::Pending => {}
        }
    }

    fn delete_confirm_modal(&mut self, ctx: &egui::Context, busy: bool) {
        let Some(name) = self.pending_remove.clone() else {
            return;
        };
        let isolated = self.launch_isolation(&name);
        // 隔离实例一删就带走了存档，说明文字要转警示色；非隔离只是抹掉
        // versions/ 下那一层目录，语气可以放平 —— 红字吓人，别滥用。
        let lines: &[&str] = if isolated {
            &["存档、模组与配置均位于 versions/ 中，将一并删除，且无法恢复。"]
        } else {
            &["仅删除 versions/ 下的这一层目录，共用目录中的存档与模组不受影响。"]
        };
        let ask = theme::Confirm {
            id: "aicraft-delete-version",
            title: "删除此版本？",
            subject: Some(&name),
            lines,
            danger: true,
            warn: isolated,
            confirm: "确认删除",
            width: 380.0,
        };
        match theme::confirm_dialog(ctx, busy, ask) {
            theme::Answer::Confirmed => {
                aicraft_core::audit::action(format!("版本列表：确认删除 {}", name));
                self.delete_version(name);
                self.pending_remove = None;
            }
            theme::Answer::Dismissed => self.pending_remove = None,
            theme::Answer::Pending => {}
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
        let lines: &[&str] = &[
            "下载源、并发、限速、内存、JVM 参数和界面偏好都会回到出厂状态。",
            "游戏目录、账号和已装好的版本不受影响。",
        ];
        let ask = theme::Confirm {
            id: "aicraft-reset-settings",
            title: "恢复默认设置？",
            subject: None,
            lines,
            danger: false,
            warn: false,
            confirm: "恢复默认",
            width: 380.0,
        };
        match theme::confirm_dialog(ctx, busy, ask) {
            theme::Answer::Confirmed => {
                aicraft_core::audit::action("设置：恢复默认设置");
                // 背景图是唯一一份落在数据目录里的「偏好」，恢复默认时也要清掉，
                // 不然文件还在，下次打开图又冒出来。
                let wallpaper = aicraft_core::settings::background_path();
                if let Err(e) = std::fs::remove_file(&wallpaper)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    logging::warn(format!("删背景图失败：{}", e));
                }
                self.wallpaper_note = None;
                self.settings = Settings::default();
                theme::set_alphas(self.settings.ui_alphas);
                logging::info("已恢复默认设置");
                self.reset_confirm = false;
            }
            theme::Answer::Dismissed => self.reset_confirm = false,
            theme::Answer::Pending => {}
        }
    }

    /// 校验完发现缺失时的询问：要不要现在补全。
    fn repair_confirm_modal(&mut self, ctx: &egui::Context, busy: bool) {
        let Some((name, problems)) = self.verify_prompt.clone() else {
            return;
        };
        let detail = format!("{} 中有 {} 个文件缺失或已损坏。", name, problems);
        let lines: &[&str] = &[&detail, "是否立即重新下载？详细信息已记录在日志中。"];
        let ask = theme::Confirm {
            id: "aicraft-repair-version",
            title: "检测到缺失的文件",
            subject: None,
            lines,
            danger: false,
            warn: false,
            confirm: "补全",
            width: 380.0,
        };
        match theme::confirm_dialog(ctx, busy, ask) {
            theme::Answer::Confirmed => {
                aicraft_core::audit::action(format!(
                    "校验：确认补全 {} 的 {} 处缺失",
                    name, problems
                ));
                self.verify_prompt = None;
                // 补全按名字走，顺手把选中项挪过去：玩家回列表时看到的
                // 应该就是他刚补的那个版本。
                if let Some(i) = self.versions.iter().position(|v| v.name == name) {
                    self.selected = Some(i);
                }
                self.start_repair(&name);
            }
            theme::Answer::Dismissed => self.verify_prompt = None,
            theme::Answer::Pending => {}
        }
    }

    /// 下载时贴在窗口底部的进度条：进度、速度、取消。
    ///
    /// 用 `Area` 而不是 `Modal`：它要浮在正文之上、贴着底边，
    /// 同时不挡住上面正在看的东西，也不吃背景点击。
    /// 启动时那张「节点进度」弹窗，连同糊掉的整屏。
    ///
    /// 顺序是有讲究的：先让后台把这一帧的画面抓下来（此时弹窗还没画过，
    /// 抓到的就是干净的首页），下一帧再铺糊图、摆弹窗。抓图没回来之前
    /// 什么都不画——宁可慢半帧，也不要把弹窗自己磨进去。
    fn launch_modal(&mut self, ctx: &egui::Context) {
        let Some(view) = self.launch_view.as_mut() else {
            return;
        };
        view.veil.request(ctx);
        if !view.veil.ready() {
            return;
        }
        let instance = view.instance.clone();
        let cancelling = view.cancelling;
        let (step, total, label) = self
            .launch_phase
            .clone()
            .unwrap_or((1, LAUNCH_STEPS, "准备中…".to_string()));
        // 游标每帧朝目标挪一点：后台报的是整数步，画面上滑过去才不跳。
        let progress = view.advance(step, total, ctx);

        view.veil.paint(ctx);
        let clicked = launchview::show(
            ctx,
            launchview::View {
                instance: &instance,
                step,
                total,
                label: &label,
                cancelling,
                progress,
            },
        );
        if clicked {
            aicraft_core::audit::action("启动弹窗：取消启动");
            if let Some(view) = self.launch_view.as_mut() {
                view.cancel();
            }
        }
    }

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
        // 启动的进度归弹窗管（launchview）：整屏已经糊了，底下再浮一条
        // 卡片只会多出一块没人看的东西。
        if self.launch_view.is_some() {
            return;
        }
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
                    .fill(theme::card_fill())
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
                                                .color(theme::accent_strong()),
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
                                    egui::RichText::new("取消后，本次已下载的文件将被删除。")
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
                    if let Some(state) = self.manage.as_mut()
                        && state.loader_open == Some(kind) {
                        state.set_loader_versions(kind, game_version.clone(), list.clone());
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
                    // 弹窗和糊底一起收掉：窗口都出来了，再盖着就只是碍事。
                    self.launch_view = None;
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
                TaskMsg::PackDownloaded(result) => match result {
                    Ok(path) => {
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("整合包")
                            .to_string();
                        self.resources.notice = Some((format!("{} 下好了，接着装", name), true));
                        // busy 要到 Finished 才清，先搁一帧，等那边收尾再读清单
                        self.pack_download = Some(path);
                    }
                    Err(e) => self.resources.notice = Some((e, false)),
                },
                TaskMsg::Exported(result) => {
                    if let Some(state) = self.manage.as_mut() {
                        state.exporting = None;
                        state.export_notice = Some(match &result {
                            Ok(report) if report.cancelled => ("导出已取消".to_string(), false),
                            Ok(report) => (
                                format!(
                                    "导出好了：{} 个文件，{}",
                                    report.files,
                                    human_size(report.bytes as f64)
                                ),
                                true,
                            ),
                            Err(e) => (format!("导出失败: {}", e), false),
                        });
                    }
                    if let Ok(report) = result
                        && !report.cancelled {
                        let _ = aicraft_core::spawn::silent("explorer.exe")
                            .arg(format!("/select,{}", report.path.display()))
                            .spawn();
                        logging::info(format!(
                            "整合包已导出到 {}（{} 个文件，{}）",
                            report.path.display(),
                            report.files,
                            human_size(report.bytes as f64)
                        ));
                        for line in &report.warnings {
                            logging::warn(line.clone());
                        }
                    }
                }
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
                        // 这里不能调 `self.persist_settings_quietly()`：整份 `&mut self`
                        // 会和循环开头借出去的 `rx` 打架。字段各写各的就不会。
                        self.settings.clamp();
                        if let Err(e) = self.settings.save(&settings_path()) {
                            logging::warn(format!("保存设置失败: {}", e));
                        }
                        self.account = Some(stored.to_account());
                        self.third_pass.clear();
                        self.third_error = None;
                        // 与离线名字那条一样：登上了就留在原地，让下面那张
                        // 「现在挂着谁」的卡片直接告诉玩家结果。
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
                    // 卡片上那份进度跟底部面板同源：同一句话，两个地方都要看得到。
                    if let Some(prompt) = self.java_prompt.as_mut()
                        && prompt.error.is_none() {
                        prompt.progress = Some(text.clone());
                    }
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
                TaskMsg::JavaMissing { major } => {
                    // 记下来等启动失败后开弹窗。这里不立刻开：
                    // 紧接着还有 Finished(Err) 要写进日志，弹窗会遮住它。
                    self.java_prompt = Some(JavaPrompt {
                        major,
                        path: None,
                        actual: None,
                        progress: None,
                        error: None,
                    });
                    logs.push((
                        format!("本机缺少 Java {}：可由启动器下载安装", major),
                        Level::Warn,
                    ));
                }
                TaskMsg::JavaMismatch {
                    major,
                    path,
                    actual,
                } => {
                    // 设置里钉死的那份和这个版本对不上 —— 不替玩家改设置，
                    // 只问一句「确定要用它吗」。设置留在原处，下次还问。
                    let shown = match actual {
                        Some(actual) => format!("Java {}", actual),
                        None => "无法识别版本".to_string(),
                    };
                    logs.push((
                        format!(
                            "设置中指定的 Java 为 {}，此版本要求 Java {}：可在弹窗中直接安装",
                            shown, major
                        ),
                        Level::Warn,
                    ));
                    self.java_prompt = Some(JavaPrompt {
                        major,
                        path: Some(path),
                        actual,
                        progress: None,
                        error: None,
                    });
                }
                TaskMsg::JavaInstalled(result) => {
                    match result {
                        Ok(runtime) => {
                            logging::info(format!(
                                "Java {} 已安装：{}",
                                runtime.major_version,
                                runtime.path.display()
                            ));
                            self.java_prompt = None;
                            // 真正的「接着启动」放在 `finished` 之后：此刻
                            // `busy` 还是「正在安装 Java」，直接起新任务会把它盖掉。
                        }
                        Err(e) => {
                            logging::error(format!("安装 Java 失败：{}", e));
                            if let Some(prompt) = self.java_prompt.as_mut() {
                                prompt.progress = None;
                                prompt.error = Some(e);
                            }
                        }
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
                    } else if matches!(&result, Err(e) if e == CANCELLED) {
                        // 启动被人按了停：不是故障，写一行就够。
                        logs.push((CANCELLED.to_string(), Level::Warn));
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
        // 刚把 Java 装好的话，接着把玩家原本想做的那件事做掉：
        // 他点那一下按钮本来就是为了进游戏，再让他按一次「启动」是多余的。
        let relaunch = if finished {
            self.java_retry.take().filter(|(instance, _)| {
                self.selected_version().map(|v| v.name.clone()) == Some(instance.clone())
            })
        } else {
            None
        };
        if relaunch.is_some() {
            aicraft_core::audit::action("缺 Java：安装完成，继续启动");
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
            // 启动失败 / 被取消：弹窗也得走，不然它会一直挂在「第 2 步」。
            self.launch_view = None;
            // 装失败时把「正在安装 X」收回来。清单那边不归这里管：
            // 拉清单走自己的 `manifest_rx`，这条任务结束跟它没关系。
            self.version_mgr.installing = None;
        }
        // 装完立刻刷新本地列表，让新版本马上可选
        if installed {
            self.rescan();
        }
        // 刚下完的整合包：等 busy 真的清空了再读清单。
        // 上面那段只在 finished 时清 busy，中途别的消息进来时它还在，
        // 这时候交出去会被 take_pack_file 的守卫挡回来，包就白下了。
        if self.busy.is_none()
            && let Some(path) = self.pack_download.take() {
            self.take_pack_file(path);
        }
        if relaunch.is_some() {
            self.start_launch();
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
///
/// 版本列表底部那张详情卡片撤掉之后，界面里已经没有直呼它的地方了
/// —— 那两枚「日志 / 崩溃日志」按钮挪进了管理页的「浏览更多」，
/// 那边直接拿 `State::root`。留着它是因为下面这几条路径规则本身还要被
/// `game_log_dir` / `game_crash_dir` 的用例钉住。
#[cfg(test)]
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
pub(crate) fn game_log_dir(run_dir: &Path) -> Option<PathBuf> {
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
pub(crate) fn game_crash_dir(run_dir: &Path) -> Option<PathBuf> {
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

/// 用系统默认程序打开一个配置文件。
///
/// 走 `explorer.exe <路径>` 而不是 `notepad.exe`：`.properties` / `.toml`
/// 在玩家机器上可能关联给了别的编辑器，硬拉记事本等于替他做决定。
/// 文件不存在时先开目录 —— 双击一个不存在的路径，Windows 只会弹一句
/// 「找不到」，还不如直接把他带到该建文件的地方。
fn open_config_file(path: &Path) {
    if path.is_file() {
        open_in_explorer(path);
    } else if let Some(parent) = path.parent() {
        open_in_explorer(parent);
    }
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
///
/// 不能直接 `cmd.status()` 干等：玩家在最后一步按了取消，这里得当场撒手，
/// 否则那颗按钮按下去只会安静地等满三分钟，然后照样把游戏放出去。
fn wait_for_game_window(pid: u32, cancel: &aicraft_core::cancel::CancelToken) -> bool {
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
    if cancel.is_cancelled() {
        return false;
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            // 盯不上就别拖着玩家：按老样子直接退，至少不比以前差。
            logging::warn(format!("无法监视游戏窗口（{}），将按原样退出", e));
            return true;
        }
    };
    // 盯着子进程自己轮询：`status()` 会一直阻塞到脚本跑完，
    // 中途玩家按了取消也没人接得住。
    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        match child.try_wait() {
            Ok(Some(status)) => return status.code() == Some(0),
            Ok(None) => std::thread::sleep(Duration::from_millis(150)),
            Err(e) => {
                logging::warn(format!("监视游戏窗口时出错（{}）", e));
                return false;
            }
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
        logging::warn("无法记录启动器自身路径，游戏结束后不能自动返回");
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
        logging::warn(format!("无法安排游戏结束后重启启动器：{}", e));
    }
}

/// 不知道百分比时的进度条：一条来回扫的光带。
///
/// 为什么不用 `egui::ProgressBar::animate(true)`：那个动画只是在填充条右端
/// 画一个跳动的圆弧，进度为 0 时整个填充宽度只有 2px，那个圆弧就缩在左上角
/// 一小团，看着像渲染坏了。这里自己画一条在轨道里来回走的带子，意思清楚得多
/// ——「还在动，但别问百分比」。
pub(crate) fn indeterminate_bar(ui: &mut egui::Ui, height: f32) {
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
    clipped.rect_filled(band, egui::CornerRadius::same(9), theme::accent_soft());
    clipped.rect_filled(
        band.shrink2(egui::vec2(0.0, 3.0)),
        egui::CornerRadius::same(6),
        theme::accent_fill(),
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
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && matches!(name, "libraries" | "assets" | "versions" | "cache") {
                removed += remove_part_files(&path);
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
    /// 要传给游戏的窗口尺寸；None 表示交给游戏自己决定。
    window_width: Option<u32>,
    window_height: Option<u32>,
    /// 以独占全屏启动。
    fullscreen: bool,
    /// 启动前把挑中的那个 Java 指定成「高性能显卡」（见 `aicraft_core::gpu`）。
    high_performance_gpu: bool,
    /// 设置里钉死的那个 Java 版本对不上、但玩家已经点头「仍然用它启动」。
    ///
    /// 只对这一次放行：每回启动都要重新问一遍，这是他要的。
    allow_java_mismatch: bool,
}

/// 按设置算出这次该给游戏多大的窗口。
///
/// 四种情况：
/// - 自动：不传，让游戏记住上次的大小；
/// - 指定宽高：照 `window_width/window_height` 传；
/// - 最大化：拿屏幕工作区尺寸当窗口尺寸。Minecraft 没有「最大化」参数，
///   铺满可用桌面就是这个意思（不是独占全屏）；
/// - 全屏：尺寸交给游戏，另由 [`is_fullscreen`] 决定要不要加 `--fullscreen`。
///
/// 工作区尺寸由 `screen::prefetch()` 在启动时后台量好；真走到这里还没量到
/// 就当场量一次（约 0.3 秒），宁可等一下也别让「最大化」无声失效。
fn window_size(settings: &Settings) -> (Option<u32>, Option<u32>) {
    match settings.window_mode {
        aicraft_core::WindowMode::Auto | aicraft_core::WindowMode::Fullscreen => (None, None),
        aicraft_core::WindowMode::Fixed => {
            (Some(settings.window_width), Some(settings.window_height))
        }
        aicraft_core::WindowMode::Maximized => match aicraft_core::screen::cached()
            .or_else(aicraft_core::screen::detect)
        {
            Some(area) => (Some(area.width), Some(area.height)),
            // 屏幕都量不到（远程桌面还没建好之类的），退回自动比传个瞎猜的尺寸强
            None => (None, None),
        },
    }
}

/// 这一份实例这次启动该用的内存 / JVM 参数 / Java。
///
/// 实例自己带了一套就用它的，否则跟全局。**只在这三项上分叉**：最小内存、
/// 窗口大小这些仍然听全局——玩家只是想让这一份多吃点内存，不该顺手把
/// 窗口也变成另一个样子。
fn launch_config_for(
    own: &aicraft_core::instancecfg::InstanceConfig,
    settings: &Settings,
) -> (u32, Vec<String>, Option<PathBuf>) {
    if own.follow_global {
        // 自动模式下这里现算：设置页显示多少，启动时就给多少。
        // 用 `memory_mb()` 而不是 `max_memory_mb`，否则界面上写着
        // 「自动分配 4.0 GiB」、真跑起来还是旧的手动值。
        return (
            settings.memory_mb(),
            settings.extra_jvm_args(),
            settings.java_path.clone(),
        );
    }
    (own.memory_mb(), own.extra_jvm_args(), own.java_path.clone())
}

/// 这次要不要以独占全屏启动。
fn is_fullscreen(settings: &Settings) -> bool {
    settings.window_mode == aicraft_core::WindowMode::Fullscreen
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

/// 玩家按了「取消启动」时，这条任务报回来的那句话。
///
/// 界面按它把日志写成「已取消启动」而不是红色的「失败」——玩家自己停的事，
/// 不该看起来像出了故障。
const CANCELLED: &str = "已取消启动";

/// 执行启动。返回游戏是否正常退出。
///
/// cancel 在每一步之间看一眼：启动那几步里有解压、有等待进程，
/// 一旦玩家喊停，能停的都在下一步之前停住，别把半截东西留在盘上。
#[allow(clippy::too_many_arguments)]
fn do_launch(
    game_dir: &Path,
    entry: &VersionEntry,
    account: &Account,
    opts: &LaunchOptions,
    mode: aicraft_core::InstanceMode,
    launcher_mode: aicraft_core::LauncherMode,
    cancel: &aicraft_core::cancel::CancelToken,
    tx: &Sender<TaskMsg>,
) -> Result<(), String> {
    let log = |s: String| {
        let _ = tx.send(TaskMsg::Log(s));
    };

    log(format!("加载 {}", entry.name));
    report_launch_step(tx, 2, "读取版本清单…");
    if cancel.is_cancelled() {
        log("取消启动：还没开始读清单".to_string());
        return Err(CANCELLED.to_string());
    }
    // 走继承链：Forge / NeoForge / Fabric 的实例只声明加载器差异，
    // 主类、库、资源索引都在父版本（原版）里。
    let chain = aicraft_core::version::load_chain_from_dir(game_dir, &entry.name)
        .map_err(|e| format!("读取版本链失败: {}", e))?;
    let legacy = chain.last().and_then(|raw| raw.minecraft_arguments.clone());
    let resolved = resolve::resolve(&chain)
        .map_err(|e| format!("合并失败: {}", e))?
        .with_legacy_arguments(legacy);

    report_launch_step(tx, 3, "挑选 Java 运行时…");
    if cancel.is_cancelled() {
        log("取消启动：清单还没读完".to_string());
        return Err(CANCELLED.to_string());
    }
    // 这个版本要哪个主版本。清单里没写就当 17 —— 和别处一个口径，
    // 猜大了会在老机器上开不出窗口，猜小了还能跑。
    let needed = resolved.java_version.major_version.unwrap_or(17);
    // Java：设置里指定了就用它，否则按版本需要的 Java 版本自动挑
    let java_path = match &opts.java_path {
        Some(path) if path.is_file() => {
            // 钉死的那份到底是不是这个版本要的，得真去问它自己一句 ——
            // 路径里的「21」「25」说明不了任何事，改过名的 jre 也有的是。
            let actual = java::probe_java(path).map(|runtime| runtime.major_version);
            let verdict = java_verdict(needed, actual);
            if verdict != JavaVerdict::Match && !opts.allow_java_mismatch {
                // 不替玩家做主：他要的是「每次启动都提醒」。
                // 报给界面，由它问一句「确定要用这个吗」。
                let _ = tx.send(TaskMsg::JavaMismatch {
                    major: needed,
                    path: path.clone(),
                    actual,
                });
                return Err(match actual {
                    Some(actual) if actual < needed => format!(
                        "设置中指定的 Java {} 低于此版本要求的 Java {}，无法启动",
                        actual, needed
                    ),
                    Some(actual) => format!(
                        "设置中指定的 Java {} 与此版本要求的 Java {} 不符",
                        actual, needed
                    ),
                    None => format!(
                        "无法识别设置中指定的 Java 版本，此版本要求 Java {}",
                        needed
                    ),
                });
            }
            if verdict != JavaVerdict::Match {
                log(format!(
                    "按用户选择继续使用该 Java（此版本要求 Java {}）",
                    needed
                ));
            }
            log(format!("使用设置中指定的 Java：{}", path.display()));
            path.clone()
        }
        Some(path) => {
            return Err(format!("设置中指定的 Java 不存在：{}", path.display()));
        }
        None => {
            let runtimes = java::detect_all();
            let Some((runtime, choice)) = java::pick_java_choice(&runtimes, needed) else {
                // 别只留一句「找不到运行时」——玩家不知道去哪找。
                // 报给界面，由它问一句「要不要我下给你」。
                let _ = tx.send(TaskMsg::JavaMissing { major: needed });
                return Err(format!(
                    "此版本要求 Java {}，本机未找到（可由启动器安装）",
                    needed
                ));
            };
            if let Some(notice) = choice.notice() {
                log(notice);
            }
            log(format!(
                "使用 Java {}（{}）",
                runtime.major_version,
                runtime.path.display()
            ));
            runtime.path.clone()
        }
    };

    // 显卡首选项要在进程起来之前写好：游戏一旦初始化 D3D，读的就是那一刻
    // 注册表里的值，之后再改对这次运行没用。写失败不拦启动——大不了跑核显，
    // 总比一句「启动失败」强。
    // 关掉开关时也走一遍：这样上次写进去的那条会被撤掉，不用玩家自己去
    // Windows 设置里找回来。只在真的要变时才动注册表，平时这一句是空的。
    if let Some(note) = aicraft_core::gpu::ensure(&java_path, opts.high_performance_gpu) {
        log(note);
    }

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
    cfg.window_width = opts.window_width;
    cfg.window_height = opts.window_height;
    cfg.fullscreen = opts.fullscreen;
    cfg.natives_dir = game_dir
        .join("versions")
        .join(&entry.name)
        .join("natives-lib");

    log(format!(
        "实例模式：{}（游戏目录：{}）",
        mode.as_str(),
        cfg.game_dir.display()
    ));

    report_launch_step(tx, 4, "拼装启动参数…");
    if cancel.is_cancelled() {
        log("取消启动：Java 还没挑好".to_string());
        return Err(CANCELLED.to_string());
    }
    let plan = launch::build_plan(&resolved, account, &cfg);

    // natives
    report_launch_step(tx, 5, "解压游戏原生库…");
    if cancel.is_cancelled() {
        log("取消启动：参数还没拼完".to_string());
        return Err(CANCELLED.to_string());
    }
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
    if cancel.is_cancelled() {
        log("取消启动：原生库还没解压完".to_string());
        return Err(CANCELLED.to_string());
    }
    log("正在启动…".to_string());
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

    // 进程刚起来那一下也可能已经按了取消：这时候游戏正在读模组，
    // 窗口还没出来，但进程是实打实在跑的，得收掉。
    if cancel.is_cancelled() {
        stop_game(&mut child);
        log("取消启动：游戏进程已结束".to_string());
        let _ = tx.send(TaskMsg::GameWindow { pid, shown: false });
        return Err(CANCELLED.to_string());
    }

    if launcher_mode != aicraft_core::LauncherMode::KeepOpen {
        // 等窗口真的出来再让界面关。进程起来只是一瞬间的事，
        // 玩家眼里「还没反应」的那十几秒全在窗口出现之前。
        let shown = wait_for_game_window(pid, cancel);
        if cancel.is_cancelled() {
            stop_game(&mut child);
            log("取消启动：游戏进程已结束".to_string());
            let _ = tx.send(TaskMsg::GameWindow { pid, shown: false });
            return Err(CANCELLED.to_string());
        }
        if shown {
            log("游戏窗口已出现，启动器将按设置退出".to_string());
        } else {
            log("未等到游戏窗口出现，启动器保持运行".to_string());
        }
        let _ = tx.send(TaskMsg::GameWindow { pid, shown });
        // 不等了。丢下的子进程不会跟着启动器一起退出。
        return Ok(());
    }

    // 选的是「关掉启动器等游戏退出」：这里同样不能死等，
    // 玩家中途按取消就得把游戏收掉，而不是等到它自己关掉窗口。
    let status = loop {
        if cancel.is_cancelled() {
            stop_game(&mut child);
            log("取消启动：游戏进程已结束".to_string());
            return Err(CANCELLED.to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(format!("等待进程失败: {}", e)),
        }
    };

    if status.success() {
        log("游戏已正常退出".to_string());
        Ok(())
    } else {
        Err(format!("游戏异常退出: {}", status))
    }
}

/// 取消启动时，把已经拉起来的游戏收掉。
///
/// 走到第 6 步时进程已经在跑了（模组多的整合包这时候正在读配置，窗口还没出来）。
/// 玩家按的是「取消启动」，屏幕后面不该留一个自己都不知道自己不该启动的游戏——
/// 那玩意儿过十几秒照样会开出窗口来。
fn stop_game(child: &mut std::process::Child) {
    // 已经自己退了就别再补一刀，`kill` 对着尸体只会报一句没用的错。
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    if let Err(e) = child.kill() {
        logging::warn(format!("结束游戏进程失败：{}", e));
        return;
    }
    let _ = child.wait();
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

/// 登录成功后统一收尾：过完 Xbox → Minecraft 三跳、拿到档案。
///
/// 换 Xbox Live → 换 XSTS → 登录 Minecraft → 读档案这四步都是网络往返，
/// 加起来能有几秒；一声不吭的话界面就只剩一句「正在连接微软…」。
///
/// 登进来的号直接进账号列表：这两页的职责只是「把号登进来」，登完就
/// 出现在「已保存账户」里，管理动作（续期、删除、换皮肤）都在那张卡片里，
/// 这两页不再各摆一套按钮。
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
        AuthErrorKind::MissingClientId => {
            "这一份构建没有带 Azure 应用 ID，正版登录用不了；请用带 ID 的版本，或自己编译一份"
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

/// 把一段用户输入变成能当文件名的样子。
///
/// Windows 不许这几个字符出现在文件名里，而整合包名字是玩家自己填的，
/// 里头出现「我的包/第一版」这种写法一点也不奇怪。清洗之后为空（比如
/// 只填了「///」）就退回调用方给的兜底名字。
fn sanitize_file_name(text: &str, fallback: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '-',
            c if (c as u32) < 0x20 => '-',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

/// PowerShell 单引号字符串里，单引号本身要写成两个。
#[cfg(windows)]
fn ps_quote(text: &str) -> String {
    text.replace('\'', "''")
}

/// 拼一段 WinForms 对话框脚本：先把输出编码钉成 UTF-8，再载入 Forms。
///
/// 不钉编码的话，中文路径会按控制台的默认代码页吐回来，变成一堆问号。
#[cfg(windows)]
fn dialog_script(body: &str) -> String {
    format!(
        "[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false\n\
         Add-Type -AssemblyName System.Windows.Forms\n\
         {body}"
    )
}

/// 目录存在才预选，免得对话框一开就报「路径不存在」。
#[cfg(windows)]
fn dialog_initial(field: &str, dir: &str) -> String {
    if dir.trim().is_empty() {
        return String::new();
    }
    let dir = ps_quote(dir);
    format!("if (Test-Path -LiteralPath '{dir}' -PathType Container) {{ $dlg.{field} = '{dir}' }}")
}

/// 点「确定」才把选中的东西写到 stdout；点取消就什么都不写，调用方据此判断。
#[cfg(windows)]
fn dialog_write(field: &str) -> String {
    format!(
        "if ($dlg.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ [Console]::Out.Write($dlg.{field}) }}"
    )
}

/// 跑一段 PowerShell 对话框脚本，把它在 stdout 上写出来的路径收回来。
///
/// 空 stdout 就是玩家点了取消。四个选择框（选目录 / 选整合包 / 选 Java /
/// 另存为）只在脚本正文和报错措辞上不同，启动、排错、解析全在这里做。
#[cfg(windows)]
fn run_dialog(script: &str, what: &str) -> Result<Option<PathBuf>, String> {
    use std::process::Stdio;

    // `spawn::silent` 已经带了 CREATE_NO_WINDOW，不用再补一次
    let output = aicraft_core::spawn::silent("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", script])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("无法启动{}: {}", what, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("{}异常退出（代码 {:?}）", what, output.status.code())
        } else {
            format!("{}出错: {}", what, stderr)
        });
    }

    let picked = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if picked.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(picked)))
    }
}

/// 弹出系统自带的文件夹选择框，返回 Ok(Some(路径)) 表示选中、Ok(None) 表示取消。
///
/// 为什么不直接引一个对话框 crate：本机连 crates.io 都拉不到，仓库要能就地编译，
/// 而 egui 没有内置的原生文件对话框。借 PowerShell 的 `FolderBrowserDialog`
/// 来干活，和 core 里借系统 curl 干 TLS 是同一种取舍。
#[cfg(windows)]
fn pick_folder_blocking(initial: &str) -> Result<Option<PathBuf>, String> {
    let body = format!(
        "$dlg = New-Object System.Windows.Forms.FolderBrowserDialog\n\
         $dlg.Description = '选择 Minecraft 游戏目录'\n\
         $dlg.ShowNewFolderButton = $true\n\
         {}\n\
         {}",
        dialog_initial("SelectedPath", initial),
        dialog_write("SelectedPath"),
    );
    run_dialog(&dialog_script(&body), "文件夹选择框")
}

#[cfg(not(windows))]
fn pick_folder_blocking(_initial: &str) -> Result<Option<PathBuf>, String> {
    Err("系统文件夹选择框目前只在 Windows 上实现".to_string())
}

/// 挑一个整合包文件。
///
/// 过滤器和 `packui::looks_like_pack` 保持一致：`.mrpack` 与 `.zip`。
/// 别的格式（比如 CurseForge 的 `.zip` 我们其实读得了清单，但装不出正确的
/// 结果）不在这一版的范围里。
#[cfg(windows)]
fn pick_pack_blocking(initial_dir: &Path) -> Result<Option<PathBuf>, String> {
    let body = format!(
        "$dlg = New-Object System.Windows.Forms.OpenFileDialog\n\
         $dlg.Title = '选择整合包'\n\
         $dlg.Filter = '整合包 (*.mrpack;*.zip)|*.mrpack;*.zip|所有文件 (*.*)|*.*'\n\
         $dlg.CheckFileExists = $true\n\
         {}\n\
         {}",
        dialog_initial("InitialDirectory", &initial_dir.display().to_string()),
        dialog_write("FileName"),
    );
    run_dialog(&dialog_script(&body), "文件选择对话框")
}

#[cfg(not(windows))]
fn pick_pack_blocking(_initial_dir: &Path) -> Result<Option<PathBuf>, String> {
    Err("系统文件选择框目前只在 Windows 上实现".to_string())
}

/// 挑一张皮肤图。
///
/// 只放 PNG：官方只收 PNG，放别的进来服务器也会拒，不如在框里就挡住。
#[cfg(windows)]
fn pick_skin_blocking(initial_dir: &Path) -> Result<Option<PathBuf>, String> {
    let body = format!(
        "$dlg = New-Object System.Windows.Forms.OpenFileDialog
\
         $dlg.Title = '选择皮肤图（64×64 或 64×32 的 PNG）'
\
         $dlg.Filter = '皮肤 (*.png)|*.png|所有文件 (*.*)|*.*'
\
         $dlg.CheckFileExists = $true
\
         {}
\
         {}",
        dialog_initial("InitialDirectory", &initial_dir.display().to_string()),
        dialog_write("FileName"),
    );
    run_dialog(&dialog_script(&body), "文件选择对话框")
}

#[cfg(not(windows))]
fn pick_skin_blocking(_initial_dir: &Path) -> Result<Option<PathBuf>, String> {
    Err("系统文件选择框目前只在 Windows 上实现".to_string())
}

/// 挑一张背景图。
///
/// 格式清单跟 `wallpaper::decode` 背后那套解码器对齐：png / jpg / webp 够用了，
/// 放 gif 进来只会解出第一帧，不如在框里就不列。
#[cfg(windows)]
fn pick_image_blocking(initial_dir: &Path) -> Result<Option<PathBuf>, String> {
    let body = format!(
        "$dlg = New-Object System.Windows.Forms.OpenFileDialog\n\
         $dlg.Title = '选择背景图片'\n\
         $dlg.Filter = '图片 (*.png;*.jpg;*.jpeg;*.webp)|*.png;*.jpg;*.jpeg;*.webp|所有文件 (*.*)|*.*'\n\
         $dlg.CheckFileExists = $true\n\
         {}\n\
         {}",
        dialog_initial("InitialDirectory", &initial_dir.display().to_string()),
        dialog_write("FileName"),
    );
    run_dialog(&dialog_script(&body), "文件选择对话框")
}

#[cfg(not(windows))]
fn pick_image_blocking(_initial_dir: &Path) -> Result<Option<PathBuf>, String> {
    Err("系统文件选择框目前只在 Windows 上实现".to_string())
}

/// 挑一个 java / javaw 可执行文件。
///
/// 只认 `java.exe` / `javaw.exe`：这两个才是启动器能 exec 的东西。
/// 玩家手头的 JDK 目录里 `bin\` 下就这两个，让他进一级比记路径轻松。
#[cfg(windows)]
fn pick_java_blocking(initial_dir: &str) -> Result<Option<PathBuf>, String> {
    let body = format!(
        "$dlg = New-Object System.Windows.Forms.OpenFileDialog\n\
         $dlg.Title = '选择 Java 可执行文件（JDK 安装目录下的 bin\\javaw.exe）'\n\
         $dlg.Filter = 'Java 可执行文件 (java.exe;javaw.exe)|java.exe;javaw.exe|所有文件 (*.*)|*.*'\n\
         $dlg.CheckFileExists = $true\n\
         {}\n\
         {}",
        dialog_initial("InitialDirectory", initial_dir),
        dialog_write("FileName"),
    );
    run_dialog(&dialog_script(&body), "文件选择对话框")
}

#[cfg(not(windows))]
fn pick_java_blocking(_initial_dir: &str) -> Result<Option<PathBuf>, String> {
    Err("系统文件选择框目前只在 Windows 上实现".to_string())
}

/// 弹系统「另存为」对话框，让用户自己挑日志存哪。
///
/// 和选目录一样走 PowerShell + WinForms（没有额外依赖），对话框阻塞的
/// 是后台线程，所以调用方必须从 `std::thread::spawn` 里调。
#[cfg(windows)]
fn pick_save_blocking(
    initial_dir: &Path,
    file_name: &str,
    title: &str,
    filter: &str,
    default_ext: &str,
) -> Result<Option<PathBuf>, String> {
    let body = format!(
        "$dlg = New-Object System.Windows.Forms.SaveFileDialog\n\
         $dlg.Title = '{}'\n\
         $dlg.Filter = '{}'\n\
         $dlg.DefaultExt = '{}'\n\
         $dlg.AddExtension = $true\n\
         $dlg.OverwritePrompt = $true\n\
         $dlg.FileName = '{}'\n\
         {}\n\
         {}",
        ps_quote(title),
        ps_quote(filter),
        ps_quote(default_ext),
        ps_quote(file_name),
        dialog_initial("InitialDirectory", &initial_dir.display().to_string()),
        dialog_write("FileName"),
    );
    run_dialog(&dialog_script(&body), "保存对话框")
}

#[cfg(not(windows))]
fn pick_save_blocking(
    _initial_dir: &Path,
    _file_name: &str,
    _title: &str,
    _filter: &str,
    _default_ext: &str,
) -> Result<Option<PathBuf>, String> {
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

/// 设置里记着的离线名字是不是被别人抄进来的。
///
/// 老版本登录正版/第三方时会顺手把账号名写进离线名字，于是离线页面上
/// 冒出一个玩家根本没填过的名字。离线名字应当只来自他自己在那张卡片里
/// 写的字，所以这种「和某个登录号同名」的值要认出来清掉；但要是他真把
/// 这个名字存成过离线号，那就是他自己写的，不能动。
fn stale_offline_name(name: &str, accounts: &[StoredAccount]) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    let copied_from_login = accounts
        .iter()
        .any(|a| !a.offline && a.name == name);
    let saved_as_offline = accounts.iter().any(|a| a.offline && a.name == name);
    copied_from_login && !saved_as_offline
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
/// 左侧导航的一级入口：一枚图标 + 一行文字。
///
/// 抽出来是因为导航有两层（一级六项、下面还有二级目录），
/// 每段一模一样的「点没点」没有必要各写一遍。
///
/// `highlight` 传进来是为了把这一行的位置报给那块会滑动的选中底；
/// 正在退场的那一层传 None —— 它报的坐标在屏幕外，会把底拽走。
fn nav_entry(
    ui: &mut egui::Ui,
    glyph: theme::NavGlyph,
    text: &str,
    selected: bool,
    highlight: Option<&theme::NavHighlight>,
) -> bool {
    let response = theme::nav_item_glyph(ui, glyph, text, selected);
    if let Some(highlight) = highlight {
        highlight.row(response.rect, selected);
    }
    let clicked = response.clicked();
    ui.add_space(2.0);
    clicked
}

fn account_kind(stored: &StoredAccount) -> &'static str {
    if stored.offline {
        "离线"
    } else if stored.is_third_party() {
        "第三方"
    } else {
        "微软"
    }
}

/// 界面上给玩家看的全称。
///
/// 日志里用短的那一版（「已移除第三方账号」读着顺），卡片和左栏是给眼睛看
/// 的，缩写留着会让人猜「第三方」到底指什么。
fn account_kind_title(stored: &StoredAccount) -> &'static str {
    if stored.offline {
        "离线账户"
    } else if stored.is_third_party() {
        "第三方账户"
    } else {
        "微软账户"
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

        ui.horizontal_centered(|ui| {
            ui.label(
                RichText::new("AICraft")
                    .size(13.5)
                    .strong()
                    .color(theme::accent_strong()),
            );
            ui.label(
                RichText::new("Launcher")
                    .size(13.5)
                    .strong()
                    .color(theme::TEXT),
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
        if let Some(rx) = &self.mirror_probe_rx
            && matches!(
                rx.try_recv(),
                Ok(()) | Err(std::sync::mpsc::TryRecvError::Disconnected)
            ) {
                self.mirror_probe_rx = None;
                ctx.request_repaint();
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
        // 账户页换过皮肤：那张 32×32 的小脸还是旧的，得放下重新取一次。
        if let Some(uuid) = self.wardrobe.poll(ctx) {
            self.skin_tex.remove(&uuid);
            self.skin_tried.remove(&uuid);
        }
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
        // 启动中那张糊底：截图回来就磨、就传纹理。
        if let Some(view) = self.launch_view.as_mut() {
            view.veil.poll(ctx);
        }
        // 背景图：先收上一次解好的，再看设置里那张要不要重解。
        self.poll_wallpaper(ctx);
        self.kick_off_wallpaper(ctx);
        self.poll_login();
        self.poll_refresh(ctx);
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
            || self.refresh_rx.is_some()
            || self.mirror_probe_rx.is_some()
            || self.wallpaper_rx.is_some()
            // 启动那几步节点要一个一个亮，得一直画
            || self.launch_view.is_some()
            // 卡片上转着圈等人回来，得一直画
            || self.wardrobe.busy();
        if waiting {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let busy = self.busy.is_some();

        // 背景图铺在最底下，之后各块半透明底色才压得住字。
        // 图没装好时这里什么都不画，界面跟没这功能时一样。
        if self.wallpaper.has_image() {
            let rect = ui.max_rect();
            self.wallpaper.paint(ui.painter(), rect);
        }

        // 自绘标题栏：系统栏被关掉了，拖动、最小化、关闭都在这里接。
        egui::Panel::top("titlebar")
            .exact_size(38.0)
            // 高度写死 38，下边缘不该有上下调整光标。
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme::panel_titlebar())
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
        self.java_prompt_modal(ui.ctx(), self.busy.is_some());
        self.account_remove_modal(ui.ctx(), self.busy.is_some());
        // 进度弹窗画在最后面（Foreground 层），压住正文但只占底部一条。
        self.progress_panel(ui.ctx());
        // 启动那张盖满整屏：它自己会先抓一帧底图，所以放在正文之后。
        self.launch_modal(ui.ctx());

        // 「账户」页同样盖住标题栏以下的全部，包括主导航。
        // 它排在最前面：管理与账户不会同时开着，但先画谁决定了视觉层次。
        if self.account_open {
            self.account_page(ui, busy);
        }

        // 「管理」是一层盖住整个窗口（含左侧主导航）的视图：
        // 进来之后视线只在这一份实例上，返回才回到版本列表。
        if self.manage.is_some() {
            // 「更改游戏版本」的候选来自官方清单，跟「下载版本」页是同一份。
            // 那份清单平时只在进下载页时才拉，这里要单独要一次：没拿到就
            // 只能让玩家对着一个空列表发愣。
            if self
                .manage
                .as_ref()
                .is_some_and(|state| state.change_game.is_some())
                && !self.version_mgr.loaded
                && !self.version_mgr.fetching
            {
                self.start_fetch_manifest();
            }
            let mut state = self.manage.take().expect("上面刚判断过");
            let action = manage::page(
                ui,
                &mut state,
                Path::new(&self.game_dir),
                &self.settings,
                &mut self.java_scan,
                &mut self.manage_icons,
                &self.version_mgr.entries,
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
            // 左侧导航：主页 / 版本管理 / 资源下载 / 设置。
            //
            // 「版本管理」不是可展开的分组，而是跳进它自己的二级目录：进了
            // 目录列表 / 版本列表 / 下载版本 任意一页，这一栏整体换成那三项，
            // 顶部一个「‹ 版本管理」回到一级。两层之间是一道分界线匀速往左
            // 推过去：退场那层被推出去、进场那层从右边顶进来。
            egui::Panel::left("nav")
                .exact_size(NAV_WIDTH)
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    egui::Frame::NONE
                        .fill(theme::panel())
                        .inner_margin(egui::Margin::symmetric(NAV_MARGIN, 10)),
                )
                .show(ui, |ui| {
                    // 一级那六项。
                    let in_versions = self.page.in_versions_group();
                    let in_resources = self.page.in_resources_group();
                    let in_more = self.page.in_more_group();
                    let progress = self.nav_switch(ui.ctx());
                    // 两层共用这一块地方，先量一次、之后不再变：
                    // 若每层各自去量 `ui.max_rect()`，第二层量到的已经
                    // 被第一层挪过的位置带偏，越换越偏。
                    let home = ui.max_rect();

                    // 那一块浅绿选中底整栏只有一块：先占个空槽，等两层都量完
                    // 再把它放到该在的那一行。换页时它会自己滑过去。
                    let highlight = theme::NavHighlight::begin(ui);

                    // 两层同时画：退场的那层往左滑出去、进场的那层从右边
                    // 顶进来，分界线走过时各自只露一侧，谁也不叠在谁身上。
                    //
                    // 只有停在原地的那一层把行位置报给那块底：滑出去的那层
                    // 报的是屏幕外的坐标，报上去底会被拽到框外。
                    let settled = progress.clamp(0.0, 1.0);
                    theme::nav_layer(ui, home, false, progress, |ui| {
                        let entry = |ui: &mut egui::Ui,
                                     glyph: theme::NavGlyph,
                                     text: &str,
                                     selected: bool| {
                            let mark = if settled < 0.5 { Some(&highlight) } else { None };
                            nav_entry(ui, glyph, text, selected, mark)
                        };

                        if entry(ui, theme::NavGlyph::Home, "主页", self.page == Page::Home) {
                            aicraft_core::audit::detail("导航：切到「主页」");
                            self.page = Page::Home;
                        }

                        if entry(ui, theme::NavGlyph::Versions, "版本管理", in_versions) {
                            aicraft_core::audit::detail("导航：进入「版本管理」目录");
                            self.page = Page::Versions;
                        }

                        if entry(ui, theme::NavGlyph::Resources, "资源下载", in_resources) {
                            aicraft_core::audit::detail("导航：进入「资源下载」目录");
                            self.page = Page::Resources;
                            self.resources.switch_kind(ProjectKind::Mod);
                            if self.resources.needs_fetch() {
                                self.start_search_mods();
                            }
                        }

                        if entry(ui, theme::NavGlyph::Settings, "设置", self.page == Page::Settings) {
                            aicraft_core::audit::detail("导航：切到「设置」");
                            self.page = Page::Settings;
                        }

                        if entry(
                            ui,
                            theme::NavGlyph::Personalize,
                            "个性化",
                            self.page == Page::Personalize,
                        ) {
                            aicraft_core::audit::detail("导航：切到「个性化」");
                            self.page = Page::Personalize;
                        }

                        if entry(ui, theme::NavGlyph::More, "拓展功能", in_more) {
                            aicraft_core::audit::detail("导航：进入「拓展功能」目录");
                            self.open_servers();
                        }
                    });

                    // 二级目录：一个返回行 + 三项。两套共用一个画法。
                    // 「资源下载」那三项不是页面而是同一页的三种内容，
                    // 所以下标存进 `resources.kind` 而不是 `Page`。
                    theme::nav_layer(ui, home, true, progress, |ui| {
                        let mark = if settled >= 0.5 { Some(&highlight) } else { None };
                        // 二级项也一样：位置报给那块底，选中时它会滑过来。
                        let sub = |ui: &mut egui::Ui, text: &str, selected: bool| {
                            let response = theme::nav_item_sub(ui, text, selected);
                            if let Some(mark) = mark {
                                mark.row(response.rect, selected);
                            }
                            ui.add_space(2.0);
                            response.clicked()
                        };

                        if in_more {
                            if theme::nav_back(ui, "拓展功能").clicked() {
                                aicraft_core::audit::detail("导航：离开「拓展功能」目录");
                                self.page = Page::Home;
                            }
                            ui.add_space(6.0);
                            if sub(ui, "服务端管理", self.page == Page::Servers)
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
                                let clicked = sub(ui, kind.label(), picked);
                                if clicked && !picked {
                                    aicraft_core::audit::detail(format!(
                                        "导航：切到「{}」",
                                        kind.label()
                                    ));
                                    self.resources.switch_kind(kind);
                                    // 换了类别等于换了一整页内容：页头也重新滑进来一次。
                                    self.title_slide_at = Some(std::time::Instant::now());
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
                                let clicked = sub(ui, text, self.page == page);
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
                    });

                    // 两层都排完了，才知道底该停在哪一行。
                    highlight.finish(
                        ui,
                        egui::Id::unique(egui::IdSalt::new("nav-highlight")),
                        self.settings.animate_ui,
                    );

                    // 只占一次位，而且占的是没挪过的那块：两层滑出去的半截
                    // 不该算进面板宽度，否则窄窗口里正文会被挤到右边去。
                    ui.advance_cursor_after_rect(home);
                });

            egui::CentralPanel::default()
                .frame(
                    // 底色只在这里填一次：窗口没有系统边框，Frame 的
                    // inner_margin 那圈也要有颜色，所以不能只靠正文里那句
                    // rect_filled。两处都填就会叠两层，背景图透不上来。
                    egui::Frame::NONE
                        .fill(theme::bg_bottom())
                        .inner_margin(egui::Margin::symmetric(22, 18)),
                )
                .show(ui, |ui| {
                    // 过渡只作用在正文这一层：底纹、导航栏和标题栏都留在原地，
                    // 翻页时它们不动，视线才不会跟着一起晃。
                    match self.page_fade(ui.ctx()) {
                        Some(progress) => {
                            // 只做淡入：位移交给页头的横移，两处都动就成了
                            // 上下左右一起晃，反而看不清是换了页还是页面在抖。
                            ui.set_opacity(fade_alpha_at(progress));
                            self.page_body(ui, busy);
                        }
                        None => self.page_body(ui, busy),
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
        account_kind, account_kind_title, fade_alpha_at, game_crash_dir, game_log_dir, game_run_dir,
        java_verdict,
        launch_config_for, log_text, now_local, is_fullscreen, offline_name_usable, stale_offline_name,
        offline_name_warning, page_after_manage_close, stop_game, wait_for_game_window, window_size,
        JavaVerdict, Page,
    };
    use crate::theme;
    use aicraft_core::auth::store::StoredAccount;
    use std::path::PathBuf;

    #[test]
    fn leaving_the_manage_page_from_the_home_gear_goes_back_home() {
        // 从首页标题旁那颗齿轮进的管理页，退出去必须落回首页；
        // 否则侧栏会亮着「版本管理」，而玩家根本没去过那儿。
        assert!(matches!(page_after_manage_close(true), Some(Page::Home)));
    }

    #[test]
    fn leaving_the_manage_page_from_the_version_list_stays_put() {
        // 从版本列表点「管理」进来的，侧栏本来就停在版本列表上，别动它。
        assert!(page_after_manage_close(false).is_none());
    }

    #[test]
    fn waiting_for_a_window_gives_up_as_soon_as_it_is_cancelled() {
        // 按了取消就不该再起一个 PowerShell 傻等三分钟。
        // 这里故意用一个不存在的 pid：真要跑起来它也会很快 exit 2，
        // 但正确实现根本走不到那一步。
        let token = aicraft_core::cancel::CancelToken::new();
        token.cancel();
        let started = std::time::Instant::now();
        assert!(!wait_for_game_window(999_999, &token));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "取消之后应当立刻返回"
        );
    }

    #[test]
    fn stopping_a_running_game_ends_it() {
        // 玩家在「拉起游戏进程…」这一步按取消时，进程是活的：
        // 收手之后它必须真的没了，否则过十几秒照样开出游戏窗口。
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/C", "ping -n 30 127.0.0.1 > nul"])
            .spawn()
            .expect("应当能起 cmd");
        assert!(matches!(child.try_wait(), Ok(None)), "先确认它还活着");
        stop_game(&mut child);
        assert!(
            matches!(child.try_wait(), Ok(Some(_))),
            "停完之后进程应当已经结束"
        );
    }

    #[test]
    fn stopping_a_game_that_already_quit_is_harmless() {
        // 第 6 步取消时进程可能已经自己退了（读模组阶段崩掉之类的）。
        // 对着尸体再补一刀不该报错，也不该把「已取消」变成「启动失败」。
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/C", "exit 0"])
            .spawn()
            .expect("应当能起 cmd");
        // 等它退干净，`try_wait` 才拿得到 Some。
        for _ in 0..50 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        stop_game(&mut child);
    }

    #[test]
    fn a_fade_starts_clear_and_ends_solid() {
        // 起点必须完全透明、终点必须完全不透明，否则换页那一下会闪。
        assert_eq!(fade_alpha_at(0.0), 0.0);
        assert_eq!(fade_alpha_at(1.0), 1.0);
    }

    #[test]
    fn a_fade_only_ever_gets_more_opaque() {
        let mut previous = fade_alpha_at(0.0);
        for step in 1..=20 {
            let here = fade_alpha_at(step as f32 / 20.0);
            assert!(here >= previous, "第 {} 步反而变淡了：{} < {}", step, here, previous);
            previous = here;
        }
    }

    #[test]
    fn a_fade_runs_ahead_of_a_straight_line() {
        // 前半程就走过大半，才是「浮上来」而不是匀速拉幕。
        assert!(fade_alpha_at(0.5) > 0.5);
    }

    #[test]
    fn a_fade_forgives_a_progress_outside_the_range() {
        // 计时偶尔会超出一点点（帧间隔抖），不该画成负数或过曝。
        assert_eq!(fade_alpha_at(-0.3), 0.0);
        assert_eq!(fade_alpha_at(1.4), 1.0);
    }

    /// 导航换层只认「同一组」，不是按页判：版本管理那三页算一层。
    #[test]
    fn each_group_of_pages_gets_its_own_layer() {
        assert_eq!(Page::Home.nav_group(), 0);
        assert_eq!(Page::Settings.nav_group(), 0);
        for page in [Page::Dirs, Page::Versions, Page::Download] {
            assert_eq!(page.nav_group(), 1, "这一页该归到版本管理那层");
        }
        assert_eq!(Page::Resources.nav_group(), 2);
        assert_eq!(Page::Servers.nav_group(), 3);
    }

    #[test]
    fn every_row_waits_for_the_one_above_it() {
        // 错峰的意思就是「后面的行起步更晚」：同一时刻，序号大的那一行
        // 进度必须更小（或者还没开始）。
        for step in 0..12 {
            let elapsed = 0.05 * step as f32;
            let mut previous = 1.0f32;
            for index in 0..12 {
                let here = theme::row_reveal_at(index, elapsed).unwrap_or(1.0);
                assert!(
                    here <= previous + f32::EPSILON,
                    "第 {} 行跑到了第 {} 行前面：{} > {}",
                    index,
                    index - 1,
                    here,
                    previous
                );
                previous = here;
            }
        }
    }

    #[test]
    fn the_first_row_starts_the_moment_the_list_appears() {
        // 第一行不排队：列表一出来它就该开始进场，否则会有一下空白。
        assert_eq!(theme::row_reveal_at(0, 0.0), Some(0.0));
    }

    #[test]
    fn a_row_that_is_done_stops_being_animated() {
        // 播完要返回 None —— 调用方靠它走「普通画法」的捷径。
        assert_eq!(theme::row_reveal_at(0, theme::ROW_REVEAL_SECS), None);
        assert_eq!(theme::row_reveal_at(0, 10.0), None);
        // 后面的行排队更久，但最终也会结束。
        assert!(theme::row_reveal_at(3, theme::ROW_REVEAL_SECS).is_some());
        assert_eq!(theme::row_reveal_at(3, 10.0), None);
    }

    #[test]
    fn the_stagger_stops_growing_after_a_while() {
        // 序号超过上限之后共用同一个起始时刻：不然长列表底部要等太久。
        let cap = theme::ROW_REVEAL_STAGGER_CAP;
        let at_cap = theme::row_reveal_at(cap, 0.0);
        let beyond = theme::row_reveal_at(cap + 20, 0.0);
        assert_eq!(at_cap, beyond);
    }

    #[test]
    fn a_row_starts_below_and_lands_exactly_in_place() {
        // 起点是整段位移，终点必须是 0：停在别的值上，行就永远偏一点。
        assert_eq!(theme::row_reveal_rise(0.0), theme::ROW_REVEAL_RISE_PX);
        assert_eq!(theme::row_reveal_rise(1.0), 0.0);
        // 超出范围的进度夹回去，不要算出「沉得更深」或「冲过头」。
        assert_eq!(theme::row_reveal_rise(-0.5), theme::ROW_REVEAL_RISE_PX);
        assert_eq!(theme::row_reveal_rise(1.5), 0.0);
    }

    #[test]
    fn the_rise_starts_fast_and_eases_in() {
        // 用户要的手感：刚出来快、接近位置时慢。看两件事——
        // 一路只减不增；前四分之一走过的路程，比后四分之一多得多。
        let mut previous = theme::row_reveal_rise(0.0);
        for step in 1..=40 {
            let here = theme::row_reveal_rise(step as f32 / 40.0);
            assert!(here <= previous, "第 {} 步反而往下掉：{} > {}", step, here, previous);
            previous = here;
        }
        let start = theme::ROW_REVEAL_RISE_PX;
        let first_quarter = start - theme::row_reveal_rise(0.25);
        let last_quarter = theme::row_reveal_rise(0.75) - theme::row_reveal_rise(1.0);
        assert!(
            first_quarter > last_quarter * 4.0,
            "开头不够快：前四分之一走了 {}，后四分之一走了 {}",
            first_quarter,
            last_quarter
        );
    }

    #[test]
    fn every_account_says_what_kind_it_is() {
        let offline = StoredAccount::offline("星星");
        assert_eq!(account_kind(&offline), "离线");
        assert_eq!(account_kind_title(&offline), "离线账户");
        let mut third = StoredAccount::offline("星星");
        third.offline = false;
        third.api_root = Some("https://littleskin.cn/api/yggdrasil".to_string());
        assert_eq!(account_kind(&third), "第三方");
        assert_eq!(account_kind_title(&third), "第三方账户");
        let mut microsoft = StoredAccount::offline("星星");
        microsoft.offline = false;
        microsoft.api_root = None;
        assert_eq!(account_kind(&microsoft), "微软");
        assert_eq!(account_kind_title(&microsoft), "微软账户");
    }

    #[test]
    fn an_offline_name_copied_from_a_login_is_not_kept() {
        let mut microsoft = StoredAccount::offline("TKZC208");
        microsoft.offline = false;
        // 登录时被抄进来的名字：列表里只有这个登录号，没人存过离线号。
        assert!(stale_offline_name("TKZC208", &[microsoft.clone()]));
        // 玩家自己写过的离线名字，登录号碰巧同名，也得留着。
        let saved_offline = StoredAccount::offline("TKZC208");
        assert!(!stale_offline_name("TKZC208", &[microsoft, saved_offline]));
        // 没名字、或者压根对不上任何账号：都不是要清的那一类。
        assert!(!stale_offline_name("  ", &[]));
        assert!(!stale_offline_name("Steve", &[]));
    }

    #[test]
    fn auto_mode_leaves_the_window_to_the_game() {
        let settings = aicraft_core::settings::Settings::default();
        assert_eq!(window_size(&settings), (None, None));
    }

    #[test]
    fn a_fixed_size_is_handed_over_verbatim() {
        let settings = aicraft_core::settings::Settings {
            window_mode: aicraft_core::WindowMode::Fixed,
            window_width: 1600,
            window_height: 900,
            ..aicraft_core::settings::Settings::default()
        };
        assert_eq!(window_size(&settings), (Some(1600), Some(900)));
    }

    #[test]
    fn fullscreen_asks_for_no_size_at_all() {
        let settings = aicraft_core::settings::Settings {
            window_mode: aicraft_core::WindowMode::Fullscreen,
            ..aicraft_core::settings::Settings::default()
        };
        // 全屏由 --fullscreen 表达；再塞一对 --width/--height 只会让游戏
        // 在退出全屏时猜一个奇怪的窗口尺寸。
        assert_eq!(window_size(&settings), (None, None));
        assert!(is_fullscreen(&settings));
    }

    #[test]
    fn global_settings_win_while_the_instance_follows_them() {
        let settings = aicraft_core::settings::Settings {
            auto_memory: false,
            max_memory_mb: 6144,
            jvm_args: "-XX:+UseG1GC".to_string(),
            java_path: Some(PathBuf::from(r"C:\Java\jdk-21\bin\javaw.exe")),
            ..aicraft_core::settings::Settings::default()
        };
        // 自己那一套填得再花哨，只要勾着「沿用全局」就一律不算数。
        let own = aicraft_core::instancecfg::InstanceConfig {
            follow_global: true,
            auto_memory: false,
            max_memory_mb: 512,
            jvm_args: "-Xss1m".to_string(),
            java_path: None,
            from_pack: false,
        };
        let (memory, args, java) = launch_config_for(&own, &settings);
        assert_eq!(memory, 6144);
        assert_eq!(args, vec!["-XX:+UseG1GC"]);
        assert_eq!(java, settings.java_path);
    }

    #[test]
    fn an_instance_with_its_own_settings_uses_them() {
        let settings = aicraft_core::settings::Settings {
            auto_memory: false,
            max_memory_mb: 6144,
            jvm_args: "-XX:+UseG1GC".to_string(),
            java_path: Some(PathBuf::from(r"C:\Java\jdk-21\bin\javaw.exe")),
            ..aicraft_core::settings::Settings::default()
        };
        let own = aicraft_core::instancecfg::InstanceConfig {
            follow_global: false,
            auto_memory: false,
            max_memory_mb: 8192,
            jvm_args: "-Xss1m -XX:+UseZGC".to_string(),
            java_path: Some(PathBuf::from(r"C:\Java\jdk-25\bin\javaw.exe")),
            from_pack: false,
        };
        let (memory, args, java) = launch_config_for(&own, &settings);
        assert_eq!(memory, 8192);
        assert_eq!(args, vec!["-Xss1m", "-XX:+UseZGC"]);
        assert_eq!(
            java,
            Some(PathBuf::from(r"C:\Java\jdk-25\bin\javaw.exe"))
        );
    }

    #[test]
    fn the_same_major_is_the_only_quiet_case() {
        assert_eq!(java_verdict(21, Some(21)), JavaVerdict::Match);
    }

    #[test]
    fn an_older_java_is_named_as_older() {
        // 这是真起不来的那种：字节码进不去更老的虚拟机。
        assert_eq!(java_verdict(21, Some(17)), JavaVerdict::Older);
    }

    #[test]
    fn a_newer_java_still_asks_but_is_not_called_broken() {
        // 更高能跑，但模组/反射容易在这里翻车，所以还是要问一句。
        assert_eq!(java_verdict(17, Some(21)), JavaVerdict::Newer);
    }

    #[test]
    fn a_java_that_will_not_say_its_version_counts_as_unknown() {
        assert_eq!(java_verdict(21, None), JavaVerdict::Unknown);
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
