//! Java 运行时探测。
//!
//! 版本清单里 `javaVersion.majorVersion` 表示该版本需要的 Java 主版本。
//! 本机同时装了 JDK 8 / 21 / 25，而 1.20.1 要 17、1.12.2 要 8 ——
//! 选错 Java 会直接启动失败，且报错往往很晦涩（`UnsupportedClassVersionError`
//! 或更糟的 `NoClassDefFoundError`）。
//!
//! 探测策略（Windows）：
//! 1. 显式指定的路径（用户在启动器里手选的）。
//! 2. `JAVA_HOME`。
//! 3. PATH 里的 java。
//! 4. 常见安装目录下的 jdk/jre 子目录。
//!
//! 结果带缓存：扫目录很慢，而 Java 安装位置几乎不变。

use std::path::{Path, PathBuf};

/// 一个可用的 Java 运行时。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaRuntime {
    /// java/javaw 可执行文件路径。
    pub path: PathBuf,
    /// 主版本号，如 8 / 17 / 21 / 25。
    pub major_version: u32,
    /// 完整版本字符串。
    pub version_string: String,
    /// 是否为 64 位。
    pub is_64bit: bool,
    /// 来源描述，便于在 UI 里展示。
    pub source: String,
}

impl JavaRuntime {
    /// 该运行时能否满足指定主版本要求。
    ///
    /// 判定用「至少」语义：Java 21 可以跑要求 17 的版本。
    /// 唯一的例外是 8 与更高版本不互通，但 Minecraft 的版本要求本身
    /// 就精确到这一层，用 >= 即可。
    pub fn satisfies(&self, required_major: u32) -> bool {
        self.major_version >= required_major
    }

    /// 该运行时是否为 `javapath` 这类符号链接路径。
    ///
    /// Oracle 的 `javapath` 是安装器建的转发链接，随安装/卸载/升级而变动，
    /// 不是稳定的 JDK 实体位置。优先选真实路径可以让「下次启动仍用同一个
    /// Java」这件事更可靠。
    pub fn is_redirector_path(&self) -> bool {
        let lower = self.path.to_string_lossy().to_ascii_lowercase();
        lower.contains("\\javapath\\") || lower.contains("/javapath/")
    }

    pub fn home(&self) -> PathBuf {
        self.path
            .parent()
            .and_then(|bin| bin.parent())
            .map(|home| home.to_path_buf())
            .unwrap_or_else(|| self.path.clone())
    }

    /// 一份给人看的标签：「Java 21 · C:\Program Files\Java\jdk-21」。
    ///
    /// 同一份安装里 `java.exe` 和 `javaw.exe` 各算一个候选，路径不同但
    /// 其实是同一个 JDK，所以展示和去重都按安装目录说话。
    pub fn label(&self) -> String {
        format!("Java {} · {}", self.major_version, self.home().display())
    }
}

/// Java 探测错误。
#[derive(Debug, Clone)]
pub struct JavaError {
    pub message: String,
}

impl std::fmt::Display for JavaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JavaError {}

/// 从 java 可执行文件读取版本信息。
pub fn probe_java(java_path: &Path) -> Option<JavaRuntime> {
    let output = crate::spawn::silent(java_path).arg("-version").output().ok()?;
    // java -version 把版本信息写到 stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = if stderr.contains("version") {
        stderr.into_owned()
    } else {
        stdout.into_owned()
    };

    let (major, full) = parse_version_output(&text)?;
    Some(JavaRuntime {
        path: java_path.to_path_buf(),
        major_version: major,
        version_string: full,
        is_64bit: text.contains("64-Bit"),
        source: String::new(),
    })
}

