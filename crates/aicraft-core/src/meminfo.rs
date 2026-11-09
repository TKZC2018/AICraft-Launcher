//! 本机物理内存。
//!
//! 设置页要画「已用 / 总量」那条进度条、还要按总量推荐一个堆大小，
//! 都得先知道这台机器有多少内存。
//!
//! 为什么不直接调 `GlobalMemoryStatusEx`：core 全局 `unsafe_code = "forbid"`，
//! 而那个 API 只有一个出参指针的形态。所以走 PowerShell 的 CIM 查询——
//! 一次约 0.4 秒，只在进设置页时后台跑一次，然后缓存在进程里。

use std::sync::OnceLock;

/// 一次内存快照，单位 MiB。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    /// 系统可见的物理内存总量。
    pub total_mb: u64,
    /// 当前可用（不是「空闲」，Windows 把 standby 缓存也算可用）。
    pub available_mb: u64,
}

impl Memory {
    /// 已被占用的量。
    pub fn used_mb(&self) -> u64 {
        self.total_mb.saturating_sub(self.available_mb)
    }

    /// 把 MiB 说成人话：1024 以下说「512 MiB」，以上说「4.0 GiB」。
    ///
    /// 内存条的实际容量几乎都是 2 的幂，但可用量不是 —— 所以两位小数，
    /// 不要四舍五入到整数（15.8 GiB 舍成 16 会让人以为装的是 16 GiB）。
    pub fn human(mb: u64) -> String {
        if mb >= 1024 {
            format!("{:.1} GiB", mb as f64 / 1024.0)
        } else {
            format!("{} MiB", mb)
        }
    }
}

/// 按物理内存推荐一个堆大小。
///
/// 取四分之一：给游戏留够，也给系统、浏览器和启动器自己留够。再取到 512
/// 的倍数，读数好看，玩家上下调一点也不会跳得莫名其妙。上限压到 8 GiB ——
/// 再往上 Java 的 GC 停顿反而变长，对这个量级的客户端没好处。
pub fn recommended_mb(total_mb: u64) -> u32 {
    let quarter = total_mb / 4;
    // 先四舍五入到 512，再夹到 [1024, 8192]
    let rounded = quarter.saturating_add(256) / 512 * 512;
    rounded.clamp(1024, 8192) as u32
}

static CACHE: OnceLock<Option<Memory>> = OnceLock::new();

/// 进程内缓存的内存快照。
///
/// 第一次调用会真的起一个 PowerShell（约 0.4 秒），之后都读缓存。
/// 想让它不挡在启动路径上，就用 [`prefetch`] 先把它焐热。
pub fn physical() -> Option<Memory> {
    *CACHE.get_or_init(detect)
}

/// 只看已经拿到手的快照，绝不阻塞。
///
/// 界面每帧都要问一次「内存条画什么」——用 [`physical`] 会在第一次进设置页时
/// 把整帧卡 0.4 秒，那正是之前 Java 扫描踩过的坑。拿不到就先不画，等
/// [`prefetch`] 那头填上、重绘一帧自然就有了。
pub fn cached() -> Option<Memory> {
    *CACHE.get()?
}

/// 起一个后台线程把 [`physical`] 的缓存填上。
///
/// 启动时调一次：真要点「开始游戏」时就不用站在那儿等半秒。
pub fn prefetch() {
    std::thread::spawn(|| {
        let _ = physical();
    });
}

/// 重新读一次（绕过缓存，给设置页想要实时数字时用）。
pub fn detect() -> Option<Memory> {
    let script = "$os = Get-CimInstance Win32_OperatingSystem; \
                  Write-Output \"$($os.TotalVisibleMemorySize) $($os.FreePhysicalMemory)\"";
    let output = crate::spawn::silent("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

/// 解析 `总 可用` 两个数（CIM 给的是 KiB）。
fn parse(text: &str) -> Option<Memory> {
    let mut numbers = text.split_whitespace().filter_map(|w| w.parse::<u64>().ok());
    let total_kb = numbers.next()?;
    let available_kb = numbers.next()?;
    if total_kb == 0 {
        return None;
    }
    Some(Memory {
        total_mb: total_kb / 1024,
        available_mb: (available_kb / 1024).min(total_kb / 1024),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_reads_both_numbers() {
        let memory = parse("16571172 1259256\n").expect("应该能读出来");
        assert_eq!(memory.total_mb, 16182);
        assert_eq!(memory.available_mb, 1229);
        assert_eq!(memory.used_mb(), 14953);
    }

    #[test]
    fn garbage_is_not_a_snapshot() {
        assert!(parse("").is_none());
        assert!(parse("不知道").is_none());
        // 只有一个数也不行：拿不到可用量就没法画那条进度条
        assert!(parse("16571172").is_none());
        assert!(parse("0 0").is_none());
    }

    #[test]
    fn available_never_exceeds_total() {
        // 系统报的可用量偶尔会比总量多一点（缓存回收的误差）
        let memory = parse("1024 2048").unwrap();
        assert_eq!(memory.available_mb, 1);
        assert_eq!(memory.used_mb(), 0);
    }

    #[test]
    fn the_recommendation_is_a_quarter_rounded_to_512() {
        // 16 GiB 的机器：四分之一是 4045，取到 4096
        assert_eq!(recommended_mb(16182), 4096);
        // 8 GiB：2048
        assert_eq!(recommended_mb(8192), 2048);
        // 32 GiB：8192，正好是上限
        assert_eq!(recommended_mb(32768), 8192);
        // 64 GiB：四分之一 16384，被上限压住
        assert_eq!(recommended_mb(65536), 8192);
        // 小机器不许低于 1 GiB —— 低于这个值游戏根本起不来
        assert_eq!(recommended_mb(2048), 1024);
        assert_eq!(recommended_mb(0), 1024);
    }

    #[test]
    fn sizes_are_written_the_way_people_read_them() {
        assert_eq!(Memory::human(512), "512 MiB");
        assert_eq!(Memory::human(1023), "1023 MiB");
        assert_eq!(Memory::human(1024), "1.0 GiB");
        // 15.8 不能舍成 16 —— 那会让人以为装的是 16 GiB 的条子
        assert_eq!(Memory::human(16182), "15.8 GiB");
        assert_eq!(Memory::human(4096), "4.0 GiB");
    }
}
