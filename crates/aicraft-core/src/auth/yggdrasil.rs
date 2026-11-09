//! 第三方登录（authlib-injector / Yggdrasil 协议）。
//!
//! 除了微软正版，Minecraft 生态里还有一批「自己搭认证服务器」的皮肤站
//! （LittleSkin、 Blessing Skin 等）。它们的登录接口遵循 Yggdrasil 协议，
//! 而客户端要能认出这些令牌，就得在启动时挂上 authlib-injector：
//!
//! ```text
//! -javaagent:<authlib-injector.jar>=<api 根地址>
//! ```
//!
//! 挂上之后，游戏把认证请求发给那台服务器；`user_type` 必须是 `mojang`
//! （不是 `msa`，也不是 `legacy`），否则 authlib-injector 不会接管。
//!
//! 这里只做四件事：规范化根地址、登录、续期、把注入器下载到数据目录。

use std::path::{Path, PathBuf};

use crate::json;
use crate::transport;

use super::{AuthError, AuthErrorKind, Http};

/// authlib-injector 的发布信息（官方源）。
pub const INJECTOR_LATEST: &str = "https://authlib-injector.yushi.moe/artifact/latest.json";
/// 同一份发布信息的 BMCLAPI 镜像。
pub const INJECTOR_LATEST_MIRROR: &str =
    "https://bmclapi2.bangbang93.com/mirrors/authlib-injector/artifact/latest.json";
/// 注入器的文件名前缀，落在数据目录下。
pub const INJECTOR_FILE_STEM: &str = "authlib-injector";

/// 一次第三方登录的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// 认证服务器根地址（已规范化，不以 `/` 结尾）。
    pub api_root: String,
    /// 游戏内名字。
    pub name: String,
    /// 32 位无连字符 UUID。
    pub uuid: String,
    /// 访问令牌。
    pub access_token: String,
    /// 客户端令牌；续期时要原样带回去。
    pub client_token: String,
}

impl Session {
    /// 转成启动器认识的账户结构。
    ///
    /// `user_type` 固定 `mojang`：authlib-injector 就是靠这个值判断
    /// 「这个会话该由我接管」。写成 `msa` 会让游戏按微软流程去验证，
    /// 令牌立刻被判无效。
    pub fn to_account(&self) -> crate::launch::Account {
        crate::launch::Account {
            username: self.name.clone(),
            uuid: self.uuid.clone(),
            access_token: self.access_token.clone(),
            user_type: "mojang".to_string(),
            demo: false,
            api_root: Some(self.api_root.clone()),
        }
    }
}

/// 把玩家填的地址整理成根地址。
///
/// 容忍这些写法（都是皮肤站文档里实际见过的）：
///
/// - `https://littleskin.cn` → 补上 `/api/yggdrasil`
/// - `https://littleskin.cn/api/yggdrasil/` → 去掉尾斜杠
/// - `https://littleskin.cn/api/yggdrasil/authserver/authenticate` → 砍掉多余路径
pub fn normalize_api_root(input: &str) -> Result<String, String> {
    let text = input.trim();
    if text.is_empty() {
        return Err("先填认证服务器地址".to_string());
    }
    let (scheme, rest) = match text.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r.to_string()),
        None => ("https".to_string(), text.to_string()),
    };
    if scheme != "http" && scheme != "https" {
        return Err("地址要以 http:// 或 https:// 开头".to_string());
    }

    let rest = rest.trim_end_matches('/');
    // 砍掉已经写进路径里的端点后缀
    let mut base = rest;
    for tail in [
        "/authserver/authenticate",
        "/authserver/refresh",
        "/authserver/validate",
        "/authserver",
        "/sessionserver",
        "/minecraft/profile",
    ] {
        if let Some(trimmed) = base.strip_suffix(tail) {
            base = trimmed.trim_end_matches('/');
            break;
        }
    }
    if base.is_empty() {
        return Err("地址里没有主机名".to_string());
    }

    // 只有主机名时补上 LittleSkin 系的惯例路径
    let host_part = base.split('/').next().unwrap_or("");
    let has_path = base.len() > host_part.len();
    let root = if has_path {
        base.to_string()
    } else {
        format!("{}/api/yggdrasil", base)
    };
    Ok(format!("{}://{}", scheme, root))
}

