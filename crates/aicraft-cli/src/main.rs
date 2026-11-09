//! aicraft 命令行入口。
//!
//! 现阶段只做三件事，目的是在没有 UI 的情况下把 core 的行为看透：
//!
//! - `list`    列出 .minecraft 下的版本及各自需要的 Java
//! - `inspect` 打印某个版本解析后的完整启动参数
//! - `launch`  真正启动游戏（默认 dry-run）
//!
//! 刻意做成 std-only，不引 clap。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use aicraft_core::java;
use aicraft_core::launch::{self, Account, LaunchConfig};
use aicraft_core::resolve;
use aicraft_core::rules::RuleContext;
use aicraft_core::version::parse_version;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = run(&args);
    std::process::exit(code);
}

fn run(args: &[String]) -> i32 {
    if args.is_empty() {
        print_usage();
        return 2;
    }

    match args[0].as_str() {
        "list" => cmd_list(&args[1..]),
        "inspect" => cmd_inspect(&args[1..]),
        "launch" => cmd_launch(&args[1..]),
        "verify" => cmd_verify(&args[1..]),
        "repair" => cmd_repair(&args[1..]),
        "versions" => cmd_versions(&args[1..]),
        "install" => cmd_install(&args[1..]),
        "login" => cmd_login(&args[1..]),
        "help" | "-h" | "--help" => {
            print_usage();
            0
        }
        "-V" | "--version" => {
            println!("AICraft Launcher {}", aicraft_core::VERSION);
            0
        }
        other => {
            eprintln!("未知子命令: {}", other);
            print_usage();
            2
        }
    }
}

fn print_usage() {
    println!(
        "AICraft Launcher {} —— MCJE 启动器核心\n\
         \n\
         用法:\n\
         \x20 aicraft list     <game_dir>                列出已安装版本\n\
         \x20 aicraft versions [--snapshots] [--limit N]  列出可安装的官方版本\n\
         \x20 aicraft install  <game_dir> <version> [选项]  安装某个版本\n\
         \x20 aicraft inspect  <game_dir> <version>       打印启动参数（不启动）\n\
         \x20 aicraft launch   <game_dir> <version> [选项]  启动游戏\n\
         \x20 aicraft verify   <game_dir> <version>       校验资源与库完整性\n\
         \x20 aicraft repair   <game_dir> <version>       校验并补全缺失文件\n\
         \n\
         install 选项:\n\
         \x20 --name <实例名>       装成指定名字的实例（同一版本可装多份）\n\
         \x20 --dry-run            只显示计划，不下载\n\
         \x20 --jobs <N>           并发下载数（默认 8）\n\
         \n\
         launch 选项:\n\
         \x20 --username <名字>    离线用户名（默认 Steve）\n\
         \x20 --java <路径>        指定 java 可执行文件\n\
         \x20 --xmx <MB>           最大内存（默认 4096）\n\
         \x20 --isolate           强制实例隔离（游戏目录 = versions/<id>）\n\
         \x20 --no-isolate        强制共享目录（游戏目录 = .minecraft）\n\
         \x20 --dry-run            只打印命令，不真正启动\n\
         \x20 --show-cmd           打印完整命令行\n\
         \n\
         verify 选项:\n\
         \x20 --hash               连 SHA-1 一并校验（慢，但能发现内容损坏）\n\
         \n\
         repair 选项:\n\
         \x20 --dry-run            只列出要下载什么，不实际下载\n\
         \x20 --jobs <N>           并发下载数（默认 6）",
        aicraft_core::VERSION
    );
    println!(
        "\n\
         login 选项:\n\
         \x20 --browser            用浏览器授权码登录（默认走设备码）\n\
         \x20 --store <路径>         账号文件位置（默认 <数据目录>\\accounts.json）"
    );
}

/// 从给定 game_dir 读取所有版本目录名（含 JSON 的）。
fn list_versions(game_dir: &Path) -> Vec<(String, PathBuf)> {
    let versions_dir = game_dir.join("versions");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&versions_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let name = match dir.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        // 约定：版本目录里的清单文件名与目录名一致
        let json = dir.join(format!("{}.json", name));
        if json.is_file() {
            out.push((name, json));
        } else if let Ok(inner) = std::fs::read_dir(&dir) {
            // 兜底：目录里任意一个 .json
            for f in inner.flatten() {
                let p = f.path();
                if p.extension().and_then(|e| e.to_str()) == Some("json") {
                    out.push((name, p));
                    break;
                }
            }
        }
    }
    out.sort_by_key(|a| a.0.to_lowercase());
    out
}

