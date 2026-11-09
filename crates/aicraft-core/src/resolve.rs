//! 版本合并：把原始清单归并成可启动的完整描述。
//!
//! 三种输入形态要统一：
//!
//! - 带 `inheritsFrom`：先递归解析父版本，再让子版本覆盖。
//! - 带 `patches`：按 `priority` 升序叠加，顶层字段优先。
//! - 都没有：直接就是完整描述。
//!
//! 合并的关键规则（与官方启动器一致）：
//! - `libraries` **追加合并**，按 `group:artifact:classifier` 去重，后者覆盖前者。
//! - `arguments` **追加合并**，不做去重（重复参数有语义，且 patch 间顺序重要）。
//! - 标量字段（mainClass / assetIndex / javaVersion）后者覆盖。
//! - 旧格式 `minecraftArguments` 若存在，最终转成 game 参数。

use std::collections::HashMap;

use crate::rules::RuleContext;
use crate::version::{
    ArgEntry, AssetIndex, Download, JavaVersion, Library, Patch, RawVersion, VersionError,
};

type Result<T> = std::result::Result<T, VersionError>;

/// 合并完成、可直接用于启动的版本描述。
#[derive(Debug, Clone)]
pub struct ResolvedVersion {
    pub id: String,
    pub main_class: String,
    /// 客户端 jar 的版本 id；为 None 表示不需要独立 jar。
    pub jar_id: Option<String>,
    pub asset_index: AssetIndex,
    pub java_version: JavaVersion,
    pub libraries: Vec<Library>,
    pub game_args: Vec<ArgEntry>,
    pub jvm_args: Vec<ArgEntry>,
    pub logging: Option<crate::version::LoggingConfig>,
    pub version_type: Option<String>,
}

/// 合并若干原始清单。
///
/// `base` 应当是按继承顺序排好的列表：最底层（父版本）在前。
/// 传入的 `patches` 会按 priority 升序参与合并。
pub fn resolve(chain: &[RawVersion]) -> Result<ResolvedVersion> {
    if chain.is_empty() {
        return Err(VersionError {
            message: "版本链为空".into(),
        });
    }

    // 收集所有层：链上每个版本的 patches（按 priority 升序）跟着它自己的主体。
    // 主体字段优先级最高，所以放在该组的最后。
    //
    // 但 HMCL 的扁平化清单会把**合并后的结果**写在顶层，同时保留 `patches`
    // 作为来源记录。这种情况下若再叠加 patches，库会重复、参数会翻倍。
    // 判定标准：所有 patch 的库都已存在于顶层 → patches 只是记录，跳过。
    let mut layers: Vec<Layer> = Vec::new();
    for raw in chain {
        let mut patches: Vec<&Patch> = raw.patches.iter().collect();
        patches.sort_by_key(|p| p.priority);

        if !patches_are_redundant(raw) {
            for patch in patches {
                layers.push(Layer {
                    main_class: patch.main_class.clone(),
                    libraries: patch.libraries.clone(),
                    game_args: patch.arguments_game.clone(),
                    jvm_args: patch.arguments_jvm.clone(),
                    java_version: Some(patch.java_version.clone()),
                    asset_index: None,
                    logging: None,
                    version_type: None,
                    jar: None,
                });
            }
        }

        layers.push(Layer {
            main_class: raw.main_class.clone(),
            libraries: raw.libraries.clone(),
            game_args: raw.arguments_game.clone(),
            jvm_args: raw.arguments_jvm.clone(),
            java_version: Some(raw.java_version.clone()),
            asset_index: Some(raw.asset_index.clone()),
            logging: raw.logging.clone(),
            version_type: raw.version_type.clone(),
            jar: raw.jar.clone(),
        });
    }

    let mut main_class: Option<String> = None;
    let mut asset_index: Option<AssetIndex> = None;
    let mut java_version = JavaVersion::default();
    let mut logging = None;
    let mut version_type = None;
    let mut jar_id = None;

    // 库按坐标去重，后者覆盖前者但保留首次出现的位置。
    let mut lib_order: Vec<String> = Vec::new();
    let mut lib_map: HashMap<String, Library> = HashMap::new();

    let mut game_args: Vec<ArgEntry> = Vec::new();
    let mut jvm_args: Vec<ArgEntry> = Vec::new();

    for layer in layers {
        if let Some(mc) = layer.main_class {
            if !mc.is_empty() {
                main_class = Some(mc);
            }
        }
        // 子层常常只写差异，`assetIndex` 整个缺省；这时不能拿一个空壳
        // 把父版本真正的索引盖掉，否则 `--assetIndex` 会变成空串，
        // 进游戏一片无材质。
        if let Some(ai) = layer.asset_index {
            if !ai.id.is_empty() || ai.url.is_some() {
                asset_index = Some(ai);
            }
        }
        if let Some(jv) = layer.java_version {
            if jv.major_version.is_some() || jv.component.is_some() {
                java_version = jv;
            }
        }
        if layer.logging.is_some() {
            logging = layer.logging;
        }
        if let Some(vt) = layer.version_type {
            version_type = Some(vt);
        }
        if let Some(jar) = layer.jar {
            if !jar.is_empty() {
                jar_id = Some(jar);
            }
        }

        for lib in layer.libraries {
            let key = library_dedupe_key(&lib);
            if !lib_map.contains_key(&key) {
                lib_order.push(key.clone());
            }
            lib_map.insert(key, lib);
        }

        game_args.extend(layer.game_args);
        jvm_args.extend(layer.jvm_args);
    }

    let main_class = main_class.ok_or_else(|| VersionError {
        message: "合并后仍缺少 mainClass".into(),
    })?;

    let asset_index = asset_index.unwrap_or_default();

    let libraries = lib_order
        .into_iter()
        .filter_map(|k| lib_map.remove(&k))
        .collect();

    Ok(ResolvedVersion {
        id: chain
            .last()
            .map(|v| v.id.clone())
            .unwrap_or_default(),
        main_class,
        jar_id,
        asset_index,
        java_version,
        libraries,
        game_args,
        jvm_args,
        logging,
        version_type,
    })
}

