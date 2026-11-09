//! HTTP 传输层 —— 通过系统 `curl.exe` 完成实际传输。
//!
//! 为什么不自己实现 HTTP：
//!
//! - TLS 是硬门槛。Windows 的 TLS 走 schannel，需要访问用户凭证容器；
//!   纯 Rust 想绕过就得自带 TLS 实现（引 OpenSSL/rustls，而本机 cargo
//!   拉不到 crates.io）。
//! - `curl.exe` 是 Windows 10 1803 起自带的组件，本机实测 8.19.0，
//!   支持 TLS、断点续传、超时、重试。
//!
//! 代价是要启动子进程。但下载本身是 IO 密集操作，进程启动开销（约 10 ms）
//! 相对一次网络往返可以忽略。
//!
//! 这一层只负责「把字节搬到本地并确认是我们要的那份」，不做并发调度。

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// 「被用户取消」用的哨兵退出码，见 [`FetchError::is_cancelled`]。
const CANCELLED_EXIT: i32 = -1;

/// 「服务端限流」用的哨兵退出码，见 [`FetchError::is_rate_limited`]。
const RATE_LIMITED_EXIT: i32 = -2;

/// 撞上限流之后等多久再试（毫秒）。
///
/// BMCLAPI 的短窗口是 10 次 / 60 秒，`Retry-After-short` 实测给到 49 秒，
/// 所以退避要按「几十秒」量级来算，毫秒级的抖动重试等于接着撞。
const RATE_LIMIT_BACKOFF_MS: [u64; 3] = [5_000, 15_000, 45_000];

/// 下载结果。
#[derive(Debug, Clone)]
pub struct FetchOutcome {
    /// 实际写入的字节数。
    pub bytes: u64,
    /// 是否命中已有文件并跳过。
    pub skipped: bool,
}

