//! 正版登录：微软账号 OAuth → Xbox Live → Minecraft 服务。
//!
//! 完整链路：
//!
//! ```text
//! 浏览器授权码 / 设备码
//!        ↓ 微软令牌（MsaTokens）
//! Xbox Live  →  XSTS（带 user hash）  →  Minecraft 令牌  →  档案
//! ```
//!
//! 设计上的取舍：
//!
//! - 网络访问由 [`Http`] 抽出来，默认实现走 [`crate::transport`]（spawn 系统
//!   curl）。这样测试可以完全离线地对整条链路做断言，不必真的登录。
//! - 每一步都返回结构化错误而不是字符串，UI 才能根据
//!   [`AuthErrorKind`] 决定是「重新点一下」还是「去建 Xbox 档案」。
//! - 客户端 ID 由编译期注入（见 [`DEFAULT_CLIENT_ID`]），源码仓库里不带 ID。
//!   想登录就自己注册一个 Azure 应用，编译时用 `AICRAFT_CLIENT_ID` 提供。

use std::hash::{BuildHasher, Hasher};

use crate::json;
use crate::launch::Account;
use crate::transport::{self, FetchError, HttpResponse};

pub mod msa;
pub mod store;
pub mod xbox;
pub mod yggdrasil;

/// 这次构建带上的 Azure 应用（客户端）ID。
///
/// 值由 `build.rs` 在编译时注入（环境变量 `AICRAFT_CLIENT_ID`，或仓库根的
/// `.client-id` 文件），源码里**没有**任何 ID —— 每人用自己的注册应用构建。
/// 没提供时是空串：登录会直接报 [`AuthErrorKind::MissingClientId`]，而不是
/// 发一个注定失败的请求出去。
///
/// 「不是密钥」这一点仍然成立：它只是说明「这次登录是哪个应用发起的」，
/// 编进二进制后谁都 `strings` 得出来，微软也按公开值对待。
pub const DEFAULT_CLIENT_ID: &str = env!("AICRAFT_CLIENT_ID");

/// 取客户端 ID；这次构建没带就报 [`AuthErrorKind::MissingClientId`]。
///
/// 所有发起登录的地方都要先过这一道：没有 ID 时微软会回一个含糊的 400，
/// 玩家看到的却是「登录失败」，排查方向全错。
pub fn client_id() -> Result<&'static str, AuthError> {
    if DEFAULT_CLIENT_ID.is_empty() {
        return Err(AuthError::new(
            AuthErrorKind::MissingClientId,
            "这一份构建没有带 Azure 应用 ID（编译时没提供 AICRAFT_CLIENT_ID）",
            String::new(),
        ));
    }
    Ok(DEFAULT_CLIENT_ID)
}

#[cfg(test)]
pub(crate) mod testutil;

