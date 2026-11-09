//! 启动子进程的小工具。
//!
//! 启动器是 `windows_subsystem = "windows"` 的 GUI 程序，本身没有控制台；
//! 但 `curl`、`java`、`powershell` 都是控制台程序，`Command::new` 直接起它们
//! 时 Windows 会给子进程新建一个控制台窗口，于是屏幕上就闪一下黑框。
//!
//! 统一走 `silent()` 带上 `CREATE_NO_WINDOW`，把那个窗口从源头掐掉。

use std::process::Command;

/// 不创建控制台窗口（`CREATE_NO_WINDOW`）。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 建一个不会弹控制台的命令。
///
/// 非 Windows 平台就是普通的 `Command::new`，行为不变。
#[allow(unused_mut)]
pub fn silent(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
