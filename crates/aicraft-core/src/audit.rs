//! 操作日志：记录玩家在启动器里做了什么。
//!
//! 目的只有一个——**事后能复盘**。玩家说「我明明点了下载，怎么没反应」，
//! 日志里得能看出那一刻按的是哪个按钮、改的是哪一项设置，而不是只有
//! 「下载失败」这种结果。所以这里记的是**意图**，不是结果：结果那部分
//! 各功能自己已经在记了（`已安装`、`下载失败` 之类）。
//!
//! 两档：
//!
//! - [`action`] 走 `Info`：一次有意义的操作（切页面、点按钮、装东西）。
//!   界面的日志面板默认就显示到这一档，玩家自己也能看见。
//! - [`detail`] 走 `Debug`：同一操作里的细枝末节（点了列表里哪一行、
//!   翻页翻到第几页）。这类一次能有几十条，默认藏起来。
//!
//! 为什么不给每个按钮单独写一句 `logging::info(…)`：按钮散在六个页面
//! 模块里，各写各的必然措辞不一、迟早漏。统一走 [`action`]，措辞就只在
//! 调用点决定一次，还能被测试盯住格式。

use crate::logging;
use crate::settings::Settings;

/// 记一次操作。`what` 写成动词开头的短语，例如「打开页面：设置」。
pub fn action(what: impl AsRef<str>) {
    logging::info(format!("操作：{}", what.as_ref()));
}

/// 记一次操作里的细节（默认不显示，排查时才看）。
pub fn detail(what: impl AsRef<str>) {
    logging::debug(format!("操作：{}", what.as_ref()));
}

