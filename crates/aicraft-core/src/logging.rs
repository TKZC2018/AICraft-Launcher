//! 分级日志。
//!
//! 启动器里散落在各处的记录（下载、安装、启动、账号、界面动作）都汇到这里：
//! 一条环形缓冲给界面实时看，一份可选文件给事后排查。两个地方共用同一套
//! [`Level`]，**记录时不设阈值**：五个等级一律进环形缓冲和文件，界面只决定「看哪一档」。
//!
//! 设计上只有全局一个 sink，用 [`init`] 设置等级与文件，之后任何线程都能直接
//! `logging::info("…")`，加锁范围只有 push 本身，不会拖慢下载线程。

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 环形缓冲最多留多少条。按 200 字一条算大约 800 KB，够翻一整个安装过程。
pub const CAPACITY: usize = 4_000;

/// 日志文件超过这个大小就轮转一份 `.old`，避免无限长。
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// 日志等级，数值越小越严重。
///
/// 作为「记录等级」时表示阈值：设成 `Info` 就记录 Error/Warn/Info，Debug 与 Trace 丢掉。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Level {
    /// 从最严重到最啰嗦，界面按这个顺序排。
    pub const ALL: [Level; 5] = [
        Level::Error,
        Level::Warn,
        Level::Info,
        Level::Debug,
        Level::Trace,
    ];

    /// 存进设置文件用的短名字。
    pub fn id(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }

    /// 导出文件里对齐用的五字母标签。
    pub fn label(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }

    /// 界面上的中文名。
    pub fn text(self) -> &'static str {
        match self {
            Level::Error => "错误",
            Level::Warn => "警告",
            Level::Info => "信息",
            Level::Debug => "调试",
            Level::Trace => "详细",
        }
    }

    pub fn parse(value: &str) -> Option<Level> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" | "err" => Some(Level::Error),
            "warn" | "warning" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            "trace" => Some(Level::Trace),
            _ => None,
        }
    }
}

/// 环形缓冲里的一条记录。`seq` 单调递增，用来判断有没有新行。
#[derive(Clone, Debug)]
pub struct Entry {
    pub seq: u64,
    pub level: Level,
    pub at: u64,
    pub message: String,
}

struct Sink {
    next_seq: AtomicU64,
    entries: Mutex<VecDeque<Entry>>,
    file: Mutex<Option<File>>,
}

fn sink() -> &'static Sink {
    static SINK: OnceLock<Sink> = OnceLock::new();
    SINK.get_or_init(|| Sink {
        next_seq: AtomicU64::new(1),
        entries: Mutex::new(VecDeque::with_capacity(256)),
        file: Mutex::new(None),
    })
}

/// 当前 Unix 秒。
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 把 Unix 秒按某个时区偏移格式化成 `2026-10-27 23:38:00`。
///
/// 偏移以秒为单位、东为正（UTC+8 → `28800`）。显示和写文件都用它：
/// 日志是给人看的，标着 `Z` 的 UTC 时间要读者自己心算一遍时区，
/// 排查问题时第一件事反而变成怀疑时间戳。
pub fn format_with_offset(secs: u64, offset_secs: i64) -> String {
    let (year, month, day, hour, minute, second) = parts_with_offset(secs, offset_secs);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// 用本机的时区偏移格式化（见 [`local_offset_secs`]）。
pub fn format_local(secs: u64) -> String {
    format_with_offset(secs, local_offset_secs())
}

/// 由 Unix 秒推出某个时区偏移下的年月日时分秒。
pub fn parts_with_offset(secs: u64, offset_secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    civil(secs as i64 + offset_secs)
}

/// 本机时区相对 UTC 的偏移（秒，东为正），第一次问的时候探测一次。
///
/// 没有标准库接口能拿这个，而 core 又禁止 `unsafe`（调 `GetTimeZoneInformation`
/// 要 FFI），所以读注册表：`ActiveTimeBias` 是「UTC = 本地 + bias」的分钟数，
/// 东八区是 `0xFFFFFE20`（-480）。实测这一趟 50ms，且只跑一次。
/// 读不到就退回 PowerShell，再不行当 UTC——时间戳差几小时，总好过整个启动失败。
pub fn local_offset_secs() -> i64 {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(detect_local_offset)
}

fn detect_local_offset() -> i64 {
    if let Some(minutes) = active_bias_minutes() {
        return -i64::from(minutes) * 60;
    }
    if let Some(secs) = powershell_offset_secs() {
        return secs;
    }
    0
}

/// 从注册表读当前生效的 bias（分钟，东为正的相反数）。
fn active_bias_minutes() -> Option<i32> {
    const KEY: &str = r"HKLM\SYSTEM\CurrentControlSet\Control\TimeZoneInformation";
    let out = crate::spawn::silent("reg")
        .args(["query", KEY, "/v", "ActiveTimeBias"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("ActiveTimeBias"))?;
    let raw = line.split_whitespace().last()?.trim_start_matches("0x");
    let value = u32::from_str_radix(raw, 16).ok()?;
    Some(value as i32)
}

/// 兜底：让 PowerShell 算一次本机偏移（秒）。
fn powershell_offset_secs() -> Option<i64> {
    let out = crate::spawn::silent("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            "[int][System.TimeZoneInfo]::Local.GetUtcOffset([DateTime]::Now).TotalSeconds",
        ])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day, rem / 3_600, (rem % 3_600) / 60, rem % 60)
}

