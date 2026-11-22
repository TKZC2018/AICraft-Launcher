//! 把文字放进系统剪贴板。
//!
//! Windows 自带一个 `clip.exe`（`C:\Windows\System32`，57 KB），从标准输入读文本
//! 写进剪贴板。用它就是零依赖：不必为了「点一下复制」去引 `arboard`——那会让
//! `cargo build --offline` 因为本地缓存里没有那个 crate 而直接失败。
//!
//! `egui` 自带的 `Context::copy_text` 不能替代它：没开 `clipboard` 特性时，
//! `egui-winit` 只把文字放进进程自己的一个变量里，粘贴到启动器外面是空的。
//!
//! 两条路，按内容挑：
//!
//! - 纯 ASCII 走 `clip.exe`，几十毫秒。
//! - 含非 ASCII 走 `powershell Set-Clipboard`，约 0.7 秒。
//!
//! 为什么不能都用 `clip.exe`：它按**控制台代码页**解释 stdin 的字节。UTF-8 的
//! 中文按 CP936 读会错位（实测「中文剪贴板测试-昔涟」变成「中文剪贴板测�?昔涟」，
//! 一个字被吃掉）。设备码是 `K7QX8M2P` 这种纯 ASCII，本来只走快路，
//! 但入口既然叫 `copy(text)`，别人拿它复制中文时不该悄悄写坏。

/// 把 `text` 放进系统剪贴板。
///
/// 失败时返回一句能直接显示给玩家的话——调用方不该因为剪贴板坏了就中断流程，
/// 设备码还印在界面上，手抄也行。
pub fn copy(text: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        if text.is_ascii() {
            return copy_with_clip(text);
        }
        copy_with_powershell(text)
    }

    #[cfg(not(windows))]
    {
        let _ = text;
        Err("这个系统上还没实现写剪贴板，麻烦手动选一下吧".to_string())
    }
}

/// 快路：`clip.exe` 从 stdin 读。只用于纯 ASCII。
#[cfg(windows)]
fn copy_with_clip(text: &str) -> Result<(), String> {
    use std::io::Write;

    let mut child = crate::spawn::silent("clip.exe")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("起不来系统剪贴板工具：{e}"))?;

    // 先把自己的 stdin 关掉（`take()` 之后 drop），clip.exe 才会认为输入结束、
    // 把攒下的内容真正写进剪贴板。不关的话它会一直等。
    let wrote = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(text.as_bytes()),
        None => Ok(()),
    };
    wrote.map_err(|e| format!("写剪贴板失败：{e}"))?;

    let status = child.wait().map_err(|e| format!("剪贴板工具没跑完：{e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("剪贴板工具返回了 {status}"))
    }
}

/// 标准 Base64（带 `+ / =`）。
///
/// 复用手写的 `codec::base64url_nopad`（它是 URL 安全字母表且去掉了填充），
/// 再把字母表换回来补齐 `=`——PowerShell 的 `-EncodedCommand` 与 .NET 的
/// `[Convert]::FromBase64String` 都只认标准 Base64。
#[cfg(windows)]
fn base64(data: &[u8]) -> String {
    let mut out: String = crate::codec::base64url_nopad(data)
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            other => other,
        })
        .collect();
    while out.len() % 4 != 0 {
        out.push('=');
    }
    out
}

/// 慢路：`powershell -EncodedCommand`，脚本本身以 UTF-16LE + Base64 传进去，
/// 所以引号、中文、`$` 都不会被 shell 层吃掉。
#[cfg(windows)]
fn copy_with_powershell(text: &str) -> Result<(), String> {
    // 先把要复制的文字编成 UTF-8 再 Base64，脚本里解码——
    // 直接把文字拼进脚本文本会遇到转义与代码页两重麻烦。
    let payload = base64(text.as_bytes());
    let script = format!(
        "$t=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}')); Set-Clipboard -Value $t",
        payload
    );

    // -EncodedCommand 要的是 UTF-16LE 的 Base64。
    let utf16: Vec<u8> = script
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    let encoded = base64(&utf16);

    let output = crate::spawn::silent("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
        .output()
        .map_err(|e| format!("起不来 PowerShell：{e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        if detail.is_empty() {
            Err(format!("写剪贴板失败（{}）", output.status))
        } else {
            Err(format!("写剪贴板失败：{detail}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读回系统剪贴板当前内容。
    ///
    /// 必须先把 `[Console]::OutputEncoding` 改成 UTF-8：PowerShell 从管道输出时
    /// 会按控制台代码页（中文机器是 CP936）编码，`String::from_utf8_lossy` 读到的
    /// 就是一堆问号——那是**读**的问题，不是写坏了。
    fn read_clipboard() -> String {
        let output = crate::spawn::silent("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-Clipboard -Raw",
            ])
            .output()
            .expect("读剪贴板");
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    /// 纯 ASCII（设备码就是这种）走 `clip.exe` 快路，要能原样往返。
    ///
    /// 标 `ignore`：会覆盖用户当前的剪贴板内容，不该在 `cargo test` 里随手跑掉。
    /// 需要时手动 `cargo test --lib clipboard -- --ignored --test-threads=1`。
    #[test]
    #[ignore]
    fn ascii_takes_the_fast_path_and_survives() {
        let token = format!("AICRAFT-{}", std::process::id());
        copy(&token).expect("写剪贴板");
        let got = read_clipboard();
        assert!(got.contains(&token), "剪贴板里应该是 {token:?}，实际读到 {got:?}");
    }

    /// 非 ASCII 走 PowerShell 慢路。这一条是上一条的对照组：
    /// 早期只走 `clip.exe` 时它会把一个汉字读坏。
    #[test]
    #[ignore]
    fn chinese_takes_the_slow_path_and_survives() {
        let token = "中文剪贴板测试-昔涟";
        copy(token).expect("写剪贴板");
        let got = read_clipboard();
        assert!(got.contains(token), "实际读到 {got:?}");
    }

    /// 引号、`$`、反斜杠这些不该在 PowerShell 那层被解释掉。
    #[test]
    #[ignore]
    fn quotes_and_dollars_are_not_interpreted() {
        let token = r#"中文 'single' "double" $notAVariable `tick`"#;
        copy(token).expect("写剪贴板");
        let got = read_clipboard();
        assert!(got.contains(token), "实际读到 {got:?}");
    }
}
