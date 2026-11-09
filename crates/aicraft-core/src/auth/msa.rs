//! 微软账号登录（Microsoft Entra ID / MSA）。
//!
//! 两条路都实现，共用同一个出口（`MsaTokens`）：
//!
//! - **浏览器授权码 + PKCE**：默认路径。打开浏览器，用户登完跳回
//!   `http://localhost:<port>/callback`，我们收下 `code` 去换令牌。
//!   这是 HMCL 等启动器在用的方式，用户体验最好（不用手抄码）。
//! - **设备码**：兜底。用户在任意设备上打开 `microsoft.com/link`
//!   输入 8 位码。适合浏览器起不来、或远程连着的场景。
//!
//! PKCE 的 `code_challenge` 是 `BASE64URL(SHA256(verifier))`，所以这个
//! 模块依赖 [`crate::sha256`] 与 [`crate::codec`]。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use crate::codec::{base64url_nopad, urlencode};
use crate::json;
use crate::sha256;

use super::{random_bytes, AuthError, AuthErrorKind, Http};

const AUTHORIZE: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize";
const DEVICECODE: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// 申请的权限：进 Xbox Live 必须的 `XboxLive.signin`；
/// `offline_access` 用来拿 refresh token，否则几小时后又要重登。
pub const SCOPE: &str = "XboxLive.signin offline_access";

/// 设备码登录的第一步返回。
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCode {
    pub device_code: String,
    /// 给用户手抄的 8 位码，形如 `ABCD1234`。
    pub user_code: String,
    /// 让用户去输入的地址，通常 `https://microsoft.com/link`。
    pub verification_uri: String,
    /// 有效期（秒）。
    pub expires_in: u64,
    /// 两次轮询之间至少间隔多少秒。
    pub interval: u64,
    /// 微软给的整句提示，可直接展示给用户。
    pub message: String,
}

/// 一组微软账号令牌。
#[derive(Debug, Clone, PartialEq)]
pub struct MsaTokens {
    pub access_token: String,
    /// 换新令牌用；某些情况下微软不给，那就只能重新登录。
    pub refresh_token: Option<String>,
    /// access token 有效期（秒）。
    pub expires_in: u64,
}

/// 浏览器授权码流程的发起端。
///
/// `start` 先占住一个本地端口，`wait_for_code` 才真正等回调。分成两步
/// 是为了让 UI 能先把授权链接显示/打开，再进入阻塞等待。
pub struct BrowserLogin {
    listener: TcpListener,
    /// 授权页要跳回来的地址，必须与发往微软的 `redirect_uri` 完全一致。
    redirect_uri: String,
    /// 防 CSRF：回调里的 `state` 必须和发出去的一致。
    state: String,
    verifier: String,
    authorize_url: String,
}

impl BrowserLogin {
    /// 绑定本地回环端口并生成 PKCE 参数与授权链接。
    pub fn start(client_id: &str) -> Result<Self, AuthError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|e| AuthError::network_message(format!("无法监听本地端口：{}", e)))?;
        let port = listener
            .local_addr()
            .map_err(|e| AuthError::network_message(format!("无法读取本地端口：{}", e)))?
            .port();

        let verifier = new_verifier();
        let challenge = challenge_for(&verifier);
        let state = base64url_nopad(&random_bytes(16));
        // 微软对 loopback 地址会忽略端口号，但路径必须与注册时一致。
        let redirect_uri = format!("http://localhost:{}/callback", port);
        let authorize_url = authorize_url(client_id, &redirect_uri, &state, &challenge);

        Ok(BrowserLogin {
            listener,
            redirect_uri,
            state,
            verifier,
            authorize_url,
        })
    }

    /// 用户需要打开的授权页地址。
    pub fn url(&self) -> &str {
        &self.authorize_url
    }

    /// 等浏览器跳回来，拿到授权码后换令牌。
    ///
    /// 超过 `timeout` 没等到就用 [`AuthErrorKind::Timeout`] 返回，
    /// 由调用方决定是重试还是改走设备码。
    pub fn wait(
        self,
        http: &dyn Http,
        client_id: &str,
        timeout: Duration,
    ) -> Result<MsaTokens, AuthError> {
        let code = self.wait_for_code(timeout)?;
        exchange_code(http, client_id, &code, &self.redirect_uri, &self.verifier)
    }

    /// 只等回调、换出授权码。
    fn wait_for_code(&self, timeout: Duration) -> Result<String, AuthError> {
        let deadline = Instant::now() + timeout;
        self.listener
            .set_nonblocking(true)
            .map_err(|e| AuthError::network_message(format!("无法设置监听超时：{}", e)))?;

        loop {
            if Instant::now() >= deadline {
                return Err(AuthError::new(
                    AuthErrorKind::Timeout,
                    "等待浏览器回调超时",
                    String::new(),
                ));
            }

            match self.listener.accept() {
                Ok((stream, _)) => {
                    // 浏览器偶尔会顺手请求 /favicon.ico，不是我们等的那一跳。
                    if let Some(result) = handle_callback(stream, &self.state) {
                        return result;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(AuthError::network_message(format!(
                        "等待回调失败：{}",
                        e
                    )))
                }
            }
        }
    }
}