/// 判断 `patches` 是否只是「来源记录」而非待合并的增量。
///
/// HMCL 保存扁平化清单时会把合并结果写到顶层字段，同时保留原始 patches
/// 供溯源。识别特征是：**每个 patch 的每一条库都能在顶层找到**。
/// 顶层库是所有 patch 库的超集，说明这份清单已经合并过了。
///
/// 为什么不再比对参数：早期版本还要求「patch 的每个参数都出现在顶层」，
/// 但实测会误判 —— HMCL 合并时可能**改写**参数值。例如 NeoForge 整合包
/// `ATM10 To the Sky` 的 patch 里写的是
///
/// ```text
/// -DignoreList=client-extra,${version_name}.jar
/// ```
///
/// 而顶层已经把它替换成实例的真实 jar 名。签名对不上 → 判定「不冗余」
/// → patches 被整份再叠加一次，game 参数从 45 条翻到 76 条。
///
/// 库的覆盖性足以判断「是否已合并」，参数不必参与判定。
fn patches_are_redundant(raw: &RawVersion) -> bool {
    if raw.patches.is_empty() {
        return false;
    }
    // 顶层库为空，说明 patches 确实需要合并
    if raw.libraries.is_empty() {
        return false;
    }

    // 旧格式（≤1.12.2）的合并结果落在 `minecraftArguments` 字符串里，
    // 其中已经包含各 patch 追加的参数（如 LiteLoader 的 --tweakClass）。
    // 只要顶层有它，就说明这份清单已经合并过了。
    if raw.minecraft_arguments.is_some() {
        return true;
    }

    let top: Vec<&str> = raw.libraries.iter().map(|l| l.name.as_str()).collect();

    for patch in &raw.patches {
        for lib in &patch.libraries {
            if !top.contains(&lib.name.as_str()) {
                // 有 patch 独有的库 -> patches 携带着顶层没有的信息
                return false;
            }
        }
    }

    true
}

