//! 版本清单模型与解析。
//!
//! 需要同时应付三套写法：
//!
//! 1. **原版 / 官方继承链**：`inheritsFrom` 指向父版本，子版本只写差异。
//! 2. **HMCL 扁平化**：没有 `inheritsFrom`，但保留 `patches` 数组记录各层来源，
//!    顶层字段已经是合并结果。本机全部版本都是这种。
//! 3. **旧格式**（≤1.12.2）：`minecraftArguments` 是一个含占位符的字符串，
//!    natives 用 `natives: {"windows": "natives-windows"}` + `downloads.classifiers`。
//!
//! 解析时把三者归一成同一个内部模型，并在 `resolve` 阶段完成 patch 合并。

use crate::json::{self, Json};
use crate::maven::MavenCoord;
use crate::rules::{Action, OsRule, Rule};

/// 解析错误。
#[derive(Debug, Clone)]
pub struct VersionError {
    pub message: String,
}

impl VersionError {
    fn new(message: impl Into<String>) -> Self {
        VersionError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for VersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for VersionError {}

type Result<T> = std::result::Result<T, VersionError>;

/// 一个可下载的文件。
#[derive(Debug, Clone, Default)]
pub struct Download {
    pub url: String,
    pub sha1: Option<String>,
    pub size: Option<u64>,
    /// 仓库内相对路径；本地库文件由坐标推导，不一定带此字段。
    pub path: Option<String>,
}

/// 一个库条目。
#[derive(Debug, Clone)]
pub struct Library {
    /// 原始坐标字符串。
    pub name: String,
    /// 解析后的坐标；解析失败时为 None（仍会尝试用 downloads 直接定位）。
    pub coord: Option<MavenCoord>,
    /// 仓库基址，缺省用 libraries.minecraft.net。
    pub url: Option<String>,
    pub rules: Vec<Rule>,
    /// 主构件的下载信息（jar 本身）。
    pub artifact: Option<Download>,
    /// 按分类符索引的下载信息（旧格式 natives 走这里）。
    pub classifiers: Vec<(String, Download)>,
    /// 旧格式的 natives 映射：平台名 -> 分类符。
    pub natives: Vec<(String, String)>,
    /// 旧格式的解压排除规则。
    pub extract_exclude: Vec<String>,
}

impl Library {
    /// 该库是否为「纯」平台本地库（只提供原生库）。
    ///
    /// 老版本（≤1.12）有一个容易踩的写法：一个条目**同时**带 `natives`
    /// 映射和 `downloads.artifact`。这种条目提供两个文件：
    ///
    /// - `artifact`：真正的代码 jar（`lwjgl-2.9.4.jar`），**必须进 classpath**
    /// - `classifiers.natives-*`：原生库，解压到 natives 目录
    ///
    /// 若只看「有没有 natives 字段」就判为纯原生库，会把主 jar 从
    /// classpath 丢掉，启动时报
    /// `NoClassDefFoundError: org/lwjgl/opengl/OpenGLException`。
    /// 所以判定要同时看 artifact 是否存在。
    pub fn is_native(&self) -> bool {
        self.is_native_only()
    }

    /// 是否只提供原生库、没有主构件。只有这类才完全排除出 classpath。
    pub fn is_native_only(&self) -> bool {
        // 新版：分类符写在坐标里，如 `org.lwjgl:lwjgl:3.3.1:natives-windows`
        if let Some(coord) = &self.coord {
            if coord.is_native() {
                return true;
            }
        }
        // 老版：有 natives 映射但**没有** artifact，才是纯原生库
        !self.natives.is_empty() && self.artifact.is_none()
    }

    /// 该条目是否还需要解压原生库。
    ///
    /// 与 `is_native_only` 互补：带 artifact 的老式条目**两者都要** ——
    /// 主 jar 进 classpath，同时原生库要解压。
    pub fn has_natives(&self) -> bool {
        if self.coord.as_ref().map(|c| c.is_native()).unwrap_or(false) {
            return true;
        }
        !self.natives.is_empty() || !self.classifiers.is_empty()
    }

    /// 给定目标平台，返回应当使用的 natives 分类符。
    ///
    /// 新格式把平台写进坐标分类符，旧格式写在 `natives` 映射里。
    pub fn native_classifier(&self, os_name: &str) -> Option<String> {
        if let Some((_, classifier)) = self.natives.iter().find(|(k, _)| k == os_name) {
            return Some(classifier.clone());
        }
        if let Some(coord) = &self.coord {
            if coord.is_native() {
                return coord.classifier.clone();
            }
        }
        None
    }
}

/// 一条参数：纯字符串或带规则的片段。
#[derive(Debug, Clone)]
pub enum ArgEntry {
    Plain(String),
    Conditional { rules: Vec<Rule>, values: Vec<String> },
}

impl ArgEntry {
    /// 在给定上下文下展开成若干参数。
    pub fn expand(&self, ctx: &crate::rules::RuleContext) -> Vec<String> {
        match self {
            ArgEntry::Plain(s) => vec![s.clone()],
            ArgEntry::Conditional { rules, values } => {
                if crate::rules::is_allowed(rules, ctx) {
                    values.clone()
                } else {
                    Vec::new()
                }
            }
        }
    }
}

/// 资源索引（assets）。
#[derive(Debug, Clone, Default)]
pub struct AssetIndex {
    pub id: String,
    pub url: Option<String>,
    pub sha1: Option<String>,
    pub size: Option<u64>,
    /// 清单里的 `assets` 字段，决定 objects 落在 assets/objects 还是 legacy 虚拟目录。
    pub virtual_root: Option<String>,
}

/// Java 版本要求。
#[derive(Debug, Clone, Default)]
pub struct JavaVersion {
    pub component: Option<String>,
    pub major_version: Option<u32>,
}

/// 日志配置。
#[derive(Debug, Clone, Default)]
pub struct LoggingConfig {
    pub argument: Option<String>,
    pub file: Option<Download>,
    pub log_type: Option<String>,
}

/// 清单的原始解析结果（未做合并）。
#[derive(Debug, Clone, Default)]
pub struct RawVersion {
    pub id: String,
    pub inherits_from: Option<String>,
    pub main_class: Option<String>,
    /// 指向自身 jar 的版本 id；等于自身 id 时说明 jar 就在版本目录里。
    pub jar: Option<String>,
    pub asset_index: AssetIndex,
    pub java_version: JavaVersion,
    pub libraries: Vec<Library>,
    pub arguments_game: Vec<ArgEntry>,
    pub arguments_jvm: Vec<ArgEntry>,
    /// 旧格式的 `minecraftArguments`，已按空格切成 token。
    pub minecraft_arguments: Option<Vec<String>>,
    pub downloads_client: Option<Download>,
    /// 服务端 jar。只有正式版才给，快照与远古版本经常没有。
    pub downloads_server: Option<Download>,
    pub logging: Option<LoggingConfig>,
    pub patches: Vec<Patch>,
    pub version_type: Option<String>,
    /// 清单要求的启动器兼容级别（现代清单写 1，老清单没有这个字段）。
    pub compliance_level: Option<u32>,
    /// 官方要求的最低启动器版本号，只是个参考值。
    pub minimum_launcher_version: Option<u32>,
}

/// `patches` 数组里的一层。
#[derive(Debug, Clone, Default)]
pub struct Patch {
    pub id: String,
    pub priority: i64,
    pub main_class: Option<String>,
    pub libraries: Vec<Library>,
    pub arguments_game: Vec<ArgEntry>,
    pub arguments_jvm: Vec<ArgEntry>,
    pub java_version: JavaVersion,
}

/// 解析一份版本 JSON。
pub fn parse_version(text: &str) -> Result<RawVersion> {
    let root = json::parse(text).map_err(|e| VersionError::new(e.to_string()))?;
    raw_from_json(&root)
}

/// 继承链最多回溯多少层。
///
/// 正常清单只有「加载器 → 原版」两层，多给几层是给 `inheritsFrom` 套娃的
/// 整合包留余量。真正的用处是防环：两个版本互相 `inheritsFrom` 时，
/// 没有这个上限就会一直读下去。
const MAX_INHERIT_DEPTH: usize = 16;

/// 按 `inheritsFrom` 读出一整条版本链，**父版本在前**。
///
/// 装 Forge / NeoForge / Fabric 之后必然要读链：它们的版本 JSON 只写差异，
/// 主类、库、参数的主体都在父版本（原版）里，只解析子版本会得到一份
/// 缺 jar、缺 assetIndex 的半成品清单。
///
/// 参数是「怎么把某个 id 的 JSON 读出来」，而不是 `game_dir`：
/// 调用方有时从磁盘读，有时从内存里的安装计划读，也有测试直接给字面量。
/// 返回的最后一层就是 `id` 自己，也就是 `resolve` 需要的那种顺序。
pub fn load_chain<F>(id: &str, mut read: F) -> Result<Vec<RawVersion>>
where
    F: FnMut(&str) -> Result<String>,
{
    let mut chain: Vec<RawVersion> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut next = Some(id.to_string());

    while let Some(current) = next {
        if seen.iter().any(|s| s == &current) {
            return Err(VersionError::new(format!(
                "版本继承链成环：{} -> {}",
                seen.join(" -> "),
                current
            )));
        }
        if seen.len() >= MAX_INHERIT_DEPTH {
            return Err(VersionError::new(format!(
                "版本继承链超过 {} 层，疑似成环：{}",
                MAX_INHERIT_DEPTH,
                seen.join(" -> ")
            )));
        }

        let text = read(&current)?;
        let raw = parse_version(&text)?;
        next = raw
            .inherits_from
            .clone()
            .filter(|p| !p.is_empty() && p != &current);
        seen.push(current);
        chain.push(raw);
    }

    chain.reverse();
    Ok(chain)
}

/// 从 `<game_dir>/versions/<id>/<id>.json` 起读出整条继承链。
///
/// 就是 [`load_chain`] 绑上「从游戏目录里的版本目录读」这个读法：
/// 装完 Forge / NeoForge / Fabric 后启动、校验都走这一条。
pub fn load_chain_from_dir(game_dir: &std::path::Path, id: &str) -> Result<Vec<RawVersion>> {
    load_chain(id, |version_id| {
        let path = game_dir
            .join("versions")
            .join(version_id)
            .join(format!("{}.json", version_id));
        std::fs::read_to_string(&path).map_err(|e| {
            VersionError::new(format!(
                "读取 {} 失败: {}（版本「{}」声明了 inheritsFrom，父版本必须先装上）",
                path.display(),
                e,
                id
            ))
        })
    })
}

/// 这个版本声明了父版本、但父版本不在盘上时，返回那个父版本的名字。
///
/// 给「启动前先说人话」用：`load_chain_from_dir` 只会抛一句
/// `读取 …26.3.json 失败: 系统找不到指定的路径`，玩家看不出该补什么。
/// 先问一句「你缺的是 26.3」，界面就能直接给一个「现在装上」的按钮。
///
/// 只读一层，不追整条链：多层的整合包缺中间那一层时，缺的也正是这一层。
pub fn missing_parent(game_dir: &std::path::Path, id: &str) -> Option<String> {
    let path = game_dir
        .join("versions")
        .join(id)
        .join(format!("{}.json", id));
    let text = std::fs::read_to_string(path).ok()?;
    let raw = parse_version(&text).ok()?;
    let parent = raw.inherits_from?;
    if parent.is_empty() || parent == id {
        return None;
    }
    let parent_json = game_dir
        .join("versions")
        .join(&parent)
        .join(format!("{}.json", parent));
    if parent_json.is_file() {
        None
    } else {
        Some(parent)
    }
}

fn raw_from_json(root: &Json) -> Result<RawVersion> {
    if root.as_object().is_none() {
        return Err(VersionError::new("版本清单顶层不是对象"));
    }
    let id = root
        .get("id")
        .and_then(Json::as_str)
        .ok_or_else(|| VersionError::new("版本清单缺少 id"))?
        .to_string();

    let mut raw = RawVersion {
        id,
        ..Default::default()
    };

    raw.inherits_from = opt_str(root, "inheritsFrom");
    raw.main_class = opt_str(root, "mainClass");
    raw.jar = opt_str(root, "jar");
    raw.version_type = opt_str(root, "type");
    raw.compliance_level = root.get("complianceLevel").and_then(Json::as_i64).map(|v| v as u32);
    raw.minimum_launcher_version = root
        .get("minimumLauncherVersion")
        .and_then(Json::as_i64)
        .map(|v| v as u32);

    if let Some(asset_index) = root.get("assetIndex") {
        raw.asset_index = AssetIndex {
            id: opt_str(asset_index, "id").unwrap_or_default(),
            url: opt_str(asset_index, "url"),
            sha1: opt_str(asset_index, "sha1"),
            size: asset_index.get("size").and_then(Json::as_i64).map(|v| v as u64),
            virtual_root: None,
        };
        // `assets` 字段若是字符串且不等于 index id，说明是 legacy 虚拟资源
        if let Some(assets) = root.get("assets").and_then(Json::as_str) {
            if assets != raw.asset_index.id {
                raw.asset_index.virtual_root = Some(assets.to_string());
            }
        }
    }

    if let Some(java) = root.get("javaVersion") {
        raw.java_version = JavaVersion {
            component: opt_str(java, "component"),
            major_version: java
                .get("majorVersion")
                .and_then(Json::as_i64)
                .map(|v| v as u32),
        };
    }

    if let Some(libs) = root.get("libraries").and_then(Json::as_array) {
        raw.libraries = libs.iter().filter_map(parse_library).collect();
    }

    if let Some(args) = root.get("arguments") {
        if let Some(game) = args.get("game").and_then(Json::as_array) {
            raw.arguments_game = parse_args(game);
        }
        if let Some(jvm) = args.get("jvm").and_then(Json::as_array) {
            raw.arguments_jvm = parse_args(jvm);
        }
    }

    if let Some(text_args) = root.get("minecraftArguments").and_then(Json::as_str) {
        raw.minecraft_arguments = Some(split_legacy_arguments(text_args));
    }

    if let Some(client) = root.get("downloads").and_then(|d| d.get("client")) {
        raw.downloads_client = parse_download(client);
    }

    if let Some(server) = root.get("downloads").and_then(|d| d.get("server")) {
        raw.downloads_server = parse_download(server);
    }

    if let Some(logging) = root.get("logging") {
        if let Some(client) = logging.get("client") {
            raw.logging = Some(LoggingConfig {
                argument: opt_str(client, "argument"),
                file: client.get("file").and_then(parse_download),
                log_type: opt_str(client, "type"),
            });
        }
    }

    if let Some(patches) = root.get("patches").and_then(Json::as_array) {
        for p in patches {
            raw.patches.push(parse_patch(p)?);
        }
    }

    Ok(raw)
}

fn parse_patch(node: &Json) -> Result<Patch> {
    let mut patch = Patch {
        id: opt_str(node, "id").unwrap_or_default(),
        priority: node.get("priority").and_then(Json::as_i64).unwrap_or(0),
        main_class: opt_str(node, "mainClass"),
        ..Default::default()
    };
    if let Some(libs) = node.get("libraries").and_then(Json::as_array) {
        patch.libraries = libs.iter().filter_map(parse_library).collect();
    }
    if let Some(args) = node.get("arguments") {
        if let Some(game) = args.get("game").and_then(Json::as_array) {
            patch.arguments_game = parse_args(game);
        }
        if let Some(jvm) = args.get("jvm").and_then(Json::as_array) {
            patch.arguments_jvm = parse_args(jvm);
        }
    }
    if let Some(java) = node.get("javaVersion") {
        patch.java_version = JavaVersion {
            component: opt_str(java, "component"),
            major_version: java
                .get("majorVersion")
                .and_then(Json::as_i64)
                .map(|v| v as u32),
        };
    }
    Ok(patch)
}

fn parse_library(node: &Json) -> Option<Library> {
    let name = node.get("name").and_then(Json::as_str)?.to_string();
    let coord = MavenCoord::parse(&name);

    let mut lib = Library {
        name,
        coord,
        url: opt_str(node, "url"),
        rules: parse_rules(node.get("rules")),
        artifact: None,
        classifiers: Vec::new(),
        natives: Vec::new(),
        extract_exclude: Vec::new(),
    };

    if let Some(downloads) = node.get("downloads") {
        if let Some(artifact) = downloads.get("artifact") {
            lib.artifact = parse_download(artifact);
        }
        if let Some(classifiers) = downloads.get("classifiers").and_then(Json::as_object) {
            for (key, value) in classifiers {
                if let Some(dl) = parse_download(value) {
                    lib.classifiers.push((key.clone(), dl));
                }
            }
        }
    }

    if let Some(natives) = node.get("natives").and_then(Json::as_object) {
        for (platform, classifier) in natives {
            if let Some(c) = classifier.as_str() {
                lib.natives.push((platform.clone(), c.to_string()));
            }
        }
    }

    if let Some(extract) = node.get("extract") {
        if let Some(exclude) = extract.get("exclude").and_then(Json::as_array) {
            lib.extract_exclude = exclude
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
        }
    }

    Some(lib)
}

fn parse_download(node: &Json) -> Option<Download> {
    if !node.is_null() {
        let url = opt_str(node, "url");
        let path = opt_str(node, "path");
        // url 或 path 至少有一个才有意义
        if url.is_none() && path.is_none() {
            return None;
        }
        Some(Download {
            url: url.unwrap_or_default(),
            sha1: opt_str(node, "sha1"),
            size: node.get("size").and_then(Json::as_i64).map(|v| v as u64),
            path,
        })
    } else {
        None
    }
}

fn parse_args(items: &[Json]) -> Vec<ArgEntry> {
    let mut out = Vec::new();
    for item in items {
        match item {
            Json::Str(s) => out.push(ArgEntry::Plain(s.clone())),
            Json::Object(_) => {
                let rules = parse_rules(item.get("rules"));
                let values = item
                    .get("value")
                    .map(|v| match v {
                        Json::Str(s) => vec![s.clone()],
                        Json::Array(arr) => arr
                            .iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect(),
                        _ => Vec::new(),
                    })
                    .unwrap_or_default();
                out.push(ArgEntry::Conditional { rules, values });
            }
            _ => {}
        }
    }
    out
}

fn parse_rules(node: Option<&Json>) -> Vec<Rule> {
    let Some(items) = node.and_then(Json::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let action = match item.get("action").and_then(Json::as_str)? {
                "allow" => Action::Allow,
                "disallow" => Action::Disallow,
                _ => return None,
            };
            let os = item.get("os").map(|os| OsRule {
                name: opt_str(os, "name"),
                version: opt_str(os, "version"),
                arch: opt_str(os, "arch"),
            });
            let mut features = Vec::new();
            if let Some(map) = item.get("features").and_then(Json::as_object) {
                for (key, value) in map {
                    if let Some(flag) = value.as_bool() {
                        features.push((key.clone(), flag));
                    }
                }
            }
            Some(Rule {
                action,
                os,
                features,
            })
        })
        .collect()
}

/// 把旧格式的参数字符串切成 token，尊重双引号。
fn split_legacy_arguments(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in text.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ' ' | '\t' | '\n' | '\r' if !in_quotes => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn opt_str(node: &Json, key: &str) -> Option<String> {
    node.get(key).and_then(Json::as_str).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODERN: &str = r#"{
        "id": "1.20.1-test",
        "mainClass": "net.minecraft.client.main.Main",
        "jar": "1.20.1-test",
        "type": "release",
        "assetIndex": {"id":"5","url":"https://example/5.json","sha1":"abc","size":412473},
        "assets": "5",
        "javaVersion": {"component":"java-runtime-gamma","majorVersion":17},
        "libraries": [
            {"name":"org.ow2.asm:asm:9.8","url":"https://maven.example/"},
            {"name":"org.lwjgl:lwjgl:3.3.1:natives-windows",
             "downloads":{"artifact":{"path":"org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows.jar",
                                       "url":"https://example/n.jar","sha1":"d","size":10}},
             "rules":[{"action":"allow","os":{"name":"windows"}}]}
        ],
        "arguments": {
            "game": ["--username", {"rules":[{"action":"allow","os":{"name":"windows"}}],"value":["--win"]}],
            "jvm": ["-cp", "${classpath}"]
        }
    }"#;

    #[test]
    fn parses_modern_manifest() {
        let v = parse_version(MODERN).unwrap();
        assert_eq!(v.id, "1.20.1-test");
        assert_eq!(v.main_class.as_deref(), Some("net.minecraft.client.main.Main"));
        assert_eq!(v.java_version.major_version, Some(17));
        assert_eq!(v.asset_index.id, "5");
        assert!(v.asset_index.virtual_root.is_none());
        assert_eq!(v.libraries.len(), 2);
        assert!(v.minecraft_arguments.is_none());
        assert_eq!(v.arguments_jvm.len(), 2);
    }

    #[test]
    fn detects_legacy_virtual_assets() {
        let text = r#"{"id":"x","mainClass":"m","assets":"pre-1.6","assetIndex":{"id":"legacy"}}"#;
        let v = parse_version(text).unwrap();
        assert_eq!(v.asset_index.virtual_root.as_deref(), Some("pre-1.6"));
    }

    #[test]
    fn parses_legacy_natives_and_classifiers() {
        let text = r#"{
            "id":"1.12.2",
            "mainClass":"net.minecraft.launchwrapper.Launch",
            "minecraftArguments":"--username ${auth_player_name} --uuid ${auth_uuid}",
            "libraries":[{
                "name":"org.lwjgl.lwjgl:lwjgl-platform:2.9.4",
                "natives":{"linux":"natives-linux","windows":"natives-windows"},
                "extract":{"exclude":["META-INF/"]},
                "downloads":{"artifact":{"path":"a/b.jar","url":"https://x/b.jar","sha1":"s","size":22},
                             "classifiers":{"natives-windows":{"path":"a/b-n.jar","url":"https://x/b-n.jar","sha1":"t","size":613748}}},
                "rules":[{"action":"allow"},{"action":"disallow","os":{"name":"osx"}}]
            }]
        }"#;
        let v = parse_version(text).unwrap();
        let args = v.minecraft_arguments.unwrap();
        assert_eq!(args[0], "--username");
        assert_eq!(args[1], "${auth_player_name}");

        let lib = &v.libraries[0];
        // 这条同时有 artifact 和 natives：它是「带原生库的普通库」，
        // 不是纯原生库。主 jar（22 字节那个占位）仍要进 classpath，
        // 原生库另外解压。曾因把它误判为纯原生库，导致
        // NoClassDefFoundError: org/lwjgl/opengl/OpenGLException
        assert!(
            !lib.is_native_only(),
            "有 artifact 的老式条目不应算纯原生库"
        );
        assert!(lib.has_natives(), "它确实需要解压原生库");
        assert_eq!(lib.native_classifier("windows").as_deref(), Some("natives-windows"));
        assert_eq!(lib.classifiers.len(), 1);
        assert_eq!(lib.extract_exclude, vec!["META-INF/".to_string()]);
        assert_eq!(lib.rules.len(), 2);
    }