/// 处理一次回调连接。不是 `/callback` 就返回 `None`（继续等）。
fn handle_callback(mut stream: TcpStream, expected_state: &str) -> Option<Result<String, AuthError>> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));

    // 只读请求头：授权码在请求行里。
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 16 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let text = String::from_utf8_lossy(&buf);
    let request_line = text.lines().next().unwrap_or("");
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    };

    if path != "/callback" {
        let _ = respond(&mut stream, 404, "Not Found");
        return None;
    }

    let result = parse_callback(query, expected_state);
    let (status, body) = match &result {
        Ok(_) => (200, success_page()),
        Err(_) => (400, failure_page()),
    };
    let _ = respond(&mut stream, status, body);
    Some(result)
}

/// 从回调的查询串里取出授权码，并校验 `state`。
pub fn parse_callback(query: &str, expected_state: &str) -> Result<String, AuthError> {
    let pairs = crate::codec::parse_form(query);
    let get = |key: &str| -> Option<&str> {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };

    if let Some(error) = get("error") {
        let description = get("error_description").unwrap_or("");
        if error == "access_denied" {
            return Err(AuthError::new(
                AuthErrorKind::Cancelled,
                "用户取消了登录",
                description.to_string(),
            ));
        }
        return Err(AuthError::new(
            AuthErrorKind::Http,
            format!("授权失败：{}", error),
            description.to_string(),
        ));
    }

    let state = get("state")
        .ok_or_else(|| AuthError::protocol("回调里没有 state，已拒绝"))?;
    if state != expected_state {
        return Err(AuthError::protocol("回调的 state 不匹配，可能被伪造"));
    }

    let code = get("code").ok_or_else(|| AuthError::protocol("回调里没有授权码"))?;
    Ok(code.to_string())
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let bytes = body.as_bytes();
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        reason,
        bytes.len()
    )?;
    stream.write_all(bytes)?;
    stream.flush()
}

fn success_page() -> &'static str {
    "<!doctype html><meta charset=\"utf-8\"><title>登录成功</title>\
     <body style=\"font-family:system-ui;text-align:center;padding-top:15vh\">\
     <h1>登录成功</h1><p>可以关闭这个页面，回到启动器了。</p></body>"
}

fn failure_page() -> &'static str {
    "<!doctype html><meta charset=\"utf-8\"><title>登录未完成</title>\
     <body style=\"font-family:system-ui;text-align:center;padding-top:15vh\">\
     <h1>登录未完成</h1><p>请回到启动器查看原因，然后重试。</p></body>"
}

/// 生成 PKCE 的 `code_verifier`：32 字节随机数的 base64url，43 个字符，
/// 在 RFC 7636 允许的 43–128 之间。
pub fn new_verifier() -> String {
    base64url_nopad(&random_bytes(32))
}