/// 解析 `java -version` 输出，返回 (主版本号, 完整版本串)。
///
/// 两种写法：
///   `java version "1.8.0_451"`  -> 8
///   `openjdk version "17.0.2" 2022-01-18` -> 17
pub fn parse_version_output(text: &str) -> Option<(u32, String)> {
    let start = text.find("version \"")? + "version \"".len();
    let rest = &text[start..];
    let end = rest.find('"')?;
    let full = &rest[..end];

    let mut parts = full.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    // 1.x.y 的老写法
    if first == 1 {
        let second: u32 = parts.next()?.parse().ok()?;
        Some((second, full.to_string()))
    } else {
        Some((first, full.to_string()))
    }
}

/// 候选 Java 的搜索位置。
pub fn candidate_paths() -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = Vec::new();

    // JAVA_HOME
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let home = PathBuf::from(home);
        for exe in ["javaw.exe", "java.exe"] {
            let p = home.join("bin").join(exe);
            if p.is_file() {
                out.push((p, "JAVA_HOME".to_string()));
            }
        }
    }

    // PATH 里的 java
    for exe in ["javaw.exe", "java.exe"] {
        if let Some(p) = which(exe) {
            out.push((p, "PATH".to_string()));
        }
    }

    // 常见安装目录
    let mut roots: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for base in [
            r"C:\Program Files\Java",
            r"C:\Program Files\Eclipse Adoptium",
            r"C:\Program Files\Microsoft",
            r"C:\Program Files (x86)\Java",
        ] {
            roots.push(PathBuf::from(base));
        }
        // 用户级 SDK 目录
        if let Some(home) = std::env::var_os("USERPROFILE") {
            roots.push(PathBuf::from(&home).join(".jdks"));
            roots.push(PathBuf::from(&home).join("scoop").join("apps"));
        }
    } else if cfg!(target_os = "macos") {
        roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
        roots.push(PathBuf::from("/usr/libexec/java_home"));
    } else {
        roots.push(PathBuf::from("/usr/lib/jvm"));
    }

    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            for exe in ["javaw.exe", "java.exe"] {
                let p = dir.join("bin").join(exe);
                if p.is_file() {
                    out.push((p, format!("扫描: {}", root.display())));
                }
            }
            // macOS 的 Contents/Home 结构
            let p = dir.join("Contents/Home/bin/java");
            if p.is_file() {
                out.push((p, "macOS bundle".to_string()));
            }
        }
    }

    out
}

/// 在 PATH 里查找可执行文件。
fn which(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 探测本机所有 Java，按安装目录去重后、按版本降序返回。
///
/// 同一份 JDK 里 `java.exe` 和 `javaw.exe` 都在 `bin/` 下，路径不同但其实是
/// 同一个东西；`javapath` 那种转发链接还会再指一遍。只按可执行文件路径去重
/// 会在设置页里列出三行一模一样的「Java 25」，所以这里按**安装目录**合并：
/// 同一目录只留一条，优先留真实路径（不是 `javapath`）、优先留 `javaw.exe`。
pub fn detect_all() -> Vec<JavaRuntime> {
    let mut found: Vec<JavaRuntime> = Vec::new();

    for (path, source) in candidate_paths() {
        let mut rt = match probe_java(&path) {
            Some(rt) => rt,
            None => continue,
        };
        rt.source = source;

        // 同一份 Java 会以多种面貌出现：`javaw.exe` 与 `java.exe` 只差文件名；
        // `javapath\javaw.exe` 是转发链接，canonicalize 之后可能指向真实 JDK，
        // 也可能只是同版本的另一处安装。所以按「主版本 + 完整版本串」合并，
        // 比路径可靠得多——路径会被链接、盘符大小写、短名 8.3 搅乱。
        let key = (rt.major_version, rt.version_string.clone());
        if let Some(slot) = found
            .iter()
            .position(|k: &JavaRuntime| (k.major_version, k.version_string.clone()) == key)
        {
            // 已有的那条如果是转发链接，就让真实路径顶替它。
            let better = found[slot].is_redirector_path() && !rt.is_redirector_path();
            if better {
                found[slot] = rt;
            }
            continue;
        }
        found.push(rt);
    }

    found.sort_by_key(|rt| std::cmp::Reverse(rt.major_version));
    found
}

/// 挑选 Java 的结果，附带匹配程度，供上层决定是否提示用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavaChoice {
    /// 主版本号精确匹配。
    Exact,
    /// 用更高版本顶替。Minecraft 的字节码是向后兼容的，
    /// 用 Java 21 跑 Java 17 的目标字节码是可行的；但仍值得提示一次，
    /// 因为少数 mod 会受模块系统/反射限制变化影响。
    Fallback {
        required: u32,
        actual: u32,
    },
}