/// 计算库的去重键。
///
/// 关键点：**不能只按 `group:artifact` 去重**。
///
/// 老版本清单里常见「同坐标、不同平台」的成对条目，例如 1.8.9：
///
/// ```text
/// org.lwjgl.lwjgl:lwjgl:2.9.4  rules=[allow, disallow osx]   ← Windows 用
/// org.lwjgl.lwjgl:lwjgl:2.9.2  rules=[allow osx]              ← macOS 用
/// ```
///
/// 若按 artifact 去重，后出现的 2.9.2 会覆盖 2.9.4；而在 Windows 上
/// 2.9.2 会被 `allow osx` 规则过滤掉 —— 结果是两条都没了，
/// 启动时报 `NoClassDefFoundError: org/lwjgl/opengl/OpenGLException`。
///
/// 所以去重键要把「针对哪个平台」也算进去：只有 rules 相同、
/// 坐标相同的条目才视为同一个。
fn library_dedupe_key(lib: &crate::version::Library) -> String {
    let base = lib
        .coord
        .as_ref()
        .map(|c| c.dedupe_key())
        .unwrap_or_else(|| lib.name.clone());

    // rules 的语义签名：动作 + 平台 + 架构
    let mut sig = String::new();
    for r in &lib.rules {
        sig.push('|');
        sig.push_str(match r.action {
            crate::rules::Action::Allow => "allow",
            crate::rules::Action::Disallow => "disallow",
        });
        if let Some(os) = &r.os {
            if let Some(n) = &os.name {
                sig.push(':');
                sig.push_str(n);
            }
            if let Some(a) = &os.arch {
                sig.push('/');
                sig.push_str(a);
            }
        }
    }

    if sig.is_empty() {
        base
    } else {
        format!("{}{}", base, sig)
    }
}

struct Layer {    main_class: Option<String>,
    libraries: Vec<Library>,
    game_args: Vec<ArgEntry>,
    jvm_args: Vec<ArgEntry>,
    java_version: Option<JavaVersion>,
    asset_index: Option<AssetIndex>,
    logging: Option<crate::version::LoggingConfig>,
    version_type: Option<String>,
    jar: Option<String>,
}

impl ResolvedVersion {
    /// 在当前上下文中生效的库（已按平台过滤）。
    pub fn effective_libraries(&self, ctx: &RuleContext) -> Vec<&Library> {
        self.libraries
            .iter()
            .filter(|lib| crate::rules::is_allowed(&lib.rules, ctx))
            .collect()
    }

    /// 旧格式兼容：若清单里只有 `minecraftArguments`，把它转成 game 参数。
    pub fn with_legacy_arguments(mut self, legacy: Option<Vec<String>>) -> Self {
        if self.game_args.is_empty() {
            if let Some(tokens) = legacy {
                self.game_args = tokens.into_iter().map(ArgEntry::Plain).collect();
            }
        }
        self
    }
}