    /// 只有 classifiers、没有 artifact 的条目才是纯原生库。
    #[test]
    fn native_only_when_no_artifact() {
        let text = r#"{
            "id":"1.8.9",
            "mainClass":"M",
            "libraries":[
                {"name":"tv.twitch:twitch-platform:6.5",
                 "natives":{"windows":"natives-windows-${arch}"},
                 "downloads":{"classifiers":{
                    "natives-windows-64":{"path":"a/n64.jar","url":"https://x/n.jar","sha1":"a","size":10}}}},
                {"name":"org.lwjgl.lwjgl:lwjgl:2.9.4",
                 "natives":{"windows":"natives-windows"},
                 "downloads":{
                    "artifact":{"path":"a/l.jar","url":"https://x/l.jar","sha1":"b","size":20},
                    "classifiers":{"natives-windows":{"path":"a/ln.jar","url":"https://x/ln.jar","sha1":"c","size":30}}}}
            ]
        }"#;
        let v = parse_version(text).unwrap();
        assert!(v.libraries[0].is_native_only(), "无 artifact 应为纯原生库");
        assert!(!v.libraries[1].is_native_only(), "有 artifact 不应为纯原生库");
        assert!(v.libraries[1].has_natives());
    }

    #[test]
    fn parses_patches() {
        let text = r#"{
            "id":"merged",
            "mainClass":"knot.KnotClient",
            "libraries":[],
            "patches":[
                {"id":"game","priority":0,"mainClass":"net.minecraft.client.main.Main","libraries":[]},
                {"id":"fabric","priority":30000,"libraries":[
                    {"name":"net.fabricmc:fabric-loader:0.16.0","url":"https://maven.fabricmc.net/"}
                ]}
            ]
        }"#;
        let v = parse_version(text).unwrap();
        assert_eq!(v.patches.len(), 2);
        assert_eq!(v.patches[0].id, "game");
        assert_eq!(v.patches[1].priority, 30000);
        assert_eq!(v.patches[1].libraries.len(), 1);
    }

    #[test]
    fn legacy_argument_splitting_respects_quotes() {
        let toks = split_legacy_arguments(r#"--a "x y" --b"#);
        assert_eq!(toks, vec!["--a", "x y", "--b"]);
    }

    #[test]
    fn rejects_missing_id() {
        assert!(parse_version(r#"{"mainClass":"x"}"#).is_err());
        assert!(parse_version("[]").is_err());
    }

    /// 父版本必须排在子版本前面，`resolve` 依赖这个顺序。
    fn chain_fixture(id: &str) -> Result<String> {
        match id {
            "1.20.1" => Ok(r#"{"id":"1.20.1","mainClass":"net.minecraft.client.main.Main",
                "assetIndex":{"id":"5","url":"https://x/5.json"},
                "libraries":[{"name":"com.a:b:1.0"}]}"#
                .to_string()),
            "1.20.1-forge-47.0.1" => Ok(r#"{"id":"1.20.1-forge-47.0.1",
                "inheritsFrom":"1.20.1","mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher",
                "libraries":[{"name":"net.minecraftforge:forge:47.0.1"}]}"#
                .to_string()),
            other => Err(VersionError::new(format!("没有版本 {}", other))),
        }
    }

    #[test]
    fn load_chain_puts_the_parent_first() {
        let chain = load_chain("1.20.1-forge-47.0.1", chain_fixture).unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].id, "1.20.1");
        assert_eq!(chain[1].id, "1.20.1-forge-47.0.1");
        assert_eq!(chain[1].inherits_from.as_deref(), Some("1.20.1"));
    }

    #[test]
    fn load_chain_without_inherits_returns_one_layer() {
        let chain = load_chain("1.20.1", chain_fixture).unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].id, "1.20.1");
    }

    #[test]
    fn load_chain_reports_a_missing_parent() {
        let err = load_chain("1.20.1-forge-47.0.1", |id| match id {
            "1.20.1-forge-47.0.1" => chain_fixture(id),
            _ => Err(VersionError::new("没有版本")),
        })
        .unwrap_err();
        assert!(err.message.contains("没有版本"), "{}", err.message);
    }

    /// 互相继承时不能一直读下去。
    #[test]
    fn load_chain_stops_on_a_cycle() {
        let err = load_chain("a", |id| {
            let parent = if id == "a" { "b" } else { "a" };
            Ok(format!(
                r#"{{"id":"{}","mainClass":"M","inheritsFrom":"{}"}}"#,
                id, parent
            ))
        })
        .unwrap_err();
        assert!(err.message.contains("成环"), "{}", err.message);
    }

    /// 父版本不在盘上时要能被指出来 —— 界面上那一句「缺 26.3」靠它。
    #[test]
    fn missing_parent_names_the_absent_layer() {
        let dir = crate::testutil::fresh_dir("version-missing-parent");
        let write = |id: &str, body: &str| {
            let d = dir.join("versions").join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(format!("{}.json", id)), body).unwrap();
        };
        write(
            "26.3-Fabric",
            r#"{"id":"26.3-Fabric","inheritsFrom":"26.3","mainClass":"M"}"#,
        );
        write("26.2", r#"{"id":"26.2","mainClass":"M"}"#);

        assert_eq!(
            missing_parent(&dir, "26.3-Fabric").as_deref(),
            Some("26.3")
        );
        // 自己就够、没有父版本：不该报缺
        assert_eq!(missing_parent(&dir, "26.2"), None);
        // 连自己的清单都没有：那是另一回事，别混进来
        assert_eq!(missing_parent(&dir, "根本没装"), None);

        // 父版本补上之后就不再缺了
        write("26.3", r#"{"id":"26.3","mainClass":"M"}"#);
        assert_eq!(missing_parent(&dir, "26.3-Fabric"), None);
    }
}