/// 传输错误。
#[derive(Debug, Clone)]
pub struct FetchError {
    pub message: String,
    /// curl 的退出码（非子进程错误时是 None）。
    pub exit_code: Option<i32>,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.exit_code {
            Some(RATE_LIMITED_EXIT) => write!(f, "{}（被服务器限流）", self.message),
            Some(code) => write!(f, "{} (curl 退出码 {})", self.message, code),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for FetchError {}

impl FetchError {
    /// 这次失败是不是因为用户点了取消。
    ///
    /// 用 `exit_code == Some(-1)` 当哨兵，而不是给结构体加字段：
    /// 结构体字面量散在二十多处，加字段会把它们全打断，而取消本来
    /// 也不是「curl 报的错」。
    pub fn is_cancelled(&self) -> bool {
        self.exit_code == Some(CANCELLED_EXIT)
    }

    /// 这次失败是因为服务端限流（HTTP 429）。
    ///
    /// 单拎出来是因为它和普通网络抖动不一样：重试前必须真的等，
    /// 而且等待时间要按服务端给的量级来算。
    pub fn is_rate_limited(&self) -> bool {
        self.exit_code == Some(RATE_LIMITED_EXIT)
    }

    /// 构造一个「已取消」错误。
    pub fn cancelled(what: &str) -> Self {
        FetchError {
            message: format!("{} 已取消", what),
            exit_code: Some(CANCELLED_EXIT),
        }
    }

    /// 该错误是否值得重试（网络抖动、超时）。
    ///
    /// curl 退出码含义：
    ///   6  = 无法解析主机
    ///   7  = 无法连接
    ///   28 = 超时
    ///   35 = TLS 握手失败
    ///   56 = 接收数据失败
    /// 这些多半是暂时性的。而 404（22）之类的重试也没用。
    pub fn is_retryable(&self) -> bool {
        matches!(self.exit_code, Some(6 | 7 | 28 | 35 | 56))
    }
}

/// 定位 curl 可执行文件。
///
/// 优先用系统自带的 `C:\Windows\System32\curl.exe`，而不是 PATH 里可能
/// 存在的其他版本（Git for Windows 也带一个 curl）。
pub fn curl_path() -> PathBuf {
    if cfg!(windows) {
        let system = PathBuf::from(r"C:\Windows\System32\curl.exe");
        if system.is_file() {
            return system;
        }
    }
    PathBuf::from(if cfg!(windows) { "curl.exe" } else { "curl" })
}

/// 检查 curl 是否可用（存在且能报出版本）。
pub fn curl_available() -> bool {
    crate::spawn::silent(curl_path())
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 下载一个 URL 到指定路径。
///
/// `expected_sha1` 若给出，下载后校验；不匹配则删除临时文件并报错。
/// 这是防止「下到一个 404 页面却被当成 jar」的关键一步。
pub fn download_to(
    url: &str,
    dest: &Path,
    expected_sha1: Option<&str>,
) -> Result<FetchOutcome, FetchError> {
    download_with_limit(url, dest, expected_sha1, None)
}

/// 同 [`download_to`]，但给这次下载一个速率上限（KB/s，None = 不限）。
///
/// 上限是**单条连接**的，总速率的拆分在 [`crate::speed::Budget`] 里做：
/// 那一层知道同时有几个下载在跑，这一层只管把算出来的数字交给 curl。
pub fn download_with_limit(
    url: &str,
    dest: &Path,
    expected_sha1: Option<&str>,
    limit_kbps: Option<u32>,
) -> Result<FetchOutcome, FetchError> {
    download_with_cancel(
        url,
        dest,
        expected_sha1,
        limit_kbps,
        &crate::cancel::CancelToken::new(),
    )
}

/// 同 [`download_with_limit`]，但可以被 [`crate::cancel::CancelToken`] 打断。
///
/// 取消的实现是：`curl` 用 `spawn` 起、主线程每 40 ms 看一眼标志，
/// 被置位就 `kill` 掉子进程并返回 [`FetchError::cancelled`]。
/// 已经下了一半的 `.part` 会被删掉，以免下次误用。
pub fn download_with_cancel(
    url: &str,
    dest: &Path,
    expected_sha1: Option<&str>,
    limit_kbps: Option<u32>,
    cancel: &crate::cancel::CancelToken,
) -> Result<FetchOutcome, FetchError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| FetchError {
            message: format!("创建目录 {} 失败: {}", parent.display(), e),
            exit_code: None,
        })?;
    }

    // 已存在且校验通过 -> 跳过，省一次网络往返
    if dest.is_file() {
        if let Some(want) = expected_sha1 {
            if let Ok(actual) = crate::sha1::sha1_file_hex(dest) {
                if actual.eq_ignore_ascii_case(want) {
                    let bytes = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
                    return Ok(FetchOutcome {
                        bytes,
                        skipped: true,
                    });
                }
            }
        } else {
            let bytes = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
            if bytes > 0 {
                return Ok(FetchOutcome {
                    bytes,
                    skipped: true,
                });
            }
        }
    }

    // 先下到 .part，校验通过再改名。这样中断不会留下半截文件被误用。
    let part = with_part_suffix(dest);

