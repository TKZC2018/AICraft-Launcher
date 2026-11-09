//! 把继承链压平成一份自包含清单（HMCL 那种写法）。
//!
//! 官方与 Fabric 的清单都是「薄」的：子版本只写差异，剩下的靠
//! `inheritsFrom` 指向父版本。这样省流量，但也带来一个后果——
//! **父版本不在盘上时，整个实例都启动不了**，哪怕客户端 jar 明明就在
//! 自己目录里（实际情况：`26.3-Fabric/26.3-Fabric.jar` 41 MB 躺着，
//! 而 `versions/26.3/26.3.json` 被删了，于是报
//! 「读取版本链失败 … 父版本必须先装上」）。
//!
//! HMCL / PCL 的做法是把合并结果直接写进顶层：没有 `inheritsFrom`，
//! 客户端 jar 复制（这里用硬链接，同一块盘上不占额外空间）进实例目录，
//! `patches` 只留作溯源记录。这样每个实例都是独立的一份，删谁都不影响谁，
//! 也能被别的启动器直接读。
//!
//! 本模块就干这件事：读链 → 合并 → 覆写实例自己的清单。

use std::path::{Path, PathBuf};

use crate::json::{self, Json};
use crate::rules::Rule;
use crate::version::{
    ArgEntry, AssetIndex, Download, JavaVersion, Library, LoggingConfig, RawVersion,
};

/// 把实例清单改写成自包含形式，返回写入了多少字节。
///
/// 本来就已经自包含的清单（没有 `inheritsFrom`）直接返回 `0`，不动它 ——
/// 官方原版清单是公用的，别的启动器也在读，没必要为了压平去重写它。
pub fn rewrite_instance(game_dir: &Path, instance: &str) -> Result<usize, String> {
    let path = json_path(game_dir, instance);
    let original =
        std::fs::read_to_string(&path).map_err(|e| format!("读取 {} 失败: {}", path.display(), e))?;
    if !has_parent(&original) {
        return Ok(0);
    }
    let text = flatten_text(game_dir, instance, &original)?;
    std::fs::write(&path, &text).map_err(|e| format!("写入 {} 失败: {}", path.display(), e))?;
    Ok(text.len())
}

/// 生成自包含清单的文本，但不写盘。
pub fn flatten_instance(game_dir: &Path, instance: &str) -> Result<String, String> {
    let path = json_path(game_dir, instance);
    let original =
        std::fs::read_to_string(&path).map_err(|e| format!("读取 {} 失败: {}", path.display(), e))?;
    flatten_text(game_dir, instance, &original)
}

/// 清单有没有指向上游。只看字段在不在，不读父版本本身。
fn has_parent(text: &str) -> bool {
    match json::parse(text) {
        Ok(value) => value
            .get("inheritsFrom")
            .and_then(Json::as_str)
            .map(|parent| !parent.is_empty())
            .unwrap_or(false),
        Err(_) => false,
    }
}