fn cmd_list(args: &[String]) -> i32 {
    let Some(game_dir) = args.first() else {
        eprintln!("缺少 game_dir");
        return 2;
    };
    let game_dir = PathBuf::from(game_dir);
    let versions = list_versions(&game_dir);
    if versions.is_empty() {
        eprintln!("在 {} 下没找到任何版本", game_dir.join("versions").display());
        return 1;
    }

    println!("发现 {} 个版本:\n", versions.len());
    println!("{:<28} {:>6}  {:<6}  主类", "版本", "Java", "实例");
    println!("{}", "-".repeat(96));
    for (name, json) in &versions {
        match std::fs::read_to_string(json).and_then(|t| {
            parse_version(&t).map_err(|e| std::io::Error::other(e.message))
        }) {
            Ok(raw) => {
                let java = raw
                    .java_version
                    .major_version
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "?".into());
                let mode = aicraft_core::detect_mode(&game_dir, name);
                let main = raw.main_class.clone().unwrap_or_default();
                let main_short = if main.len() > 40 {
                    format!("…{}", &main[main.len() - 39..])
                } else {
                    main
                };
                println!("{:<28} {:>6}  {:<6}  {}", name, java, mode.as_str(), main_short);
            }
            Err(e) => {
                println!("{:<28} {:>6}  {:<6}  <解析失败: {}>", name, "?", "?", e);
            }
        }
    }
    0
}

/// 加载并解析一个版本，返回 (ResolvedVersion, 旧格式参数)。
fn load_version(
    game_dir: &Path,
    version_name: &str,
) -> Result<(resolve::ResolvedVersion, aicraft_core::version::RawVersion, PathBuf), String> {
    let json_path = game_dir
        .join("versions")
        .join(version_name)
        .join(format!("{}.json", version_name));

    // 走继承链：Forge / NeoForge / Fabric 的清单只写加载器差异，
    // 主类、库、资源索引都在父版本里，单解析本层会缺 jar 与 assetIndex。
    let chain = aicraft_core::version::load_chain_from_dir(game_dir, version_name)
        .map_err(|e| e.to_string())?;
    let raw = chain
        .last()
        .cloned()
        .ok_or_else(|| format!("版本 {} 的继承链是空的", version_name))?;

    let legacy = raw.minecraft_arguments.clone();
    let resolved = resolve::resolve(&chain)
        .map_err(|e| format!("合并失败: {}", e))?
        .with_legacy_arguments(legacy);

    Ok((resolved, raw, json_path))
}

fn cmd_inspect(args: &[String]) -> i32 {
    if args.len() < 2 {
        eprintln!("用法: aicraft inspect <game_dir> <version>");
        return 2;
    }
    let game_dir = PathBuf::from(&args[0]);
    let version_name = &args[1];

    let (resolved, raw, json_path) = match load_version(&game_dir, version_name) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };

    println!("清单: {}", json_path.display());
    println!("版本: {}", resolved.id);
    println!("主类: {}", resolved.main_class);
    println!(
        "Java: {}",
        resolved
            .java_version
            .major_version
            .map(|v| v.to_string())
            .unwrap_or_else(|| "未声明".into())
    );
    println!("资源索引: {}", resolved.asset_index.id);
    if let Some(vr) = &resolved.asset_index.virtual_root {
        println!("虚拟资源: {}", vr);
    }
    println!("库总数: {}", resolved.libraries.len());
    println!(
        "patches: {}",
        raw.patches
            .iter()
            .map(|p| format!("{}({})", p.id, p.priority))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let ctx = RuleContext::current();
    let effective = resolved.effective_libraries(&ctx);
    println!("当前平台生效库: {}", effective.len());

    // 客户端 jar 位置
    let client_jar = pick_client_jar(&game_dir, &resolved);
    println!(
        "客户端 jar: {}",
        client_jar
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<无>".into())
    );

    let mut cfg = LaunchConfig::new(&game_dir);
    let inspect_mode = aicraft_core::detect_mode(&game_dir, version_name);
    cfg = cfg.with_instance_mode(&game_dir, version_name, inspect_mode);
    cfg.client_jar = client_jar;
    println!(
        "实例模式: {}（游戏目录 {}）",
        inspect_mode.as_str(),
        cfg.game_dir.display()
    );
    let account = Account::offline("Steve");
    let plan = launch::build_plan(&resolved, &account, &cfg);

    println!("\n--- classpath ({} 项) ---", plan.classpath.len());
    for p in plan.classpath.iter().take(10) {
        println!("  {}", p.display());
    }
    if plan.classpath.len() > 10 {
        println!("  … 其余 {} 项", plan.classpath.len() - 10);
    }

    println!("\n--- JVM 参数 ---");
    for a in &plan.jvm_args {
        println!("  {}", a);
    }
    println!("\n--- 游戏参数 ---");
    for a in &plan.game_args {
        println!("  {}", a);
    }

    // 检查关键文件是否存在
    println!("\n--- 完整性抽查 ---");
    let mut missing = 0;
    for p in &plan.classpath {
        if !p.is_file() {
            println!("  缺失: {}", p.display());
            missing += 1;
        }
    }
    if missing == 0 {
        println!("  classpath 内所有 jar 均存在");
    } else {
        println!("  共 {} 个 jar 缺失", missing);
    }

    // 该版本需要的 Java 是否可用
    if let Some(need) = resolved.java_version.major_version {
        println!("\n--- Java 匹配 ---");
        let runtimes = java::detect_all();
        if runtimes.is_empty() {
            println!("  未探测到任何 Java");
        } else {
            for r in &runtimes {
                println!(
                    "  Java {:<3} {}  [{}]",
                    r.major_version,
                    r.path.display(),
                    r.source
                );
            }
            match java::pick_java(&runtimes, need) {
                Some(r) => println!(
                    "  -> 选用 Java {} ({})",
                    r.major_version,
                    r.path.display()
                ),
                None => println!("  -> 没有满足 Java {} 的运行时", need),
            }
        }
    }
    0
}