    let mut last_err = None;
    // 限流单独记一份：撞 429 时的等待是「几十秒」量级，
    // 如果和普通抖动共用那 3 次机会，一次限流就把额度用光了。
    let mut rate_limited = 0usize;
    let mut attempt = 0usize;
    while attempt < 3 {
        if attempt > 0 {
            // 简单的线性退避，避免瞬间重试打满对端
            crate::logging::warn(format!("下载失败，第 {} 次重试：{}", attempt + 1, url));
            std::thread::sleep(std::time::Duration::from_millis(300 * attempt as u64));
        }

        match run_curl_cancellable(url, &part, limit_kbps, cancel) {
            Ok(()) => {
                // 校验
                if let Some(want) = expected_sha1 {
                    match crate::sha1::sha1_file_hex(&part) {
                        Ok(actual) if actual.eq_ignore_ascii_case(want) => {}
                        Ok(actual) => {
                            let _ = std::fs::remove_file(&part);
                            last_err = Some(FetchError {
                                message: format!(
                                    "校验失败: {} 期望 sha1={} 实际={}",
                                    dest.display(),
                                    want,
                                    actual
                                ),
                                exit_code: None,
                            });
                            // 校验失败重试没意义（内容就是错的），直接退出
                            return Err(last_err.unwrap());
                        }
                        Err(e) => {
                            let _ = std::fs::remove_file(&part);
                            return Err(FetchError {
                                message: format!("读取下载文件失败: {}", e),
                                exit_code: None,
                            });
                        }
                    }
                }

                std::fs::rename(&part, dest).map_err(|e| FetchError {
                    message: format!("重命名到 {} 失败: {}", dest.display(), e),
                    exit_code: None,
                })?;

                let bytes = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
                crate::logging::trace(format!("下载完成 {}（{} 字节）", dest.display(), bytes));
                return Ok(FetchOutcome {
                    bytes,
                    skipped: false,
                });
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                // 用户按了取消：不重试，直接把「已取消」往上抛
                if e.is_cancelled() {
                    return Err(e);
                }

                if e.is_rate_limited() {
                    // 限流不算「这个文件坏了」，只算「现在别问」。
                    // 单独一份额度，等够了再来。
                    if rate_limited >= RATE_LIMIT_BACKOFF_MS.len() {
                        return Err(e);
                    }
                    let wait_ms = RATE_LIMIT_BACKOFF_MS[rate_limited];
                    rate_limited += 1;
                    crate::logging::warn(format!(
                        "服务器限流，等 {} 秒后重试（第 {} 次）：{}",
                        wait_ms / 1000,
                        rate_limited,
                        url
                    ));
                    std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                    // 不推进 attempt：限流不是这个文件的错，别占它的重试额度
                    last_err = Some(e);
                    continue;
                }

                if !e.is_retryable() {
                    return Err(e);
                }
                last_err = Some(e);
            }
        }
        // 只有走到这里才算用掉一次普通重试额度；
        // 上面限流分支是 `continue`，不占额度。
        attempt += 1;
    }

    Err(last_err.unwrap_or_else(|| FetchError {
        message: format!("下载 {} 失败", url),
        exit_code: None,
    }))
}