/// `code_verifier` → `code_challenge`（S256）。
pub fn challenge_for(verifier: &str) -> String {
    base64url_nopad(&sha256::sha256(verifier.as_bytes()))
}

/// 拼授权页地址。
pub fn authorize_url(
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> String {
    format!(
        "{}?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}\
         &code_challenge={}&code_challenge_method=S256&prompt=select_account",
        AUTHORIZE,
        urlencode(client_id),
        urlencode(redirect_uri),
        urlencode(SCOPE),
        urlencode(state),
        urlencode(challenge),
    )
}

/// 用默认浏览器打开授权页。
///
/// 注意不能用 `cmd /c start`：那个会把 URL 里未转义的 `&` 当成命令分隔符，
/// 把后面的查询参数丢掉。`rundll32` 不经过 cmd，最稳。
pub fn open_browser(url: &str) -> Result<(), AuthError> {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler").arg(url);
        c
    };

    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };

    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };

    cmd.spawn()
        .map(|_| ())
        .map_err(|e| AuthError::network_message(format!("无法打开浏览器：{}", e)))
}

/// 申请设备码。
pub fn request_device_code(http: &dyn Http, client_id: &str) -> Result<DeviceCode, AuthError> {
    let resp = http
        .post_form(
            DEVICECODE,
            &[("client_id", client_id), ("scope", SCOPE)],
        )
        .map_err(AuthError::network)?;
    if !resp.is_success() {
        return Err(AuthError::http_at("申请设备码", resp.status, &resp.body));
    }

    let json = parse_json(&resp.body)?;
    let field = |name: &str| -> Result<String, AuthError> {
        json.get(name)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| AuthError::protocol(format!("设备码响应里没有 {}", name)))
    };

    Ok(DeviceCode {
        device_code: field("device_code")?,
        user_code: field("user_code")?,
        verification_uri: field("verification_uri")?,
        expires_in: json
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .unwrap_or(900) as u64,
        interval: json
            .get("interval")
            .and_then(|v| v.as_i64())
            .unwrap_or(5)
            .max(1) as u64,
        message: json
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// 轮询一次设备码。
///
/// 用户还没授权时微软返回 400 + `authorization_pending`，这不是错误，
/// 对应 [`AuthErrorKind::Pending`]，调用方睡一会儿再来。
pub fn poll_device_code_once(
    http: &dyn Http,
    client_id: &str,
    code: &DeviceCode,
) -> Result<MsaTokens, AuthError> {
    let resp = http
        .post_form(
            TOKEN,
            &[
                ("grant_type", DEVICE_CODE_GRANT),
                ("device_code", &code.device_code),
                ("client_id", client_id),
            ],
        )
        .map_err(AuthError::network)?;

    if !resp.is_success() {
        return Err(AuthError::http_at("换取微软令牌", resp.status, &resp.body));
    }
    parse_tokens(&resp.body)
}

/// 一直轮询到用户授权、超时或出错。
///
/// 设备码的有效期由微软给（`expires_in`），期间 `authorization_pending`
/// 是常态；收到 `slow_down` 要把间隔加 5 秒（RFC 8628 的要求）。
pub fn wait_for_device_code(
    http: &dyn Http,
    client_id: &str,
    code: &DeviceCode,
) -> Result<MsaTokens, AuthError> {
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let mut interval = code.interval;

    loop {
        if Instant::now() >= deadline {
            return Err(AuthError::new(
                AuthErrorKind::Expired,
                "设备码已过期，请重新开始登录",
                String::new(),
            ));
        }

        match poll_device_code_once(http, client_id, code) {
            Ok(tokens) => return Ok(tokens),
            Err(e) if e.kind == AuthErrorKind::Pending => {
                std::thread::sleep(Duration::from_secs(interval));
            }
            Err(e) if e.kind == AuthErrorKind::SlowDown => {
                interval += 5;
                std::thread::sleep(Duration::from_secs(interval));
            }
            Err(e) => return Err(e),
        }
    }
}

/// 用授权码换令牌（浏览器流程的最后一步）。
pub fn exchange_code(
    http: &dyn Http,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<MsaTokens, AuthError> {
    let resp = http
        .post_form(
            TOKEN,
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", client_id),
                ("code_verifier", verifier),
            ],
        )
        .map_err(AuthError::network)?;

    if !resp.is_success() {
        return Err(AuthError::http_at("用授权码换令牌", resp.status, &resp.body));
    }
    parse_tokens(&resp.body)
}

/// 用 refresh token 换新的一对令牌。
pub fn refresh(
    http: &dyn Http,
    client_id: &str,
    refresh_token: &str,
) -> Result<MsaTokens, AuthError> {
    let resp = http
        .post_form(
            TOKEN,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", client_id),
                ("scope", SCOPE),
            ],
        )
        .map_err(AuthError::network)?;

    if !resp.is_success() {
        return Err(AuthError::http_at("续期微软令牌", resp.status, &resp.body));
    }
    parse_tokens(&resp.body)
}

fn parse_tokens(body: &str) -> Result<MsaTokens, AuthError> {
    let json = parse_json(body)?;
    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AuthError::protocol("令牌响应里没有 access_token"))?;

    Ok(MsaTokens {
        access_token: access_token.to_string(),
        refresh_token: json
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        expires_in: json
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .unwrap_or(3600) as u64,
    })
}

