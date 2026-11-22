//! 清理不再被任何实例引用的旧库。
//!
//! 换加载器时（Forge / NeoForge / Fabric 互相换），新清单只会列出自己那一套库，
//! 旧加载器留下的 jar 会一直躺在 `libraries/` 里 —— 一次换版几十上百兆，
//! 换几次就是好几百兆，而从界面上完全看不出来。
//!
//! 做法是「问所有实例要什么，多出来的删掉」：
//!
//! 1. 逐个读 `versions/*/[同名].json`，解析继承链并合并；
//! 2. 收集这些实例在**当前平台**上会用到的库文件相对路径；
//! 3. 遍历 `libraries/`，把不在集合里的 `.jar` 删掉。
//!
//! 两条保守规则：
//!
//! - 只要有**任何一个**实例的清单读不出来（比如父版本被删了），整次清理放弃。
//!   宁可留着旧库，也不能因为一份坏清单把别人的库删了。
//! - 只删 `.jar`。`.sha1`、`_remote.repositories` 这类元数据不碰 —— 它们不占
//!   地方，删掉却可能让别的启动器以为文件坏了。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::rules::RuleContext;
use crate::version::Library;

/// 一次清理的结果。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PruneReport {
    /// 删掉的文件数。
    pub removed: usize,
    /// 删掉的字节数。
    pub bytes: u64,
    /// 留下来的 jar 数（含删不掉的那些）。
    pub kept: usize,
}

/// 删掉 `libraries/` 里不再被任何实例引用的 `.jar`。
///
/// 没有 `libraries/` 目录时返回空报告；单个文件删不掉（游戏正开着）只记在
/// `kept` 里，不算错 —— 下次换加载器再说。
pub fn prune_libraries(game_dir: &Path, ctx: &RuleContext) -> Result<PruneReport, String> {
    // 没有实例、或者实例们一个库都没声明时，`libraries/` 里的一切都「没人用」——
    // 但那更像「清单还没装好」而不是「可以删了」。宁可什么都不做。
    if crate::install::installed_instances(game_dir).is_empty() {
        return Ok(PruneReport::default());
    }
    let needed = needed_libraries(game_dir, ctx)?;
    if needed.is_empty() {
        return Ok(PruneReport::default());
    }
    let root = game_dir.join("libraries");
    if !root.is_dir() {
        return Ok(PruneReport::default());
    }

    let mut files = Vec::new();
    collect_jars(&root, &mut files);

    let mut report = PruneReport::default();
    for file in files {
        let Ok(rel) = file.strip_prefix(&root) else {
            continue;
        };
        let key = rel.to_string_lossy().replace('\\', "/");
        if needed.contains(&key) {
            report.kept += 1;
            continue;
        }
        let bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(&file).is_ok() {
            report.removed += 1;
            report.bytes += bytes;
        } else {
            report.kept += 1;
        }
    }
    Ok(report)
}

/// 所有实例合起来会用到的库，返回 `libraries/` 下的相对路径（用 `/` 分隔）。
pub fn needed_libraries(game_dir: &Path, ctx: &RuleContext) -> Result<HashSet<String>, String> {
    let mut needed = HashSet::new();
    for instance in crate::install::installed_instances(game_dir) {
        let chain = crate::version::load_chain_from_dir(game_dir, &instance)
            .map_err(|e| format!("读不了 {} 的清单：{}", instance, e.message))?;
        let resolved = crate::resolve::resolve(&chain)
            .map_err(|e| format!("合并不了 {} 的清单：{}", instance, e.message))?;
        for lib in resolved.effective_libraries(ctx) {
            for rel in library_paths(lib) {
                needed.insert(rel);
            }
        }
    }
    Ok(needed)
}

/// 一个库可能占用的所有仓库相对路径。
///
/// 故意取「全部可能」而不是「当前平台实际要的那一个」：老版本（≤1.12）的条目
/// 同时带主构件与 natives 分类符，两个文件都要留在盘上，漏掉任何一个都会让
/// 启动时报 `NoClassDefFoundError`。多留几个不占额外空间，少留就是坏档。
fn library_paths(lib: &Library) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    if let Some(dl) = &lib.artifact {
        paths.push(dl.path.clone().unwrap_or_default());
    }
    for (_, dl) in &lib.classifiers {
        paths.push(dl.path.clone().unwrap_or_default());
    }
    if let Some(coord) = &lib.coord {
        paths.push(coord.to_path());
    }
    paths
        .into_iter()
        .map(|p| p.replace('\\', "/"))
        .filter(|p| !p.is_empty())
        .collect()
}

fn collect_jars(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            collect_jars(&path, out);
        } else if is_jar(&path) {
            out.push(path);
        }
    }
}