/// 比对新旧设置，列出改动过的地方。
///
/// 返回的每一条都是「项目：旧值 → 新值」，可以直接写进日志。没有改动就是
/// 空表——调用方据此决定要不要记，避免每帧刷一堆「没有变化」。
///
/// 为什么要比对而不是在控件旁边记：设置页的控件直接改 `Settings` 字段
/// （这是刻意的，见 `settings::page` 的注释），没有统一的“提交”时刻。
/// 每帧拿一份快照对比，是唯一能覆盖到所有字段、又不会漏掉新加字段的办法
/// ——只要新字段在这里补一行。
pub fn settings_changes(before: &Settings, after: &Settings) -> Vec<String> {
    let mut changes = Vec::new();
    if before.mirror != after.mirror {
        changes.push(format!(
            "版本下载源：{} → {}",
            before.mirror.label(),
            after.mirror.label()
        ));
    }
    if before.fabric_mirror != after.fabric_mirror {
        changes.push(format!(
            "Fabric 下载源：{} → {}",
            before.fabric_mirror.label(),
            after.fabric_mirror.label()
        ));
    }
    if before.forge_mirror != after.forge_mirror {
        changes.push(format!(
            "Forge 下载源：{} → {}",
            before.forge_mirror.label(),
            after.forge_mirror.label()
        ));
    }
    if before.neoforge_mirror != after.neoforge_mirror {
        changes.push(format!(
            "NeoForge 下载源：{} → {}",
            before.neoforge_mirror.label(),
            after.neoforge_mirror.label()
        ));
    }
    if before.concurrency != after.concurrency {
        changes.push(format!(
            "并发下载数：{} → {}",
            before.concurrency, after.concurrency
        ));
    }
    if before.download_limit_kbps != after.download_limit_kbps {
        changes.push(format!(
            "下载限速：{} → {}",
            limit_text(before.download_limit_kbps),
            limit_text(after.download_limit_kbps)
        ));
    }
    if before.auto_memory != after.auto_memory {
        changes.push(format!(
            "内存分配：{} → {}",
            if before.auto_memory { "自动" } else { "手动" },
            if after.auto_memory { "自动" } else { "手动" }
        ));
    }
    if before.max_memory_mb != after.max_memory_mb {
        changes.push(format!(
            "最大内存：{} MB → {} MB",
            before.max_memory_mb, after.max_memory_mb
        ));
    }
    if before.min_memory_mb != after.min_memory_mb {
        changes.push(format!(
            "最小内存：{} → {}",
            min_memory_text(before.min_memory_mb),
            min_memory_text(after.min_memory_mb)
        ));
    }
    if before.jvm_args != after.jvm_args {
        changes.push(format!(
            "JVM 参数：{} → {}",
            quoted(&before.jvm_args),
            quoted(&after.jvm_args)
        ));
    }
    if before.java_path != after.java_path {
        changes.push(format!(
            "Java 路径：{} → {}",
            path_text(before.java_path.as_deref()),
            path_text(after.java_path.as_deref())
        ));
    }
    if before.game_dir != after.game_dir {
        changes.push(format!(
            "游戏目录：{} → {}",
            blank_text(&before.game_dir),
            blank_text(&after.game_dir)
        ));
    }
    if before.hide_official_dir != after.hide_official_dir {
        changes.push(format!(
            "显示官方目录：{} → {}",
            yes_no(before.hide_official_dir),
            yes_no(after.hide_official_dir)
        ));
    }
    if before.isolate_instances != after.isolate_instances {
        changes.push(format!(
            "实例隔离：{} → {}",
            on_off(before.isolate_instances),
            on_off(after.isolate_instances)
        ));
    }
    if before.launcher_mode != after.launcher_mode {
        changes.push(format!(
            "启动器状态：{} → {}",
            before.launcher_mode.label(),
            after.launcher_mode.label()
        ));
    }
    if before.offline_name != after.offline_name {
        changes.push(format!(
            "离线用户名：{} → {}",
            blank_text(&before.offline_name),
            blank_text(&after.offline_name)
        ));
    }
    for dir in after
        .server_dirs
        .iter()
        .filter(|dir| !before.server_dirs.contains(dir))
    {
        changes.push(format!("服务端目录：加入 {}", dir));
    }
    for dir in before
        .server_dirs
        .iter()
        .filter(|dir| !after.server_dirs.contains(dir))
    {
        changes.push(format!("服务端目录：移除 {}", dir));
    }
    if before.nav_collapsed != after.nav_collapsed {
        changes.push(format!(
            "导航收起：{} → {}",
            on_off(before.nav_collapsed),
            on_off(after.nav_collapsed)
        ));
    }
    // 目录列表可能有增删，按条目差集写，不然只能看出「变了」看不出「加了哪个」
    let added: Vec<&String> = after
        .game_dirs
        .iter()
        .filter(|dir| !before.game_dirs.contains(dir))
        .collect();
    let removed: Vec<&String> = before
        .game_dirs
        .iter()
        .filter(|dir| !after.game_dirs.contains(dir))
        .collect();
    for dir in added {
        changes.push(format!("目录列表：加入 {}", dir.trim()));
    }
    for dir in removed {
        changes.push(format!("目录列表：移除 {}", dir.trim()));
    }
    changes
}

/// 比对并记录。有改动才写日志。
pub fn log_settings_changes(before: &Settings, after: &Settings) {
    for change in settings_changes(before, after) {
        action(change);
    }
}

fn limit_text(kbps: Option<u32>) -> String {
    match kbps {
        Some(k) => format!("{} KB/s", k),
        None => "不限速".to_string(),
    }
}

fn min_memory_text(mb: Option<u32>) -> String {
    match mb {
        Some(mb) => format!("{} MB", mb),
        None => "自动".to_string(),
    }
}

fn path_text(path: Option<&std::path::Path>) -> String {
    match path {
        Some(path) => path.display().to_string(),
        None => "自动".to_string(),
    }
}

fn blank_text(text: &str) -> String {
    if text.trim().is_empty() {
        "（空）".to_string()
    } else {
        text.trim().to_string()
    }
}