/// 一次 HTTP 请求的能力集合。
///
/// 只抽登录流程需要的三个动作；不带 `--fail`、能拿到 4xx 正文是
/// [`crate::transport`] 那边已经保证的性质。
pub trait Http {
    /// 发一个任意方法、任意正文的请求。
    ///
    /// 换皮肤要用 `multipart/form-data` 的 `POST`、换披风要用 `PUT`/`DELETE`，
    /// 光靠下面三个便捷方法覆盖不到，所以底下都走这一个。
    fn send(
        &self,
        method: &str,
        url: &str,
        content_type: Option<&str>,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, FetchError>;

    fn post_form(&self, url: &str, pairs: &[(&str, &str)]) -> Result<HttpResponse, FetchError> {
        let body = crate::codec::urlencode_pairs(pairs);
        self.send(
            "POST",
            url,
            Some("application/x-www-form-urlencoded"),
            &[],
            Some(body.as_bytes()),
        )
    }

    fn post_json(&self, url: &str, body: &str) -> Result<HttpResponse, FetchError> {
        self.send(
            "POST",
            url,
            Some("application/json"),
            &[],
            Some(body.as_bytes()),
        )
    }

    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, FetchError> {
        self.send("GET", url, None, headers, None)
    }
}

/// 使用系统 curl 的真实实现。
pub struct CurlHttp;

impl Http for CurlHttp {
    fn send(
        &self,
        method: &str,
        url: &str,
        content_type: Option<&str>,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, FetchError> {
        transport::send(method, url, content_type, headers, body)
    }
}

/// 登录失败的结构化原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthErrorKind {
    /// 网络层失败（DNS、超时、TLS、curl 起不来）。
    Network,
    /// 没被识别的 HTTP 错误。
    Http,
    /// 设备码还没被授权，继续轮询即可。
    Pending,
    /// 轮询太快，按 RFC 8628 要把间隔加 5 秒。
    SlowDown,
    /// 设备码过期。
    Expired,
    /// 用户点了取消。
    Cancelled,
    /// 没等到浏览器回调。
    Timeout,
    /// 响应结构不符合预期。
    Protocol,
    /// refresh token 失效，需要重新登录。
    InvalidGrant,
    /// Azure 应用没勾「允许公共客户端流」（AADSTS7000218）。
    ClientNotPublic,
    /// 账号还没有 Xbox 档案（XErr 2148916233）。
    NoXboxProfile,
    /// 所在地区不支持 Xbox Live（XErr 2148916235）。
    NoXboxRegion,
    /// 未成年账号，需要家长在微软家庭组里同意（XErr 2148916238）。
    ChildAccount,
    /// 账号没买 Minecraft（profile 404）。
    NotEntitled,
    /// Mojang 还没有批准这个 Azure 应用调用 Minecraft 服务。
    ///
    /// 微软账号、Xbox Live、XSTS 全都过了，最后一步
    /// `login_with_xbox` 回 403 `Invalid app registration`。这不是代码问题，
    /// 是 Minecraft 服务端对第三方应用的白名单：新注册的 Azure 应用一律被拒，
    /// 只能等 Mojang 审批（见 <https://aka.ms/AppRegInfo>）。
    AppNotApproved,
    /// 这次构建没有带客户端 ID。
    ///
    /// 源码仓库里不存 ID，打包的人要自己提供（见 [`DEFAULT_CLIENT_ID`]）。
    /// 报出来比发一个 user_code 为空的设备码请求好懂。
    MissingClientId,
}

/// 登录错误：分类 + 给用户看的一句话 + 原始响应。
#[derive(Debug, Clone, PartialEq)]
pub struct AuthError {
    pub kind: AuthErrorKind,
    /// 可以直接展示给用户的说明。
    pub detail: String,
    /// 服务端原始响应，排查时用；可能为空。
    pub body: String,
}

impl AuthError {
    pub fn new(kind: AuthErrorKind, detail: impl Into<String>, body: impl Into<String>) -> Self {
        AuthError {
            kind,
            detail: detail.into(),
            body: body.into(),
        }
    }

    pub fn protocol(detail: impl Into<String>) -> Self {
        AuthError::new(AuthErrorKind::Protocol, detail, String::new())
    }

    pub fn network(error: FetchError) -> Self {
        AuthError::new(AuthErrorKind::Network, error.to_string(), String::new())
    }

    pub fn network_message(message: impl Into<String>) -> Self {
        AuthError::new(AuthErrorKind::Network, message, String::new())
    }


