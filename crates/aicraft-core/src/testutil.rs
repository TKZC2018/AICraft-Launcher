//! 测试辅助：创建可用的临时目录，并在离开作用域时自动删掉。
//!
//! 直接用 `std::env::temp_dir().join("固定名字")` 在受限环境里会踩两个坑：
//!
//! 1. **并发冲突**：cargo 默认并行跑测试，多个用例抢同一个目录会互相删文件。
//! 2. **权限**：某些沙箱对 `%TEMP%` 下的固定路径有写入限制，表现为
//!    `create_dir_all` 返回 `PermissionDenied`，看起来像代码 bug，实则环境使然。
//!
//! 所以改用「每次唯一 + 允许回退到当前目录」的方式。
//!
//! 另外，回退到当前目录时目录就落在仓库里了 —— 以前靠每个用例手写
//! `testutil::cleanup(&tmp)` 收拾，只要有人忘了写（实际上忘了一大半），
//! 仓库根目录下就会不断堆 `aicraft-test-*`。现在 `fresh_dir` 返回的
//! [`TempDir`] 在 Drop 时会自己删，写不写 `cleanup` 都不会漏。

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// 一个临时目录，Drop 时递归删除。
///
/// 通过 `Deref<Target = Path>` 和 `AsRef<Path>` 提供 `&Path` 接口，
/// 所以既能 `dir.join("x")`，也能直接传给收 `&Path` 的函数。
pub struct TempDir {
    path: PathBuf,
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 创建一个全新的空临时目录；离开作用域时自动删除。
///
/// 优先 `%TEMP%`，失败则退回当前工作目录——后者通常是可写的。
pub fn fresh_dir(tag: &str) -> TempDir {
    let unique = format!(
        "aicraft-test-{}-{}-{}",
        tag,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );

    for base in [
        std::env::temp_dir(),
        std::env::current_dir().unwrap_or_default(),
    ] {
        let dir = base.join(&unique);
        if std::fs::create_dir_all(&dir).is_ok() {
            return TempDir { path: dir };
        }
    }

    // 两处都失败时仍然返回一个路径，让调用方自己报错。
    TempDir {
        path: std::env::temp_dir().join(&unique),
    }
}

/// 提前清理（Drop 时还会再试一次，重复调用无害）。
pub fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_removes_itself_on_drop() {
        let path;
        {
            let dir = fresh_dir("selfclean");
            path = dir.to_path_buf();
            std::fs::write(dir.join("x.txt"), b"x").expect("应能写入");
            assert!(path.is_dir());
        }
        assert!(!path.exists(), "离开作用域后目录该被删掉");
    }

    #[test]
    fn temp_dir_behaves_like_a_path() {
        let dir = fresh_dir("aspath");
        std::fs::write(dir.join("a.txt"), b"a").expect("应能写入");
        // &TempDir 能直接当 &Path 传给函数
        assert!(dir.join("a.txt").is_file());
        cleanup(&dir);
        assert!(!Path::new(&*dir).exists());
    }
}