/// 已提示过的事件记录文件位置。
///
/// 优先放在 game_dir 下的启动器私有目录，这样「换一个 .minecraft」就等于
/// 全新开始，符合直觉。但如果 game_dir 不可写（只读盘、权限受限、
/// 被沙箱限制），退回用户级配置目录——记录提示这件事本身不该因为
/// 游戏目录只读而失效，否则每次启动都会重复弹窗，反而更烦。
fn notice_log_path(game_dir: &Path) -> PathBuf {
    let local = game_dir.join("aicraft").join("notices.txt");
    if local_can_write(game_dir) {
        return local;
    }
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("aicraft").join("notices.txt")
}

/// 探测某个目录下能否创建文件。
///
/// 用「创建再删掉」的试探，比看只读属性可靠：目录可能因为没有写入 ACL、
/// 或是网络盘/沙箱限制而不可写，这些情况属性位看不出来。
fn local_can_write(dir: &Path) -> bool {
    let probe_dir = dir.join("aicraft");
    if std::fs::create_dir_all(&probe_dir).is_err() {
        return false;
    }
    let probe = probe_dir.join(".write-probe");
    let ok = std::fs::write(&probe, b"x").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// 该提示是否已经展示过。
fn notice_seen(game_dir: &Path, key: &str) -> bool {
    let path = notice_log_path(game_dir);
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|line| line.trim() == key)
}

/// 记录一条已展示的提示。
///
/// 写失败不影响启动——提示记录是纯优化，不是功能。
fn record_notice(game_dir: &Path, key: &str) {
    let path = notice_log_path(game_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{}", key);
    }
}

/// 载入版本并跑校验，返回报告。
fn run_verify(
    game_dir: &Path,
    version_name: &str,
    mode: aicraft_core::VerifyMode,
) -> Result<aicraft_core::VerifyReport, String> {
    let (resolved, _, _) = load_version(game_dir, version_name)?;
    let ctx = RuleContext::current();
    Ok(aicraft_core::verify_version(
        game_dir,
        &resolved,
        &ctx,
        mode,
    ))
}

/// 打印校验报告。
fn print_report(report: &aicraft_core::VerifyReport, mode: aicraft_core::VerifyMode) {
    println!("检查 {} 个文件", report.checked);
    println!("  assets 正常: {}", report.assets_ok);
    println!("  库文件正常: {}", report.libs_ok);

    if report.is_clean() {
        println!("\n✅ 全部完整，无需补全");
        return;
    }

    println!("\n发现 {} 个问题:", report.total_problems());
    for (kind, count) in &report.counts {
        println!("  {}: {}", kind, count);
    }

    println!("\n--- 明细 ---");
    // 按类别分组，便于阅读；同类内部按路径排序
    for (kind, _) in &report.counts {
        println!("\n[{}]", kind);
        let mut items: Vec<_> = report.problems.iter().filter(|p| &p.kind == kind).collect();
        items.sort_by(|a, b| a.path.cmp(&b.path));
        for item in items.iter().take(20) {
            println!("  {}", item.reason.as_str());
            println!("      {}", item.path.display());
        }
        if items.len() > 20 {
            println!("  … 其余 {} 项", items.len() - 20);
        }
    }

    if mode == aicraft_core::VerifyMode::Existence {
        println!("\n提示: 加 --hash 可以连文件内容一起校验（较慢，但能发现损坏）");
    }
}

fn cmd_verify(args: &[String]) -> i32 {
    if args.len() < 2 {
        eprintln!("用法: aicraft verify <game_dir> <version> [--hash]");
        return 2;
    }
    let mut game_dir = None;
    let mut version_name = None;
    let mut use_hash = false;
    for a in args {
        match a.as_str() {
            "--hash" => use_hash = true,
            other => {
                if game_dir.is_none() {
                    game_dir = Some(PathBuf::from(other));
                } else if version_name.is_none() {
                    version_name = Some(other.to_string());
                }
            }
        }
    }
    let (Some(game_dir), Some(version_name)) = (game_dir, version_name) else {
        eprintln!("用法: aicraft verify <game_dir> <version> [--hash]");
        return 2;
    };

    let mode = if use_hash {
        aicraft_core::VerifyMode::Hash
    } else {
        aicraft_core::VerifyMode::Existence
    };

    println!(
        "校验 {} （{} 模式）…",
        version_name,
        if use_hash { "含哈希" } else { "仅存在性" }
    );

    let started = std::time::Instant::now();
    match run_verify(&game_dir, &version_name, mode) {
        Ok(report) => {
            println!();
            print_report(&report, mode);
            println!("\n耗时 {:.0} ms", started.elapsed().as_millis());
            if report.is_clean() {
                0
            } else {
                1
            }
        }
        Err(e) => {
            eprintln!("{}", e);
            1
        }
    }
}

