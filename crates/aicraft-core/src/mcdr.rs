//! MCDReforged（下称 MCDR）支持：环境探测、骨架生成、目录识别。
//!
//! MCDR 不是服务端核心，它是**套在服务端外面的 Python 守护进程**：自己住在
//! 一个目录里，把真正的服务端推进 `server/` 子目录，靠 `config.yml` 里的
//! `start_command` 把子进程拉起来并接管它的标准输入输出，于是玩家能敲
//! `!!MCDR` 这类命令。
//!
//! 这一层做三件事：
//!
//! 1. **探测本机环境**（[`detect`]）：找得到 Python 3.9+ 吗，装没装 mcdreforged。
//! 2. **生成骨架**（[`create_skeleton`]）：`config.yml`、`permission.yml`、
//!    `plugins/`、`logs/`、`server/`。
//! 3. **识别已有目录**（[`is_mcdr`]）：玩家自己装好的 MCDR 目录也要能认出来。
//!
//! 这里**不做迁移**。把一个已经在跑的服务端移动到 `server/` 子目录里，中途
//! 失败就是存档全毁；那件事单独做、单独测。

use std::path::{Path, PathBuf};

/// MCDR 的主配置文件。
pub const CONFIG_FILE: &str = "config.yml";

/// MCDR 的权限文件。
///
/// 它同时也是「这个目录被 init 过」的判据——MCDR 自己就是拿它判断的
/// （缺了它启动时会说 `Permission file 'permission.yml' is missing`，
/// 然后提示你先跑 `mcdreforged init`）。
pub const PERMISSION_FILE: &str = "permission.yml";

/// 插件目录。
pub const PLUGINS_DIR: &str = "plugins";

/// 日志目录。
pub const LOGS_DIR: &str = "logs";

/// 真正的服务端所在的子目录。
pub const SERVER_SUBDIR: &str = "server";

/// MCDR 要求的最低 Python 版本。
pub const MIN_PYTHON: (u32, u32) = (3, 9);

/// PyPI 上的包名。
pub const PACKAGE: &str = "mcdreforged";

// ---------------------------------------------------------------- 环境探测

/// 一份能用的 Python。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Python {
    /// `python.exe` 的路径（用来写进启动脚本）。
    pub path: PathBuf,
    /// 版本串，如 `3.12.10`。
    pub version: String,
    pub major: u32,
    pub minor: u32,
}

impl Python {
    /// 「Python 3.12.10」这种给人看的一行。
    pub fn label(&self) -> String {
        format!("Python {}", self.version)
    }

    /// 够不够新（[`MIN_PYTHON`]）。
    pub fn usable(&self) -> bool {
        (self.major, self.minor) >= MIN_PYTHON
    }
}

/// 本机环境探测的结果。
///
/// 三种状态各自对应界面上的三种提示，所以这三件事分开存而不是合成一个
/// `Result`：缺 Python 和「有 Python 但没装 MCDR」要给的指引完全不同。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    /// 找得到的 Python。`None` 表示一个都没有（或者都太旧）。
    pub python: Option<Python>,
    /// 装着的 MCDR 版本串，如 `2.16.2`。
    pub mcdr_version: Option<String>,
    /// 探测过程中值得说一句的话（比如「找到了 Python 3.8，但它太旧」）。
    pub note: Option<String>,
}

impl Env {
    /// 两样都齐了，可以直接建 MCDR 目录。
    pub fn ready(&self) -> bool {
        self.python.as_ref().is_some_and(Python::usable) && self.mcdr_version.is_some()
    }

    /// 缺什么，用一句话说清楚。齐了返回 `None`。
    pub fn missing(&self) -> Option<String> {
        let floor = format!("{}.{}", MIN_PYTHON.0, MIN_PYTHON.1);
        match (&self.python, &self.mcdr_version) {
            (None, _) => Some(format!(
                "没找到 Python {floor} 以上。先去装一个 Python，装的时候记得勾上「Add Python to PATH」。"
            )),
            (Some(p), None) if !p.usable() => Some(format!(
                "本机只有 {}，MCDR 要 {floor} 以上。",
                p.label()
            )),
            (Some(p), None) => Some(format!(
                "{} 有了，但没装 MCDR。在命令行里跑一句：\n{} -m pip install {}",
                p.label(),
                p.path.display(),
                PACKAGE
            )),
            (_, Some(_)) => None,
        }
    }
}

