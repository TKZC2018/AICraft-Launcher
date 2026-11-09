//! `rules` 规则求值。
//!
//! 版本清单里 `libraries[].rules` 与 `arguments.*[].rules` 都用同一套语义：
//!
//! - 没有 `rules` 字段 → 一律生效。
//! - 有 `rules` 字段 → 从「不生效」开始，按**书写顺序**逐条判定，
//!   命中的规则用它的 `action` 覆盖当前结果，**后命中者胜出**。
//!
//! 顺序敏感这点很容易被写错：`1.12.2` 的旧式 natives 条目就是
//! `[{allow}, {disallow, os:{name:osx}}]`，若改成「任一 allow 即通过」，
//! 在 macOS 上会错误地保留该库。

use std::fmt;

/// 规则涉及的平台名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsName {
    Windows,
    Osx,
    Linux,
}

impl OsName {
    /// 当前编译/运行平台。未知平台归入 Linux（Minecraft 只有这三类）。
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            OsName::Windows
        } else if cfg!(target_os = "macos") {
            OsName::Osx
        } else {
            OsName::Linux
        }
    }

    /// 与清单中 `os.name` 取值一致的字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            OsName::Windows => "windows",
            OsName::Osx => "osx",
            OsName::Linux => "linux",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "windows" => Some(OsName::Windows),
            "osx" | "macos" => Some(OsName::Osx),
            "linux" => Some(OsName::Linux),
            _ => None,
        }
    }
}

impl fmt::Display for OsName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 架构。清单里写成 `x86` / `x86_64` / `arm64`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86,
    X86_64,
    Arm64,
}