/// 初始化日志：必要时打开日志文件（父目录不存在就建）。
///
/// 没有记录阈值：所有等级都会进内存和文件，界面只负责挑选显示哪一档。
/// 重复调用是安全的，文件只在第一次设置时打开。
pub fn init(file: Option<PathBuf>) {
    let sink = sink();
    let Some(path) = file else {
        return;
    };

    // 注意顺序：先只读地看一眼要不要开文件，确认需要之后才真正开。
    // 不能在**持有** `sink.file` 锁的时候调 `log()`：它内部还要再锁同一把锁，
    // 而 std 的 Mutex 不可重入，会当场把自己锁死（日志文件打不开时必然触发，
    // 表现就是窗口建好了却永远不显示，进程只剩一个后台壳）。
    if lock(&sink.file).is_some() {
        return;
    }

    match open_log_file(&path) {
        Some(file) => {
            let mut slot = lock(&sink.file);
            if slot.is_none() {
                *slot = Some(file);
            }
        }
        // 这里已经没有锁在手上了，可以安全地记一条警告
        None => log(
            Level::Warn,
            format!("打不开日志文件 {}，文件日志已关闭", path.display()),
        ),
    }
}

/// 记一条日志。任何等级都会记下来（内存与文件各一份）。
pub fn log(level: Level, message: impl AsRef<str>) {
    let sink = sink();
    let message = message.as_ref();
    let at = now();
    let seq = sink.next_seq.fetch_add(1, Ordering::Relaxed);

    {
        let mut entries = lock(&sink.entries);
        if entries.len() >= CAPACITY {
            entries.pop_front();
        }
        entries.push_back(Entry {
            seq,
            level,
            at,
            message: message.to_string(),
        });
    }

    let mut slot = lock(&sink.file);
    if let Some(file) = slot.as_mut() {
        let _ = writeln!(file, "{}", file_line(at, level, message));
        let _ = file.flush();
    }
}

pub fn error(message: impl AsRef<str>) {
    log(Level::Error, message);
}

pub fn warn(message: impl AsRef<str>) {
    log(Level::Warn, message);
}

pub fn info(message: impl AsRef<str>) {
    log(Level::Info, message);
}

pub fn debug(message: impl AsRef<str>) {
    log(Level::Debug, message);
}

pub fn trace(message: impl AsRef<str>) {
    log(Level::Trace, message);
}

/// 取一份快照。`min` 只用于调用方自己收窄范围（`None` = 全部都取）。
pub fn entries(min: Option<Level>) -> Vec<Entry> {
    let sink = sink();
    let entries = lock(&sink.entries);
    entries
        .iter()
        .filter(|entry| match min {
            None => true,
            Some(min) => entry.level <= min,
        })
        .cloned()
        .collect()
}

/// 环形缓冲里现在有多少条（所有等级都在里面）。
pub fn total() -> usize {
    lock(&sink().entries).len()
}

