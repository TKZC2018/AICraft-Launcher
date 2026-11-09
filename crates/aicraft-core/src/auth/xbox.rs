//! Xbox Live 与 Minecraft 服务链路。
//!
//! 拿到微软账号的 access token 之后，还要再过三跳才能换到能进游戏的
//! Minecraft 令牌：
//!
//! 1. `user.auth.xboxlive.com/user/authenticate` —— 用微软令牌换 Xbox Live 令牌（XBL）；
//! 2. `xsts.auth.xboxlive.com/xsts/authorize` —— 用 XBL 换 XSTS 令牌，
//!    顺带拿到 user hash（uhs）；
//! 3. `api.minecraftservices.com/authentication/login_with_xbox` ——
//!    用 `XBL3.0 x=<uhs>;<xsts>` 换 Minecraft 令牌；
//! 4. `GET /minecraft/profile` —— 确认这个账号**买了游戏**。
//!
//! 第 2 跳的失败最需要翻译：Xbox 用 `XErr` 数字告诉你具体原因
//! （没建档案 / 地区不支持 / 未成年），直接甩给用户一串数字没人看得懂。

use crate::json::{self, Json};

use super::{json_escape, AuthError, AuthErrorKind, Http};

const XBL_AUTH: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_AUTH: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MC_PROFILE: &str = "https://api.minecraftservices.com/minecraft/profile";

/// Xbox Live 令牌及其对应的 user hash。
#[derive(Debug, Clone, PartialEq)]
pub struct XboxTokens {
    pub token: String,
    /// XSTS 返回的 `DisplayClaims.xui[0].uhs`，登录 Minecraft 时要用。
    pub user_hash: String,
}

/// Minecraft 服务令牌。
#[derive(Debug, Clone, PartialEq)]
pub struct MinecraftToken {
    pub access_token: String,
    /// 有效期（秒），微软一般给 86400。
    pub expires_in: u64,
}

/// 玩家的 Minecraft 档案。
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// 32 位无连字符的 UUID。
    pub id: String,
    pub name: String,
}

/// 用微软账号令牌换 Xbox Live 令牌。
///
/// `RpsTicket` 必须是 `d=<token>` 形式，少了 `d=` 前缀会直接 400。
pub fn authenticate(
    http: &dyn Http,
    msa_access_token: &str,
) -> Result<XboxTokens, AuthError> {
    let body = format!(
        r#"{{"Properties":{{"AuthMethod":"RPS","SiteName":"user.auth.xboxlive.com","RpsTicket":"d={}"}},"RelyingParty":"http://auth.xboxlive.com","TokenType":"JWT"}}"#,
        json_escape(msa_access_token)
    );

    let resp = http
        .post_json(XBL_AUTH, &body)
        .map_err(AuthError::network)?;
    if !resp.is_success() {
        return Err(AuthError::http_at("换 Xbox Live 令牌", resp.status, &resp.body));
    }

    let json = parse_body(&resp.body)?;
    let token = str_at(&json, &["Token"])
        .ok_or_else(|| AuthError::protocol("Xbox Live 响应里没有 Token"))?;
    let uhs = str_at(&json, &["DisplayClaims", "xui", "0", "uhs"])
        .ok_or_else(|| AuthError::protocol("Xbox Live 响应里没有 user hash"))?;

    Ok(XboxTokens {
        token: token.to_string(),
        user_hash: uhs.to_string(),
    })
}

/// 用 Xbox Live 令牌换 XSTS 令牌。
///
/// `SandboxId` 必须是 `RETAIL`；`RelyingParty` 必须是
/// `rp://api.minecraftservices.com/`，这两处写错都会被拒。
pub fn authorize(http: &dyn Http, xbl: &XboxTokens) -> Result<XboxTokens, AuthError> {
    let body = format!(
        r#"{{"Properties":{{"SandboxId":"RETAIL","UserTokens":["{}"]}},"RelyingParty":"rp://api.minecraftservices.com/","TokenType":"JWT"}}"#,
        json_escape(&xbl.token)
    );

    let resp = http
        .post_json(XSTS_AUTH, &body)
        .map_err(AuthError::network)?;
    if !resp.is_success() {
        return Err(AuthError::http_at("换 XSTS 令牌", resp.status, &resp.body));
    }

    let json = parse_body(&resp.body)?;
    let token = str_at(&json, &["Token"])
        .ok_or_else(|| AuthError::protocol("XSTS 响应里没有 Token"))?;
    let uhs = str_at(&json, &["DisplayClaims", "xui", "0", "uhs"])
        .ok_or_else(|| AuthError::protocol("XSTS 响应里没有 user hash"))?;

    Ok(XboxTokens {
        token: token.to_string(),
        user_hash: uhs.to_string(),
    })
}

