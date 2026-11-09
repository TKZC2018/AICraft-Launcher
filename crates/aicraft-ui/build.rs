// AICraft Launcher 的图标资源。
//
// 只有一枚图标和一份版本信息，所以不引第三方 crate（比如 embed-resource），
// 直接调 Windows SDK 的 rc.exe 把 .rc 编成 .res，再交给链接器。
// 找不到 rc.exe 时只警告、不中断构建：没有图标的 exe 仍然能用，
// 比因为缺一个 SDK 工具就编不过去要好。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=aicraft.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // 交叉编译到别的平台（或者宿主不是 Windows）时没有资源这一说，直接跳过。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let Some(rc) = find_rc() else {
        println!("cargo:warning=找不到 rc.exe（Windows SDK），本次构建不带 exe 图标与版本信息");
        return;
    };

    let out = match std::env::var_os("OUT_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => return,
    };
    let res = out.join("aicraft.res");
    let rc_file = Path::new("aicraft.rc");

    let output = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(rc_file)
        .output();

    match output {
        Ok(o) if o.status.success() && res.is_file() => {
            println!("cargo:rustc-link-arg={}", res.display());
        }
        Ok(o) => {
            // rc.exe 的诊断有的走 stdout、有的走 stderr，两边都看一下，
            // 否则构建告警会是一条没有内容的「rc.exe 失败」。
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            let text = text.trim().replace('\n', " ");
            let text = if text.is_empty() {
                "退出码非 0，但没有输出".to_string()
            } else {
                text
            };
            println!("cargo:warning=rc.exe 失败，本次构建不带图标：{text}");
        }
        Err(e) => println!("cargo:warning=无法运行 rc.exe：{e}"),
    }
}

/// 依次尝试：环境变量 `RC`、Windows SDK 里版本号最高的一份 x64/arm64 rc.exe。
fn find_rc() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RC").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }

    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86" => "x86",
        _ => "x64",
    };

    // 10.0.x 都放在 bin\<version>\<arch>\ 下，版本号按字符串排序即可选到最新。
    let roots = [
        std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
        std::env::var_os("ProgramFiles").map(PathBuf::from),
    ];
    for root in roots.into_iter().flatten() {
        let bin = root.join("Windows Kits").join("10").join("bin");
        let Ok(entries) = std::fs::read_dir(&bin) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        versions.sort();
        for version in versions.into_iter().rev() {
            let candidate = version.join(arch).join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
