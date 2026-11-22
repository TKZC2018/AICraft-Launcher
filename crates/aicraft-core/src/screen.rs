//! 屏幕可用区域。
//!
//! 「最大化窗口」这个选项没有对应的游戏参数 —— Minecraft 只认
//! --width/--height 和 --fullscreen。所以要让窗口铺满桌面，只能先量出
//! 屏幕的工作区（去掉任务栏的那块），再把这两个数当窗口尺寸传过去。
//!
//! 为什么不直接调 GetSystemMetrics / SystemParametersInfo：core 全局
//! unsafe_code = "forbid"，而那些 API 都要传指针。所以走 PowerShell 的
//! WinForms 屏幕对象 —— 一次约 0.3 秒，启动时后台跑一次就缓存住。

use std::sync::OnceLock;

/// 屏幕工作区尺寸（像素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkArea {
    /// 可用宽度。
    pub width: u32,
    /// 可用高度（已扣掉任务栏）。
    pub height: u32,
}

static CACHE: OnceLock<Option<WorkArea>> = OnceLock::new();

/// 只看已经量到的结果，绝不阻塞。
///
/// 界面每帧都可能问一次「最大化该开多大」——用它；第一次还没量完就先按
/// 默认值给，等 prefetch 那头填上再重绘。
pub fn cached() -> Option<WorkArea> {
    *CACHE.get()?
}

/// 真的去量一次（约 0.3 秒），结果写进缓存。
pub fn detect() -> Option<WorkArea> {
    let script = "Add-Type -AssemblyName System.Windows.Forms; \
                  $a = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea; \
                  Write-Output \"$($a.Width) $($a.Height)\"";
    let output = crate::spawn::silent("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

/// 起一个后台线程把缓存填上，别挡在启动路径上。
pub fn prefetch() {
    std::thread::spawn(|| {
        let _ = physical();
    });
}

/// 量一次并写进缓存（首次调用真的会起 PowerShell）。
pub fn physical() -> Option<WorkArea> {
    *CACHE.get_or_init(detect)
}

/// 解析「宽 高」两个数。
fn parse(text: &str) -> Option<WorkArea> {
    let mut numbers = text.split_whitespace().filter_map(|w| w.parse::<u32>().ok());
    let width = numbers.next()?;
    let height = numbers.next()?;
    // 太小说明量歪了（比如桌面还没起来），不如不给
    if width < 640 || height < 480 {
        return None;
    }
    Some(WorkArea { width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_area_reads_both_numbers() {
        let area = parse("1920 1032\n").expect("应该能读出来");
        assert_eq!(area.width, 1920);
        assert_eq!(area.height, 1032);
    }

    #[test]
    fn garbage_is_not_an_area() {
        assert!(parse("").is_none());
        assert!(parse("不知道").is_none());
        assert!(parse("1920").is_none());
    }

    #[test]
    fn an_absurdly_small_area_is_rejected() {
        // 桌面没起来的时候会回 0 或者一个很小的数，这种尺寸传过去游戏会崩
        assert!(parse("0 0").is_none());
        assert!(parse("1024 1").is_none());
    }
}
