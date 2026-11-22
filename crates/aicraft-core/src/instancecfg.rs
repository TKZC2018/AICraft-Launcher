//! 单个实例自己的游戏设置（Java、内存、JVM 参数）。
//!
//! 默认全部沿用全局设置；玩家在「管理 → 游戏设置」里取消勾选「沿用全局设置」
//! 之后，这一份才开始有自己的一套。
//!
//! 存在 `versions/<实例>/aicraft-instance.json`，跟实例目录待在一起：整个
//! `versions/` 拷到别的机器上，这些设置也跟着走。没有放进全局的 `Settings`
//! 里维护一张「按名字索引」的表——实例可以随手改名、随手删，那张表迟早会
//! 攒下一堆对不上号的死项。

use std::path::{Path, PathBuf};

use crate::json::{self, Json};

/// 配置文件名。放在实例目录（`versions/<实例>/`）下。
pub const FILE_NAME: &str = "aicraft-instance.json";

/// 一份实例自己的游戏设置。
///
/// `follow_global` 为真时其余字段一律不用——保留着是为了玩家把勾点回去
/// 再点回来时，原来填的那些还在。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceConfig {
    /// 沿用全局设置。
    pub follow_global: bool,
    /// 内存自动分配（按本机物理内存算推荐值）。
    pub auto_memory: bool,
    /// 手动指定时的堆大小（MB）。
    pub max_memory_mb: u32,
    /// 额外 JVM 参数，空白分隔。
    pub jvm_args: String,
    /// 指定的 Java；`None` = 按版本要求自动挑。
    pub java_path: Option<PathBuf>,
    /// 这个实例是从整合包导入来的。
    ///
    /// 整合包自己写着「我是 1.20.1 + Fabric 0.16.10」，那套组合是打包的人
    /// 验过的；换掉游戏版本或者加载器种类，包里的模组大概率当场报错。
    /// 所以这类实例把这两项锁住，只留「换加载器版本」这一条路。
    pub from_pack: bool,
}

impl Default for InstanceConfig {
    fn default() -> Self {
        InstanceConfig {
            follow_global: true,
            auto_memory: true,
            max_memory_mb: 4096,
            jvm_args: String::new(),
            java_path: None,
            from_pack: false,
        }
    }
}

impl InstanceConfig {
    /// 实际要传给游戏的堆大小（MB）。
    ///
    /// 自动模式按本机内存现算；探不到就先用手动那个值，免得在「还没量完」
    /// 的窗口里给出一个 0。
    pub fn memory_mb(&self) -> u32 {
        crate::meminfo::effective_mb(self.auto_memory, self.max_memory_mb)
    }