/// 生成一个 clientToken。
///
/// Yggdrasil 要求客户端自己出一个令牌，续期时原样带回去 —— 服务器靠它
/// 区分「同一个客户端的多次登录」。这里不用 UUID：随机 16 字节转十六进制
/// 就够，且不必为格式化再引一个依赖。
fn client_token() -> String {
    super::random_bytes(16)
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

/// 认证接口的完整地址。
pub fn endpoint(api_root: &str, path: &str) -> String {
    format!("{}/{}", api_root.trim_end_matches('/'), path)
}

/// 登录：`POST {root}/authserver/authenticate`。
pub fn authenticate(
    http: &dyn Http,
    api_root: &str,
    username: &str,
    password: &str,
) -> Result<Session, AuthError> {
    let root = normalize_api_root(api_root).map_err(|detail| {
        AuthError::new(AuthErrorKind::Protocol, detail, String::new())
    })?;
    if username.trim().is_empty() {
        return Err(AuthError::new(
            AuthErrorKind::Protocol,
            "先填邮箱或用户名",
            String::new(),
        ));
    }

    let body = format!(
        "{{\"username\":\"{}\",\"password\":\"{}\",\"clientToken\":\"{}\",\"agent\":{{\"name\":\"Minecraft\",\"version\":1}}}}",
        super::json_escape(username.trim()),
        super::json_escape(password),
        super::json_escape(&client_token()),
    );

    let url = endpoint(&root, "authserver/authenticate");
    let response = http.post_json(&url, &body)?;
    let ok = response.is_success();
    let text = response.body;

    if !ok {
        return Err(server_error(&root, &text));
    }
    parse_session(&root, &text)
}

/// 续期：`POST {root}/authserver/refresh`。
pub fn refresh(
    http: &dyn Http,
    api_root: &str,
    access_token: &str,
    client_token: &str,
) -> Result<Session, AuthError> {
    let root = normalize_api_root(api_root).map_err(|detail| {
        AuthError::new(AuthErrorKind::Protocol, detail, String::new())
    })?;
    let body = format!(
        "{{\"accessToken\":\"{}\",\"clientToken\":\"{}\"}}",
        super::json_escape(access_token),
        super::json_escape(client_token),
    );
    let url = endpoint(&root, "authserver/refresh");
    let response = http.post_json(&url, &body)?;
    let ok = response.is_success();
    let text = response.body;
    if !ok {
        return Err(server_error(&root, &text));
    }
    parse_session(&root, &text)
}

/// 从响应正文里读出一个会话。
pub fn parse_session(api_root: &str, text: &str) -> Result<Session, AuthError> {
    let value = json::parse(text).map_err(|e| {
        AuthError::new(
            AuthErrorKind::Protocol,
            format!("服务器返回的不是 JSON：{}", e),
            text.to_string(),
        )
    })?;

    let access_token = value
        .get("accessToken")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if access_token.is_empty() {
        return Err(AuthError::new(
            AuthErrorKind::Protocol,
            "响应里没有 accessToken",
            text.to_string(),
        ));
    }
    let client_token = value
        .get("clientToken")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let profile = value.get("selectedProfile");
    let name = profile
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let uuid = profile
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if name.is_empty() || uuid.is_empty() {
        return Err(AuthError::new(
            AuthErrorKind::Protocol,
            "这个账号还没有创建角色：先在认证服务器的网站上建一个",
            text.to_string(),
        ));
    }

    Ok(Session {
        api_root: api_root.to_string(),
        name,
        uuid,
        access_token,
        client_token,
    })
}

/// 服务器拒绝时，把它的错误说明尽量读出来。
///
/// Yggdrasil 的错误有两种形状：直接 `{"error":"ForbiddenOperationException",
/// "errorMessage":"..."}`，或者包在 `{"error":{...}}` 里。两种都试一遍，
/// 读不出来就退回状态码。
fn server_error(api_root: &str, text: &str) -> AuthError {
    let mut detail = format!("{} 拒绝了这次登录", api_root);
    if let Ok(value) = json::parse(text) {
        let error = value.get("error");
        let message = error
            .and_then(|e| e.get("errorMessage"))
            .or_else(|| value.get("errorMessage"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if !message.is_empty() {
            detail = message;
        } else if let Some(kind) = error.and_then(|e| e.as_str()) {
            detail = format!("{}（{}）", detail, kind);
        }
    }
    AuthError::new(AuthErrorKind::Http, detail, text.to_string())
}

/// 注入器 jar 的落盘位置。
pub fn injector_path(data_dir: &Path) -> PathBuf {
    data_dir.join(format!("{}.jar", INJECTOR_FILE_STEM))
}

/// 确保数据目录里有 authlib-injector，返回它的路径。
///
/// 已经有一份就不再下载 —— 注入器与小版本关系不大，1.2.x 通吃
/// 2013 年之后的客户端。`mirror` 为真时先试 BMCLAPI。
pub fn ensure_injector(data_dir: &Path, mirror: bool) -> Result<PathBuf, String> {
    let dest = injector_path(data_dir);
    if dest.is_file() {
        return Ok(dest);
    }
    std::fs::create_dir_all(data_dir)
        .map_err(|e| format!("创建 {} 失败：{}", data_dir.display(), e))?;

    let sources: Vec<&str> = if mirror {
        vec![INJECTOR_LATEST_MIRROR, INJECTOR_LATEST]
    } else {
        vec![INJECTOR_LATEST, INJECTOR_LATEST_MIRROR]
    };

    let mut last_error = String::new();
    for source in sources {
        let text = match transport::fetch_text(source) {
            Ok(t) => t,
            Err(e) => {
                last_error = format!("{}：{}", source, e.message);
                continue;
            }
        };
        let url = match json::parse(&text)
            .ok()
            .and_then(|v| v.get("download_url").and_then(|u| u.as_str()).map(|s| s.to_string()))
        {
            Some(url) => url,
            None => {
                last_error = format!("{}：发布信息里没有 download_url", source);
                continue;
            }
        };
        match transport::download_to(&url, &dest, None) {
            Ok(_) => return Ok(dest),
            Err(e) => last_error = format!("{}：{}", url, e.message),
        }
    }

    Err(format!("下载 authlib-injector 失败：{}", last_error))
}

/// 启动时注入的那个参数。
pub fn agent_argument(injector: &Path, api_root: &str) -> String {
    format!("-javaagent:{}={}", injector.display(), api_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_normalised() {
        assert_eq!(
            normalize_api_root("https://littleskin.cn").unwrap(),
            "https://littleskin.cn/api/yggdrasil"
        );
        assert_eq!(
            normalize_api_root("littleskin.cn").unwrap(),
            "https://littleskin.cn/api/yggdrasil"
        );
        assert_eq!(
            normalize_api_root("https://littleskin.cn/api/yggdrasil/").unwrap(),
            "https://littleskin.cn/api/yggdrasil"
        );
        assert_eq!(
            normalize_api_root("https://littleskin.cn/api/yggdrasil/authserver/authenticate")
                .unwrap(),
            "https://littleskin.cn/api/yggdrasil"
        );
        // 自建站常常把 yggdrasil 挂在别的路径上，路径要原样留着
        assert_eq!(
            normalize_api_root("http://mc.example.com/yggdrasil").unwrap(),
            "http://mc.example.com/yggdrasil"
        );
        assert!(normalize_api_root("").is_err());
        assert!(normalize_api_root("ftp://x/y").is_err());
    }

    #[test]
    fn a_session_is_read_from_the_response() {
        let text = r#"{"accessToken":"abc","clientToken":"cli","availableProfiles":[{"id":"4f1e","name":"Steve"}],"selectedProfile":{"id":"4f1e","name":"Steve"}}"#;
        let session = parse_session("https://x/api/yggdrasil", text).unwrap();
        assert_eq!(session.name, "Steve");
        assert_eq!(session.uuid, "4f1e");
        assert_eq!(session.access_token, "abc");
        assert_eq!(session.client_token, "cli");
        let account = session.to_account();
        assert_eq!(account.user_type, "mojang");
        assert_eq!(account.api_root.as_deref(), Some("https://x/api/yggdrasil"));
    }

    #[test]
    fn a_session_without_a_profile_is_explained() {
        let text = r#"{"accessToken":"abc","clientToken":"cli","availableProfiles":[]}"#;
        let error = parse_session("https://x/api/yggdrasil", text).unwrap_err();
        assert!(error.detail.contains("还没有创建角色"), "{}", error.detail);
    }

    #[test]
    fn server_errors_prefer_their_own_message() {
        let text = r#"{"error":"ForbiddenOperationException","errorMessage":"密码不对"}"#;
        let error = server_error("https://x", text);
        assert_eq!(error.detail, "密码不对");
        let nested = r#"{"error":{"errorMessage":"该角色已被封禁"}}"#;
        assert_eq!(server_error("https://x", nested).detail, "该角色已被封禁");
    }

    #[test]
    fn the_agent_argument_points_at_the_injector() {
        let path = PathBuf::from(r"C:\data\authlib-injector.jar");
        assert_eq!(
            agent_argument(&path, "https://littleskin.cn/api/yggdrasil"),
            r"-javaagent:C:\data\authlib-injector.jar=https://littleskin.cn/api/yggdrasil"
        );
    }
}