fn with_part_suffix(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

/// 组装一次下载用的 curl 命令。
///
/// 下载只有这一条组装路径：可取消的 [`run_curl_cancellable`] 用它，
/// 以后再加参数也只改这一处。
/// URL 在最后追加：curl 要求所有选项排在 URL 之前。
/// 量一次「拿得到多少东西」的耗时（秒）。
///
/// 只用于给下载源选址：要的不是精确带宽，而是「这台机器连这个源
/// 通不通、回得有多快」。所以真下正文，但只读前几 KB 就掐断——
/// 探测不该在开场拖住几十兆的下载。
pub fn probe_latency(url: &str, timeout_secs: u32) -> Option<f64> {
    let output = crate::spawn::silent(curl_path())
        .args([
            "--silent",
            "--show-error",
            "--location",
            "--fail",
            "--max-time",
            &timeout_secs.to_string(),
            // 只要前 32 KB：够判断通不通，又不至于把整份清单搬下来。
            "--range",
            "0-32767",
            "--user-agent",
            "AICraftLauncher/0.1",
            // 正文丢进空设备，stdout 只留耗时。
            "--output",
            null_device(),
            "--write-out",
            "%{time_total}",
            url,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// 丢弃输出用的空设备路径。
fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

fn curl_command(url: &str, out: &Path, limit_kbps: Option<u32>) -> std::process::Command {
    let mut cmd = crate::spawn::silent(curl_path());
    cmd.args([
        "--silent",          // 不打印进度
        "--show-error",      // 但错误要看得见
        "--fail",            // HTTP >= 400 视为失败
        "--location",        // 跟随重定向
        "--max-time",
        "120",               // 单个文件最多两分钟
        "--retry",
        "0",                 // 重试由我们自己控制
        "--user-agent",
        "AICraftLauncher/0.1",
        // 正文走 --output 写文件，stdout 留给状态码：
        // `--fail` 只给「22」这个笼统的退出码，分不清 404 和 429，
        // 而这两者要不要重试、等多久，差得很远。
        "--write-out",
        "%{http_code}",
        "--output",
    ]);
    cmd.arg(out);

    // curl 的速率单位后缀：k = 1024 字节/秒。只在这一条连接上生效。
    if let Some(kbps) = limit_kbps {
        cmd.arg("--limit-rate").arg(format!("{}k", kbps));
    }

    cmd.arg(url);
    cmd
}

/// 跑一次下载，支持中途取消。
///
/// 为什么不用 `Command::output()`：那是同步等待，取消信号进不来。
/// 这里改成 `spawn` + `try_wait` 轮询——40 ms 一次，人感觉不出来，
/// 又足够灵敏。
fn run_curl_cancellable(
    url: &str,
    out: &Path,
    limit_kbps: Option<u32>,
    cancel: &crate::cancel::CancelToken,
) -> Result<(), FetchError> {
    let mut cmd = curl_command(url, out, limit_kbps);
    cmd.stderr(Stdio::piped());
    cmd.stdout(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| FetchError {
        message: format!("无法启动 curl: {}", e),
        exit_code: None,
    })?;

    let mut killed_by_user = false;
    let status = loop {
        if cancel.is_cancelled() {
            killed_by_user = true;
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(40)),
            Err(e) => {
                let _ = child.kill();
                return Err(FetchError {
                    message: format!("等待 curl 失败: {}", e),
                    exit_code: None,
                });
            }
        }
    };

    if killed_by_user {
        return Err(FetchError::cancelled(url));
    }

    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let mut buf = Vec::new();
        let _ = io::Read::read_to_end(&mut pipe, &mut buf);
        stderr = String::from_utf8_lossy(&buf).to_string();
    }

    let mut status_text = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let mut buf = Vec::new();
        let _ = io::Read::read_to_end(&mut pipe, &mut buf);
        status_text = String::from_utf8_lossy(&buf).to_string();
    }
    let http_status: Option<u16> = status_text.trim().parse().ok();

    match status {
        Some(status) if status.success() => Ok(()),
        Some(status) => Err(failure(url, status.code(), http_status, &stderr)),
        None => Err(FetchError::cancelled(url)),
    }
}

/// 把一次失败的 curl 调用整理成 [`FetchError`]。
///
/// 单独抽出来是因为「HTTP 状态码」和「进程退出码」要一起看：
/// 429 在 `--fail` 下同样报 22，光凭退出码分不出来，
/// 而它需要的处置（等几十秒再试）和 404（别试了）完全相反。
fn failure(
    url: &str,
    exit_code: Option<i32>,
    http_status: Option<u16>,
    stderr: &str,
) -> FetchError {
    if http_status == Some(429) {
        return FetchError {
            message: format!("下载 {} 失败: 服务器限流（HTTP 429）", url),
            exit_code: Some(RATE_LIMITED_EXIT),
        };
    }
    FetchError {
        message: format!("下载 {} 失败: {}", url, stderr.trim()),
        exit_code,
    }
}

/// 把一个小文本文件直接下载到内存，不经过磁盘。
///
/// 为什么不复用 `download_to`：那需要先挑一个可写目录落盘，再读回来。
/// 版本清单、版本 JSON、资源索引这类文件只有几百 KB，
/// 而且经常发生在「还没有可写 game_dir」的时候（比如只查询有哪些版本）。
/// 直接读 curl 的 stdout 既省一次磁盘往返，也避开了临时目录不可写的坑。
pub fn fetch_text(url: &str) -> Result<String, FetchError> {
    let output = crate::spawn::silent(curl_path())
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "120",
            "--retry",
            "0",
            "--user-agent",
            "AICraftLauncher/0.1",
            url,
        ])
        .output()
        .map_err(|e| FetchError {
            message: format!("无法启动 curl: {}", e),
            exit_code: None,
        })?;

    if output.status.success() {
        return String::from_utf8(output.stdout).map_err(|e| FetchError {
            message: format!("响应不是合法 UTF-8: {}", e),
            exit_code: None,
        });
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(FetchError {
        message: format!("下载 {} 失败: {}", url, stderr.trim()),
        exit_code: output.status.code(),
    })
}