fn cmd_repair(args: &[String]) -> i32 {
    if args.len() < 2 {
        eprintln!("用法: aicraft repair <game_dir> <version> [--dry-run] [--jobs N]");
        return 2;
    }
    let mut game_dir = None;
    let mut version_name = None;
    let mut dry_run = false;
    let mut jobs = 6usize;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => dry_run = true,
            "--jobs" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match v.parse() {
                        Ok(n) => jobs = n,
                        Err(_) => {
                            eprintln!("--jobs 需要数字");
                            return 2;
                        }
                    }
                }
            }
            other => {
                if game_dir.is_none() {
                    game_dir = Some(PathBuf::from(other));
                } else if version_name.is_none() {
                    version_name = Some(other.to_string());
                }
            }
        }
        i += 1;
    }

    let (Some(game_dir), Some(version_name)) = (game_dir, version_name) else {
        eprintln!("用法: aicraft repair <game_dir> <version> [--dry-run] [--jobs N]");
        return 2;
    };

    println!("校验 {} …", version_name);
    let report = match run_verify(&game_dir, &version_name, aicraft_core::VerifyMode::Existence) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };

    if report.is_clean() {
        println!("\n✅ 全部完整，无需补全");
        return 0;
    }

    println!("需要补全 {} 个文件", report.total_problems());
    for (kind, count) in &report.counts {
        println!("  {}: {}", kind, count);
    }

    let config = aicraft_core::RepairConfig {
        concurrency: jobs,
        dry_run,
        mirror: aicraft_core::Mirror::Official,
        limit_kbps: None,
    };

    if dry_run {
        println!("\n--- 将下载（未实际执行）---");
        for item in report.problems.iter().take(30) {
            match &item.url {
                Some(url) => println!("  {}\n      {}", item.path.display(), url),
                None => println!("  {}\n      (无下载地址，需手动处理)", item.path.display()),
            }
        }
        if report.problems.len() > 30 {
            println!("  … 其余 {} 项", report.problems.len() - 30);
        }
        return 0;
    }

    // 下载前清理损坏文件
    for item in &report.problems {
        if let Err(e) = aicraft_core::repair::prepare_for_download(item) {
            eprintln!("准备 {} 失败: {}", item.path.display(), e);
        }
    }

    println!("\n开始下载（并发 {}）…", jobs);
    let started = std::time::Instant::now();
    let result = aicraft_core::repair(&report.problems, &config);
    let elapsed = started.elapsed();

    println!("\n--- 结果 ---");
    println!("  下载成功: {}", result.downloaded);
    println!("  已存在跳过: {}", result.skipped);
    println!("  失败: {}", result.failed);
    if result.no_url > 0 {
        println!("  无下载地址: {}", result.no_url);
    }
    println!(
        "  传输 {} MB，耗时 {:.1} 秒",
        result.bytes as f64 / 1_048_576.0,
        elapsed.as_secs_f64()
    );

    if result.failed > 0 {
        println!("\n失败明细:");
        for r in &result.results {
            if let aicraft_core::repair::RepairOutcome::Failed { message } = &r.outcome {
                println!("  {}\n      {}", r.item.path.display(), message);
            }
        }
    }

    if result.no_url > 0 {
        println!("\n以下文件没有下载地址，需手动处理:");
        for r in &result.results {
            if matches!(r.outcome, aicraft_core::repair::RepairOutcome::NoUrl) {
                println!("  {}", r.item.path.display());
            }
        }
    }

    if result.is_success() {
        println!("\n✅ 补全完成");
        0
    } else {
        1
    }
}

/// 决定客户端 jar 用哪个文件。 约定：`versions/<id>/<jar_id>.jar`。清单里 `jar` 字段若指向别的版本，
/// 说明复用父版本的 jar。
fn pick_client_jar(
    game_dir: &Path,
    resolved: &resolve::ResolvedVersion,
) -> Option<PathBuf> {
    let jar_id = resolved.jar_id.as_ref().unwrap_or(&resolved.id);
    let candidate = game_dir
        .join("versions")
        .join(jar_id)
        .join(format!("{}.jar", jar_id));
    if candidate.is_file() {
        return Some(candidate);
    }
    // 退一步：版本目录里任意 jar
    let dir = game_dir.join("versions").join(jar_id);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("jar") {
                return Some(p);
            }
        }
    }
    None
}

