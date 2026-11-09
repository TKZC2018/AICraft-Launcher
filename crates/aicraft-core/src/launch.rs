//! 启动参数拼装。
//!
//! 把「版本描述 + 账户 + 运行环境」变成一条可直接执行的命令行。
//!
//! 占位符有两套来源：
//! - 新版：`arguments` 里的 `${...}`；
//! - 旧版：`minecraftArguments` 字符串里的 `${...}`。
//!
//! 两者共用同一张替换表，所以统一处理。
//!
//! 需要注意的细节：
//! - `--assetIndex` 只用 assetIndex.id，不用 assets 的 legacy 名。
//! - 旧版没有 `--assetIndex` 时要补上，否则游戏找不到资源。
//! - classpath 分隔符在 Windows 上是 `;`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::resolve::{library_download, ResolvedVersion};
use crate::rules::{Features, OsInfo, RuleContext};

/// 账户信息。离线/正版都走这里，只是 token 不同。
#[derive(Debug, Clone)]
pub struct Account {
    pub username: String,
    pub uuid: String,
    pub access_token: String,
    /// `msa` / `legacy` / `mojang`
    pub user_type: String,
    /// 是否演示模式。
    pub demo: bool,
    /// 第三方登录的认证服务器根地址（authlib-injector 用）。
    ///
    /// 为 `Some` 时启动要挂 `-javaagent:authlib-injector.jar=<这个地址>`，
    /// 并且 `user_type` 一定是 `mojang`。`None` 表示微软正版或纯离线。
    pub api_root: Option<String>,
}

impl Account {
    /// 离线账户：uuid 由用户名派生，token 是占位符。
    ///
    /// uuid 必须是 32 位无连字符十六进制：Mojang 的 offline 规则就是这么算的，
    /// 否则部分 mod（如需要 UUID 的模组）会报错。
    pub fn offline(username: &str) -> Self {
        Account {
            username: username.to_string(),
            uuid: offline_uuid(username),
            access_token: "0".repeat(32),
            user_type: "legacy".to_string(),
            demo: false,
            api_root: None,
        }
    }
}

/// 由用户名派生离线 UUID（MD5 风格，Mojang 的 offline player 规则）。
///
/// 这里用 UUID v3（MD5 命名空间）语义：对 `OfflinePlayer:<name>` 取 MD5，
/// 再设置版本位与变体位。为保持零依赖，MD5 自行实现。
pub fn offline_uuid(username: &str) -> String {
    let mut input = Vec::with_capacity(32 + username.len());
    input.extend_from_slice(b"OfflinePlayer:");
    input.extend_from_slice(username.as_bytes());
    let digest = md5(&input);
    // 设置版本号 3 与 RFC 4122 变体
    let mut bytes = digest;
    bytes[6] = (bytes[6] & 0x0F) | 0x30;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let mut out = String::with_capacity(32);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// 精简 MD5 实现（RFC 1321），仅用于生成离线 UUID。
fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6,
        10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    // 填充
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let sum = a
                .wrapping_add(f)
                .wrapping_add(K[i])
                .wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// 启动配置：路径与运行环境。
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    /// .minecraft 目录。
    pub game_dir: PathBuf,
    /// assets 目录。
    pub assets_dir: PathBuf,
    /// libraries 目录。
    pub libraries_dir: PathBuf,
    /// natives 解压目录。
    pub natives_dir: PathBuf,
    /// 版本 jar 所在目录。
    pub version_dir: PathBuf,
    /// 客户端 jar 的完整路径；None 表示不放进 classpath。
    pub client_jar: Option<PathBuf>,
    /// 日志配置文件路径（若版本要求）。
    pub logging_file: Option<PathBuf>,
    /// 最大内存（MB）。None 表示不指定。
    pub max_memory_mb: Option<u32>,
    /// 最小内存（MB）。
    pub min_memory_mb: Option<u32>,
    /// 额外 JVM 参数，追加在清单参数之后。
    pub extra_jvm_args: Vec<String>,
    /// 启动器自己注入的 JVM 参数，排在清单参数之后、玩家参数之前。
    ///
    /// 与 `extra_jvm_args` 分开是有意的：那边是玩家在设置里写的，
    /// 这里是我们为了让它跑起来必须加的东西（第三方登录的
    /// `-javaagent:authlib-injector.jar`），不该混进玩家的输入框。
    pub injected_jvm_args: Vec<String>,
    /// 额外游戏参数。
    pub extra_game_args: Vec<String>,
    /// 启动器名称，用于 -Dminecraft.launcher.brand。
    pub launcher_name: String,
    pub launcher_version: String,
}