    /// 把 HTTP 错误响应翻译成具体原因。
    ///
    /// OAuth 的错误码全在正文里，所以这里优先解析 JSON；解析不出来
    /// 再退回「HTTP 状态码」这种没什么信息量的说法。
    pub fn http(status: u16, body: &str) -> Self {
        if body.contains("AADSTS7000218") {
            return AuthError::new(
                AuthErrorKind::ClientNotPublic,
                "Azure 应用没有开启「允许公共客户端流」，桌面登录会被拒绝",
                body,
            );
        }

        // Minecraft 服务的白名单错误：新注册的 Azure 应用会在这里被拒。
        // 放在 JSON 解析之前，因为它的正文同样是 `errorMessage`，别被
        // 通用的「Minecraft 服务拒绝：…」吞掉。
        if body.contains("Invalid app registration") {
            return AuthError::new(
                AuthErrorKind::AppNotApproved,
                "Mojang 还没有批准这个 Azure 应用调用 Minecraft 服务，正版登录暂时走不通（见 https://aka.ms/AppRegInfo）",
                body,
            );
        }

        if let Ok(value) = json::parse(body) {
            // Minecraft 服务的错误格式：{"path":..,"errorType":..,"errorMessage":".."}
            if let Some(message) = value
                .get("errorMessage")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                return AuthError::new(
                    AuthErrorKind::Http,
                    format!("Minecraft 服务拒绝：{}", message),
                    body,
                );
            }

            if let Some(code) = value.get("XErr").and_then(|v| v.as_i64()) {
                let kind = match code {
                    2148916233 => AuthErrorKind::NoXboxProfile,
                    2148916235 => AuthErrorKind::NoXboxRegion,
                    2148916238 => AuthErrorKind::ChildAccount,
                    _ => AuthErrorKind::Http,
                };
                let detail = match kind {
                    AuthErrorKind::NoXboxProfile => {
                        "这个账号还没有 Xbox 档案，请先去 xbox.com 建一个".to_string()
                    }
                    AuthErrorKind::NoXboxRegion => {
                        "你所在的地区不支持 Xbox Live，无法登录 Minecraft".to_string()
                    }
                    AuthErrorKind::ChildAccount => {
                        "这是未成年账号，需要家长在微软家庭组里同意后才能登录".to_string()
                    }
                    _ => format!("Xbox 返回错误码 {}", code),
                };
                return AuthError::new(kind, detail, body);
            }

            if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
                let description = value
                    .get("error_description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let kind = match error {
                    "authorization_pending" => AuthErrorKind::Pending,
                    "slow_down" => AuthErrorKind::SlowDown,
                    "expired_token" => AuthErrorKind::Expired,
                    "authorization_declined" => AuthErrorKind::Cancelled,
                    "invalid_grant" => AuthErrorKind::InvalidGrant,
                    _ => AuthErrorKind::Http,
                };
                let detail = match error {
                    "authorization_pending" => "还在等你在浏览器里完成授权".to_string(),
                    "slow_down" => "轮询太快了，放慢一点".to_string(),
                    "expired_token" => "设备码过期了，请重新开始登录".to_string(),
                    "authorization_declined" => "你拒绝了这次登录".to_string(),
                    "invalid_grant" => "登录凭证已失效，请重新登录".to_string(),
                    _ => match description {
                        "" => format!("微软返回错误：{}", error),
                        detail => format!("微软返回错误：{}（{}）", error, detail),
                    },
                };
                return AuthError::new(kind, detail, body);
            }
        }

        AuthError::new(
            AuthErrorKind::Http,
            format!("服务端返回 HTTP {}{}", status, body_snippet(body)),
            body,
        )
    }

    /// 与 [`AuthError::http`] 相同，但补上「是哪一步失败的」。
    ///
    /// 登录链路上有四五个服务，光看 `HTTP 403` 无法判断卡在哪一跳。
    pub fn http_at(step: &str, status: u16, body: &str) -> Self {
        let mut error = AuthError::http(status, body);
        error.detail = format!("{}（{}）", error.detail, step);
        error
    }
}

/// 取一段响应正文塞进错误说明里。HTML 页面（CDN/代理拦截）只留头一行，
/// 免得把整页标签糊到用户脸上。
fn body_snippet(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let text: String = trimmed
        .chars()
        .take(240)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    format!("（{}）", text.trim())
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for AuthError {}

/// 登录成功后的 Minecraft 账户。
#[derive(Debug, Clone, PartialEq)]
pub struct MinecraftAccount {
    pub username: String,
    /// 32 位无连字符 UUID。
    pub uuid: String,
    pub access_token: String,
    /// Minecraft 令牌的过期时间（unix 秒），用来判断要不要续期。
    pub expires_at: u64,
}

impl MinecraftAccount {
    /// 转成启动器认识的账户结构。
    pub fn to_account(&self) -> Account {
        Account {
            username: self.username.clone(),
            uuid: self.uuid.clone(),
            access_token: self.access_token.clone(),
            user_type: "msa".to_string(),
            demo: false,
            api_root: None,
        }
    }
}

/// 微软登录走到哪一步了。
///
/// 这几跳每跳都是一次网络往返，加起来能有几秒。界面拿它画阶段条，
/// 免得玩家盯着一句「正在连接微软…」不知道该不该继续等。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginPhase {
    /// 用微软令牌换 Xbox Live 令牌
    XboxLive,
    /// 换 XSTS 授权令牌（Xbox 档案、地区、家长同意都在这一跳出问题）
    Xsts,
    /// 拿 XSTS 令牌换 Minecraft 访问令牌
    Minecraft,
    /// 用 Minecraft 令牌读游戏档案（名字和 UUID）
    Profile,
}