fn cmd_versions(args: &[String]) -> i32 {
    let mut include_snapshots = false;
    let mut limit = 30usize;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--snapshots" => include_snapshots = true,
            "--limit" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(n) = v.parse() {
                        limit = n;
                    }
                }
            }
            other => {
                eprintln!("未知选项: {}", other);
                return 2;
            }
        }
        i += 1;
    }

    println!("正在拉取官方版本清单…");
    let manifest = match aicraft_core::install::fetch_manifest() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };

    println!(
        "共 {} 个版本（最新正式版 {}，最新快照 {}）\n",
        manifest.versions.len(),
        manifest.latest_release.as_deref().unwrap_or("?"),
        manifest.latest_snapshot.as_deref().unwrap_or("?")
    );

    println!("{:<24} {:<10} 发布时间", "版本", "类型");
    println!("{}", "-".repeat(62));

    let mut shown = 0;
    for v in &manifest.versions {
        if !include_snapshots && v.version_type != "release" {
            continue;
        }
        let time = if v.release_time.len() >= 10 {
            &v.release_time[..10]
        } else {
            &v.release_time[..]
        };
        println!("{:<24} {:<10} {}", v.id, v.version_type, time);
        shown += 1;
        if shown >= limit {
            break;
        }
    }
    if !include_snapshots {
        println!("\n（只显示正式版，加 --snapshots 可看快照与旧版）");
    }
    0
}

fn cmd_install(args: &[String]) -> i32 {
    if args.len() < 2 {
        eprintln!("用法: aicraft install <game_dir> <version> [--name <实例名>] [--dry-run] [--jobs N]");
        return 2;
    }

    let mut positional: Vec<String> = Vec::new();
    let mut dry_run = false;
    let mut jobs = 8usize;
    let mut instance_name: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => dry_run = true,
            "--name" => {
                i += 1;
                match args.get(i) {
                    Some(v) => instance_name = Some(v.clone()),
                    None => {
                        eprintln!("--name 需要实例名");
                        return 2;
                    }
                }
            }
            "--jobs" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match v.parse() {
                        Ok(n) => jobs = n,
                        Err(_) => {
                            eprintln!("--jobs 需要数字");
                            return 2;
                        }
                    }
                }
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }

    if positional.len() < 2 {
        eprintln!("用法: aicraft install <game_dir> <version> [--name <实例名>] [--dry-run] [--jobs N]");
        return 2;
    }
    let game_dir = PathBuf::from(positional[0].clone());
    let version = positional[1].clone();

    if let Some(name) = &instance_name {
        if !aicraft_core::install::is_valid_instance_name(name.trim()) {
            eprintln!(
                "实例名 '{}' 不合法：不能含 \\ / : * ? \" < > |，也不能是 . 或 ..",
                name
            );
            return 2;
        }
    }

    println!("正在拉取官方版本清单…");
    let manifest = match aicraft_core::install::fetch_manifest_in(Some(&game_dir)) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };

    let Some(summary) = manifest.find(&version) else {
        eprintln!("官方清单里没有版本 '{}'", version);
        eprintln!("可用最新正式版: {}", manifest.latest_release.unwrap_or_default());
        eprintln!("提示: 用 `aicraft versions` 查看可安装的版本");
        return 1;
    };

    println!("目标版本: {} ({})", summary.id, summary.version_type);

    // 落盘目录名：没给 --name 就用官方版本号；给了就按实例名，
    // 这样同一个版本可以装成多份。
    let instance = instance_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&version);
    if instance != version {
        println!("实例名: {}（官方 {}）", instance, version);
    }

    if !dry_run && aicraft_core::install::is_installed(&game_dir, instance) {
        println!("\n实例 '{}' 已存在。换个 --name 可以再装一份，或用 `aicraft repair` 补齐文件。", instance);
        return 0;
    }

    println!("下载版本清单…");
    let version_json = match aicraft_core::install::fetch_version_json_in(&summary.url, Some(&game_dir)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };

    // 资源索引需要先拿到内容才能展开对象列表
    let parsed = match aicraft_core::version::parse_version(&version_json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("解析版本清单失败: {}", e);
            return 1;
        }
    };

    let index_text = if let Some(url) = &parsed.asset_index.url {
        println!("下载资源索引…");
        // 索引只有几百 KB，直接读进内存；正式文件会在安装计划里
        // 作为 AssetIndex 条目落到 assets/indexes/ 下。
        match aicraft_core::transport::fetch_text(url) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("资源索引下载失败: {}", e);
                return 1;
            }
        }
    } else {
        None
    };

    let ctx = RuleContext::current();
    let plan = match aicraft_core::install::plan_install_as(
        &game_dir,
        &version_json,
        index_text.as_deref(),
        &ctx,
        Some(instance),
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };

    println!("\n安装计划:");
    for (kind, n) in &plan.stats {
        println!("  {:<8} {}", kind.as_str(), n);
    }
    println!(
        "  合计 {} 个文件，约 {:.1} MB",
        plan.items.len(),
        plan.total_bytes as f64 / 1_048_576.0
    );

    if dry_run {
        println!("\n(dry-run，未实际下载)");
        return 0;
    }

    println!("\n开始安装（并发 {}）…", jobs);
    let started = std::time::Instant::now();
    let mut last_marker = 0usize;

    let report = match aicraft_core::install::execute_install(&plan, jobs, |ev| {
        if let aicraft_core::install::InstallProgress::Tick { done, total, bytes } = ev {
            // 每完成 5% 打印一次，避免刷屏
            let step = (total / 20).max(1);
            if done == total || done >= last_marker + step {
                last_marker = done;
                println!(
                    "  {}/{}  {:.1} MB",
                    done,
                    total,
                    bytes as f64 / 1_048_576.0
                );
            }
        }
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("安装失败: {}", e);
            return 1;
        }
    };

    println!(
        "\n完成: 下载 {}，跳过 {}，失败 {}，共 {:.1} MB，耗时 {:.0} 秒",
        report.downloaded,
        report.skipped,
        report.failed,
        report.bytes as f64 / 1_048_576.0,
        started.elapsed().as_secs()
    );

    if report.failed > 0 {
        println!("\n失败明细:");
        for (path, msg) in report.failures.iter().take(10) {
            println!("  {}\n      {}", path.display(), msg);
        }
        return 1;
    }

    println!("\n✅ {} 安装完成", version);
    println!("现在可以运行: aicraft launch {} {}", game_dir.display(), version);
    0
}

