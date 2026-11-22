//! 让游戏跑在独显上。
//!
//! 双显卡笔记本上，Windows 默认把 `javaw.exe` 派给核显是常有的事 —— 同一份
//! 整合包，别人 120 帧、自己 40 帧，往往就是这一步没做。系统「设置 → 系统 →
//! 屏幕 → 显示卡」里给某个 exe 选「高性能」，落到的就是这个位置：
//!
//! ```text
//! HKCU\Software\Microsoft\DirectX\UserGpuPreferences
//!   C:\Program Files\Java\jdk-21\bin\javaw.exe = GpuPreference=2;
//! ```
//!
//! 这是**当前用户**的一份偏好，不改系统、不动驱动、随时能撤，所以这里直接
//! 读写它，不要求玩家自己去找那个设置页。
//!
//! 为什么不走 `D3D12` 的 `IDXGIFactory6::EnumAdapterByGpuPreference`：那条路
//! 要改游戏的渲染初始化，而 MC 的渲染层是 lwjgl 的，够不着。注册表这条是
//! 唯一对任何 Java 版本、任何加载器都生效的办法。

use std::path::Path;

/// 「高性能」在注册表里的写法。`1` 是省电，`0` 是让 Windows 自己决定。
pub const HIGH_PERFORMANCE: &str = "GpuPreference=2;";

#[cfg(windows)]
const KEY: &str = r"HKCU\Software\Microsoft\DirectX\UserGpuPreferences";

/// 注册表里拿什么当键名：exe 的**最终路径**。
///
/// 要 `canonicalize` 一遍的理由很实际：`C:\ProgramData\Oracle\Java\javapath\
/// javaw.exe` 这类是转发层，Windows 认的是它指向的真身。不解析成真身的话，
/// 我们写进去的那条永远不会被命中，玩家会以为开关坏了。
///
/// 但 Windows 的 `canonicalize` 会带上 `\\?\` 前缀，而系统自己写这个键时
/// 不带 —— 所以解析完得把那四个字符去掉，否则两边写的不是同一个名字。
pub fn key_path(exe: &Path) -> String {
    let resolved = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let text = resolved.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        // 网络盘上的 java 解析出来是 `\\?\UNC\server\share\...`，
        // 系统那边写的是 `\\server\share\...`。
        return format!(r"\\{}", rest);
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None => text,
    }
}

/// 这个可执行文件现在是不是被指定成了「高性能」。
#[cfg(windows)]
pub fn is_high_performance(exe: &Path) -> bool {
    let name = key_path(exe);
    let output = crate::spawn::silent("reg")
        .args(["query", KEY, "/v", &name])
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout).contains(HIGH_PERFORMANCE)
}