fn parse_json(body: &str) -> Result<json::Json, AuthError> {
    json::parse(body).map_err(|e| AuthError::protocol(format!("响应不是合法 JSON：{}", e)))
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
    fn verifier_and_challenge_are_pkce_shaped() {
        let verifier = new_verifier();
        // base64url 无填充，32 字节 → 43 个字符
        assert_eq!(verifier.len(), 43);
        assert!(verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));

        let challenge = challenge_for(&verifier);
        assert_eq!(challenge.len(), 43);
        // S256 的固定向量：challenge 必须是 SHA-256 的 base64url
        let expected = base64url_nopad(&sha256::sha256(verifier.as_bytes()));
        assert_eq!(challenge, expected);
        assert_ne!(challenge, verifier);
    }

    #[test]
    fn scope_requests_signin_and_offline_access() {
        assert!(SCOPE.contains("XboxLive.signin"));
        assert!(SCOPE.contains("offline_access"));
    }

    #[test]
    fn authorize_url_carries_all_required_parameters() {
        let url = authorize_url(
            "client-1",
            "http://localhost:8080/callback",
            "state-1",
            "challenge-1",
        );
        assert!(url.starts_with(AUTHORIZE));
        assert!(url.contains("client_id=client-1"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fcallback"));
        assert!(url.contains("scope=XboxLive.signin%20offline_access"));
        assert!(url.contains("code_challenge=challenge-1"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("prompt=select_account"));
        assert!(url.contains("state=state-1"));
    }

    #[test]
    fn device_code_is_parsed() {
        let mock = MockHttp::new(vec![ok(
            r#"{"device_code":"dev-1","user_code":"ABCD1234","verification_uri":"https://microsoft.com/link","expires_in":900,"interval":5,"message":"去这个网站输码"}"#,
        )]);
        let code = request_device_code(&mock, "client-1").unwrap();
        assert_eq!(code.device_code, "dev-1");
        assert_eq!(code.user_code, "ABCD1234");
        assert_eq!(code.expires_in, 900);
        assert_eq!(code.interval, 5);
        assert!(code.message.contains("网站"));

        let log = mock.log.borrow();
        assert_eq!(log[0].1, DEVICECODE);
        assert!(log[0].2.contains("client_id=client-1"));
        assert!(log[0].2.contains("scope=XboxLive.signin%20offline_access"));
    }

    #[test]
    fn pending_is_not_an_error_but_a_kind() {
        let mock = MockHttp::new(vec![HttpResponse {
            status: 400,
            body: r#"{"error":"authorization_pending","error_description":"..."}"#.to_string(),
        }]);
        let code = DeviceCode {
            device_code: "dev".to_string(),
            user_code: "U".to_string(),
            verification_uri: "v".to_string(),
            expires_in: 900,
            interval: 1,
            message: String::new(),
        };
        let err = poll_device_code_once(&mock, "c", &code).unwrap_err();
        assert_eq!(err.kind, AuthErrorKind::Pending);
    }

    #[test]
    fn token_response_is_parsed() {
        let mock = MockHttp::new(vec![ok(
            r#"{"access_token":"at","refresh_token":"rt","expires_in":3599}"#,
        )]);
        let code = DeviceCode {
            device_code: "dev".to_string(),
            user_code: "U".to_string(),
            verification_uri: "v".to_string(),
            expires_in: 900,
            interval: 1,
            message: String::new(),
        };
        let tokens = poll_device_code_once(&mock, "c", &code).unwrap();
        assert_eq!(tokens.access_token, "at");
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt"));
        assert_eq!(tokens.expires_in, 3599);
    }

    #[test]
    fn exchange_code_sends_verifier() {
        let mock = MockHttp::new(vec![ok(r#"{"access_token":"at","expires_in":3600}"#)]);
        let tokens = exchange_code(&mock, "c", "the-code", "http://localhost:1/callback", "v")
            .unwrap();
        assert_eq!(tokens.access_token, "at");
        assert!(tokens.refresh_token.is_none());

        let body = &mock.log.borrow()[0].2;
        assert!(body.contains("grant_type=authorization_code"));
        assert!(body.contains("code=the-code"));
        assert!(body.contains("code_verifier=v"));
        assert!(body.contains("client_id=c"));
    }

    #[test]
    fn refresh_sends_refresh_token_grant() {
        let mock = MockHttp::new(vec![ok(r#"{"access_token":"at2","expires_in":3600}"#)]);
        let tokens = refresh(&mock, "c", "rt").unwrap();
        assert_eq!(tokens.access_token, "at2");
        assert!(mock.log.borrow()[0].2.contains("grant_type=refresh_token"));
        assert!(mock.log.borrow()[0].2.contains("refresh_token=rt"));
    }

    #[test]
    fn callback_requires_matching_state() {
        assert!(parse_callback("code=abc&state=s1", "s1").is_ok());
        let err = parse_callback("code=abc&state=s2", "s1").unwrap_err();
        assert_eq!(err.kind, AuthErrorKind::Protocol);
        assert!(err.detail.contains("state"));
    }

    #[test]
    fn callback_access_denied_is_cancelled() {
        let err = parse_callback("error=access_denied&error_description=no", "s").unwrap_err();
        assert_eq!(err.kind, AuthErrorKind::Cancelled);
    }

    /// 起一个假的「浏览器」连上回环端口，验证整条回调 → 换码链路。
    #[test]
    fn browser_login_completes_loopback_handshake() {
        let login = BrowserLogin::start("client-1").unwrap();
        let url = login.url().to_string();
        // 从授权链接里取出 state，模拟浏览器原样带回
        let state = url
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_string();
        let port: u16 = url
            .split("redirect_uri=http%3A%2F%2Flocalhost%3A")
            .nth(1)
            .unwrap()
            .split('%')
            .next()
            .unwrap()
            .parse()
            .unwrap();

        let fake_browser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let request = format!(
                "GET /callback?code=the-code&state={} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                state
            );
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            response
        });

        let mock = MockHttp::new(vec![ok(r#"{"access_token":"at","expires_in":3600}"#)]);
        let tokens = login
            .wait(&mock, "client-1", Duration::from_secs(5))
            .unwrap();
        assert_eq!(tokens.access_token, "at");

        let response = fake_browser.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("登录成功"));
        assert!(mock.log.borrow()[0].2.contains("code=the-code"));
    }

    #[test]
    fn browser_login_times_out() {
        let login = BrowserLogin::start("client-1").unwrap();
        let mock = MockHttp::new(Vec::new());
        let err = login
            .wait(&mock, "client-1", Duration::from_millis(50))
            .unwrap_err();
        assert_eq!(err.kind, AuthErrorKind::Timeout);
    }
}