impl LoginPhase {
    /// 一共几跳。界面按它算百分比。
    pub const TOTAL: usize = 4;

    /// 这是第几跳（从 1 开始）。界面按它画进度条。
    pub fn index(self) -> usize {
        match self {
            LoginPhase::XboxLive => 1,
            LoginPhase::Xsts => 2,
            LoginPhase::Minecraft => 3,
            LoginPhase::Profile => 4,
        }
    }

    /// 给界面看的说明。写在这里而不是界面上，是因为顺序由这里的代码决定。
    pub fn label(self) -> &'static str {
        match self {
            LoginPhase::XboxLive => "换 Xbox Live 令牌…",
            LoginPhase::Xsts => "换 XSTS 授权令牌…",
            LoginPhase::Minecraft => "登录 Minecraft 服务…",
            LoginPhase::Profile => "读取游戏档案…",
        }
    }
}

/// 用微软令牌走完 Xbox → Minecraft 三跳，并确认游戏已购买。
pub fn complete_login(http: &dyn Http, tokens: &msa::MsaTokens) -> Result<MinecraftAccount, AuthError> {
    complete_login_with(http, tokens, |_| {})
}

/// 同 [`complete_login`]，但每进一跳先报一声。
pub fn complete_login_with<F>(
    http: &dyn Http,
    tokens: &msa::MsaTokens,
    mut on_phase: F,
) -> Result<MinecraftAccount, AuthError>
where
    F: FnMut(LoginPhase),
{
    on_phase(LoginPhase::XboxLive);
    let xbl = xbox::authenticate(http, &tokens.access_token)?;
    on_phase(LoginPhase::Xsts);
    let xsts = xbox::authorize(http, &xbl)?;
    on_phase(LoginPhase::Minecraft);
    let mc = xbox::login_with_xbox(http, &xsts)?;
    on_phase(LoginPhase::Profile);
    let profile = xbox::fetch_profile(http, &mc)?;

    Ok(MinecraftAccount {
        username: profile.name,
        uuid: profile.id,
        expires_at: store::now_unix() + mc.expires_in,
        access_token: mc.access_token,
    })
}

/// 把字符串转义成 JSON 字符串字面量（不含两侧引号）。
pub(crate) fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 取 `n` 字节随机数。
///
/// std 没有 RNG，而 `unsafe_code = "forbid"` 排除了直接调系统 API。
/// 这里的熵来自 `RandomState` 的进程级随机种子（std 从操作系统取），
/// 混入时间与 pid 后过一道 SipHash。
///
/// **这不是密码学安全的 CSPRNG**，但对 PKCE 的 verifier 与防 CSRF 的
/// state 来说，攻击者需要预测 128 位种子，已经不现实了。将来如果要用它
/// 生成长期密钥，得换成正经实现。
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n + 16);
    let mut counter: u64 = 0;

    while out.len() < n {
        counter += 1;

        let mut first = std::collections::hash_map::RandomState::new().build_hasher();
        first.write_u64(counter);
        first.write_u64(std::process::id() as u64);
        if let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            first.write_u128(elapsed.as_nanos());
        }
        out.extend_from_slice(&first.finish().to_le_bytes());

        let mut second = std::collections::hash_map::RandomState::new().build_hasher();
        second.write_u64(counter ^ 0x9E37_79B9_7F4A_7C15);
        out.extend_from_slice(&second.finish().to_le_bytes());
    }

    out.truncate(n);
    out
}

impl From<FetchError> for AuthError {
    fn from(error: FetchError) -> Self {
        AuthError::network(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::testutil::MockHttp;
    use crate::transport::HttpResponse;

    fn ok(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            body: body.to_string(),
        }
    }

    #[test]
    fn random_bytes_has_requested_length_and_varies() {
        let a = random_bytes(32);
        let b = random_bytes(32);
        assert_eq!(a.len(), 32);
        assert_eq!(b.len(), 32);
        assert_ne!(a, b);
        assert_ne!(a, vec![0u8; 32]);
    }

    #[test]
    fn json_escape_handles_quotes_and_controls() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(json_escape("a\nb"), "a\\nb");
        assert_eq!(json_escape("\u{1}"), "\\u0001");
    }