/// 从下载信息里取出库文件的具体位置。
pub fn library_download(lib: &Library, ctx: &RuleContext) -> Option<Download> {
    // 本地库：按平台取分类符
    if lib.is_native() {
        let classifier = lib.native_classifier(ctx.os.name.as_str())?;
        if let Some((_, dl)) = lib.classifiers.iter().find(|(k, _)| k == &classifier) {
            return Some(dl.clone());
        }
        // 新格式：坐标自带分类符，主构件即 natives jar
        if let Some(artifact) = &lib.artifact {
            if lib
                .coord
                .as_ref()
                .map(|c| c.classifier.as_deref() == Some(classifier.as_str()))
                .unwrap_or(false)
            {
                return Some(artifact.clone());
            }
        }
        return None;
    }
    lib.artifact.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Arch, Features, OsInfo, OsName};
    use crate::version::parse_version;

    fn win_ctx() -> RuleContext {
        RuleContext::new(
            OsInfo::for_test(OsName::Windows, Arch::X86_64),
            Features::default(),
        )
    }

    /// 加载器的子版本清单通常只写差异，`assetIndex` 整个不写。
    /// 若拿它的空壳去盖父版本，`--assetIndex` 就成了空串，
    /// 进游戏一片无材质——而源头只是「子层没写这个字段」。
    #[test]
    fn an_empty_asset_index_does_not_erase_the_parents() {
        let parent = parse_version(
            r#"{"id":"26.3","mainClass":"M","assetIndex":{"id":"34","url":"https://x/34.json"}}"#,
        )
        .unwrap();
        let child = parse_version(
            r#"{"id":"26.3-Fabric","inheritsFrom":"26.3","mainClass":"C"}"#,
        )
        .unwrap();
        let resolved = resolve(&[parent, child]).unwrap();
        assert_eq!(resolved.asset_index.id, "34");
        assert_eq!(resolved.asset_index.url.as_deref(), Some("https://x/34.json"));
    }

    /// 反过来，子层若真的写了新索引，就该按它来。
    #[test]
    fn a_real_asset_index_in_the_child_still_wins() {
        let parent = parse_version(
            r#"{"id":"p","mainClass":"M","assetIndex":{"id":"5"}}"#,
        )
        .unwrap();
        let child = parse_version(
            r#"{"id":"c","inheritsFrom":"p","assetIndex":{"id":"34","url":"https://x/34.json"}}"#,
        )
        .unwrap();
        let resolved = resolve(&[parent, child]).unwrap();
        assert_eq!(resolved.asset_index.id, "34");
    }

    #[test]
    fn child_overrides_main_class() {        let parent = parse_version(
            r#"{"id":"p","mainClass":"Parent","libraries":[{"name":"a:b:1.0"}]}"#,
        )
        .unwrap();
        let child = parse_version(
            r#"{"id":"c","inheritsFrom":"p","mainClass":"Child","libraries":[{"name":"d:e:2.0"}]}"#,
        )
        .unwrap();
        let resolved = resolve(&[parent, child]).unwrap();
        assert_eq!(resolved.main_class, "Child");
        assert_eq!(resolved.libraries.len(), 2);
        assert_eq!(resolved.id, "c");
    }

    #[test]
    fn child_overrides_same_library_version() {
        let parent = parse_version(
            r#"{"id":"p","mainClass":"M","libraries":[{"name":"g:a:1.0"},{"name":"g:b:1.0"}]}"#,
        )
        .unwrap();
        let child = parse_version(
            r#"{"id":"c","libraries":[{"name":"g:a:2.0"}]}"#,
        )
        .unwrap();
        let resolved = resolve(&[parent, child]).unwrap();
        assert_eq!(resolved.libraries.len(), 2);
        let a = resolved
            .libraries
            .iter()
            .find(|l| l.name.starts_with("g:a"))
            .unwrap();
        assert_eq!(a.name, "g:a:2.0");
        // 首次出现的位置保留
        assert_eq!(resolved.libraries[0].name, "g:a:2.0");
    }

    #[test]
    fn natives_and_plain_jar_coexist() {
        let v = parse_version(
            r#"{"id":"x","mainClass":"M","libraries":[
                {"name":"org.lwjgl:lwjgl:3.3.1"},
                {"name":"org.lwjgl:lwjgl:3.3.1:natives-windows","rules":[{"action":"allow","os":{"name":"windows"}}]}
            ]}"#,
        )
        .unwrap();
        let resolved = resolve(&[v]).unwrap();
        assert_eq!(resolved.libraries.len(), 2, "natives 不应被去重吃掉");
    }

    #[test]
    fn patches_apply_in_priority_order() {
        // fabric patch 优先级高，其库应排在 game 之后但仍被保留
        let v = parse_version(
            r#"{"id":"merged","mainClass":"M","libraries":[],
                "patches":[
                  {"id":"fabric","priority":30000,"libraries":[{"name":"net.fabricmc:loader:1.0"}]},
                  {"id":"game","priority":0,"libraries":[{"name":"net.minecraft:client:1.0"}]}
                ]}"#,
        )
        .unwrap();
        let resolved = resolve(&[v]).unwrap();
        assert_eq!(resolved.libraries.len(), 2);
        assert_eq!(resolved.libraries[0].name, "net.minecraft:client:1.0");
        assert_eq!(resolved.libraries[1].name, "net.fabricmc:loader:1.0");
    }

    #[test]
    fn effective_libraries_filters_by_platform() {
        let v = parse_version(
            r#"{"id":"x","mainClass":"M","libraries":[
                {"name":"g:plain:1"},
                {"name":"g:win:1","rules":[{"action":"allow","os":{"name":"windows"}}]},
                {"name":"g:mac:1","rules":[{"action":"allow","os":{"name":"osx"}}]}
            ]}"#,
        )
        .unwrap();
        let resolved = resolve(&[v]).unwrap();
        let ctx = win_ctx();
        let names: Vec<_> = resolved
            .effective_libraries(&ctx)
            .iter()
            .map(|l| l.name.clone())
            .collect();
        assert_eq!(names, vec!["g:plain:1", "g:win:1"]);
    }

    #[test]
    fn legacy_arguments_used_when_no_game_args() {
        let v = parse_version(
            r#"{"id":"1.12.2","mainClass":"M","minecraftArguments":"--username ${auth_player_name}"}"#,
        )
        .unwrap();
        let legacy = v.minecraft_arguments.clone();
        let resolved = resolve(&[v]).unwrap();
        assert!(resolved.game_args.is_empty());
        let resolved = resolved.with_legacy_arguments(legacy);
        assert_eq!(resolved.game_args.len(), 2);
    }

    #[test]
    fn missing_main_class_is_an_error() {
        let v = parse_version(r#"{"id":"x","libraries":[]}"#).unwrap();
        assert!(resolve(&[v]).is_err());
    }

    /// HMCL 扁平清单：顶层已是合并结果，patches 只是来源记录。
    /// 若不识别，库会重复、JVM 参数会翻倍（实际踩到过）。
    /// 同坐标、不同平台的条目必须共存。
    ///
    /// 1.8.9 的真实形态：2.9.4 用于 Windows，2.9.2 仅用于 macOS。
    /// 若按 artifact 去重，后者会覆盖前者；而后者在 Windows 上会被
    /// rules 过滤，结果两条都没了，启动报 NoClassDefFoundError。
    #[test]
    fn same_artifact_different_platform_coexist() {
        let v = parse_version(
            r#"{"id":"1.8.9","mainClass":"M","libraries":[
                {"name":"org.lwjgl.lwjgl:lwjgl:2.9.4",
                 "rules":[{"action":"allow"},{"action":"disallow","os":{"name":"osx"}}]},
                {"name":"org.lwjgl.lwjgl:lwjgl:2.9.2",
                 "rules":[{"action":"allow","os":{"name":"osx"}}]}
            ]}"#,
        )
        .unwrap();
        let resolved = resolve(&[v]).unwrap();
        assert_eq!(
            resolved.libraries.len(),
            2,
            "两个平台的变体都应保留，实际只有 {:?}",
            resolved.libraries.iter().map(|l| &l.name).collect::<Vec<_>>()
        );

        // Windows 上应命中 2.9.4
        let ctx = win_ctx();
        let names: Vec<_> = resolved
            .effective_libraries(&ctx)
            .iter()
            .map(|l| l.name.clone())
            .collect();
        assert_eq!(names, vec!["org.lwjgl.lwjgl:lwjgl:2.9.4"]);
    }

    /// 但坐标与 rules 都相同、只是版本不同的，仍应后者覆盖前者。
    #[test]
    fn later_version_wins_when_rules_identical() {
        let v = parse_version(
            r#"{"id":"x","mainClass":"M","libraries":[
                {"name":"g:a:1.0"},
                {"name":"g:a:2.0"}
            ]}"#,
        )
        .unwrap();
        let resolved = resolve(&[v]).unwrap();
        assert_eq!(resolved.libraries.len(), 1);
        assert_eq!(resolved.libraries[0].name, "g:a:2.0");
    }

    #[test]
    fn redundant_patches_are_not_applied_twice() {
        let v = parse_version(
            r#"{"id":"merged","mainClass":"M",
                "libraries":[{"name":"net.minecraft:client:1.0"},{"name":"net.fabricmc:loader:1.0"}],
                "arguments":{"jvm":["-cp","${classpath}"],"game":["--username","${auth_player_name}"]},
                "patches":[
                  {"id":"game","priority":0,
                   "libraries":[{"name":"net.minecraft:client:1.0"}],
                   "arguments":{"jvm":["-cp","${classpath}"]}},
                  {"id":"fabric","priority":30000,
                   "libraries":[{"name":"net.fabricmc:loader:1.0"}],
                   "arguments":{"game":["--username","${auth_player_name}"]}}
                ]}"#,
        )
        .unwrap();
        let resolved = resolve(&[v]).unwrap();
        assert_eq!(resolved.libraries.len(), 2, "顶层已合并，不应重复叠加");
        assert_eq!(resolved.jvm_args.len(), 2, "jvm 参数不应翻倍");
        assert_eq!(resolved.game_args.len(), 2, "game 参数不应翻倍");
    }

    /// 反过来：patches 里确有顶层没有的东西时，必须合并。
    #[test]
    fn non_redundant_patches_still_merge() {
        let v = parse_version(
            r#"{"id":"merged","mainClass":"M",
                "libraries":[{"name":"net.minecraft:client:1.0"}],
                "patches":[
                  {"id":"game","priority":0,"libraries":[{"name":"net.minecraft:client:1.0"}]},
                  {"id":"fabric","priority":30000,"libraries":[{"name":"net.fabricmc:loader:1.0"}]}
                ]}"#,
        )
        .unwrap();
        let resolved = resolve(&[v]).unwrap();
        assert_eq!(resolved.libraries.len(), 2);
        assert!(resolved.libraries.iter().any(|l| l.name.contains("fabricmc")));
    }

    /// 回归：patch 的参数与顶层「值被改写」时，不应因此误判为不冗余。
    ///
    /// 真实场景（ATM10 To the Sky）：patch 里写
    /// `-DignoreList=client-extra,${version_name}.jar`，
    /// 而顶层已把它替换为实例真实 jar 名。按参数签名比对会失败，
    /// 导致 patches 被整份再叠加一次，game 参数从 45 条翻到 76 条。
    #[test]
    fn rewritten_patch_argument_does_not_force_merge() {
        let v = parse_version(
            r#"{"id":"ATM10","mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher",
                "libraries":[{"name":"net.neoforged:forge:1.0"}],
                "arguments":{
                  "game":["--launchTarget","forgeclient"],
                  "jvm":["-DignoreList=client-extra,ATM10.jar","-p","${library_directory}"]
                },
                "patches":[
                  {"id":"game","priority":0,"libraries":[{"name":"net.neoforged:forge:1.0"}]},
                  {"id":"neoforge","priority":30000,"libraries":[{"name":"net.neoforged:forge:1.0"}],
                   "arguments":{
                     "game":["--launchTarget","forgeclient"],
                     "jvm":["-DignoreList=client-extra,${version_name}.jar","-p","${library_directory}"]
                   }}
                ]}"#,
        )
        .unwrap();
        let resolved = resolve(&[v]).unwrap();

        // 顶层已合并，patches 不应再叠加
        assert_eq!(
            resolved.game_args.len(),
            2,
            "game 参数被重复叠加: {:?}",
            resolved.game_args.len()
        );
        assert_eq!(
            resolved.jvm_args.len(),
            3,
            "jvm 参数被重复叠加: {}",
            resolved.jvm_args.len()
        );
        assert_eq!(resolved.libraries.len(), 1);
    }
}