fn cmd_launch(args: &[String]) -> i32 {
    if args.len() < 2 {
        eprintln!("用法: aicraft launch <game_dir> <version> [选项]");
        return 2;
    }
    let game_dir = PathBuf::from(&args[0]);
    let version_name = args[1].clone();

    let mut username = "Steve".to_string();
    let mut java_path: Option<PathBuf> = None;
    let mut xmx: u32 = 4096;
    let mut dry_run = false;
    let mut show_cmd = false;
    // None = 自动探测；Some(true) = 强制隔离；Some(false) = 强制共享
    let mut isolate_flag: Option<bool> = None;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--username" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    username = v.clone();
                }
            }
            "--java" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    java_path = Some(PathBuf::from(v));
                }
            }
            "--xmx" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match v.parse() {
                        Ok(m) => xmx = m,
                        Err(_) => {
                            eprintln!("--xmx 需要数字");
                            return 2;
                        }
                    }
                }
            }
            "--dry-run" => dry_run = true,
            "--show-cmd" => show_cmd = true,
            "--isolate" => isolate_flag = Some(true),
            "--no-isolate" => isolate_flag = Some(false),
            other => {
                eprintln!("未知选项: {}", other);
                return 2;
            }
        }
        i += 1;
    }

    let (resolved, _raw, _) = match load_version(&game_dir, &version_name) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };

    // 选 Java
    let needed = resolved.java_version.major_version.unwrap_or(17);
    let mut java_notice: Option<String> = None;
    let runtime = match &java_path {
        Some(p) => match java::probe_java(p) {
            Some(mut r) => {
                r.source = "手动指定".into();
                if r.major_version < needed {
                    eprintln!(
                        "注意: 手动指定的 Java {} 低于该版本要求的 Java {}",
                        r.major_version, needed
                    );
                }
                r
            }
            None => {
                eprintln!("无法从 {} 读取 Java 版本", p.display());
                return 1;
            }
        },
        None => {
            let runtimes = java::detect_all();
            match java::pick_java_choice(&runtimes, needed) {
                Some((r, choice)) => {
                    if let Some(text) = choice.notice() {
                        java_notice = Some(text);
                    }
                    r.clone()
                }
                None => {
                    eprintln!("找不到满足 Java {} 的运行时", needed);
                    eprintln!("已探测到:");
                    for r in &runtimes {
                        eprintln!("  Java {} {}", r.major_version, r.path.display());
                    }
                    return 1;
                }
            }
        }
    };

    let client_jar = pick_client_jar(&game_dir, &resolved);

    // 实例隔离：默认按版本目录里有没有用户数据自动判断，
    // 也可以用 --isolate / --no-isolate 强制。
    let mode = match isolate_flag {
        Some(true) => aicraft_core::InstanceMode::Isolated,
        Some(false) => aicraft_core::InstanceMode::Shared,
        None => aicraft_core::detect_mode(&game_dir, &version_name),
    };
    if mode == aicraft_core::InstanceMode::Isolated && isolate_flag == Some(true) {
        // 强制隔离时把目录结构建出来
        let _ = aicraft_core::ensure_instance(&game_dir, &version_name);
    }

    let mut cfg = LaunchConfig::new(&game_dir);
    cfg = cfg.with_instance_mode(&game_dir, &version_name, mode);
    cfg.client_jar = client_jar;
    cfg.max_memory_mb = Some(xmx);

    // Java 版本对不上时的提示：同一个版本只提示一次。
    //
    // 判断依据是「用户是否已经知道这件事」，而不是「是否每次启动」。
    // 反复弹同样的提示只会让人习惯性忽略，反而掩盖真正的新问题。
    // 用户升级 Java 或换版本后会重新评估，所以记录按 (版本 id, 所需 Java,
    // 实际 Java) 三元组存，任一变化都会再次提示。
    if let Some(notice) = &java_notice {
        let key = format!(
            "{}|{}|{}",
            version_name, needed, runtime.major_version
        );
        if !notice_seen(&game_dir, &key) {
            eprintln!();
            eprintln!("─────────────────────────────────────────────");
            eprintln!("  Java 版本提示");
            eprintln!("─────────────────────────────────────────────");
            for line in notice.lines() {
                eprintln!("  {}", line);
            }
            eprintln!("─────────────────────────────────────────────");
            eprintln!();
            record_notice(&game_dir, &key);
        }
    }

    // natives 目录：每个版本一个独立目录，避免不同版本的 dll 互相污染。
    // 直接复用 versions/<id>/natives-lib 会随卸载版本一起消失，但换来的是
    // 切换版本时绝不会加载到错误的 dll —— 这类崩溃（0xc0000005）极难排查。
    let natives_dir = game_dir
        .join("versions")
        .join(&version_name)
        .join("natives-lib");
    cfg.natives_dir = natives_dir.clone();

    // 日志配置：若版本要求，且文件已存在就带上
    if let Some(logging) = &resolved.logging {
        if let Some(file) = &logging.file {
            if let Some(path) = &file.path {
                let p = game_dir.join("assets").join("log_configs").join(
                    Path::new(path)
                        .file_name()
                        .unwrap_or_default(),
                );
                if p.is_file() {
                    cfg.logging_file = Some(p);
                }
            }
        }
    }

    // 启动前校准账号：已保存的正版会话过期就自动续期，失败才退回离线。
    let (account, account_note) = resolve_account(&username);
    if let Some(note) = &account_note {
        eprintln!("{}", note);
    }
    let plan = launch::build_plan(&resolved, &account, &cfg);

    let mut cmd = Command::new(&runtime.path);
    for a in plan.to_argv() {
        cmd.arg(a);
    }
    // 进程工作目录也跟着实例走：模组/配置里有不少相对路径，
    // 落在 .minecraft 根会让它们写错地方。
    cmd.current_dir(&cfg.game_dir);
    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

    if show_cmd || dry_run {
        println!("Java: {} (Java {})", runtime.path.display(), runtime.major_version);
        println!("工作目录: {} [{}]", cfg.game_dir.display(), mode.as_str());
        println!("\n命令行:");
        println!(
            "\"{}\" {}",
            runtime.path.display(),
            plan.to_argv().join(" ")
        );
    }

    if dry_run {
        println!("\n(dry-run，未真正启动)");
        return 0;
    }

    // 解压 natives。必须在启动前完成：缺 dll 会以
    // UnsatisfiedLinkError 的形式在渲染线程里炸掉，报错离根因很远。
    let ctx = RuleContext::current();
    let natives = launch::natives_to_extract(&resolved, &ctx, &cfg);
    if !natives.is_empty() {
        // 落点听计划的：26.2+ 的清单要求 dll 在 `${natives_directory}/java`。
        println!("解压 natives -> {}", plan.natives_dir.display());
        match aicraft_core::extract_natives(&natives, &plan.natives_dir) {
            Ok(report) => {
                if report.extracted > 0 || report.overwritten > 0 {
                    println!(
                        "  解压 {} 个文件（覆盖 {}，跳过 {}）",
                        report.extracted, report.overwritten, report.skipped
                    );
                } else {
                    println!("  已是最新，无需解压");
                }
            }
            Err(e) => {
                eprintln!("natives 解压失败: {}", e);
                return 1;
            }
        }
    }

    println!("正在启动 {} …", version_name);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("启动失败: {}", e);
            return 1;
        }
    };
    match child.wait() {
        Ok(status) => {
            println!("游戏进程退出: {}", status);
            if status.success() {
                0
            } else {
                status.code().unwrap_or(1)
            }
        }
        Err(e) => {
            eprintln!("等待进程失败: {}", e);
            1
        }
    }
}