impl Arch {
    pub fn current() -> Self {
        #[cfg(target_arch = "x86")]
        {
            Arch::X86
        }
        #[cfg(target_arch = "x86_64")]
        {
            Arch::X86_64
        }
        #[cfg(target_arch = "aarch64")]
        {
            Arch::Arm64
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Arch::X86_64
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Arch::X86 => "x86",
            Arch::X86_64 => "x86_64",
            Arch::Arm64 => "arm64",
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 当前平台的描述，可在测试里替换成任意平台。
#[derive(Debug, Clone)]
pub struct OsInfo {
    pub name: OsName,
    /// 系统版本号字符串，仅少量清单会用到；留空表示不做版本判定。
    pub version: String,
    pub arch: Arch,
}

impl OsInfo {
    /// 探测当前平台。
    pub fn detect() -> Self {
        OsInfo {
            name: OsName::current(),
            version: detect_os_version(),
            arch: Arch::current(),
        }
    }

    /// 构造一个指定平台的描述，供测试使用。
    pub fn for_test(name: OsName, arch: Arch) -> Self {
        OsInfo {
            name,
            version: String::new(),
            arch,
        }
    }
}

impl Default for OsInfo {
    fn default() -> Self {
        Self::detect()
    }
}

/// 启动器支持的特性开关，对应清单里的 `features` 判定。
#[derive(Debug, Clone, Default)]
pub struct Features {
    pub has_custom_resolution: bool,
    pub is_demo_user: bool,
    pub has_quick_plays_support: bool,
}

impl Features {
    /// 按名称取特性值。未知特性返回 None（该规则即不匹配）。
    pub fn get(&self, key: &str) -> Option<bool> {
        match key {
            "has_custom_resolution" => Some(self.has_custom_resolution),
            "is_demo_user" => Some(self.is_demo_user),
            "has_quick_plays_support" => Some(self.has_quick_plays_support),
            _ => None,
        }
    }
}

/// 求值上下文。
#[derive(Debug, Clone, Default)]
pub struct RuleContext {
    pub os: OsInfo,
    pub features: Features,
}

impl RuleContext {
    pub fn new(os: OsInfo, features: Features) -> Self {
        RuleContext { os, features }
    }

    /// 使用当前平台与默认特性。
    pub fn current() -> Self {
        RuleContext {
            os: OsInfo::detect(),
            features: Features::default(),
        }
    }
}

/// 规则动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Allow,
    Disallow,
}

/// `os` 子条件。
#[derive(Debug, Clone, Default)]
pub struct OsRule {
    pub name: Option<String>,
    pub version: Option<String>,
    pub arch: Option<String>,
}

impl OsRule {
    fn matches(&self, os: &OsInfo) -> bool {
        if let Some(name) = &self.name {
            match OsName::parse(name) {
                Some(n) if n == os.name => {}
                _ => return false,
            }
        }
        if let Some(arch) = &self.arch {
            if !arch.eq_ignore_ascii_case(os.arch.as_str()) {
                return false;
            }
        }
        if let Some(version) = &self.version {
            // 清单里这里本是正则；核心只做前缀/包含的宽松匹配，
            // 足以覆盖实际出现过的写法，且避免引入正则依赖。
            if !os_version_matches(version, &os.version) {
                return false;
            }
        }
        true
    }
}

/// 探测系统版本号字符串。
///
/// `os.version` 条件在实际清单里极少出现（主要是早年 macOS 的
/// `^10\\.` 之类），拿不到时返回空串，求值时会按「不做拦截」处理，
/// 比猜一个值更安全。
fn detect_os_version() -> String {
    #[cfg(target_os = "windows")]
    {
        // 不引 winapi：读注册表的 CurrentVersion 就够用，且失败无副作用。
        // 拿不到返回空串，交由 os_version_matches 宽松处理。
        crate::spawn::silent("cmd")
            .args(["/C", "ver"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| {
                // 形如 "Microsoft Windows [Version 10.0.26200.1234]"
                s.split("Version ").nth(1).map(|v| {
                    v.trim_end_matches(|c: char| c == ']' || c.is_whitespace())
                        .to_string()
                })
            })
            .unwrap_or_default()
    }
    #[cfg(not(target_os = "windows"))]
    {
        String::new()
    }
}

/// 极简的 `os.version` 匹配。
///
/// 清单里这个字段写的是正则，但实际只出现过极少几种形式
/// （`^10\.`、`^\d+\.\d+` 之类）。为不引入正则库，这里实现一个
/// 只认字面数字、`.`、`\d`、`^`、`$` 的子集匹配器；
/// 遇到量词等无法理解的记号时，按「不拦截」处理，避免误杀。
fn os_version_matches(pattern: &str, actual: &str) -> bool {
    if actual.is_empty() {
        // 无法判定版本时不做拦截，避免误杀。
        return true;
    }

    let mut pat = pattern;
    if let Some(rest) = pat.strip_prefix('^') {
        pat = rest;
    }
    let anchored_end = pat.ends_with('$');
    let pat = pat.trim_end_matches('$');

    let mut actual_iter = actual.chars();
    let mut chars = pat.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                match chars.next() {
                    Some('d') => {
                        // 一个数字
                        match actual_iter.next() {
                            Some(a) if a.is_ascii_digit() => {}
                            _ => return false,
                        }
                    }
                    Some('.') => {
                        // 字面点号
                        match actual_iter.next() {
                            Some('.') => {}
                            _ => return false,
                        }
                    }
                    // 其它转义：按不拦截处理
                    _ => return true,
                }
            }
            '0'..='9' | '.' => match actual_iter.next() {
                Some(a) if a == c => {}
                _ => return false,
            },
            // 一旦出现量词或分组，说明超出本匹配器的能力，放行
            '+' | '*' | '?' | '{' | '(' | '[' => return true,
            _ => return true,
        }
    }

    if !anchored_end {
        // 未锚定结尾：只要前缀匹配上就算通过
        return true;
    }
    // 锚定结尾时要求剩余部分为空
    actual_iter.next().is_none()
}

/// 一条规则。
#[derive(Debug, Clone)]
pub struct Rule {
    pub action: Action,
    pub os: Option<OsRule>,
    /// 需要全部满足的特性条件。
    pub features: Vec<(String, bool)>,
}