/// 清空缓冲。文件已经写出去的收不回来，这是有意的。
pub fn clear() {
    lock(&sink().entries).clear();
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// 日志文件里一行长什么样：`2026-10-27 23:35:00 INFO  消息`（本地时间）。
fn file_line(at: u64, level: Level, message: &str) -> String {
    format!("{} {:<5} {}", format_local(at), level.label(), message)
}

fn open_log_file(path: &Path) -> Option<File> {
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return None;
        }
    }
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    /// 全局 sink 是共享的，测试之间必须串行，否则计数互相污染。
    fn guard() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear();
        guard
    }

    #[test]
    fn level_parsing_and_order() {
        assert_eq!(Level::parse("WARN"), Some(Level::Warn));
        assert_eq!(Level::parse("trace"), Some(Level::Trace));
        assert_eq!(Level::parse("nope"), None);
        assert!(Level::Error < Level::Info);
        assert!(Level::Trace > Level::Debug);
        for level in Level::ALL {
            assert_eq!(Level::parse(level.id()), Some(level));
        }
    }

    #[test]
    fn every_level_is_recorded() {
        let _guard = guard();
        debug("调试也要记");
        trace("详细也要记");
        info("信息");
        error("错误");
        let all = entries(None);
        assert_eq!(all.len(), 4, "没有阈值，四个等级都该进缓冲");
        assert_eq!(all[0].level, Level::Debug);
        assert_eq!(all[1].level, Level::Trace);
        assert_eq!(all[3].level, Level::Error);
    }

    #[test]
    fn display_filter_keeps_severity_order() {
        let _guard = guard();
        info("i");
        warn("w");
        error("e");
        let only_errors = entries(Some(Level::Error));
        assert_eq!(only_errors.len(), 1);
        assert_eq!(only_errors[0].message, "e");
        let warn_and_up = entries(Some(Level::Warn));
        assert_eq!(warn_and_up.len(), 2);
    }

    #[test]
    fn ring_buffer_is_capped() {
        let _guard = guard();
        for i in 0..(CAPACITY + 50) {
            info(format!("line {i}"));
        }
        let all = entries(None);
        assert_eq!(all.len(), CAPACITY);
        assert_eq!(all[0].message, "line 50");
        assert_eq!(all[CAPACITY - 1].message, format!("line {}", CAPACITY + 49));
    }

    #[test]
    fn sequence_numbers_are_monotonic() {
        let _guard = guard();
        info("a");
        info("b");
        let all = entries(None);
        assert!(all[1].seq > all[0].seq);
    }

    #[test]
    fn formatting_honours_the_time_zone_offset() {
        // 东八区：UTC 23:35 就是第二天早上 07:35
        assert_eq!(
            format_with_offset(1_793_144_100, 8 * 3600),
            "2026-10-28 07:35:00"
        );
        // UTC 本身
        assert_eq!(format_with_offset(1_793_144_100, 0), "2026-10-27 23:35:00");
        // 西五区跨回前一天
        assert_eq!(
            format_with_offset(1_793_144_100, -5 * 3600),
            "2026-10-27 18:35:00"
        );
        // 偏移为负、且时间早于 1970 时不该 panic（div_euclid 不是截断除法）
        assert_eq!(format_with_offset(0, -3600), "1969-12-31 23:00:00");
    }

    #[test]
    fn file_lines_carry_level_and_time() {
        assert_eq!(
            file_line(1_793_144_100, Level::Warn, "地铁没赶上车"),
            format!("{} WARN  地铁没赶上车", format_local(1_793_144_100))
        );
        // 时间戳跟着本机时区走，测试里不写死某一天的字符串
        assert_eq!(
            file_line(0, Level::Error, "第一条"),
            format!("{} ERROR 第一条", format_local(0))
        );
    }

    #[test]
    fn open_log_file_creates_parent_and_appends() {
        let dir = crate::testutil::fresh_dir("logging-file");
        let path = dir.join("nested").join("launcher.log");
        {
            let mut file = open_log_file(&path).expect("应能创建日志文件");
            file.write_all(b"first\n").expect("写入第一行");
        }
        {
            let mut file = open_log_file(&path).expect("应能再次打开追加");
            file.write_all(b"second\n").expect("追加第二行");
        }
        let text = std::fs::read_to_string(&path).expect("日志文件应存在");
        assert_eq!(text, "first\nsecond\n");
    }

    /// 回归：日志文件打不开时 `init` 不能把自己锁死。
    ///
    /// 老实现是「持着 `sink.file` 锁 → 打开失败 → 调 `log()`」，
    /// 而 `log()` 要再锁同一把不可重入的 Mutex，于是永久阻塞。
    /// 启动器里这段跑在创建界面的线程上，症状就是进程活着、窗口永不出现。
    /// 这里用带超时的线程去调，真锁死了是测试失败而不是整个测试进程挂住。
    #[test]
    fn init_returns_even_when_log_file_cannot_be_opened() {
        let _guard = guard();
        let dir = crate::testutil::fresh_dir("logging-badfile");
        // 让父路径是个普通文件：`create_dir_all` 必然失败
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").expect("应先能写下占位文件");
        let path = blocker.join("launcher.log");

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            init(Some(path));
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(3)).is_ok(),
            "init 没能返回：八成又被 sink.file 锁住了"
        );

        // 打不开文件时应当留下一条警告，说明文件日志已关闭
        let warned = entries(None)
            .iter()
            .any(|entry| entry.level == Level::Warn && entry.message.contains("文件日志已关闭"));
        assert!(warned, "应当记一条「文件日志已关闭」的警告");
    }
}