/// 用 XSTS 令牌换 Minecraft 访问令牌。
pub fn login_with_xbox(
    http: &dyn Http,
    xsts: &XboxTokens,
) -> Result<MinecraftToken, AuthError> {
    let identity = format!("XBL3.0 x={};{}", xsts.user_hash, xsts.token);
    let body = format!(
        r#"{{"identityToken":"{}"}}"#,
        json_escape(&identity)
    );

    let resp = http
        .post_json(MC_LOGIN, &body)
        .map_err(AuthError::network)?;
    if !resp.is_success() {
        // 401 通常意味着 XSTS 令牌已被消费或过期，重新走一遍即可。
        return Err(AuthError::http_at("登录 Minecraft 服务", resp.status, &resp.body));
    }

    let json = parse_body(&resp.body)?;
    let token = str_at(&json, &["access_token"])
        .ok_or_else(|| AuthError::protocol("Minecraft 登录响应里没有 access_token"))?;

    Ok(MinecraftToken {
        access_token: token.to_string(),
        expires_in: json
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .unwrap_or(86_400)
            .max(0) as u64,
    })
}

/// 取玩家的 Minecraft 档案。
///
/// **404 是正常结果**，含义是「这个账号没买游戏」，不能当成网络错误。
pub fn fetch_profile(http: &dyn Http, token: &MinecraftToken) -> Result<Profile, AuthError> {
    let auth = format!("Bearer {}", token.access_token);
    let resp = http
        .get(MC_PROFILE, &[("Authorization", auth.as_str())])
        .map_err(AuthError::network)?;

    if resp.status == 404 {
        return Err(AuthError::new(
            AuthErrorKind::NotEntitled,
            "这个账号还没有购买 Minecraft",
            resp.body,
        ));
    }
    if !resp.is_success() {
        return Err(AuthError::http_at("读取 Minecraft 档案", resp.status, &resp.body));
    }

    let json = parse_body(&resp.body)?;
    let id = str_at(&json, &["id"])
        .ok_or_else(|| AuthError::protocol("档案响应里没有 id"))?;
    let name = str_at(&json, &["name"])
        .ok_or_else(|| AuthError::protocol("档案响应里没有 name"))?;

    Ok(Profile {
        id: id.to_string(),
        name: name.to_string(),
    })
}

/// 解析响应体；解析失败按协议错误处理。
fn parse_body(body: &str) -> Result<Json, AuthError> {
    json::parse(body).map_err(|e| AuthError::protocol(format!("响应不是合法 JSON：{}", e)))
}