/// 客户端 ID：写死在 [`aicraft_core::auth::DEFAULT_CLIENT_ID`] 里，不可改。
fn configured_client_id() -> String {
    aicraft_core::auth::DEFAULT_CLIENT_ID.to_string()
}

/// 正版登录：默认设备码（终端里最省事），`--browser` 走浏览器回调。
///
/// 客户端 ID 写死在代码里，不需要也不允许改。
fn cmd_login(args: &[String]) -> i32 {
    use aicraft_core::auth::{self, msa, store};

    let client_id: &str = aicraft_core::auth::DEFAULT_CLIENT_ID;
    let mut browser = false;
    let mut store_path = store::default_path();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--browser" => browser = true,
            "--store" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    store_path = PathBuf::from(p);
                }
            }
            other => {
                eprintln!("未知选项: {}", other);
                return 2;
            }
        }
        i += 1;
    }

    if !aicraft_core::transport::curl_available() {
        eprintln!("找不到可用的 curl，无法联网。");
        return 1;
    }

    let http = auth::CurlHttp;

    let tokens = if browser {
        let login = match msa::BrowserLogin::start(client_id) {
            Ok(login) => login,
            Err(e) => {
                eprintln!("{}", e);
                return 1;
            }
        };
        println!("请在浏览器里完成登录：");
        println!("  {}", login.url());
        if let Err(e) = msa::open_browser(login.url()) {
            println!("（自动打开失败：{}，请手动复制上面的链接）", e);
        }
        match login.wait(&http, client_id, std::time::Duration::from_secs(300)) {
            Ok(tokens) => tokens,
            Err(e) => {
                print_auth_error(&e);
                return 1;
            }
        }
    } else {
        let code = match msa::request_device_code(&http, client_id) {
            Ok(code) => code,
            Err(e) => {
                print_auth_error(&e);
                return 1;
            }
        };
        println!("1. 打开 {}", code.verification_uri);
        println!("2. 输入代码 {}", code.user_code);
        if !code.message.is_empty() {
            println!("   {}", code.message);
        }
        println!("等待你在浏览器里完成授权……（{} 秒内有效）", code.expires_in);
        match msa::wait_for_device_code(&http, client_id, &code) {
            Ok(tokens) => tokens,
            Err(e) => {
                print_auth_error(&e);
                return 1;
            }
        }
    };

    let account = match auth::complete_login(&http, &tokens) {
        Ok(account) => account,
        Err(e) => {
            print_auth_error(&e);
            return 1;
        }
    };

    println!(
        "登录成功：{}（{}）",
        account.username, account.uuid
    );

    let mut accounts = store::AccountStore::load_or_default(&store_path);
    accounts.upsert(store::StoredAccount::from_login(&account, &tokens));
    match accounts.save(&store_path) {
        Ok(()) => println!("账号已保存到 {}", store_path.display()),
        Err(e) => {
            eprintln!("保存账号失败：{}", e);
            return 1;
        }
    }

    0
}