impl JavaChoice {
    /// 是否需要在下次启动前提示用户。
    pub fn needs_notice(&self) -> bool {
        matches!(self, JavaChoice::Fallback { .. })
    }

    /// 给用户看的一句说明。精确匹配时返回 None。
    pub fn notice(&self) -> Option<String> {
        match self {
            JavaChoice::Exact => None,
            JavaChoice::Fallback { required, actual } => Some(format!(
                "该版本声明需要 Java {}，本机未找到，将使用 Java {} 代替。\n\
                 Minecraft 的字节码向后兼容，通常可以正常启动；\n\
                 若遇到启动失败或模组异常，再单独安装 Java {} 会更稳妥。",
                required, actual, required
            )),
        }
    }
}

/// 挑选满足要求的最佳 Java，并说明匹配程度。
///
/// 策略：
/// 1. 主版本号精确匹配优先（最稳）。
/// 2. 否则取「满足要求的最小版本」顶替——不用 Java 25 去跑要求 17 的版本，
///    版本差越大，模块系统与 mod 兼容性的风险越高。
/// 3. 同版本多个候选时，避开 `javapath` 这类转发链接，优先真实路径。
pub fn pick_java_choice(
    runtimes: &[JavaRuntime],
    required_major: u32,
) -> Option<(&JavaRuntime, JavaChoice)> {
    // 排序辅助：真实路径优先于 javapath 转发链接
    let prefer = |a: &&JavaRuntime, b: &&JavaRuntime| {
        a.is_redirector_path().cmp(&b.is_redirector_path())
    };

    if let Some(exact) = runtimes
        .iter()
        .filter(|r| r.major_version == required_major)
        .min_by(prefer)
    {
        return Some((exact, JavaChoice::Exact));
    }

    let fallback = runtimes
        .iter()
        .filter(|r| r.satisfies(required_major))
        .min_by(|a, b| {
            a.major_version
                .cmp(&b.major_version)
                .then_with(|| prefer(a, b))
        })?;

    Some((
        fallback,
        JavaChoice::Fallback {
            required: required_major,
            actual: fallback.major_version,
        },
    ))
}