/// 按路径取字符串。`xui` 是数组，所以路径里可以有数字下标。
fn str_at<'a>(value: &'a Json, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = match current {
            Json::Array(items) => items.get(key.parse::<usize>().ok()?)?,
            other => other.get(key)?,
        };
    }
    current.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::HttpResponse;

    use crate::auth::testutil::MockHttp;

    fn ok(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            body: body.to_string(),
        }
    }

    #[test]
    fn xbl_request_uses_d_prefixed_rps_ticket() {
        let mock = MockHttp::new(vec![ok(
            r#"{"Token":"xbl-token","DisplayClaims":{"xui":[{"uhs":"uhs-1"}]}}"#,
        )]);
        let tokens = authenticate(&mock, "msa-token").unwrap();

        assert_eq!(tokens.token, "xbl-token");
        assert_eq!(tokens.user_hash, "uhs-1");

        let log = mock.log.borrow();
        assert_eq!(log[0].1, XBL_AUTH);
        assert!(log[0].2.contains(r#""RpsTicket":"d=msa-token""#));
        assert!(log[0].2.contains(r#""SiteName":"user.auth.xboxlive.com""#));
    }

    #[test]
    fn xsts_requires_retail_sandbox_and_minecraft_relying_party() {
        let mock = MockHttp::new(vec![ok(
            r#"{"Token":"xsts-token","DisplayClaims":{"xui":[{"uhs":"uhs-2"}]}}"#,
        )]);
        let xsts = authorize(
            &mock,
            &XboxTokens {
                token: "xbl-token".to_string(),
                user_hash: "uhs-1".to_string(),
            },
        )
        .unwrap();
        assert_eq!(xsts.token, "xsts-token");
        assert_eq!(xsts.user_hash, "uhs-2");

        let body = &mock.log.borrow()[0].2;
        assert!(body.contains(r#""SandboxId":"RETAIL""#));
        assert!(body.contains(r#""RelyingParty":"rp://api.minecraftservices.com/""#));
    }

    #[test]
    fn login_sends_xbl3_identity_token() {
        let mock = MockHttp::new(vec![ok(r#"{"access_token":"mc-token","expires_in":86400}"#)]);
        let token = login_with_xbox(
            &mock,
            &XboxTokens {
                token: "xsts-token".to_string(),
                user_hash: "uhs-2".to_string(),
            },
        )
        .unwrap();
        assert_eq!(token.access_token, "mc-token");
        assert_eq!(token.expires_in, 86_400);
        assert!(mock.log.borrow()[0]
            .2
            .contains(r#""identityToken":"XBL3.0 x=uhs-2;xsts-token""#));
    }

    #[test]
    fn profile_parses_uuid_and_name() {
        let mock = MockHttp::new(vec![ok(
            r#"{"id":"069a79f444e94726a5befca90e38aaf5","name":"Notch"}"#,
        )]);
        let profile = fetch_profile(
            &mock,
            &MinecraftToken {
                access_token: "mc-token".to_string(),
                expires_in: 86_400,
            },
        )
        .unwrap();
        assert_eq!(profile.id, "069a79f444e94726a5befca90e38aaf5");
        assert_eq!(profile.name, "Notch");
    }

    #[test]
    fn profile_404_means_not_entitled() {
        let mock = MockHttp::new(vec![HttpResponse {
            status: 404,
            body: r#"{"error":"NOT_FOUND","errorMessage":"The server has not found anything matching the request URI"}"#.to_string(),
        }]);
        let err = fetch_profile(
            &mock,
            &MinecraftToken {
                access_token: "mc-token".to_string(),
                expires_in: 86_400,
            },
        )
        .unwrap_err();
        assert_eq!(err.kind, AuthErrorKind::NotEntitled);
        assert!(err.detail.contains("购买"));
    }

    #[test]
    fn xsts_error_code_is_translated() {
        let mock = MockHttp::new(vec![HttpResponse {
            status: 401,
            body: r#"{"Identity":"0","XErr":2148916233,"Message":"","Redirect":"https://start.ui.xboxlive.com/CreateAccount"}"#
                .to_string(),
        }]);
        let err = authorize(
            &mock,
            &XboxTokens {
                token: "xbl".to_string(),
                user_hash: "uhs".to_string(),
            },
        )
        .unwrap_err();
        assert_eq!(err.kind, AuthErrorKind::NoXboxProfile);
        assert!(err.detail.contains("Xbox"));
    }

    #[test]
    fn xsts_region_and_child_codes_are_distinct() {
        for (code, expected) in [
            (2148916235i64, AuthErrorKind::NoXboxRegion),
            (2148916238, AuthErrorKind::ChildAccount),
        ] {
            let body = format!(r#"{{"XErr":{}}}"#, code);
            let mock = MockHttp::new(vec![HttpResponse { status: 401, body }]);
            let err = authorize(
                &mock,
                &XboxTokens {
                    token: "xbl".to_string(),
                    user_hash: "uhs".to_string(),
                },
            )
            .unwrap_err();
            assert_eq!(err.kind, expected);
        }
    }

    #[test]
    fn xsts_error_without_json_falls_back_to_http() {
        let mock = MockHttp::new(vec![HttpResponse {
            status: 500,
            body: "boom".to_string(),
        }]);
        let err = authorize(
            &mock,
            &XboxTokens {
                token: "xbl".to_string(),
                user_hash: "uhs".to_string(),
            },
        )
        .unwrap_err();
        assert_eq!(err.kind, AuthErrorKind::Http);
    }
}