/// 启动前决定用哪个账号。
///
/// 顺序：账号文件里最近用过的正版账号 → 过期就用 refresh token 续期 →
/// 续不了再退回离线（并说明原因）。宁可明确降级，也不要拿过期令牌进服：
/// 那会在服务器端报一句和本启动器毫无关系的「验证失败」，更难排查。
fn resolve_account(fallback_name: &str) -> (Account, Option<String>) {
    use aicraft_core::auth::{self, store};

    let path = store::default_path();
    let mut saved = store::AccountStore::load_or_default(&path);
    let Some(mut stored) = saved.accounts.first().cloned() else {
        return (Account::offline(fallback_name), None);
    };

    if !stored.is_expired(store::now_unix()) {
        return (stored.to_account(), None);
    }

    let client_id = configured_client_id();

    if !aicraft_core::transport::curl_available() {
        return (
            Account::offline(fallback_name),
            Some("找不到可用的 curl，无法续期正版会话；本次用离线模式".to_string()),
        );
    }

    match stored.refresh(&auth::CurlHttp, &client_id) {
        Ok(account) => {
            saved.upsert(stored.clone());
            if let Err(e) = saved.save(&path) {
                return (
                    account.to_account(),
                    Some(format!("续期成功，但保存账号失败：{}", e)),
                );
            }
            (
                account.to_account(),
                Some(format!("正版会话已过期，已自动续期（{}）", account.username)),
            )
        }
        Err(e) => (
            Account::offline(fallback_name),
            Some(format!("自动续期失败：{}；本次用离线模式", e)),
        ),
    }
}

/// 打印登录错误；对常见错误补一句「接下来该做什么」。
fn print_auth_error(error: &aicraft_core::auth::AuthError) {
    use aicraft_core::auth::AuthErrorKind;

    eprintln!("登录失败：{}", error);
    match error.kind {
        AuthErrorKind::ClientNotPublic => {
            eprintln!("去 Azure 应用 → 身份验证，勾上「允许公共客户端流」。");
        }
        AuthErrorKind::NoXboxProfile => {
            eprintln!("先打开 https://www.xbox.com 登录一次，把 Xbox 档案建出来。");
        }
        AuthErrorKind::NoXboxRegion => {
            eprintln!("这需要把微软账号的地区改到支持 Xbox Live 的地方。");
        }
        AuthErrorKind::ChildAccount => {
            eprintln!("让家长在 https://account.microsoft.com/family 里同意。");
        }
        AuthErrorKind::NotEntitled => {
            eprintln!("需要先在 Microsoft Store 或 minecraft.net 买一份 Java 版。");
        }
        AuthErrorKind::AppNotApproved => {
            eprintln!("微软账号、Xbox Live、XSTS 都已经通过了，卡在 Minecraft 服务这一跳。");
            eprintln!("这是 Mojang 对第三方启动器的白名单：新注册的 Azure 应用会被拒绝，");
            eprintln!("只能等审批（https://aka.ms/AppRegInfo）。审批通过前请用离线模式。");
        }
        AuthErrorKind::Network => {
            eprintln!("检查网络；这个启动器走系统 curl，代理设置也要跟着设。");
        }
        _ => {}
    }
    if !error.body.is_empty() {
        let snippet: String = error.body.chars().take(500).collect();
        eprintln!("服务端原始响应: {}", snippet.trim());
    }
}
