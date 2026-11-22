//! 编译时注入 Azure 应用的客户端 ID。
//!
//! 这个 ID 不进源码：它跟着「这一次构建」走，谁打包谁提供。查找顺序是
//!
//! 1. 环境变量 `AICRAFT_CLIENT_ID`
//! 2. 仓库根目录的 `.client-id` 文件（已在 `.gitignore` 里）
//!
//! 两条都没有时注入空串，编译照常通过 —— 只是正版登录会直接报「这个构建
//! 没有带客户端 ID」，而不是发出一个必然失败的请求。这样源码仓库里不存在
//! 任何可用的 ID，任何人都能用自己注册的应用构建一份。

use std::path::PathBuf;

/// 从一段文本里取出 ID：去掉首尾空白，空串当没写。
fn clean(text: &str) -> String {
    text.trim().to_string()
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // ID 从环境变量来的时候，改了要重编，不然会拿到上一次注入的值。
    println!("cargo:rerun-if-env-changed=AICRAFT_CLIENT_ID");

    let root = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .map(|dir| dir.join("..").join(".."))
        .unwrap_or_default();
    let file = root.join(".client-id");
    println!("cargo:rerun-if-changed={}", file.display());

    let from_env = std::env::var("AICRAFT_CLIENT_ID")
        .map(|value| clean(&value))
        .unwrap_or_default();
    let from_file = std::fs::read_to_string(&file)
        .map(|text| clean(&text))
        .unwrap_or_default();

    let id = if from_env.is_empty() { from_file } else { from_env };
    if id.is_empty() {
        println!(
            "cargo:warning=没有提供客户端 ID：设环境变量 AICRAFT_CLIENT_ID，\
             或者在仓库根写一个 .client-id 文件。正版登录会提示缺少 ID。"
        );
    }

    println!("cargo:rustc-env=AICRAFT_CLIENT_ID={}", id);
}