/// 把它写成「高性能」。
#[cfg(windows)]
pub fn set_high_performance(exe: &Path) -> Result<(), String> {
    let name = key_path(exe);
    let output = crate::spawn::silent("reg")
        .args([
            "add",
            KEY,
            "/v",
            &name,
            "/t",
            "REG_SZ",
            "/d",
            HIGH_PERFORMANCE,
            "/f",
        ])
        .output()
        .map_err(|e| format!("调用 reg 失败: {}", e))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// 撤掉这条偏好，回到「让 Windows 自己决定」。
///
/// 返回是否真的删掉了：本来就没有这一条时返回 `false`，不算失败 ——
/// 玩家关掉开关时，这里几乎总是「本来就没有」。
#[cfg(windows)]
pub fn clear_preference(exe: &Path) -> Result<bool, String> {
    let name = key_path(exe);
    let output = crate::spawn::silent("reg")
        .args(["delete", KEY, "/v", &name, "/f"])
        .output()
        .map_err(|e| format!("调用 reg 失败: {}", e))?;
    Ok(output.status.success())
}

/// 按设置把这条偏好对齐，返回该写进日志的一句话（没变化时 `None`）。
///
/// 只在真的要变的时候才动注册表：每次启动都 `reg add` 一遍没必要，
/// 而且会让审计日志里多出一堆什么都没改的记录。
pub fn ensure(exe: &Path, wanted: bool) -> Option<String> {
    let current = is_high_performance(exe);
    if wanted == current {
        return None;
    }
    if wanted {
        match set_high_performance(exe) {
            Ok(()) => Some(format!("已把 {} 指定为使用高性能显卡", exe.display())),
            Err(e) => Some(format!("没能指定高性能显卡（{}）；游戏仍可启动，可能跑在核显上", e.trim())),
        }
    } else {
        match clear_preference(exe) {
            Ok(true) => Some(format!("已撤销 {} 的高性能显卡指定", exe.display())),
            Ok(false) => None,
            Err(e) => Some(format!("没能撤销高性能显卡指定：{}", e.trim())),
        }
    }
}

// ---- 非 Windows：这套机制是 Windows 独有的，这里全是空实现 ----

#[cfg(not(windows))]
pub fn is_high_performance(_exe: &Path) -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_high_performance(_exe: &Path) -> Result<(), String> {
    Err("只有 Windows 才有显卡首选项".to_string())
}

#[cfg(not(windows))]
pub fn clear_preference(_exe: &Path) -> Result<bool, String> {
    Ok(false)
}

#[cfg(not(windows))]
pub fn ensure(_exe: &Path, _wanted: bool) -> Option<String> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// 一条谁都不会真去用的路径：拿它当探针，读写都不碰任何真实程序。
    fn probe() -> std::path::PathBuf {
        std::path::PathBuf::from(r"C:\__aicraft_gpu_probe__\javaw.exe")
    }

    #[test]
    fn a_probe_path_is_never_high_performance() {
        assert!(!is_high_performance(&probe()));
    }

    #[test]
    fn clearing_something_that_is_not_there_is_not_a_failure() {
        // 关掉开关时几乎总是这个情况，不该报错吓人。
        assert_eq!(clear_preference(&probe()), Ok(false));
    }

    /// 本机已经有一条系统自己写的偏好时，我们算出来的键名必须和它一模一样。
    ///
    /// 这一条是「玩家在 Windows 设置里已经选过高性能」那种情况的回归测试：
    /// 两边名字差一个字符，我们就会以为没设过，又写一条平行的、永远不生效的。
    #[test]
    fn our_key_matches_what_windows_itself_writes() {
        let exe = std::path::PathBuf::from(r"C:\Program Files\Java\jdk-21\bin\javaw.exe");
        if !exe.is_file() {
            return; // 这台机器上没装那个 JDK，跳过
        }
        assert_eq!(key_path(&exe), r"C:\Program Files\Java\jdk-21\bin\javaw.exe");
    }

    #[test]
    fn the_key_is_a_plain_windows_path() {
        // `\\?\` 前缀必须被去掉：系统自己写这个键时不带它。
        let key = key_path(&probe());
        assert!(!key.starts_with(r"\\?\"), "键名里不该留 UNC 前缀: {}", key);
        assert!(key.ends_with("javaw.exe"));
    }

    #[test]
    fn nothing_changes_when_the_preference_already_matches() {
        // 没装过这条偏好的探针 = 「不想要」；不想要 + 现在也没有 → 什么都不做。
        assert!(ensure(&probe(), false).is_none());
    }

    /// 真写一遍、读一遍、再撤掉。
    ///
    /// 用一条谁都不会去执行的假路径当探针，所以除了注册表里多一条几毫秒后
    /// 就被删掉的字符串，不碰任何真实程序。这条测试是唯一能证明「开关真的
    /// 会落到系统里」的东西 —— 只测 `key_path` 的话，`reg` 参数写错了也发现不了。
    #[test]
    fn the_preference_survives_a_write_and_comes_back_off() {
        let exe = probe();
        // 上一次跑挂了可能留了条尾巴，先清干净再开始。
        let _ = clear_preference(&exe);
        assert!(!is_high_performance(&exe));

        set_high_performance(&exe).expect("写注册表失败");
        assert!(is_high_performance(&exe), "写完读回来不是高性能");

        assert_eq!(clear_preference(&exe), Ok(true), "撤掉一条存在的偏好该返回 true");
        assert!(!is_high_performance(&exe), "撤掉之后还读得到");
    }
}