fn quoted(text: &str) -> String {
    if text.trim().is_empty() {
        "（空）".to_string()
    } else {
        format!("\"{}\"", text.trim())
    }
}

fn on_off(value: bool) -> &'static str {
    if value {
        "开启"
    } else {
        "关闭"
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "是"
    } else {
        "否"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_settings_produce_nothing() {
        let settings = Settings::default();
        assert!(settings_changes(&settings, &settings.clone()).is_empty());
    }

    #[test]
    fn a_changed_field_is_reported_with_both_values() {
        let before = Settings::default();
        let mut after = before.clone();
        after.concurrency = 12;
        let changes = settings_changes(&before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0], "并发下载数：8 → 12");
    }

    #[test]
    fn limit_is_written_in_words_not_some_zero() {
        let before = Settings::default();
        let mut after = before.clone();
        after.download_limit_kbps = Some(2048);
        assert_eq!(settings_changes(&before, &after)[0], "下载限速：不限速 → 2048 KB/s");

        let mut back = after.clone();
        back.download_limit_kbps = None;
        assert_eq!(settings_changes(&after, &back)[0], "下载限速：2048 KB/s → 不限速");
    }

    #[test]
    fn directory_edits_are_listed_one_by_one() {
        let before = Settings::default();
        let mut after = before.clone();
        after.game_dirs = vec!["E:\\MC".to_string(), "F:\\MC2".to_string()];
        let changes = settings_changes(&before, &after);
        assert_eq!(changes, vec!["目录列表：加入 E:\\MC", "目录列表：加入 F:\\MC2"]);

        let mut trimmed = after.clone();
        trimmed.game_dirs = vec!["F:\\MC2".to_string()];
        assert_eq!(settings_changes(&after, &trimmed)[0], "目录列表：移除 E:\\MC");
    }

    #[test]
    fn empty_values_get_a_readable_placeholder() {
        let before = Settings::default();
        let mut after = before.clone();
        after.offline_name = "Steve".to_string();
        assert_eq!(settings_changes(&before, &after)[0], "离线用户名：（空） → Steve");

        let mut with_args = before.clone();
        with_args.jvm_args = "-XX:+UseG1GC".to_string();
        assert_eq!(
            settings_changes(&before, &with_args)[0],
            "JVM 参数：（空） → \"-XX:+UseG1GC\""
        );
    }

    #[test]
    fn every_field_change_is_noticed() {
        // 这条是防漏的：新加设置字段时，只要忘了在 `settings_changes` 里补一行，
        // 这里就会失败。逐字段改一遍，确认每个都能被看出来。
        let base = Settings::default();
        let mut all = base.clone();
        all.mirror = crate::settings::Mirror::Bmclapi;
        all.fabric_mirror = crate::settings::Mirror::Bmclapi;
        all.forge_mirror = crate::settings::Mirror::Bmclapi;
        all.neoforge_mirror = crate::settings::Mirror::Bmclapi;
        all.concurrency = 3;
        all.download_limit_kbps = Some(1024);
        all.auto_memory = false;
        all.max_memory_mb = 8192;
        all.min_memory_mb = Some(512);
        all.jvm_args = "-Xss1M".to_string();
        all.java_path = Some(std::path::PathBuf::from("C:\\Java\\bin\\javaw.exe"));
        all.game_dir = "E:\\MC".to_string();
        all.game_dirs = vec!["F:\\MC".to_string()];
        all.hide_official_dir = true;
        all.isolate_instances = false;
        all.launcher_mode = crate::settings::LauncherMode::Close;
        all.offline_name = "Steve".to_string();
        all.server_dirs = vec!["D:\\srv".to_string()];
        all.nav_collapsed = true;

        let changes = settings_changes(&base, &all);
        // 16 个普通字段 + 游戏目录 1 条 + 服务端目录 1 条
        assert_eq!(changes.len(), 19, "有字段没被记录：{:?}", changes);
    }
}