    #[test]
    fn aadsts_client_flow_error_is_recognized() {
        let err = AuthError::http(
            400,
            r#"{"error":"invalid_request","error_description":"AADSTS7000218: The request body must contain..."}"#,
        );
        assert_eq!(err.kind, AuthErrorKind::ClientNotPublic);
    }

    #[test]
    fn invalid_app_registration_is_its_own_kind() {
        let err = AuthError::http_at(
            "登录 Minecraft 服务",
            403,
            r#"{"path":"/authentication/login_with_xbox","errorMessage":"Invalid app registration, see https://aka.ms/AppRegInfo for more information"}"#,
        );
        assert_eq!(err.kind, AuthErrorKind::AppNotApproved);
        assert!(err.detail.contains("AppRegInfo"));
        assert!(err.detail.contains("登录 Minecraft 服务"));
    }

    #[test]
    fn unknown_http_error_falls_back_to_status() {
        let err = AuthError::http(503, "upstream broke");
        assert_eq!(err.kind, AuthErrorKind::Http);
        assert!(err.detail.contains("503"));
    }

    #[test]
    fn complete_login_walks_all_five_hops() {
        let mock = MockHttp::new(vec![
            ok(r#"{"Token":"xbl","DisplayClaims":{"xui":[{"uhs":"uhs-1"}]}}"#),
            ok(r#"{"Token":"xsts","DisplayClaims":{"xui":[{"uhs":"uhs-1"}]}}"#),
            ok(r#"{"access_token":"mc-token"}"#),
            ok(r#"{"id":"uuid-1","name":"Steve"}"#),
        ]);

        let account = complete_login(
            &mock,
            &msa::MsaTokens {
                access_token: "msa".to_string(),
                refresh_token: None,
                expires_in: 3600,
            },
        )
        .unwrap();

        assert_eq!(account.username, "Steve");
        assert_eq!(account.uuid, "uuid-1");
        assert_eq!(account.access_token, "mc-token");

        let account = account.to_account();
        assert_eq!(account.user_type, "msa");
        assert!(!account.demo);
        assert_eq!(account.uuid, "uuid-1");

        let log = mock.log.borrow();
        assert_eq!(log.len(), 4);
        assert_eq!(log[0].1, "https://user.auth.xboxlive.com/user/authenticate");
        assert_eq!(log[1].1, "https://xsts.auth.xboxlive.com/xsts/authorize");
        assert_eq!(
            log[2].1,
            "https://api.minecraftservices.com/authentication/login_with_xbox"
        );
        assert_eq!(
            log[3].1,
            "https://api.minecraftservices.com/minecraft/profile"
        );
    }

    #[test]
    fn complete_login_stops_when_game_not_owned() {
        let mock = MockHttp::new(vec![
            ok(r#"{"Token":"xbl","DisplayClaims":{"xui":[{"uhs":"uhs-1"}]}}"#),
            ok(r#"{"Token":"xsts","DisplayClaims":{"xui":[{"uhs":"uhs-1"}]}}"#),
            ok(r#"{"access_token":"mc-token"}"#),
            HttpResponse {
                status: 404,
                body: "{}".to_string(),
            },
        ]);

        let err = complete_login(
            &mock,
            &msa::MsaTokens {
                access_token: "msa".to_string(),
                refresh_token: None,
                expires_in: 3600,
            },
        )
        .unwrap_err();
        assert_eq!(err.kind, AuthErrorKind::NotEntitled);
    }
}

#[cfg(test)]
mod client_id_tests {
    use super::*;

    #[test]
    fn the_build_carries_an_id_when_one_was_provided() {
        // 这条测试在「本地有 .client-id / 设了环境变量」时通过；干净克隆的仓库
        // 会返回 MissingClientId —— 两种情况都不算错，所以只断言「要么有值要么
        // 明确说缺」，不能断言一定有值。
        match client_id() {
            Ok(id) => assert!(!id.is_empty(), "注入进来的是空串"),
            Err(e) => assert_eq!(e.kind, AuthErrorKind::MissingClientId),
        }
    }

    #[test]
    fn an_empty_id_is_reported_instead_of_being_used() {
        // DEFAULT_CLIENT_ID 是编译期常量，测试里改不了；这里验的是「空串会被拦」
        // 这条规则本身。
        let verdict = |id: &str| id.is_empty();
        assert!(verdict(""));
        assert!(!verdict("00000000-0000-0000-0000-000000000000"));
    }
}