/// 取一段原始字节（皮肤 PNG 这类二进制内容）。
///
/// 与 [`fetch_text`] 的区别只在于不把它当 UTF-8 解读：皮肤是二进制，
/// 强行转字符串会在第一个非法的字节上失败。
pub fn fetch_bytes(url: &str) -> Result<Vec<u8>, FetchError> {
    let output = crate::spawn::silent(curl_path())
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "120",
            "--retry",
            "0",
            "--user-agent",
            "AICraftLauncher/0.1",
            url,
        ])
        .output()
        .map_err(|e| FetchError {
            message: format!("无法启动 curl: {}", e),
            exit_code: None,
        })?;

    if output.status.success() {
        return Ok(output.stdout);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(FetchError {
        message: format!("下载 {} 失败: {}", url, stderr.trim()),
        exit_code: output.status.code(),
    })
}

/// 一次 HTTP 请求的完整结果。
///
/// 登录流程必须能拿到 **4xx 的正文**：OAuth 的错误信息（
/// `authorization_pending`、`AADSTS7000218`、Xbox 的 2148916233 ……）
/// 全在错误响应体里，只拿状态码等于把「为什么失败」扔掉。
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    /// 2xx 视为成功。
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// 发送一个带正文的 POST，返回状态码 + 正文。
///
/// 与 [`fetch_text`] 的关键差别：**不加 `--fail`**。加了的话 curl 遇到
/// 4xx 会直接丢弃正文并按错误退出，我们就拿不到 OAuth 的错误码了。
/// 所以这里改为让 curl 正常返回，再用 `--write-out %{http_code}`
/// 把状态码贴在 stdout 末尾自己切出来。
///
/// `content_type` 为 `None` 时不发 `Content-Type` 头。
pub fn post_raw(
    url: &str,
    content_type: Option<&str>,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<HttpResponse, FetchError> {
    let mut last_err = None;
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(300 * attempt as u64));
        }
        match run_http("POST", url, content_type, headers, Some(body)) {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                if !e.is_retryable() {
                    return Err(e);
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| FetchError {
        message: format!("POST {} 失败", url),
        exit_code: None,
    }))
}

/// 以 `application/x-www-form-urlencoded` 发送表单。
///
/// OAuth 的 `/token` 与 `/devicecode` 端点都要求这种格式。
pub fn post_form(url: &str, pairs: &[(&str, &str)]) -> Result<HttpResponse, FetchError> {
    let body = crate::codec::urlencode_pairs(pairs);
    post_raw(
        url,
        Some("application/x-www-form-urlencoded"),
        &[],
        body.as_bytes(),
    )
}

/// 以 `application/json` 发送 JSON 正文。
pub fn post_json(url: &str, body: &str) -> Result<HttpResponse, FetchError> {
    post_raw(url, Some("application/json"), &[], body.as_bytes())
}

