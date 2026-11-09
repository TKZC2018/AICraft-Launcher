//! aicraft-core —— MCJE 启动器核心。
//!
//! 设计原则：**零 UI 依赖、零外部 crate 依赖**。
//!
//! 这样做的理由是实际的：本机 cargo 无法访问 crates.io（Windows schannel
//! 在受限令牌下拿不到凭证），而 core 真正需要的外部能力只有 JSON 解析。
//! 自己实现一个够用的小解析器，换来的是「clone 下来就能编译」这一性质，
//! 以及一个可以脱离 UI 单独测试、单独做基准的核心。
//!
//! 分层：
//!
//! - [`json`]     最小 JSON 解析器
//! - [`maven`]    Maven 坐标 → 仓库路径
//! - [`rules`]    `rules` 规则求值（平台 / 架构 / 特性）
//! - [`version`]  版本清单模型与解析（兼容新旧格式）
//! - [`resolve`]  版本合并（inheritsFrom / patches）
//! - [`flatten`]  把继承链压平成一份自包含清单（HMCL 那种写法）
//! - [`launch`]   classpath 与启动参数拼装
//! - [`natives`]  平台本地库解压（自带 ZIP / DEFLATE 实现）
//! - [`java`]     Java 运行时探测与选择
//! - [`meminfo`]  本机物理内存（设置页的内存条与推荐值）
//! - [`modicon`]  从模组 jar 里取出作者放的图标（管理页的模组列表）
//! - [`modupdate`] 按文件哈希认出模组并检查更新（管理页的「更新」）
//! - [`codec`]    Base64URL / 百分号编码（正版登录的 PKCE 与表单）
//! - [`sha256`]   SHA-256（PKCE 的 code_challenge）
//! - [`auth`]     正版登录（微软 OAuth → Xbox Live → Minecraft 档案）
//! - [`logging`]  分级日志（环形缓冲 + 可选文件 sink）
//! - [`audit`]    操作日志：玩家点了什么、改了什么（复盘用）
//! - [`settings`] 启动器设置（版本下载源 / 并发 / 内存 / JVM / Java 路径）
//! - [`spawn`]    不弹控制台地起子进程
//! - [`speed`]    下载限速（总配额在并发流之间均分）
//! - [`cancel`]   下载取消令牌（跨线程共享的开关）
//! - [`server`]   服务端管理：装核心、写启动脚本、改 server.properties
//! - [`loader`]   模组加载器（Fabric / Forge / NeoForge）与 Fabric API 的安装
//! - [`modrinth`] Modrinth 模组搜索与安装（资源下载页的数据来源）
//! - [`modpack`]  整合包导入（Modrinth / HMCL / CurseForge 三种格式）
//! - [`curseforge`] CurseForge 整合包里 projectID/fileID → 下载地址
//! - [`png`]      最小 PNG 解码（只为皮肤头像）
//! - [`skin`]     皮肤头像：查档案、下皮肤、裁出那张脸
//!
//! 典型用法：
//!
//! ```no_run
//! use aicraft_core::{version, resolve, launch, java};
//!
//! let text = std::fs::read_to_string("1.20.1.json").unwrap();
//! let raw = version::parse_version(&text).unwrap();
//! let legacy = raw.minecraft_arguments.clone();
//! let resolved = resolve::resolve(&[raw]).unwrap().with_legacy_arguments(legacy);
//!
//! let account = launch::Account::offline("Steve");
//! let config = launch::LaunchConfig::new("E:\\Minecraft\\.minecraft");
//! let plan = launch::build_plan(&resolved, &account, &config);
//! ```

pub mod audit;
pub mod auth;
pub mod cancel;
pub mod codec;
pub mod flatten;
pub mod install;
pub mod instance;
pub mod logging;
pub mod java;
pub mod json;
pub mod launch;
pub mod loader;
pub mod maven;
pub mod meminfo;
pub mod modicon;
pub mod modupdate;
pub mod curseforge;
pub mod modpack;
pub mod modrinth;
pub mod natives;
pub mod png;
pub mod repair;
pub mod resolve;
pub mod rules;
pub mod seal;
pub mod server;
pub mod sha1;
pub mod settings;
pub mod sha256;
pub mod skin;
pub mod spawn;
pub mod speed;
pub mod transport;
pub mod verify;
pub mod version;

/// 测试辅助工具（临时目录）。
///
/// 一直是 public 而不是 `#[cfg(test)]`：`tests/` 下的集成测试链接的是普通
/// 库构建，看不到只在单元测试里编入的模块，于是它们各自手抄了一份
/// `fresh_dir` —— 那份副本没有清理逻辑，`cargo test` 会在仓库根目录下
/// 堆一堆 `aicraft-*`。让它public，两边就能共用同一份会自删的实现。
/// 发布构建里没人引用它，LTO 会整个丢掉。
#[doc(hidden)]
pub mod testutil;

pub use auth::{AuthError, AuthErrorKind, MinecraftAccount};
pub use auth::store::{AccountStore, StoredAccount};
pub use install::{
    InstallKind, InstallPlan, InstallProgress, InstallReport, InstallItem, VersionManifest,
    VersionSummary, execute_install, fetch_manifest, fetch_manifest_in,
    fetch_version_json_in, installed_count, installed_instances, instance_official_id,
    instances_of, is_installed, is_valid_instance_name,
    parse_manifest, plan_install, plan_install_as, rename_instance,
};
pub use instance::{
    InstanceMode, Item, detect_mode, ensure_instance, game_dir_for, list_dirs, list_files,
    remove_item, version_dir,
};
pub use java::{JavaChoice, JavaRuntime, detect_all, pick_java, pick_java_choice};
pub use launch::{Account, LaunchConfig, LaunchPlan, build_plan};
pub use maven::MavenCoord;
pub use natives::{ExtractReport, extract_natives};
pub use repair::{
    Progress, RepairConfig, RepairOutcome, RepairReport, repair, repair_with_progress,
};
pub use resolve::{ResolvedVersion, resolve};
pub use rules::{Features, OsInfo, RuleContext};
pub use sha1::sha1_hex;
pub use sha256::{sha256, sha256_hex};
pub use codec::{base64url_nopad, percent_decode, urlencode};
pub use settings::{LauncherMode, Mirror, Settings, official_game_dir};
pub use verify::{Problem, VerifyMode, VerifyReport, verify_version};
pub use version::{RawVersion, load_chain, load_chain_from_dir, parse_version};

/// 库的语义化版本。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