fn flatten_text(game_dir: &Path, instance: &str, original: &str) -> Result<String, String> {
    let chain = crate::version::load_chain_from_dir(game_dir, instance)
        .map_err(|e| format!("读取版本链失败: {}", e))?;
    if chain.is_empty() {
        return Err("版本链为空，没有可压平的内容".to_string());
    }
    let resolved = crate::resolve::resolve(&chain).map_err(|e| format!("合并版本清单失败: {}", e))?;

    // 先在实例原清单上改，而不是从零拼一份：
    // `releaseTime` / `time` / `type` / `_aicraftLoader` / `_aicraftOfficialId`
    // 这些字段合并阶段根本不碰，重建会把它们丢掉，而加载器识别正靠它们。
    let mut value = json::parse(original).map_err(|e| format!("解析清单失败: {}", e))?;
    let Json::Object(entries) = &mut value else {
        return Err("版本清单顶层不是对象".to_string());
    };

    // 压平之后父版本不再是运行前提，这个字段必须消失，
    // 否则启动时还是会回去找 `versions/<父版本>/`。
    entries.retain(|(key, _)| key != "inheritsFrom");

    set(entries, "id", Json::Str(instance.to_string()));
    set(entries, "mainClass", Json::Str(resolved.main_class.clone()));
    set(entries, "jar", Json::Str(instance.to_string()));
    set(entries, "root", Json::Bool(true));

    if let Some(java) = java_json(&resolved.java_version) {
        set(entries, "javaVersion", java);
    }
    if let Some(index) = asset_index_json(&resolved.asset_index) {
        set(entries, "assetIndex", index);
    }
    // `assets` 在老版本里指虚拟资源目录（比如 `pre-1.6`），和索引 id 不同；
    // 现代版本这里恰好等于索引 id（26.3 的 assets 是 "34"），HMCL 也照样写着，
    // 所以按「等于索引 id」就写，别删——少一个字段会让别的启动器多疑。
    set(
        entries,
        "assets",
        Json::Str(
            resolved
                .asset_index
                .virtual_root
                .clone()
                .unwrap_or_else(|| resolved.asset_index.id.clone()),
        ),
    );
    if let Some(logging) = &resolved.logging {
        set(entries, "logging", logging_json(logging));
    }
    if let Some(client) = client_download(&chain) {
        set(
            entries,
            "downloads",
            Json::Object(vec![("client".to_string(), download_json(client))]),
        );
    }

    set(
        entries,
        "libraries",
        Json::Array(resolved.libraries.iter().map(library_json).collect()),
    );
    set(entries, "arguments", arguments_json(&resolved));
    // 这两项只是「给别的启动器看的承诺」，官方清单里有，HMCL 也照抄。
    // 缺失不影响我们自己启动，但别的启动器看到会把它当成一份可疑的清单。
    if let Some(level) = chain.iter().rev().find_map(|raw| raw.compliance_level) {
        set(entries, "complianceLevel", Json::Number(level as f64));
    }
    if let Some(version) = chain
        .iter()
        .rev()
        .find_map(|raw| raw.minimum_launcher_version)
    {
        set(
            entries,
            "minimumLauncherVersion",
            Json::Number(version as f64),
        );
    }
    set(entries, "patches", patches_json(&chain));

    // 客户端 jar 要落在自己目录里：清单已经宣称 `"jar": "<实例名>"`，
    // 文件却不在，启动时会找不到主类。优先硬链接（同盘零成本）。
    link_client_jar(game_dir, instance, &resolved.jar_id, &chain);

    Ok(json::write(&value))
}

fn json_path(game_dir: &Path, instance: &str) -> PathBuf {
    crate::instance::version_dir(game_dir, instance).join(format!("{}.json", instance))
}

fn set(entries: &mut Vec<(String, Json)>, key: &str, value: Json) {
    match entries.iter_mut().find(|(k, _)| k == key) {
        Some((_, slot)) => *slot = value,
        None => entries.push((key.to_string(), value)),
    }
}

/// 把父版本的客户端 jar 搬（硬链接）进实例目录。
///
/// 找不到任何来源就安静地跳过：清单已经写好，缺的就是那一个文件，
/// 玩家至少还能从报错里看出「jar 不在」而不是「父版本不在」。
fn link_client_jar(
    game_dir: &Path,
    instance: &str,
    jar_id: &Option<String>,
    chain: &[RawVersion],
) {
    let dest = crate::instance::version_dir(game_dir, instance).join(format!("{}.jar", instance));
    if dest.is_file() {
        return;
    }
    let Some(source) = client_jar_source(game_dir, instance, jar_id, chain) else {
        return;
    };
    let linked = std::fs::hard_link(&source, &dest).is_ok();
    if !linked && std::fs::copy(&source, &dest).is_err() {
        return;
    }
    crate::logging::debug(format!(
        "{} 的客户端 jar 从 {} {}进来了",
        instance,
        source.display(),
        if linked { "链接" } else { "复制" }
    ));
}