    /// 把额外 JVM 参数文本切成参数列表。
    pub fn extra_jvm_args(&self) -> Vec<String> {
        self.jvm_args
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    /// 收进合法范围。堆大小与全局设置同一口径（512 MiB ~ 64 GiB）。
    pub fn clamp(&mut self) {
        self.max_memory_mb = self.max_memory_mb.clamp(512, 65536);
        self.jvm_args = self.jvm_args.trim().to_string();
    }

    pub fn to_json(&self) -> String {
        let java = match &self.java_path {
            Some(path) => format!("\"{}\"", crate::auth::json_escape(&path.display().to_string())),
            None => "null".to_string(),
        };
        format!(
            "{{\n  \"follow_global\": {},\n  \"auto_memory\": {},\n  \"max_memory_mb\": {},\n  \"jvm_args\": \"{}\",\n  \"java_path\": {},\n  \"from_pack\": {}\n}}\n",
            self.follow_global,
            self.auto_memory,
            self.max_memory_mb,
            crate::auth::json_escape(&self.jvm_args),
            java,
            self.from_pack,
        )
    }

    pub fn from_json(text: &str) -> Result<InstanceConfig, String> {
        let root = json::parse(text).map_err(|e| e.to_string())?;
        if root.as_object().is_none() {
            return Err("实例设置文件的根节点不是对象".to_string());
        }
        let mut cfg = InstanceConfig::default();
        if let Some(flag) = root.get("follow_global").and_then(Json::as_bool) {
            cfg.follow_global = flag;
        }
        if let Some(flag) = root.get("auto_memory").and_then(Json::as_bool) {
            cfg.auto_memory = flag;
        }
        if let Some(n) = root.get("max_memory_mb").and_then(Json::as_i64) {
            cfg.max_memory_mb = n.max(0) as u32;
        }
        if let Some(text) = root.get("jvm_args").and_then(Json::as_str) {
            cfg.jvm_args = text.to_string();
        }
        if let Some(path) = root.get("java_path").and_then(Json::as_str) {
            let path = path.trim();
            if !path.is_empty() {
                cfg.java_path = Some(PathBuf::from(path));
            }
        }
        if let Some(flag) = root.get("from_pack").and_then(Json::as_bool) {
            cfg.from_pack = flag;
        }
        cfg.clamp();
        Ok(cfg)
    }
}

/// 这份实例的设置文件在哪。
pub fn path(game_dir: &Path, instance: &str) -> PathBuf {
    crate::instance::version_dir(game_dir, instance).join(FILE_NAME)
}

/// 读一份实例设置。文件不存在（或读坏了）就当「沿用全局」。
///
/// 读坏时不能报错打断启动：设置文件是附属品，坏了顶多退回全局那一套，
/// 而启动本身不该因此失败。
pub fn load(game_dir: &Path, instance: &str) -> InstanceConfig {
    match std::fs::read_to_string(path(game_dir, instance)) {
        Ok(text) => InstanceConfig::from_json(&text).unwrap_or_default(),
        Err(_) => InstanceConfig::default(),
    }
}

/// 写一份实例设置。先写临时文件再改名，写一半断电不会留下半个文件。
pub fn save(game_dir: &Path, instance: &str, config: &InstanceConfig) -> Result<(), String> {
    let mut clean = config.clone();
    clean.clamp();
    let file = path(game_dir, instance);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建实例目录失败: {}", e))?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, clean.to_json()).map_err(|e| format!("写实例设置失败: {}", e))?;
    std::fs::rename(&tmp, &file).map_err(|e| format!("保存实例设置失败: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::fresh_dir;

    #[test]
    fn a_pack_instance_remembers_where_it_came_from() {
        let dir = fresh_dir("instancecfg-from-pack");
        let config = InstanceConfig {
            from_pack: true,
            ..InstanceConfig::default()
        };
        save(&dir, "1.20.1-Fabric", &config).expect("save");
        assert!(load(&dir, "1.20.1-Fabric").from_pack);
        // 老文件里没有这个字段：读出来是 false，不能因此当成「从包里来的」。
        let old = fresh_dir("instancecfg-old-file");
        let file = path(&old, "26.3");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"follow_global": false}"#).unwrap();
        assert!(!load(&old, "26.3").from_pack);
    }

    #[test]
    fn a_missing_file_means_follow_the_global_settings() {
        let dir = fresh_dir("instancecfg-missing");
        let config = load(&dir, "1.20.1");
        assert!(config.follow_global);
        assert!(config.auto_memory);
    }

    #[test]
    fn what_is_written_can_be_read_back() {
        let dir = fresh_dir("instancecfg-roundtrip");
        let config = InstanceConfig {
            follow_global: false,
            auto_memory: false,
            max_memory_mb: 6144,
            jvm_args: "-XX:+UseG1GC".to_string(),
            java_path: Some(PathBuf::from(r"C:\Program Files\Java\jdk-21\bin\javaw.exe")),
            from_pack: true,
        };
        save(&dir, "1.20.1-Fabric", &config).expect("save");
        let back = load(&dir, "1.20.1-Fabric");
        assert_eq!(back, config);
    }

    #[test]
    fn settings_stay_inside_the_instance_folder() {
        let dir = fresh_dir("instancecfg-place");
        let file = path(&dir, "26.3");
        assert!(file.ends_with(Path::new("26.3").join(FILE_NAME)));
    }

    #[test]
    fn a_broken_file_falls_back_instead_of_failing_the_launch() {
        let dir = fresh_dir("instancecfg-broken");
        let file = path(&dir, "26.3");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{ this is not json").unwrap();
        assert!(load(&dir, "26.3").follow_global);
    }

    #[test]
    fn silly_numbers_are_pulled_back_into_range() {
        let mut config = InstanceConfig {
            max_memory_mb: 3,
            ..InstanceConfig::default()
        };
        config.clamp();
        assert_eq!(config.max_memory_mb, 512);

        let mut config = InstanceConfig {
            max_memory_mb: 999_999,
            ..InstanceConfig::default()
        };
        config.clamp();
        assert_eq!(config.max_memory_mb, 65536);
    }

    #[test]
    fn extra_arguments_are_split_on_whitespace() {
        let config = InstanceConfig {
            jvm_args: "  -XX:+UseG1GC   -Dfile.encoding=UTF-8 ".to_string(),
            ..InstanceConfig::default()
        };
        assert_eq!(
            config.extra_jvm_args(),
            vec!["-XX:+UseG1GC", "-Dfile.encoding=UTF-8"]
        );
    }
}
