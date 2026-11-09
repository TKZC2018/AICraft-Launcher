//! Maven 坐标解析。
//!
//! 版本清单里的 `libraries[].name` 形如：
//!   `org.ow2.asm:asm:9.8`
//!   `org.lwjgl:lwjgl:3.3.1:natives-windows`
//!   `com.example:lib:1.0@zip`
//!
//! 展开成仓库内的相对路径时，group 的点号要换成斜杠。

use std::fmt;

/// 一个解析后的 Maven 坐标。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MavenCoord {
    pub group: String,
    pub artifact: String,
    pub version: String,
    /// 无分类符时为 None。
    pub classifier: Option<String>,
    /// 扩展名，默认 "jar"。
    pub extension: String,
}

impl MavenCoord {
    /// 解析坐标字符串。格式不合法时返回 None。
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }

        // 扩展名可以跟在 '@' 之后
        let (body, extension) = match text.rsplit_once('@') {
            Some((b, ext)) if !ext.is_empty() => (b, ext.to_string()),
            _ => (text, "jar".to_string()),
        };

        let parts: Vec<&str> = body.split(':').collect();
        if parts.len() < 3 {
            return None;
        }
        // 至少 group:artifact:version，第四个是分类符
        let group = parts[0];
        let artifact = parts[1];
        let version = parts[2];
        if group.is_empty() || artifact.is_empty() || version.is_empty() {
            return None;
        }
        let classifier = if parts.len() >= 4 && !parts[3].is_empty() {
            Some(parts[3].to_string())
        } else {
            None
        };

        Some(MavenCoord {
            group: group.to_string(),
            artifact: artifact.to_string(),
            version: version.to_string(),
            classifier,
            extension,
        })
    }

    /// 仓库内的相对路径（用 '/' 分隔）。
    pub fn to_path(&self) -> String {
        let group_path = self.group.replace('.', "/");
        let mut file = format!("{}-{}", self.artifact, self.version);
        if let Some(classifier) = &self.classifier {
            file.push('-');
            file.push_str(classifier);
        }
        file.push('.');
        file.push_str(&self.extension);
        format!("{}/{}/{}/{}", group_path, self.artifact, self.version, file)
    }

    /// 用于去重的键：group:artifact[:classifier]。
    ///
    /// 刻意不含 version —— 同一坐标的多个版本在继承链里应当后者覆盖前者。
    /// 但必须含 classifier，否则 `lwjgl:3.3.1` 与 `lwjgl:3.3.1:natives-windows`
    /// 会被误判成同一条而丢掉平台本地库。
    pub fn dedupe_key(&self) -> String {
        match &self.classifier {
            Some(c) => format!("{}:{}:{}", self.group, self.artifact, c),
            None => format!("{}:{}", self.group, self.artifact),
        }
    }

    /// 是否为平台本地库（分类符以 `natives-` 开头）。
    pub fn is_native(&self) -> bool {
        self.classifier
            .as_deref()
            .map(|c| c.starts_with("natives-"))
            .unwrap_or(false)
    }
}

impl fmt::Display for MavenCoord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.artifact, self.version)?;
        if let Some(c) = &self.classifier {
            write!(f, ":{}", c)?;
        }
        if self.extension != "jar" {
            write!(f, "@{}", self.extension)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_coordinate() {
        let c = MavenCoord::parse("org.ow2.asm:asm:9.8").unwrap();
        assert_eq!(c.group, "org.ow2.asm");
        assert_eq!(c.artifact, "asm");
        assert_eq!(c.version, "9.8");
        assert_eq!(c.classifier, None);
        assert_eq!(c.extension, "jar");
        assert!(!c.is_native());
        assert_eq!(c.to_path(), "org/ow2/asm/asm/9.8/asm-9.8.jar");
    }

    #[test]
    fn parses_classifier() {
        let c = MavenCoord::parse("org.lwjgl:lwjgl:3.3.1:natives-windows").unwrap();
        assert_eq!(c.classifier.as_deref(), Some("natives-windows"));
        assert!(c.is_native());
        assert_eq!(
            c.to_path(),
            "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows.jar"
        );
    }

    #[test]
    fn parses_extension() {
        let c = MavenCoord::parse("com.example:lib:1.0@zip").unwrap();
        assert_eq!(c.extension, "zip");
        assert_eq!(c.to_path(), "com/example/lib/1.0/lib-1.0.zip");
    }

    #[test]
    fn dedupe_key_keeps_classifier() {
        let plain = MavenCoord::parse("org.lwjgl:lwjgl:3.3.1").unwrap();
        let native = MavenCoord::parse("org.lwjgl:lwjgl:3.3.1:natives-windows").unwrap();
        assert_ne!(plain.dedupe_key(), native.dedupe_key());
    }

    #[test]
    fn rejects_malformed() {
        assert!(MavenCoord::parse("only:two").is_none());
        assert!(MavenCoord::parse("").is_none());
        assert!(MavenCoord::parse("::1.0").is_none());
    }
}