impl LaunchConfig {
    /// 按惯例从 .minecraft 路径推出一套默认配置（共享模式）。
    pub fn new(game_dir: impl Into<PathBuf>) -> Self {
        let game_dir = game_dir.into();
        LaunchConfig {
            assets_dir: game_dir.join("assets"),
            libraries_dir: game_dir.join("libraries"),
            natives_dir: game_dir.join("versions").join("natives"),
            version_dir: game_dir.join("versions"),
            client_jar: None,
            logging_file: None,
            game_dir,
            max_memory_mb: Some(4096),
            min_memory_mb: None,
            extra_jvm_args: Vec::new(),
            injected_jvm_args: Vec::new(),
            extra_game_args: Vec::new(),
            launcher_name: "AICraft Launcher".to_string(),
            launcher_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// 改成实例隔离模式。
    ///
    /// 只把「游戏工作目录」指向 `versions/<id>/`，
    /// `assets` 与 `libraries` 仍指向 .minecraft 根 —— 它们是只读共享资源，
    /// 每个实例复制一份纯属浪费（单版本就有几百 MB）。
    ///
    /// 隔离之后，存档、模组、配置、截图都落在实例目录里，
    /// 换版本不会互相干扰。
    pub fn with_instance(mut self, instance_dir: impl Into<PathBuf>) -> Self {
        self.game_dir = instance_dir.into();
        self
    }

    /// 按探测到的模式自动选择目录。
    pub fn with_instance_mode(
        self,
        game_dir: &Path,
        version_id: &str,
        mode: crate::instance::InstanceMode,
    ) -> Self {
        let dir = crate::instance::game_dir_for(game_dir, version_id, mode);
        self.with_instance(dir)
    }
}

/// 拼装好的启动计划。
#[derive(Debug, Clone)]
pub struct LaunchPlan {
    /// JVM 可执行文件占位；由调用方填入实际 java 路径。
    pub main_class: String,
    pub jvm_args: Vec<String>,
    pub game_args: Vec<String>,
    /// classpath 元素，已按平台过滤且顺序正确。
    pub classpath: Vec<PathBuf>,
    /// 需要从 natives 目录加载的平台本地库。
    pub natives: Vec<PathBuf>,
    /// 原生库最终必须落在的目录。
    ///
    /// 一般就是 `config.natives_dir`，但 26.2 起的官方清单把
    /// `-Djava.library.path` 写成 `${natives_directory}/java`，多出一层
    /// `java/`。dll 若仍平铺在 `natives_dir` 下，游戏会在启动初期就报
    /// `UnsatisfiedLinkError: Failed to locate library: lwjgl.dll`，
    /// 而第一个碰它的往往是 Sodium 这类模组，看着像模组崩了。
    /// 所以这里回读生成好的参数，告诉调用方 dll 该放哪。
    pub natives_dir: PathBuf,
}

impl LaunchPlan {
    /// 生成最终 argv（不含 java 可执行文件本身）。
    pub fn to_argv(&self) -> Vec<String> {
        let mut argv = self.jvm_args.clone();
        argv.push(self.main_class.clone());
        argv.extend(self.game_args.clone());
        argv
    }

    /// classpath 的字符串形式。Windows 用 ';'，其余用 ':'。
    pub fn classpath_string(&self) -> String {
        join_classpath(&self.classpath)
    }
}

/// 把 classpath 序列拼成启动器要传给 JVM 的那一根字符串。
///
/// 单独抽出来是因为 `build_plan` 填 `${classpath}` 占位符和
/// `LaunchPlan::classpath_string` 必须是同一套拼法，否则两者会悄悄分叉。
fn join_classpath(classpath: &[PathBuf]) -> String {
    let sep = if cfg!(windows) { ';' } else { ':' };
    classpath
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(&sep.to_string())
}

/// 构建启动计划。
pub fn build_plan(
    version: &ResolvedVersion,
    account: &Account,
    config: &LaunchConfig,
) -> LaunchPlan {
    let features = Features {
        is_demo_user: account.demo,
        ..Features::default()
    };
    let ctx = RuleContext::new(OsInfo::detect(), features);

    // ---- classpath ----
    let mut classpath: Vec<PathBuf> = Vec::new();

    for lib in version.effective_libraries(&ctx) {
        // 纯原生库（没有主构件）才完全跳过 classpath。
        // 老式条目既有主 jar 又有原生库，主 jar 必须进 classpath。
        if lib.is_native_only() {
            continue;
        }
        let path = match library_path(lib, &ctx, config) {
            Some(p) => p,
            None => continue,
        };
        classpath.push(path);
    }

    // 客户端 jar 放在 classpath 首位（官方就是这样，且 Forge/NeoForge 依赖这个顺序）
    if let Some(client_jar) = &config.client_jar {
        classpath.insert(0, client_jar.clone());
    }

    // ---- 占位符替换表 ----
    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("auth_player_name", account.username.clone());
    vars.insert("version_name", version.id.clone());
    vars.insert("game_directory", path_str(&config.game_dir));
    vars.insert("assets_root", path_str(&config.assets_dir));
    vars.insert("assets_index_name", version.asset_index.id.clone());
    vars.insert("auth_uuid", account.uuid.clone());
    vars.insert("auth_access_token", account.access_token.clone());
    vars.insert("auth_session", format!("token:{}", account.access_token));
    vars.insert("user_type", account.user_type.clone());
    vars.insert(
        "version_type",
        version
            .version_type
            .clone()
            .unwrap_or_else(|| "release".to_string()),
    );
    vars.insert("natives_directory", path_str(&config.natives_dir));
    vars.insert("launcher_name", config.launcher_name.clone());
    vars.insert("launcher_version", config.launcher_version.clone());
    // 微软账户体系引入的字段。离线启动时给空串：留空会让游戏认为
    // 「没有 MSA 身份」，走单机/离线路径；而保留字面 `${clientid}`
    // 会被当成一段真实字符串传给游戏，反而更糟。
    vars.insert("clientid", String::new());
    vars.insert("auth_xuid", String::new());
    vars.insert("user_properties", "{}".to_string());
    // 分辨率等可选参数，未显式指定时留空
    vars.insert("resolution_width", String::new());
    vars.insert("resolution_height", String::new());
    vars.insert("game_assets", path_str(&config.assets_dir.join("virtual").join(&version.asset_index.id)));
    vars.insert(
        "assets_index_name_legacy",
        version.asset_index.id.clone(),
    );
    vars.insert("classpath", join_classpath(&classpath));
    vars.insert("library_directory", path_str(&config.libraries_dir));
    vars.insert("classpath_separator", {
        if cfg!(windows) {
            ";".to_string()
        } else {
            ":".to_string()
        }
    });
    if let Some(mb) = config.max_memory_mb {
        vars.insert("max_memory", format!("{}M", mb));
    }

    // ---- JVM 参数 ----
    let mut jvm_args: Vec<String> = Vec::new();
    if let Some(mb) = config.max_memory_mb {
        jvm_args.push(format!("-Xmx{}M", mb));
    }
    if let Some(mb) = config.min_memory_mb {
        jvm_args.push(format!("-Xms{}M", mb));
    }

    for entry in &version.jvm_args {
        for arg in entry.expand(&ctx) {
            let replaced = substitute(&arg, &vars);
            // -cp 的取值随后由${classpath}提供，避免重复
            if replaced == "-cp" || replaced == "-classpath" {
                jvm_args.push(replaced);
                continue;
            }
            if replaced.is_empty() {
                continue;
            }
            jvm_args.push(replaced);
        }
    }

    // 旧格式清单（≤1.12.2，只有 `minecraftArguments` 字符串）不带
    // `arguments.jvm`，因此上面循环不会产生任何 JVM 参数。这种情况下
    // 必须由启动器补上官方那几条标准参数，否则：
    //
    //   - 缺 `-Djava.library.path` → 游戏找不到 lwjgl64.dll，
    //     报 `UnsatisfiedLinkError: no lwjgl64 in java.library.path`
    //   - 缺 `-cp` → 主类根本加载不到
    //
    // 判定用「清单是否自带 java.library.path」，而不是「是不是旧格式」：
    // 有些新版清单同样把这条写在自己的 arguments 里，不能重复添加。
    if !jvm_args
        .iter()
        .any(|a| a.contains("java.library.path"))
    {
        jvm_args.push(format!(
            "-Djava.library.path={}",
            vars["natives_directory"]
        ));
    }

    // 确保 classpath 一定被传入：若清单没给 -cp，补上
    if !jvm_args.iter().any(|a| a == "-cp" || a == "-classpath") {
        jvm_args.push("-cp".to_string());
        jvm_args.push(vars["classpath"].clone());
    }

    if let Some(log_file) = &config.logging_file {
        if let Some(logging) = &version.logging {
            if let Some(argument) = &logging.argument {
                let mut extra = HashMap::new();
                extra.insert("path", path_str(log_file));
                jvm_args.push(substitute(argument, &extra));
            }
        }
    }

    jvm_args.extend(config.injected_jvm_args.iter().cloned());
    jvm_args.extend(config.extra_jvm_args.iter().cloned());

    // 到这里 jvm_args 已经定型，可以反推出原生库该落在哪个目录。
    let natives_dir = library_path_dir(&jvm_args, config);

    // ---- 游戏参数 ----
    let mut game_args: Vec<String> = Vec::new();
    for entry in &version.game_args {
        for arg in entry.expand(&ctx) {
            let replaced = substitute(&arg, &vars);
            if !replaced.is_empty() {
                game_args.push(replaced);
            }
        }
    }

    // 旧版清单可能没有 --assetIndex，补一个
    if !game_args.iter().any(|a| a == "--assetIndex") && !version.asset_index.id.is_empty() {
        game_args.push("--assetIndex".to_string());
        game_args.push(version.asset_index.id.clone());
    }

    game_args.extend(config.extra_game_args.iter().cloned());

    LaunchPlan {
        main_class: version.main_class.clone(),
        jvm_args,
        game_args,
        classpath,
        // 原生库清单由 `natives_to_extract` 单独提供：
        // 它需要区分「主 jar」和「原生 jar」，混在 classpath 构建里会搞混。
        natives: Vec::new(),
        natives_dir,
    }
}

/// 从已经替换好占位符的 JVM 参数里，回读 `-Djava.library.path` 指向的目录。
///
/// - 清单没写这一条（旧版本靠 `build_plan` 补）→ 回落 `config.natives_dir`；
/// - 清单写的是 `${natives_directory}/java` → 得到 `<natives_dir>/java`，
///   natives 解压时就该落到那里（26.2+ 的官方清单正是这么写的）；
/// - 相对路径 / 含子目录的写法一律尊重，只要它指得出来。
fn library_path_dir(jvm_args: &[String], config: &LaunchConfig) -> PathBuf {
    let Some(arg) = jvm_args
        .iter()
        .find(|a| a.starts_with("-Djava.library.path="))
    else {
        return config.natives_dir.clone();
    };
    let raw = arg["-Djava.library.path=".len()..].trim();
    if raw.is_empty() {
        return config.natives_dir.clone();
    }
    // 用分号/冒号分隔的多条路径：只认第一条，dll 也只需要一个落点。
    let first = raw
        .split(if cfg!(windows) { ';' } else { ':' })
        .next()
        .unwrap_or(raw)
        .trim();
    if first.is_empty() {
        return config.natives_dir.clone();
    }
    PathBuf::from(first)
}

/// 求一个库在本地的绝对路径。
fn library_path(
    lib: &crate::version::Library,
    ctx: &RuleContext,
    config: &LaunchConfig,
) -> Option<PathBuf> {
    if let Some(dl) = library_download(lib, ctx) {
        if let Some(path) = &dl.path {
            return Some(config.libraries_dir.join(path.replace('/', std::path::MAIN_SEPARATOR_STR)));
        }
    }
    // 没有 downloads 字段时按坐标推导
    let coord = lib.coord.as_ref()?;
    Some(
        config
            .libraries_dir
            .join(coord.to_path().replace('/', std::path::MAIN_SEPARATOR_STR)),
    )
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// 替换 `${key}` 占位符。未知占位符原样保留，便于排查。
pub fn substitute(input: &str, vars: &HashMap<&str, String>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let key = &after[..end];
                match vars.get(key) {
                    Some(value) => out.push_str(value),
                    None => {
                        out.push_str("${");
                        out.push_str(key);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                // 没有闭合，原样输出剩余
                out.push_str(&rest[start..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 从版本清单里的库信息补充 natives 需要解压的目标。
///
/// 返回 (来源 jar, 排除前缀列表)。
pub fn natives_to_extract(
    version: &ResolvedVersion,
    ctx: &RuleContext,
    config: &LaunchConfig,
) -> Vec<(PathBuf, Vec<String>)> {
    let mut out = Vec::new();
    for lib in version.effective_libraries(ctx) {
        // 用 has_natives 而非 is_native：老式条目（如 lwjgl 2.x）
        // 同时提供主 jar 和原生库，主 jar 进 classpath，原生库要解压，
        // 两者都得做，不能因为「它是 native」就把整条跳过。
        if !lib.has_natives() {
            continue;
        }
        if let Some(path) = native_jar_path(lib, ctx, config) {
            out.push((path, lib.extract_exclude.clone()));
        }
    }
    out
}

/// 从分类符里读出它打的是哪个架构。
///
/// 顺序要紧：`natives-windows-arm64` 也以 `64` 结尾，必须先认 `arm64`。
/// 没写架构的（`natives-windows`、`natives-macos`）按 x64 算——
/// 这正是 Mojang 从 LWJGL 3.3 起的约定：不标架构的那份就是 x64 包。
fn classifier_arch(classifier: &str) -> crate::rules::Arch {
    use crate::rules::Arch;
    if classifier.ends_with("-arm64") {
        return Arch::Arm64;
    }
    if classifier.ends_with("-x86") || classifier.ends_with("-32") {
        return Arch::X86;
    }
    Arch::X86_64
}

/// 求一个库的原生 jar 路径（注意：不是主构件）。
///
/// 这里要自己按架构挑一份。26.3 的清单里
/// `org.lwjgl:lwjgl:3.4.3:natives-windows` 与 `:natives-windows-arm64`
/// 的 `rules` **都只有 `{"os":{"name":"windows"}}`、没写 arch**，
/// 于是两条在 x64 上都会被保留；两个 jar 里的 `lwjgl.dll` 同名，
/// 后解压的那个（arm64）会把先写的顶掉，JVM 一加载就报
/// `UnsatisfiedLinkError: Failed to locate library: lwjgl.dll`
/// ——看着像 Sodium 崩了，其实是我们喂错了架构的 dll。
fn native_jar_path(
    lib: &crate::version::Library,
    ctx: &RuleContext,
    config: &LaunchConfig,
) -> Option<PathBuf> {
    let classifier = lib.native_classifier(ctx.os.name.as_str())?;
    let classifier = expand_arch_placeholder(&classifier, ctx);
    if classifier_arch(&classifier) != ctx.os.arch {
        return None;
    }

    // 老格式：classifiers 里有明确条目
    if let Some((_, dl)) = lib.classifiers.iter().find(|(k, _)| k == &classifier) {
        if let Some(rel) = &dl.path {
            return Some(
                config
                    .libraries_dir
                    .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)),
            );
        }
    }

    // 新格式：坐标自带分类符，主构件即原生 jar
    let coord = lib.coord.as_ref()?;
    if coord.classifier.as_deref() == Some(classifier.as_str()) {
        return Some(
            config
                .libraries_dir
                .join(coord.to_path().replace('/', std::path::MAIN_SEPARATOR_STR)),
        );
    }
    None
}

/// 展开 classifier 里的 `${arch}` 占位符（老版本写法）。
fn expand_arch_placeholder(classifier: &str, ctx: &RuleContext) -> String {
    if !classifier.contains("${arch}") {
        return classifier.to_string();
    }
    let bits = match ctx.os.arch {
        crate::rules::Arch::X86 => "32",
        _ => "64",
    };
    classifier.replace("${arch}", bits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::resolve;
    use crate::version::parse_version;

    #[test]
    fn offline_uuid_is_stable_and_32_hex() {
        let a = offline_uuid("Notch");
        let b = offline_uuid("Notch");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn md5_matches_known_vectors() {
        let hex = |s: &[u8]| -> String {
            md5(s).iter().map(|b| format!("{:02x}", b)).collect()
        };
        assert_eq!(hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn substitutes_known_placeholders() {
        let mut vars = HashMap::new();
        vars.insert("a", "1".to_string());
        vars.insert("b", "2".to_string());
        assert_eq!(substitute("${a}-${b}", &vars), "1-2");
        assert_eq!(substitute("${a}", &vars), "1");
        assert_eq!(substitute("none", &vars), "none");
    }

    #[test]
    fn unknown_placeholder_is_preserved() {
        let vars = HashMap::new();
        assert_eq!(substitute("${missing}", &vars), "${missing}");
    }

    #[test]
    fn unterminated_placeholder_is_preserved() {
        let vars = HashMap::new();
        assert_eq!(substitute("abc ${oops", &vars), "abc ${oops");
    }

    fn simple_version() -> ResolvedVersion {
        let v = parse_version(
            r#"{"id":"1.20.1-test","mainClass":"net.minecraft.client.main.Main",
                "assetIndex":{"id":"5"},"assets":"5","javaVersion":{"majorVersion":17},
                "libraries":[
                  {"name":"g:a:1.0","downloads":{"artifact":{"path":"g/a/1.0/a-1.0.jar","url":"","sha1":"","size":1}}}
                ],
                "arguments":{
                  "game":["--username","${auth_player_name}","--version","${version_name}",
                          "--gameDir","${game_directory}","--assetsDir","${assets_root}"],
                  "jvm":["-Djava.library.path=${natives_directory}","-cp","${classpath}"]
                }}"#,
        )
        .unwrap();
        resolve(&[v]).unwrap()
    }

    #[test]
    fn builds_expected_plan() {
        let version = simple_version();
        let account = Account::offline("Steve");
        let mut cfg = LaunchConfig::new("C:\\mc");
        cfg.client_jar = Some(PathBuf::from("C:\\mc\\versions\\x\\x.jar"));
        let plan = build_plan(&version, &account, &cfg);

        assert_eq!(plan.main_class, "net.minecraft.client.main.Main");
        assert_eq!(plan.classpath.len(), 2);
        // 客户端 jar 必须在首位
        assert!(plan.classpath[0].ends_with("x.jar"));
        assert!(plan.jvm_args.iter().any(|a| a == "-Xmx4096M"));
        assert!(plan
            .jvm_args
            .iter()
            .any(|a| a.contains("java.library.path")));
        // classpath 已被替换进 -cp 后面
        let cp_index = plan.jvm_args.iter().position(|a| a == "-cp").unwrap();
        assert!(plan.jvm_args[cp_index + 1].contains("a-1.0.jar"));
        // 游戏参数里用户名被替换
        let u = plan.game_args.iter().position(|a| a == "--username").unwrap();
        assert_eq!(plan.game_args[u + 1], "Steve");
    }

    #[test]
    fn offline_account_has_zero_token() {
        let acc = Account::offline("Steve");
        assert_eq!(acc.access_token.len(), 32);
        assert_eq!(acc.user_type, "legacy");
        assert_eq!(acc.uuid.len(), 32);
    }

    /// 旧格式清单（只有 minecraftArguments）没有 arguments.jvm，
    /// 必须补上 -Djava.library.path，否则游戏找不到 native dll：
    /// `UnsatisfiedLinkError: no lwjgl64 in java.library.path`
    #[test]
    fn legacy_manifest_gets_java_library_path() {
        let v = parse_version(
            r#"{"id":"1.8.9","mainClass":"net.minecraft.client.main.Main",
                "minecraftArguments":"--username ${auth_player_name} --version ${version_name}",
                "assetIndex":{"id":"1.8"},"assets":"1.8",
                "javaVersion":{"majorVersion":8},
                "libraries":[]}"#,
        )
        .unwrap();
        let legacy = v.minecraft_arguments.clone();
        let resolved = resolve(&[v]).unwrap().with_legacy_arguments(legacy);

        let cfg = LaunchConfig::new("C:\\mc");
        let plan = build_plan(&resolved, &Account::offline("Steve"), &cfg);

        assert!(
            plan.jvm_args
                .iter()
                .any(|a| a.starts_with("-Djava.library.path=")),
            "旧格式清单应补出 java.library.path，实际参数: {:?}",
            plan.jvm_args
        );
        // 并且要指向 natives 目录
        let arg = plan
            .jvm_args
            .iter()
            .find(|a| a.starts_with("-Djava.library.path="))
            .unwrap();
        assert!(
            arg.contains("natives"),
            "java.library.path 应指向 natives 目录: {}",
            arg
        );
    }

    /// 新版清单自带 java.library.path 时不能重复添加。
    #[test]
    fn modern_manifest_does_not_duplicate_library_path() {
        let version = simple_version();
        let cfg = LaunchConfig::new("C:\\mc");
        let plan = build_plan(&version, &Account::offline("A"), &cfg);
        let count = plan
            .jvm_args
            .iter()
            .filter(|a| a.starts_with("-Djava.library.path="))
            .count();
        assert_eq!(count, 1, "不应重复: {:?}", plan.jvm_args);
    }

    /// 老清单的 java.library.path 就是 natives 目录本身：
    /// dll 平铺在那儿，`plan.natives_dir` 必须跟着一致。
    #[test]
    fn flat_library_path_keeps_natives_at_the_top() {
        let version = simple_version();
        let cfg = LaunchConfig::new("C:\\\\mc");
        let plan = build_plan(&version, &Account::offline("A"), &cfg);
        assert_eq!(plan.natives_dir, cfg.natives_dir);
    }

    /// 26.2 起的清单写 `${natives_directory}/java`，多一层子目录。
    /// 把这个子目录解出来，dll 才会被 JVM 找到；否则 Sodium 这类
    /// 在 preLaunch 就摸 LWJGL 的模组会报 Failed to locate library: lwjgl.dll。
    #[test]
    fn java_subfolder_in_library_path_moves_the_natives() {
        let v = parse_version(
            r#"{"id":"26.3","mainClass":"net.minecraft.client.main.Main",
                "assetIndex":{"id":"34"},"assets":"34","javaVersion":{"majorVersion":25},
                "libraries":[],
                "arguments":{
                  "game":["--username","${auth_player_name}"],
                  "jvm":["-Djava.library.path=${natives_directory}/java","-cp","${classpath}"]
                }}"#,
        )
        .unwrap();
        let resolved = resolve(&[v]).unwrap();
        let cfg = LaunchConfig::new("C:\\\\mc");
        let plan = build_plan(&resolved, &Account::offline("A"), &cfg);

        assert_eq!(plan.natives_dir, cfg.natives_dir.join("java"));
        assert!(plan
            .jvm_args
            .iter()
            .any(|a| a.ends_with("/java") && a.starts_with("-Djava.library.path=")));
    }

    /// 清单里写相对路径时也照它来，别偷偷换成绝对路径——
    /// 相对路径是相对工作目录（实例目录）解析的。
    #[test]
    fn a_relative_library_path_is_respected() {
        let v = parse_version(
            r#"{"id":"x","mainClass":"m","assetIndex":{"id":"1"},"libraries":[],
                "arguments":{"game":[],"jvm":["-Djava.library.path=natives-local"]}}"#,
        )
        .unwrap();
        let resolved = resolve(&[v]).unwrap();
        let cfg = LaunchConfig::new("C:\\\\mc");
        let plan = build_plan(&resolved, &Account::offline("A"), &cfg);
        assert_eq!(plan.natives_dir, PathBuf::from("natives-local"));
    }

    /// 26.3 的 lwjgl 原生包按分类符分 x64 / arm64 / x86 三份，
    /// 而三条的 `rules` 里**都没写 arch**。x64 上必须只挑那份 x64 的，
    /// 否则 arm64 的 dll 会覆盖掉 x64 的，JVM 一加载就
    /// `UnsatisfiedLinkError: Failed to locate library: lwjgl.dll`。
    #[test]
    fn only_the_native_jar_for_this_cpu_is_picked() {
        let v = parse_version(
            r#"{"id":"26.3","mainClass":"m","assetIndex":{"id":"34"},"libraries":[
                 {"name":"org.lwjgl:lwjgl:3.4.3:natives-windows",
                  "rules":[{"action":"allow","os":{"name":"windows"}}],
                  "downloads":{"artifact":{"path":"org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-windows.jar","url":"","sha1":"","size":1}}},
                 {"name":"org.lwjgl:lwjgl:3.4.3:natives-windows-arm64",
                  "rules":[{"action":"allow","os":{"name":"windows"}}],
                  "downloads":{"artifact":{"path":"org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-windows-arm64.jar","url":"","sha1":"","size":1}}},
                 {"name":"org.lwjgl:lwjgl:3.4.3:natives-windows-x86",
                  "rules":[{"action":"allow","os":{"name":"windows"}}],
                  "downloads":{"artifact":{"path":"org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-windows-x86.jar","url":"","sha1":"","size":1}}}
               ]}"#,
        )
        .unwrap();
        let resolved = resolve(&[v]).unwrap();
        let cfg = LaunchConfig::new("C:\\\\mc");

        let picked = |arch: crate::rules::Arch| -> Vec<String> {
            let ctx = RuleContext::new(
                crate::rules::OsInfo {
                    name: crate::rules::OsName::Windows,
                    version: String::new(),
                    arch,
                },
                crate::rules::Features::default(),
            );
            natives_to_extract(&resolved, &ctx, &cfg)
                .iter()
                .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };

        assert_eq!(
            picked(crate::rules::Arch::X86_64),
            vec!["lwjgl-3.4.3-natives-windows.jar".to_string()]
        );
        assert_eq!(
            picked(crate::rules::Arch::Arm64),
            vec!["lwjgl-3.4.3-natives-windows-arm64.jar".to_string()]
        );
        assert_eq!(
            picked(crate::rules::Arch::X86),
            vec!["lwjgl-3.4.3-natives-windows-x86.jar".to_string()]
        );
    }

    #[test]
    fn classifier_arch_reads_the_suffix_right_way_round() {
        use crate::rules::Arch;
        // `natives-windows-arm64` 也以 64 结尾，别被它骗了。
        assert_eq!(classifier_arch("natives-windows-arm64"), Arch::Arm64);
        assert_eq!(classifier_arch("natives-macos-arm64"), Arch::Arm64);
        assert_eq!(classifier_arch("natives-windows"), Arch::X86_64);
        assert_eq!(classifier_arch("natives-macos"), Arch::X86_64);
        assert_eq!(classifier_arch("natives-windows-x86"), Arch::X86);
        assert_eq!(classifier_arch("natives-linux-32"), Arch::X86);
    }

    #[test]
    fn classpath_separator_matches_platform() {        let version = simple_version();
        let mut cfg = LaunchConfig::new("C:\\mc");
        // 需要至少两项才能观察到分隔符
        cfg.client_jar = Some(PathBuf::from("C:\\mc\\versions\\x\\x.jar"));
        let plan = build_plan(&version, &Account::offline("A"), &cfg);
        assert!(plan.classpath.len() >= 2);
        let s = plan.classpath_string();
        let sep = if cfg!(windows) { ';' } else { ':' };
        assert!(s.contains(sep), "classpath 应含分隔符 {:?}: {}", sep, s);
    }
}