/// 依次尝试的 Python 启动方式。
///
/// `py -3` 排在第一个：Windows 的 py 启动器会挑一个**真的装过**的 Python，
/// 而 `python` 在没装的人机器上可能指向应用商店的占位程序——那个东西
/// 不报错、也不干活，`Command` 拿到的是空输出。
fn candidates() -> [(&'static str, &'static [&'static str]); 3] {
    [
        ("py", &["-3"]),
        ("python", &[]),
        ("python3", &[]),
    ]
}

/// 探测本机环境。会起几个子进程，别在绘制里直接调。
pub fn detect() -> Env {
    let mut env = Env::default();
    let mut too_old: Option<Python> = None;

    for (program, args) in candidates() {
        let Some(python) = probe(program, args) else {
            continue;
        };
        if !python.usable() {
            // 记下来，等会儿没有更好的再用它说话——「你装的是 3.8，太旧」
            // 比「没找到 Python」有用。
            too_old.get_or_insert(python);
            continue;
        }
        env.python = Some(python);
        break;
    }

    if env.python.is_none() {
        if let Some(old) = too_old {
            env.note = Some(format!("只有 {}，太旧了。", old.label()));
        }
        return env;
    }

    let python = env.python.as_ref().expect("刚赋过值");
    env.mcdr_version = mcdr_version_of(&python.path);
    env
}

/// 用某个解释器问一句 `sys.version_info`。
fn probe(program: &str, args: &[&str]) -> Option<Python> {
    let output = crate::spawn::silent(program)
        .args(args)
        .arg("-c")
        .arg("import sys;print('%d.%d.%d' % sys.version_info[:3])")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let (major, minor) = parse_major_minor(&version)?;
    // 版本号问得出来不代表解释器可用：应用商店那套占位程序会返回成功、
    // 但什么都不打印。空串已经在上面被 `parse_major_minor` 挡掉了。
    let path = resolve_exe(&output, program)?;
    Some(Python {
        path,
        version,
        major,
        minor,
    })
}

/// 把 `3.12.10` 拆成 `(3, 12)`。
fn parse_major_minor(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.trim().split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    Some((major, minor))
}

/// 找出这个解释器的真实路径。
///
/// 直接问解释器自己最可靠（`py -3` 和 `python` 指向的可能是同一个、也可能
/// 不是），探不到就退回命令行上那个名字。
fn resolve_exe(_output: &std::process::Output, program: &str) -> Option<PathBuf> {
    let asked = crate::spawn::silent(program)
        .arg("-c")
        .arg("import sys;print(sys.executable)")
        .output()
        .ok()?;
    if asked.status.success() {
        let text = String::from_utf8_lossy(&asked.stdout).trim().to_string();
        if !text.is_empty() {
            return Some(PathBuf::from(text));
        }
    }
    // 探不到就用名字，让 Windows 自己去 PATH 里找。
    Some(PathBuf::from(program))
}

/// 问一句 `python -m mcdreforged -V`，成功就返回版本串。
pub fn mcdr_version_of(python: &Path) -> Option<String> {
    let output = crate::spawn::silent(python)
        .arg("-m")
        .arg(PACKAGE)
        .arg("-V")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_mcdr_version(&text)
}

/// 从 `MCDReforged 2.16.2` 里抠出版本号。
fn parse_mcdr_version(text: &str) -> Option<String> {
    let line = text.lines().find(|line| line.contains("MCDReforged"))?;
    line.split_whitespace()
        .find(|word| word.starts_with(|c: char| c.is_ascii_digit()))
        .map(str::to_string)
}

/// 问 PyPI 上 MCDR 的最新版本。检查更新用。
///
/// 走清华镜像：官方 pypi.org 在这边 1.3 秒，清华 0.7 秒，而这一整块 JSON
/// 有 200 KB 上下（里面带着每个历史版本的元数据），差得明显。
pub fn latest_version() -> Result<String, String> {
    let text = crate::transport::fetch_text(
        "https://pypi.tuna.tsinghua.edu.cn/pypi/mcdreforged/json",
    )
    .map_err(|e| format!("问 PyPI 失败: {e}"))?;
    parse_latest(&text)
}

fn parse_latest(text: &str) -> Result<String, String> {
    let root = crate::json::parse(text).map_err(|e| format!("PyPI 的响应读不了: {}", e))?;
    root.get("info")
        .and_then(|info| info.get("version"))
        .and_then(crate::json::Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| "PyPI 的响应里没有版本号".to_string())
}

// ---------------------------------------------------------------- 目录识别

/// 这个目录是不是一个被 MCDR 套起来的服务端目录。
///
/// 判据就是 MCDR 自己的判据：`config.yml` 与 `permission.yml` 都在。
/// 少了 `permission.yml` 的目录 MCDR 自己也不认（会提示先跑 `init`），
/// 我们跟着它走，免得「我们说是 MCDR、它说不是」。
pub fn is_mcdr(dir: &Path) -> bool {
    dir.is_dir() && dir.join(CONFIG_FILE).is_file() && dir.join(PERMISSION_FILE).is_file()
}

/// 真正的服务端所在目录（MCDR 根目录下的 `server/`）。
pub fn inner_dir(dir: &Path) -> PathBuf {
    dir.join(SERVER_SUBDIR)
}

// ---------------------------------------------------------------- 骨架

/// 建一份 MCDR 骨架。
///
/// **不调 `mcdreforged init`**：那要起一个 Python 进程，还得依赖玩家的解释器
/// 能跑起来；而它生成的东西（两个 yml + 几个空目录）我们自己写更可控，
/// 缺的配置键 MCDR 启动时会自己补默认值。
///
/// 调用前 `server/` 里应该已经装好了核心——`start_command` 要照着它写。
pub fn create_skeleton(
    dir: &Path,
    start_command: &str,
    handler: &str,
    language: &str,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("建 {} 失败: {}", dir.display(), e))?;
    for sub in [PLUGINS_DIR, LOGS_DIR, SERVER_SUBDIR] {
        let path = dir.join(sub);
        std::fs::create_dir_all(&path).map_err(|e| format!("建 {} 失败: {}", path.display(), e))?;
    }

    let config = dir.join(CONFIG_FILE);
    // 已经有一份就别覆盖——里面可能被玩家调过（比如改了端口、加了插件参数）。
    if !config.exists() {
        std::fs::write(&config, config_body(start_command, handler, language))
            .map_err(|e| format!("写 {} 失败: {}", config.display(), e))?;
    }

    let permission = dir.join(PERMISSION_FILE);
    if !permission.exists() {
        std::fs::write(&permission, permission_body())
            .map_err(|e| format!("写 {} 失败: {}", permission.display(), e))?;
    }
    Ok(())
}

fn config_body(start_command: &str, handler: &str, language: &str) -> String {
    // 只写我们在意的几个键。其余二十来个 MCDR 会自己补默认值，
    // 全写出来的话这一屏注释比配置本身还长，玩家反而找不到要改的那行。
    format!(
        "\
# MCDReforged 配置（AICraft 生成）
# 完整的键可以看 https://docs.mcdreforged.com/zh-cn/latest/configuration.html
language: {language}

# 真正的服务端在 server/ 子目录里
working_directory: {SERVER_SUBDIR}

# 换版本 / 换核心之后，这一行要跟着改
start_command: {start_command}

handler: {handler}
"
    )
}

fn permission_body() -> String {
    "\
# MCDReforged 权限配置（AICraft 生成）
# 想给自己开管理权限：把游戏 ID 填到 owner: 下面
default_level: user

owner:
admin:
helper:
user:
guest:
"
        .to_string()
}

/// 写 MCDR 的 `start.bat`。
///
/// 这个脚本和普通服务端的 `start.bat` 不是一回事：它拉起来的是 MCDR，
/// 不是 Java。**必须开真控制台**——MCDR 的 `!!MCDR` 命令全靠它自己的
/// 命令行输入，没有控制台就只剩一个不会响的窗。
pub fn write_start_bat(dir: &Path, python: &Path) -> Result<PathBuf, String> {
    let bat = dir.join("start.bat");
    let body = format!(
        "@echo off\r\n\
         chcp 65001 >nul\r\n\
         cd /d \"%~dp0\"\r\n\
         echo MCDReforged 正在启动…（这个窗口别关，关了服务端就停）\r\n\
         \"{}\" -m {PACKAGE}\r\n\
         pause\r\n",
        python.display()
    );
    std::fs::write(&bat, body).map_err(|e| format!("写 {} 失败: {}", bat.display(), e))?;
    Ok(bat)
}

/// 这套核心该用哪个 handler。
///
/// MCDR 认的 handler 有 ten 来个（vanilla / bukkit / forge / arclight /
/// cat_server / velocity…）。我们这几种的对应关系：
///
/// - 原版、Fabric 都是原版的输出格式 → `vanilla_handler`
/// - Forge 的日志格式自己一套 → `forge_handler`
/// - NeoForge 是 Forge 分出来的，日志格式同源 → 也用 `forge_handler`
///   （MCDR 文档里没有单列 NeoForge，这是推断，实测不对劲再改）
pub fn handler_for(core: crate::server::CoreKind) -> &'static str {
    use crate::server::CoreKind;
    match core {
        CoreKind::Vanilla | CoreKind::Fabric => "vanilla_handler",
        CoreKind::Forge | CoreKind::NeoForge => "forge_handler",
        // 代理端我们不装 MCDR（它没有「世界」可管）。
        CoreKind::Velocity => "vanilla_handler",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::CoreKind;

    #[test]
    fn a_version_is_read_from_what_the_command_prints() {
        let text = "MCDReforged 2.16.2\n";
        assert_eq!(parse_mcdr_version(text).as_deref(), Some("2.16.2"));
    }

    #[test]
    fn a_command_that_printed_nothing_is_not_a_version() {
        assert!(parse_mcdr_version("").is_none());
        assert!(parse_mcdr_version("No module named mcdreforged").is_none());
    }

    #[test]
    fn the_pypi_answer_carries_the_latest_version() {
        let text = r#"{"info":{"version":"2.16.2","name":"mcdreforged"}}"#;
        assert_eq!(parse_latest(text).unwrap(), "2.16.2");
    }

    #[test]
    fn a_pypi_answer_without_a_version_is_an_error() {
        assert!(parse_latest(r#"{"info":{}}"#).is_err());
        assert!(parse_latest("not json").is_err());
    }

    #[test]
    fn python_versions_are_split_the_way_the_comparison_needs() {
        assert_eq!(parse_major_minor("3.12.10"), Some((3, 12)));
        assert_eq!(parse_major_minor("3.9"), Some((3, 9)));
        assert_eq!(parse_major_minor(""), None);
        assert_eq!(parse_major_minor("python"), None);
    }

    #[test]
    fn python_three_eight_is_below_the_floor_and_three_nine_is_not() {
        let old = Python {
            path: PathBuf::from("python"),
            version: "3.8.10".to_string(),
            major: 3,
            minor: 8,
        };
        assert!(!old.usable(), "3.8 低于 MCDR 要求的 3.9");
        let ok = Python {
            version: "3.9.0".to_string(),
            major: 3,
            minor: 9,
            ..old
        };
        assert!(ok.usable(), "3.9 正好是下限，得算可用");
    }

    #[test]
    fn what_is_missing_is_said_in_terms_the_player_can_act_on() {
        let nothing = Env::default();
        let said = nothing.missing().unwrap();
        assert!(said.contains("Python"), "缺 Python 时要说 Python: {said}");

        let only_python = Env {
            python: Some(Python {
                path: PathBuf::from("C:\\Python312\\python.exe"),
                version: "3.12.10".to_string(),
                major: 3,
                minor: 12,
            }),
            mcdr_version: None,
            note: None,
        };
        let said = only_python.missing().unwrap();
        assert!(said.contains("pip install"), "缺包时要给出安装命令: {said}");
        assert!(said.contains("mcdreforged"));

        let ready = Env {
            python: only_python.python.clone(),
            mcdr_version: Some("2.16.2".to_string()),
            note: None,
        };
        assert!(ready.ready());
        assert!(ready.missing().is_none());
    }

    #[test]
    fn a_too_old_python_is_named_rather_than_reported_as_missing() {
        let env = Env {
            python: Some(Python {
                path: PathBuf::from("python"),
                version: "3.8.10".to_string(),
                major: 3,
                minor: 8,
            }),
            mcdr_version: None,
            note: None,
        };
        let said = env.missing().unwrap();
        assert!(said.contains("3.8.10"), "要说清本机装的是什么: {said}");
        assert!(said.contains("以上"));
    }

    #[test]
    fn a_directory_with_both_files_is_an_mcdr_one() {
        let dir = crate::testutil::fresh_dir("mcdr-detect");
        let root = dir.as_ref();
        assert!(!is_mcdr(root), "空目录不是 MCDR 目录");

        std::fs::write(root.join(CONFIG_FILE), "language: zh_cn\n").unwrap();
        assert!(!is_mcdr(root), "光有 config.yml 还不够");

        std::fs::write(root.join(PERMISSION_FILE), "default_level: user\n").unwrap();
        assert!(is_mcdr(root));
    }

    #[test]
    fn a_skeleton_has_everything_mcdr_asks_for_at_startup() {
        let dir = crate::testutil::fresh_dir("mcdr-skeleton");
        let root = dir.as_ref();
        create_skeleton(root, "java -jar server.jar nogui", "vanilla_handler", "zh_cn").unwrap();

        assert!(root.join(CONFIG_FILE).is_file());
        assert!(root.join(PERMISSION_FILE).is_file());
        assert!(root.join(PLUGINS_DIR).is_dir());
        assert!(root.join(LOGS_DIR).is_dir());
        assert!(root.join(SERVER_SUBDIR).is_dir());

        let config = std::fs::read_to_string(root.join(CONFIG_FILE)).unwrap();
        assert!(config.contains("working_directory: server"));
        assert!(config.contains("start_command: java -jar server.jar nogui"));
        assert!(config.contains("handler: vanilla_handler"));
        assert!(config.contains("language: zh_cn"));

        // 建完就是个 MCDR 目录了
        assert!(is_mcdr(root));
    }

    #[test]
    fn an_existing_config_is_never_overwritten() {
        let dir = crate::testutil::fresh_dir("mcdr-keep-config");
        let root = dir.as_ref();
        std::fs::create_dir_all(root).unwrap();
        let mine = "# 我自己的配置\nlanguage: en_us\nserver-port: 25000\n";
        std::fs::write(root.join(CONFIG_FILE), mine).unwrap();

        create_skeleton(root, "java -jar server.jar nogui", "vanilla_handler", "zh_cn").unwrap();

        assert_eq!(std::fs::read_to_string(root.join(CONFIG_FILE)).unwrap(), mine);
        // 缺的那个还是补上了
        assert!(root.join(PERMISSION_FILE).is_file());
    }

    #[test]
    fn each_core_gets_the_handler_that_speaks_its_log_format() {
        assert_eq!(handler_for(CoreKind::Vanilla), "vanilla_handler");
        assert_eq!(handler_for(CoreKind::Fabric), "vanilla_handler");
        assert_eq!(handler_for(CoreKind::Forge), "forge_handler");
        assert_eq!(handler_for(CoreKind::NeoForge), "forge_handler");
    }

    #[test]
    fn the_start_script_runs_mcdr_not_java() {
        let dir = crate::testutil::fresh_dir("mcdr-bat");
        let root = dir.as_ref();
        std::fs::create_dir_all(root).unwrap();

        let bat = write_start_bat(root, Path::new(r"C:\Python312\python.exe")).unwrap();
        let body = std::fs::read_to_string(bat).unwrap();

        assert!(body.contains(r"C:\Python312\python.exe"), "要用探测到的那个解释器");
        assert!(body.contains("-m mcdreforged"));
        assert!(!body.contains("server.jar"), "MCDR 的启动脚本不该直接拉 jar");
        assert!(body.contains("pause"), "窗口要停住，不然玩家看不到报错");
    }
}