/// 挑选满足要求的最佳 Java（只要结果，不要匹配信息）。
pub fn pick_java(runtimes: &[JavaRuntime], required_major: u32) -> Option<&JavaRuntime> {
    pick_java_choice(runtimes, required_major).map(|(rt, _)| rt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_version_string() {
        let (major, full) = parse_version_output("java version \"1.8.0_451\"").unwrap();
        assert_eq!(major, 8);
        assert_eq!(full, "1.8.0_451");
    }

    #[test]
    fn parses_modern_version_string() {
        let text = "openjdk version \"17.0.2\" 2022-01-18\nOpenJDK Runtime Environment";
        let (major, full) = parse_version_output(text).unwrap();
        assert_eq!(major, 17);
        assert_eq!(full, "17.0.2");
    }

    #[test]
    fn parses_java21_and_25() {
        assert_eq!(
            parse_version_output("openjdk version \"21.0.1\" 2023-10-17").unwrap().0,
            21
        );
        assert_eq!(
            parse_version_output("java version \"25.0.4\" 2026-07-21 LTS").unwrap().0,
            25
        );
    }

    #[test]
    fn detects_64bit() {
        let text = "java version \"17.0.2\" 2022-01-18\nJava HotSpot(TM) 64-Bit Server VM";
        // parse_version_output 只处理版本，位数判断在 probe_java 里
        assert!(text.contains("64-Bit"));
        assert_eq!(parse_version_output(text).unwrap().0, 17);
    }

    fn rt(major: u32) -> JavaRuntime {
        JavaRuntime {
            path: PathBuf::from(format!("java{}.exe", major)),
            major_version: major,
            version_string: format!("{}.0", major),
            is_64bit: true,
            source: "test".into(),
        }
    }

    #[test]
    fn satisfies_uses_at_least_semantics() {
        assert!(rt(21).satisfies(17));
        assert!(rt(17).satisfies(17));
        assert!(!rt(8).satisfies(17));
    }

    #[test]
    fn pick_prefers_exact_match() {
        let list = vec![rt(25), rt(21), rt(17), rt(8)];
        assert_eq!(pick_java(&list, 17).unwrap().major_version, 17);
        assert_eq!(pick_java(&list, 21).unwrap().major_version, 21);
    }

    #[test]
    fn pick_falls_back_to_minimum_satisfying() {
        let list = vec![rt(25), rt(21)];
        // 要 17，没有精确匹配，选 21 而不是 25
        assert_eq!(pick_java(&list, 17).unwrap().major_version, 21);
    }

    #[test]
    fn pick_returns_none_when_too_old() {
        let list = vec![rt(8)];
        assert!(pick_java(&list, 17).is_none());
    }

    #[test]
    fn exact_match_reports_no_notice() {
        let list = vec![rt(25), rt(21), rt(17), rt(8)];
        let (r, choice) = pick_java_choice(&list, 17).unwrap();
        assert_eq!(r.major_version, 17);
        assert_eq!(choice, JavaChoice::Exact);
        assert!(!choice.needs_notice());
        assert!(choice.notice().is_none());
    }

    #[test]
    fn fallback_reports_notice() {
        let list = vec![rt(25), rt(21)];
        let (r, choice) = pick_java_choice(&list, 17).unwrap();
        assert_eq!(r.major_version, 21, "应选满足要求的最小版本");
        assert_eq!(
            choice,
            JavaChoice::Fallback {
                required: 17,
                actual: 21
            }
        );
        assert!(choice.needs_notice());
        let notice = choice.notice().unwrap();
        assert!(notice.contains("Java 17"));
        assert!(notice.contains("Java 21"));
    }

    /// 本机真实情况：有 8/21/25，缺 17。
    #[test]
    fn real_machine_scenario() {
        let list = vec![rt(25), rt(21), rt(8)];
        // 1.12.2 要 8
        assert_eq!(pick_java_choice(&list, 8).unwrap().1, JavaChoice::Exact);
        // 1.20.1 要 17 -> 用 21 顶替，需提示
        let (r, c) = pick_java_choice(&list, 17).unwrap();
        assert_eq!(r.major_version, 21);
        assert!(c.needs_notice());
        // 1.21 要 21
        assert_eq!(pick_java_choice(&list, 21).unwrap().1, JavaChoice::Exact);
        // 26.x 要 25
        assert_eq!(pick_java_choice(&list, 25).unwrap().1, JavaChoice::Exact);
    }

    #[test]
    fn prefers_real_path_over_javapath() {
        let mut redirector = rt(25);
        redirector.path = PathBuf::from(
            r"C:\PROGRAM FILES\COMMON FILES\ORACLE\JAVA\JAVAPATH\javaw.exe",
        );
        assert!(redirector.is_redirector_path());

        let mut real = rt(25);
        real.path = PathBuf::from(r"C:\Program Files\Java\jdk-25.0.4\bin\javaw.exe");
        assert!(!real.is_redirector_path());

        // 顺序里 javapath 在前，但应选中真实路径
        let list = vec![redirector, real.clone()];
        let (r, _) = pick_java_choice(&list, 25).unwrap();
        assert_eq!(r.path, real.path);
    }
}