/// 发送一个带自定义头的 GET，返回状态码 + 正文。
///
/// 主要给 `GET /minecraft/profile` 用：它用 `Authorization: Bearer …`
/// 鉴权，而且「404 = 没买游戏」是一个**需要区分**的正常结果，
/// 不能混进网络错误里。
pub fn get_with_headers(url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, FetchError> {
    run_http("GET", url, None, headers, None)
}

/// 请求一个 URL，把正文与状态码一起带回来。
fn run_http(
    method: &str,
    url: &str,
    content_type: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<HttpResponse, FetchError> {
    let mut cmd = crate::spawn::silent(curl_path());
    cmd.args([
        "--silent",
        "--show-error",
        // 跟随重定向：微软的登录端点会把 /authorize 转给租户专用域名
        "--location",
        "--max-time",
        "60",
        "--request",
        method,
        // 状态码贴在正文后面，便于区分「正文」与「结果」
        "--write-out",
        "\n%{http_code}",
        "--user-agent",
        "AICraftLauncher/0.1",
    ]);

    // 回环地址永远不该走代理：否则登录回调会被转发到公司代理上，
    // 而代理也访问不到本机端口。
    if is_loopback_url(url) {
        cmd.arg("--noproxy").arg("*");
    }

    if let Some(ct) = content_type {
        cmd.arg("--header").arg(format!("Content-Type: {}", ct));
    }
    for (k, v) in headers {
        cmd.arg("--header").arg(format!("{}: {}", k, v));
    }
    cmd.arg("--header").arg("Accept: application/json");

    if body.is_some() {
        // 正文从 stdin 喂进去，避免命令行长度与引号转义的坑
        cmd.arg("--data-binary").arg("@-");
    }
    cmd.arg(url);

    cmd.stdin(if body.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| FetchError {
        message: format!("无法启动 curl: {}", e),
        exit_code: None,
    })?;

    // 正文通常只有几百字节，但如果对端把管道读满而子进程又在往 stdout 写，
    // 单线程 wait 会互相等死。写入放独立线程，读与等待留给主线程。
    let writer = body.map(|bytes| {
        let bytes = bytes.to_vec();
        let mut stdin = child.stdin.take().expect("stdin 已设置管道");
        std::thread::spawn(move || {
            use std::io::Write;
            // 忽略写入错误：对端提前关闭连接时，真正的错误会由 curl 退出码体现
            let _ = stdin.write_all(&bytes);
            let _ = stdin.flush();
        })
    });

    let output = child.wait_with_output().map_err(|e| FetchError {
        message: format!("等待 curl 失败: {}", e),
        exit_code: None,
    })?;
    if let Some(handle) = writer {
        let _ = handle.join();
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // --write-out 的结果是 stdout 最后一行
    let (body_text, status) = match stdout.rsplit_once('\n') {
        Some((body, code)) => match code.trim().parse::<u16>() {
            Ok(code) if code > 0 => (body.to_string(), code),
            _ => {
                return Err(FetchError {
                    message: format!("{} {} 未拿到状态码: {}", method, url, stderr.trim()),
                    exit_code: output.status.code(),
                })
            }
        },
        None => {
            return Err(FetchError {
                message: format!("{} {} 响应格式异常: {}", method, url, stdout.trim()),
                exit_code: output.status.code(),
            })
        }
    };

    crate::logging::trace(format!("{} {} -> HTTP {}（{} 字节）", method, url, status, body_text.len()));

    // 状态码拿到了，但 curl 本身报错（多为连接层问题）
    if !output.status.success() && status == 0 {
        return Err(FetchError {
            message: format!("{} {} 失败: {}", method, url, stderr.trim()),
            exit_code: output.status.code(),
        });
    }

    Ok(HttpResponse {
        status,
        body: body_text,
    })
}

/// 判断 URL 是否指向本机回环地址。
pub fn is_loopback_url(url: &str) -> bool {
    let rest = match url.strip_prefix("http://") {
        Some(r) => r,
        None => return false,
    };
    // 先截掉路径，剩下的就是 authority
    let authority = rest.split('/').next().unwrap_or("");

    // IPv6 用方括号包着，里面的冒号不是端口分隔符，不能直接按 ':' 切
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        match stripped.split_once(']') {
            Some((h, _)) => format!("[{}]", h),
            None => return false,
        }
    } else {
        authority.split(':').next().unwrap_or("").to_string()
    };

    matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

/// 判断某个路径是否需要下载（在 verify 里复用）。
pub fn needs_fetch(path: &Path, expected_size: Option<u64>) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => {
            if !meta.is_file() {
                return true;
            }
            match expected_size {
                // 尺寸不符说明文件坏了或下了半截
                Some(size) => meta.len() != size,
                // 没有尺寸信息时，非空就认为存在
                None => meta.len() == 0,
            }
        }
        Err(_) => true,
    }
}

/// 让 io 错误能转成 FetchError，方便上层统一处理。
impl From<io::Error> for FetchError {
    fn from(e: io::Error) -> Self {
        FetchError {
            message: e.to_string(),
            exit_code: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_is_available_on_this_machine() {
        // 这条测试在本机应当通过：Windows 10+ 自带 curl
        assert!(curl_available(), "curl 不可用，下载功能无法工作");
    }

    #[test]
    fn part_suffix_is_appended() {
        let p = with_part_suffix(Path::new("a/b/c.jar"));
        assert_eq!(p, PathBuf::from("a/b/c.jar.part"));
    }

    #[test]
    fn retryable_classification() {
        let mk = |code| FetchError {
            message: "x".into(),
            exit_code: Some(code),
        };
        assert!(mk(6).is_retryable()); // 解析失败
        assert!(mk(7).is_retryable()); // 连接失败
        assert!(mk(28).is_retryable()); // 超时
        assert!(mk(56).is_retryable()); // 接收失败
        assert!(!mk(22).is_retryable()); // HTTP 错误
        assert!(!mk(1).is_retryable());
        // 限流是单独一档：它可重试，但走的是「等几十秒」那条路，
        // 不能混进普通重试额度里
        assert!(mk(RATE_LIMITED_EXIT).is_rate_limited());
        assert!(!mk(22).is_rate_limited());
        assert!(!mk(RATE_LIMITED_EXIT).is_cancelled());
    }

    #[test]
    fn a_429_is_recognised_from_the_status_code() {
        // `--fail` 对 404 和 429 都给退出码 22，只能靠 --write-out 的
        // 状态码区分。这条测试把这个区分钉住。
        let limited = failure("https://x/y.jar", Some(22), Some(429), "");
        assert!(limited.is_rate_limited());
        assert!(!limited.is_retryable());

        let missing = failure("https://x/y.jar", Some(22), Some(404), "curl: (22)");
        assert!(!missing.is_rate_limited());
        assert!(!missing.is_retryable());

        // 拿不到状态码时（比如 DNS 失败）退回按退出码判断
        let dns = failure("https://x/y.jar", Some(6), None, "curl: (6)");
        assert!(!dns.is_rate_limited());
        assert!(dns.is_retryable());
    }

    #[test]
    fn rate_limit_backoff_grows() {
        // 退避必须递增，且总等待在「几十秒」量级——
        // 服务端给的 Retry-After 实测就是 49 秒左右。
        for pair in RATE_LIMIT_BACKOFF_MS.windows(2) {
            assert!(
                pair[1] > pair[0],
                "退避时间应当递增：{:?}",
                RATE_LIMIT_BACKOFF_MS
            );
        }
        let total: u64 = RATE_LIMIT_BACKOFF_MS.iter().sum();
        assert!(total >= 30_000, "总等待太短，撞限流等于白等：{}ms", total);
    }

    #[test]
    fn needs_fetch_detects_size_mismatch() {
        let tmp = crate::testutil::fresh_dir("aicraft_needs_fetch_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let f = tmp.join("x.bin");

        // 不存在
        assert!(needs_fetch(&f, Some(10)));
        // 存在且尺寸匹配
        std::fs::write(&f, vec![0u8; 10]).unwrap();
        assert!(!needs_fetch(&f, Some(10)));
        // 尺寸不符
        assert!(needs_fetch(&f, Some(20)));
        // 无期望尺寸且非空 -> 不需要
        assert!(!needs_fetch(&f, None));
        // 空文件 -> 需要
        std::fs::write(&f, b"").unwrap();
        assert!(needs_fetch(&f, None));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 起一个只服务一次请求的本机 HTTP 服务器。
    ///
    /// 返回 `(url, 收到的请求原文)`。这里刻意用真实回环连接而不是 mock：
    /// 要验证的正是「curl 正常退出、状态码自己从 stdout 尾巴切出来」这条
    /// 路径 —— 其中任何一环写错，登录流程都会在 4xx 处丢掉错误信息。
    fn spawn_http_server(
        status: &'static str,
        body: &'static str,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf: Vec<u8> = Vec::new();
            let mut tmp = [0u8; 4096];

            // 先读到头部结束
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    Err(_) => break,
                }
            }
            let head_end = buf
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| p + 4)
                .unwrap_or(buf.len());
            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
            let mut body_bytes: Vec<u8> = buf[head_end..].to_vec();

            // 再按 Content-Length 补齐正文
            let content_len: usize = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    if k.eq_ignore_ascii_case("content-length") {
                        v.trim().parse().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            while body_bytes.len() < content_len {
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => body_bytes.extend_from_slice(&tmp[..n]),
                    Err(_) => break,
                }
            }

            let _ = tx.send(format!(
                "{}\n\n{}",
                head,
                String::from_utf8_lossy(&body_bytes)
            ));

            let resp = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        });
        (format!("http://{}", addr), rx)
    }

    #[test]
    fn post_json_keeps_4xx_body() {
        let (url, rx) = spawn_http_server("400 Bad Request", r#"{"error":"authorization_pending"}"#);
        let resp = post_json(&format!("{}/token", url), r#"{"grant_type":"x"}"#).unwrap();

        assert_eq!(resp.status, 400);
        assert!(!resp.is_success());
        // 这一段是整条登录链的错误来源，绝不能丢
        assert_eq!(resp.body, r#"{"error":"authorization_pending"}"#);

        let seen = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(
            seen.to_lowercase()
                .contains("content-type: application/json"),
            "请求头里没有 JSON Content-Type:\n{}",
            seen
        );
        assert!(seen.contains(r#"{"grant_type":"x"}"#), "正文没发出去:\n{}", seen);
    }

    #[test]
    fn post_form_urlencodes_pairs() {
        let (url, rx) = spawn_http_server("200 OK", r#"{"token_type":"Bearer"}"#);
        let resp = post_form(
            &format!("{}/token", url),
            &[("grant_type", "authorization_code"), ("code", "a+b/c=")],
        )
        .unwrap();

        assert_eq!(resp.status, 200);
        assert!(resp.is_success());

        let seen = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(
            seen.contains("grant_type=authorization_code&code=a%2Bb%2Fc%3D"),
            "表单编码不正确:\n{}",
            seen
        );
        assert!(seen
            .to_lowercase()
            .contains("content-type: application/x-www-form-urlencoded"));
    }

    #[test]
    fn get_with_headers_reports_404() {
        // 「404 = 这个号没买游戏」必须能作为正常结果返回，而不是网络错误
        let (url, _rx) = spawn_http_server("404 Not Found", r#"{"errorMessage":"NOT_FOUND"}"#);
        let resp = get_with_headers(
            &format!("{}/minecraft/profile", url),
            &[("Authorization", "Bearer x")],
        )
        .unwrap();

        assert_eq!(resp.status, 404);
        assert!(resp.body.contains("NOT_FOUND"));
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback_url("http://127.0.0.1:8080/callback"));
        assert!(is_loopback_url("http://localhost/callback"));
        assert!(is_loopback_url("http://[::1]:9000/x"));
        assert!(!is_loopback_url("https://login.microsoftonline.com/x"));
        // 不能被子域名骗过去
        assert!(!is_loopback_url("http://127.0.0.1.evil.com/x"));
    }
}