fn client_jar_source(
    game_dir: &Path,
    instance: &str,
    jar_id: &Option<String>,
    chain: &[RawVersion],
) -> Option<PathBuf> {
    let mut candidates: Vec<String> = Vec::new();
    // 1. 合并阶段记下的 jar 指向（子版本写了 `jar` 时最准）
    if let Some(id) = jar_id {
        if !id.is_empty() && id != instance {
            candidates.push(id.clone());
        }
    }
    // 2. 链上真正自带 `downloads.client` 的那一层 —— 原版客户端 jar 就在这里
    for raw in chain.iter().rev() {
        if raw.downloads_client.is_some() && raw.id != instance {
            candidates.push(raw.id.clone());
        }
    }
    // 3. 每层自己声明的 `jar` 字段
    for raw in chain.iter().rev() {
        if let Some(jar) = &raw.jar {
            if !jar.is_empty() && jar != instance {
                candidates.push(jar.clone());
            }
        }
    }

    for id in candidates {
        let path = game_dir
            .join("versions")
            .join(&id)
            .join(format!("{}.jar", id));
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn client_download(chain: &[RawVersion]) -> Option<&Download> {
    chain
        .iter()
        .rev()
        .find_map(|raw| raw.downloads_client.as_ref())
        .filter(|dl| !dl.url.is_empty())
}

fn java_json(java: &JavaVersion) -> Option<Json> {
    let mut entries = Vec::new();
    if let Some(component) = &java.component {
        entries.push(("component".to_string(), Json::Str(component.clone())));
    }
    if let Some(major) = java.major_version {
        entries.push(("majorVersion".to_string(), Json::Number(major as f64)));
    }
    if entries.is_empty() {
        None
    } else {
        Some(Json::Object(entries))
    }
}

fn asset_index_json(index: &AssetIndex) -> Option<Json> {
    if index.id.is_empty() && index.url.is_none() {
        return None;
    }
    let mut entries = vec![("id".to_string(), Json::Str(index.id.clone()))];
    if let Some(url) = &index.url {
        entries.push(("url".to_string(), Json::Str(url.clone())));
    }
    if let Some(sha1) = &index.sha1 {
        entries.push(("sha1".to_string(), Json::Str(sha1.clone())));
    }
    if let Some(size) = index.size {
        entries.push(("size".to_string(), Json::Number(size as f64)));
    }
    Some(Json::Object(entries))
}

fn logging_json(logging: &LoggingConfig) -> Json {
    let mut client = Vec::new();
    if let Some(argument) = &logging.argument {
        client.push(("argument".to_string(), Json::Str(argument.clone())));
    }
    if let Some(file) = &logging.file {
        client.push(("file".to_string(), download_json(file)));
    }
    if let Some(log_type) = &logging.log_type {
        client.push(("type".to_string(), Json::Str(log_type.clone())));
    }
    Json::Object(vec![(
        "client".to_string(),
        Json::Object(client),
    )])
}

fn download_json(download: &Download) -> Json {
    let mut entries = Vec::new();
    if !download.url.is_empty() {
        entries.push(("url".to_string(), Json::Str(download.url.clone())));
    }
    if let Some(path) = &download.path {
        entries.push(("path".to_string(), Json::Str(path.clone())));
    }
    if let Some(sha1) = &download.sha1 {
        entries.push(("sha1".to_string(), Json::Str(sha1.clone())));
    }
    if let Some(size) = download.size {
        entries.push(("size".to_string(), Json::Number(size as f64)));
    }
    Json::Object(entries)
}

fn library_json(lib: &Library) -> Json {
    let mut entries = vec![("name".to_string(), Json::Str(lib.name.clone()))];
    if let Some(url) = &lib.url {
        entries.push(("url".to_string(), Json::Str(url.clone())));
    }
    if !lib.rules.is_empty() {
        entries.push(("rules".to_string(), rules_json(&lib.rules)));
    }

    let mut downloads = Vec::new();
    if let Some(artifact) = &lib.artifact {
        downloads.push(("artifact".to_string(), download_json(artifact)));
    }
    if !lib.classifiers.is_empty() {
        downloads.push((
            "classifiers".to_string(),
            Json::Object(
                lib.classifiers
                    .iter()
                    .map(|(key, dl)| (key.clone(), download_json(dl)))
                    .collect(),
            ),
        ));
    }
    if !downloads.is_empty() {
        entries.push(("downloads".to_string(), Json::Object(downloads)));
    }

    if !lib.natives.is_empty() {
        entries.push((
            "natives".to_string(),
            Json::Object(
                lib.natives
                    .iter()
                    .map(|(platform, classifier)| {
                        (platform.clone(), Json::Str(classifier.clone()))
                    })
                    .collect(),
            ),
        ));
    }
    if !lib.extract_exclude.is_empty() {
        entries.push((
            "extract".to_string(),
            Json::Object(vec![(
                "exclude".to_string(),
                Json::Array(
                    lib.extract_exclude
                        .iter()
                        .map(|pat| Json::Str(pat.clone()))
                        .collect(),
                ),
            )]),
        ));
    }

    Json::Object(entries)
}

fn rules_json(rules: &[Rule]) -> Json {
    Json::Array(
        rules
            .iter()
            .map(|rule| {
                let action = match rule.action {
                    crate::rules::Action::Allow => "allow",
                    crate::rules::Action::Disallow => "disallow",
                };
                let mut entries = vec![("action".to_string(), Json::Str(action.to_string()))];
                if let Some(os) = &rule.os {
                    let mut os_entries = Vec::new();
                    if let Some(name) = &os.name {
                        os_entries.push(("name".to_string(), Json::Str(name.clone())));
                    }
                    if let Some(version) = &os.version {
                        os_entries.push(("version".to_string(), Json::Str(version.clone())));
                    }
                    if let Some(arch) = &os.arch {
                        os_entries.push(("arch".to_string(), Json::Str(arch.clone())));
                    }
                    entries.push(("os".to_string(), Json::Object(os_entries)));
                }
                if !rule.features.is_empty() {
                    entries.push((
                        "features".to_string(),
                        Json::Object(
                            rule.features
                                .iter()
                                .map(|(key, flag)| (key.clone(), Json::Bool(*flag)))
                                .collect(),
                        ),
                    ));
                }
                Json::Object(entries)
            })
            .collect(),
    )
}

fn arguments_json(resolved: &crate::resolve::ResolvedVersion) -> Json {
    Json::Object(vec![
        ("game".to_string(), args_json(&resolved.game_args)),
        ("jvm".to_string(), args_json(&resolved.jvm_args)),
    ])
}

fn args_json(args: &[ArgEntry]) -> Json {
    Json::Array(
        args.iter()
            .map(|entry| match entry {
                ArgEntry::Plain(text) => Json::Str(text.clone()),
                ArgEntry::Conditional { rules, values } => Json::Object(vec![
                    ("rules".to_string(), rules_json(rules)),
                    (
                        "value".to_string(),
                        Json::Array(values.iter().map(|v| Json::Str(v.clone())).collect()),
                    ),
                ]),
            })
            .collect(),
    )
}

/// `patches` 只留溯源信息：每层一行，不重复带库和参数。
///
/// 带库的话，`resolve` 里的 `patches_are_redundant` 会把它当成「待合并的增量」
/// 再加一遍——那是早期真踩过的坑（参数从 45 条翻到 76 条）。
/// 留空的 `libraries` 会被判为纯记录，直接跳过。
fn patches_json(chain: &[RawVersion]) -> Json {
    Json::Array(
        chain
            .iter()
            .enumerate()
            .map(|(i, raw)| {
                Json::Object(vec![
                    ("id".to_string(), Json::Str(raw.id.clone())),
                    ("version".to_string(), Json::Str(raw.id.clone())),
                    (
                        "priority".to_string(),
                        Json::Number((i as f64) * 10000.0),
                    ),
                ])
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::fresh_dir;

    const PARENT: &str = r#"{
        "id":"26.3",
        "mainClass":"net.minecraft.client.main.Main",
        "type":"release",
        "releaseTime":"2026-10-27T19:04:43+0000",
        "assetIndex":{"id":"34","url":"https://x/34.json","sha1":"aa","size":597035},
        "javaVersion":{"component":"java-runtime-delta","majorVersion":25},
        "downloads":{"client":{"url":"https://x/client.jar","sha1":"bb","size":41483720}},
        "libraries":[
            {"name":"com.example:base:1.0","downloads":{"artifact":{"url":"https://x/base.jar","sha1":"cc","size":10}}},
            {"name":"org.lwjgl:lwjgl:3.4.3:natives-windows","downloads":{"artifact":{"url":"https://x/w.jar","sha1":"dd","size":10}}}
        ],
        "arguments":{"game":["--username","${auth_player_name}"],"jvm":["-Djava.library.path=${natives_directory}/java"]}
    }"#;

    const CHILD: &str = r#"{
        "id":"26.3-Fabric",
        "inheritsFrom":"26.3",
        "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
        "type":"release",
        "time":"2026-10-27T19:04:43+0000",
        "arguments":{"game":[],"jvm":["-DFabricMcEmu= net.minecraft.client.main.Main "]},
        "libraries":[{"name":"net.fabricmc:fabric-loader:0.20.1","downloads":{"artifact":{"url":"https://x/fl.jar","sha1":"ee","size":10}}}],
        "_aicraftOfficialId":"fabric-loader-0.20.1-26.3",
        "_aicraftLoader":{"kind":"fabric","version":"0.20.1"}
    }"#;

    fn seed() -> (crate::testutil::TempDir, std::path::PathBuf) {
        let dir = fresh_dir("flatten");
        let game = dir.join("mc");
        for (name, text) in [("26.3", PARENT), ("26.3-Fabric", CHILD)] {
            let vdir = game.join("versions").join(name);
            std::fs::create_dir_all(&vdir).unwrap();
            std::fs::write(vdir.join(format!("{}.json", name)), text).unwrap();
        }
        // 父版本的客户端 jar：压平时要硬链接进实例目录
        std::fs::write(game.join("versions/26.3/26.3.jar"), b"jar-bytes").unwrap();
        (dir, game)
    }

    #[test]
    fn flattening_drops_the_parent_link_and_keeps_everything_else() {
        let (_dir, game) = seed();
        let text = flatten_instance(&game, "26.3-Fabric").unwrap();
        let value = json::parse(&text).unwrap();

        assert!(
            value.get("inheritsFrom").is_none(),
            "压平后不能再留 inheritsFrom: {}",
            text
        );
        assert_eq!(value.get("id").and_then(Json::as_str), Some("26.3-Fabric"));
        assert_eq!(value.get("jar").and_then(Json::as_str), Some("26.3-Fabric"));
        assert_eq!(value.get("root").and_then(Json::as_bool), Some(true));
        // 加载器识别靠这两个私有字段，压平不能顺手扔掉
        assert_eq!(
            value.get("_aicraftOfficialId").and_then(Json::as_str),
            Some("fabric-loader-0.20.1-26.3")
        );
        assert!(value.get("_aicraftLoader").is_some());
        // 来源记录还在
        let patches = value.get("patches").and_then(Json::as_array).unwrap();
        assert_eq!(patches.len(), 2);

        // 关键：库和参数是合并后的，三条都在
        let libs = value.get("libraries").and_then(Json::as_array).unwrap();
        let names: Vec<&str> = libs
            .iter()
            .filter_map(|l| l.get("name").and_then(Json::as_str))
            .collect();
        assert_eq!(
            names,
            vec![
                "com.example:base:1.0",
                "org.lwjgl:lwjgl:3.4.3:natives-windows",
                "net.fabricmc:fabric-loader:0.20.1"
            ]
        );
        let game_args = value
            .get("arguments")
            .and_then(|a| a.get("game"))
            .and_then(Json::as_array)
            .unwrap();
        assert_eq!(game_args.len(), 2);
    }

    #[test]
    fn a_flattened_instance_resolves_without_its_parent() {
        let (_dir, game) = seed();
        rewrite_instance(&game, "26.3-Fabric").unwrap();

        // 把父版本整个删掉，模拟「26.3.json 被删了」的现场
        std::fs::remove_dir_all(game.join("versions/26.3")).unwrap();

        let chain = crate::version::load_chain_from_dir(&game, "26.3-Fabric")
            .expect("压平后不该再依赖父版本");
        assert_eq!(chain.len(), 1);
        let resolved = crate::resolve::resolve(&chain).unwrap();
        assert_eq!(resolved.libraries.len(), 3);
        assert_eq!(resolved.jar_id.as_deref(), Some("26.3-Fabric"));
        // jar 已经搬进实例目录，删掉父版本不影响启动
        assert!(game.join("versions/26.3-Fabric/26.3-Fabric.jar").is_file());
        assert_eq!(resolved.asset_index.id, "34");
        assert_eq!(resolved.java_version.major_version, Some(25));
    }

    #[test]
    fn rewriting_twice_changes_nothing() {
        let (_dir, game) = seed();
        rewrite_instance(&game, "26.3-Fabric").unwrap();
        let first = flatten_instance(&game, "26.3-Fabric").unwrap();
        rewrite_instance(&game, "26.3-Fabric").unwrap();
        let second = flatten_instance(&game, "26.3-Fabric").unwrap();
        assert_eq!(first, second, "重复压平应当是幂等的");
    }
}