impl Rule {
    /// 该规则是否命中当前上下文。
    pub fn matches(&self, ctx: &RuleContext) -> bool {
        if let Some(os_rule) = &self.os {
            if !os_rule.matches(&ctx.os) {
                return false;
            }
        }
        for (key, expected) in &self.features {
            match ctx.features.get(key) {
                Some(actual) if actual == *expected => {}
                _ => return false,
            }
        }
        true
    }
}

/// 求值一组规则。
///
/// 空规则表示「无限制」，返回 true。
pub fn is_allowed(rules: &[Rule], ctx: &RuleContext) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for rule in rules {
        if rule.matches(ctx) {
            allowed = rule.action == Action::Allow;
        }
    }
    allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win() -> RuleContext {
        RuleContext::new(OsInfo::for_test(OsName::Windows, Arch::X86_64), Features::default())
    }

    fn mac() -> RuleContext {
        RuleContext::new(OsInfo::for_test(OsName::Osx, Arch::Arm64), Features::default())
    }

    fn allow_win() -> Rule {
        Rule {
            action: Action::Allow,
            os: Some(OsRule {
                name: Some("windows".into()),
                ..Default::default()
            }),
            features: vec![],
        }
    }

    fn disallow_win() -> Rule {
        Rule {
            action: Action::Disallow,
            os: Some(OsRule {
                name: Some("windows".into()),
                ..Default::default()
            }),
            features: vec![],
        }
    }

    #[test]
    fn empty_rules_allow_everything() {
        assert!(is_allowed(&[], &win()));
        assert!(is_allowed(&[], &mac()));
    }

    #[test]
    fn single_allow_rule_scopes_to_platform() {
        let rules = vec![allow_win()];
        assert!(is_allowed(&rules, &win()));
        assert!(!is_allowed(&rules, &mac()));
    }

    #[test]
    fn no_matching_rule_means_denied() {
        let rules = vec![allow_win()];
        let linux = RuleContext::new(
            OsInfo::for_test(OsName::Linux, Arch::X86_64),
            Features::default(),
        );
        assert!(!is_allowed(&rules, &linux));
    }

    /// 旧式 natives 的典型写法：先 allow，再按平台 disallow。
    #[test]
    fn later_rule_wins_in_order() {
        let rules = vec![
            Rule {
                action: Action::Allow,
                os: None,
                features: vec![],
            },
            disallow_win(),
        ];
        assert!(!is_allowed(&rules, &win()));
        assert!(is_allowed(&rules, &mac()));
    }

    #[test]
    fn allow_disallow_allow_ends_allowed() {
        let rules = vec![
            allow_win(),
            disallow_win(),
            allow_win(),
        ];
        assert!(is_allowed(&rules, &win()));
    }

    #[test]
    fn arch_rule_matches_only_matching_arch() {
        let rule = Rule {
            action: Action::Allow,
            os: Some(OsRule {
                arch: Some("x86".into()),
                ..Default::default()
            }),
            features: vec![],
        };
        assert!(!is_allowed(std::slice::from_ref(&rule), &win()));
        let x86 = RuleContext::new(
            OsInfo::for_test(OsName::Windows, Arch::X86),
            Features::default(),
        );
        assert!(is_allowed(&[rule], &x86));
    }

    #[test]
    fn feature_rule_requires_flag_on() {
        let rule = Rule {
            action: Action::Allow,
            os: None,
            features: vec![("has_custom_resolution".into(), true)],
        };
        let mut ctx = win();
        assert!(!is_allowed(std::slice::from_ref(&rule), &ctx));
        ctx.features.has_custom_resolution = true;
        assert!(is_allowed(&[rule], &ctx));
    }

    #[test]
    fn unknown_feature_never_matches() {
        let rule = Rule {
            action: Action::Allow,
            os: None,
            features: vec![("some_future_feature".into(), true)],
        };
        assert!(!is_allowed(&[rule], &win()));
    }
}