fn is_jar(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext.eq_ignore_ascii_case("jar"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Arch, Features, OsInfo, OsName};

    fn ctx() -> RuleContext {
        RuleContext::new(
            OsInfo::for_test(OsName::Windows, Arch::X86_64),
            Features::default(),
        )
    }

    fn write_instance(game_dir: &Path, id: &str, libs: &str) {
        let dir = game_dir.join("versions").join(id);
        std::fs::create_dir_all(&dir).expect("应能建实例目录");
        let json = format!(r#"{{"id":"{}","mainClass":"M","libraries":[{}]}}"#, id, libs);
        std::fs::write(dir.join(format!("{}.json", id)), json).expect("应能写清单");
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("有父目录")).expect("应能建目录");
        std::fs::write(path, b"x").expect("应能写文件");
    }

    const ASM: &str = r#"{"name":"org.ow2.asm:asm:9.8","downloads":{"artifact":{"path":"org/ow2/asm/asm/9.8/asm-9.8.jar","url":"https://x/a.jar"}}}"#;

    #[test]
    fn a_library_nobody_uses_is_removed() {
        let dir = crate::testutil::fresh_dir("prune-unused");
        write_instance(&dir, "demo", ASM);

        let keep = dir.join("libraries/org/ow2/asm/asm/9.8/asm-9.8.jar");
        let stale = dir.join("libraries/net/fabricmc/fabric-loader/0.14.0/fabric-loader-0.14.0.jar");
        touch(&keep);
        touch(&stale);

        let report = prune_libraries(&dir, &ctx()).expect("应能清理");
        assert_eq!(report.removed, 1);
        assert!(keep.is_file(), "实例要用的库不能删");
        assert!(!stale.exists(), "没人用的旧库该被删掉");
    }

    #[test]
    fn a_library_another_instance_uses_is_kept() {
        let dir = crate::testutil::fresh_dir("prune-shared");
        write_instance(&dir, "one", ASM);
        write_instance(&dir, "two", ASM);
        let shared = dir.join("libraries/org/ow2/asm/asm/9.8/asm-9.8.jar");
        touch(&shared);

        let report = prune_libraries(&dir, &ctx()).expect("应能清理");
        assert_eq!(report.removed, 0);
        assert!(shared.is_file());
    }

    #[test]
    fn a_broken_instance_stops_the_whole_prune() {
        let dir = crate::testutil::fresh_dir("prune-broken");
        write_instance(&dir, "demo", ASM);
        // 父版本不在盘上：这条链读不出来，整次清理必须放弃
        let broken = dir.join("versions/broken");
        std::fs::create_dir_all(&broken).expect("应能建目录");
        std::fs::write(
            broken.join("broken.json"),
            r#"{"id":"broken","inheritsFrom":"missing","mainClass":"M","libraries":[]}"#,
        )
        .expect("应能写清单");

        let stale = dir.join("libraries/net/fabricmc/fabric-loader/0.14.0/fabric-loader-0.14.0.jar");
        touch(&stale);

        assert!(prune_libraries(&dir, &ctx()).is_err());
        assert!(stale.is_file(), "清单坏了就不该动任何文件");
    }

    #[test]
    fn nothing_is_deleted_when_no_instance_declares_any_library() {
        let dir = crate::testutil::fresh_dir("prune-empty");
        write_instance(&dir, "demo", "");
        let orphan = dir.join("libraries/net/fabricmc/fabric-loader/0.14.0/fabric-loader-0.14.0.jar");
        touch(&orphan);

        let report = prune_libraries(&dir, &ctx()).expect("应能清理");
        assert_eq!(report.removed, 0, "没参照物时不该删东西");
        assert!(orphan.is_file());
    }

    #[test]
    fn a_game_dir_without_instances_is_untouched() {
        let dir = crate::testutil::fresh_dir("prune-noinstance");
        let orphan = dir.join("libraries/net/fabricmc/fabric-loader/0.14.0/fabric-loader-0.14.0.jar");
        touch(&orphan);

        let report = prune_libraries(&dir, &ctx()).expect("应能清理");
        assert_eq!(report.removed, 0);
        assert!(orphan.is_file());
    }

    #[test]
    fn metadata_files_are_left_alone() {
        let dir = crate::testutil::fresh_dir("prune-meta");
        write_instance(&dir, "demo", ASM);
        let note = dir.join("libraries/org/ow2/asm/asm/9.8/asm-9.8.jar.sha1");
        touch(&note);

        prune_libraries(&dir, &ctx()).expect("应能清理");
        assert!(note.is_file(), "只该删 jar，元数据不碰");
    }

    #[test]
    fn an_old_style_natives_jar_stays() {
        let dir = crate::testutil::fresh_dir("prune-natives");
        write_instance(
            &dir,
            "demo",
            r#"{"name":"org.lwjgl:lwjgl:2.9.4","natives":{"windows":"natives-windows"},
                "downloads":{"artifact":{"path":"org/lwjgl/lwjgl/2.9.4/lwjgl-2.9.4.jar","url":"https://x/m.jar"},
                "classifiers":{"natives-windows":{"path":"org/lwjgl/lwjgl/2.9.4/lwjgl-2.9.4-natives-windows.jar","url":"https://x/n.jar"}}}}"#,
        );
        let main = dir.join("libraries/org/lwjgl/lwjgl/2.9.4/lwjgl-2.9.4.jar");
        let natives = dir.join("libraries/org/lwjgl/lwjgl/2.9.4/lwjgl-2.9.4-natives-windows.jar");
        touch(&main);
        touch(&natives);

        let report = prune_libraries(&dir, &ctx()).expect("应能清理");
        assert_eq!(report.removed, 0);
        assert!(main.is_file());
        assert!(natives.is_file(), "natives 分类符的 jar 也要留");
    }
}
