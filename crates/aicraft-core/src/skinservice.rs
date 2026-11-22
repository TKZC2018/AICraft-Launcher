//! 换皮肤、换披风。
//!
//! 走的是 Minecraft 服务（api.minecraftservices.com）上那几个官方端点：
//!
//! - POST   /minecraft/profile/skins          上传一张皮肤（multipart，带 variant）
//! - DELETE /minecraft/profile/skins/active   删掉自己的皮肤，回到默认的 Steve/Alex
//! - PUT    /minecraft/profile/capes/active   换成已拥有的某件披风（JSON 带 capeId）
//! - DELETE /minecraft/profile/capes/active   把披风摘掉
//! - GET    /minecraft/profile                读自己现在穿着什么
//!
//! 三件需要说清楚的事：
//!
//! - **披风只能从已拥有的里面挑**。接口不接受上传披风图，capeId 必须是账号
//!   已经拥有的那几件之一，否则服务端回 400。所以界面上该是把「我有的披风」
//!   列出来让人点，而不是给一个上传框。
//! - **每次写操作都会把完整档案回给我们**。于是这四个动作统一返回 Wardrobe，
//!   界面拿到就能直接重画，不用再补一次 GET。
//! - **上传是 multipart，正文里有二进制**。curl 走 stdin 喂数据，这里只负责
//!   把边界、Content-Disposition 和字段名拼对。
//!
//! 官方对皮肤图的要求是 64×64（1.8 之后）或 64×32（老皮肤）的 PNG，
//! 超过这个尺寸的「高清皮肤」不是原版能用的东西 —— 上传前先按这个卡一道，
//! 免得把一张 128×128 发上去再吃一个莫名其妙的 400。

use crate::auth::Http;
use crate::json::{self, Json};
use crate::png;

/// Minecraft 服务根地址。测试里换掉即可。
pub const API_ROOT: &str = "https://api.minecraftservices.com";

/// 皮肤的模型。classic 是普通手臂（Steve），slim 是细手臂（Alex）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Classic,
    Slim,
}

impl Variant {
    /// 接口里那个小写字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Variant::Classic => "classic",
            Variant::Slim => "slim",
        }
    }

    /// 给界面看的中文名。
    pub fn label(self) -> &'static str {
        match self {
            Variant::Classic => "经典",
            Variant::Slim => "纤细",
        }
    }

    /// 从接口回的大小写形式认回来：CLASSIC / SLIM / classic / slim。
    pub fn parse(text: &str) -> Option<Variant> {
        if text.eq_ignore_ascii_case("classic") {
            Some(Variant::Classic)
        } else if text.eq_ignore_ascii_case("slim") {
            Some(Variant::Slim)
        } else {
            None
        }
    }
}

/// 账号身上穿着的一张皮肤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skin {
    /// 这张皮肤的 id；同一张图永远是同一个 id。
    pub id: String,
    /// 是否正在生效（ACTIVE / INACTIVE）。
    pub active: bool,
    /// 贴图地址，可以直接下载来当预览。
    pub url: String,
    /// 模型；认不出来时为 None。
    pub variant: Option<Variant>,
}

/// 账号拥有的一件披风。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cape {
    pub id: String,
    pub active: bool,
    pub url: String,
    /// 披风名，例如 Migrator、Minecon2012。这个字段未必有。
    pub alias: Option<String>,
}

/// 账号现在穿着什么。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wardrobe {
    /// 玩家名，接口顺手回的。
    pub name: String,
    pub uuid: String,
    pub skins: Vec<Skin>,
    pub capes: Vec<Cape>,
}

impl Wardrobe {
    /// 正在生效的那张皮肤。
    pub fn active_skin(&self) -> Option<&Skin> {
        self.skins.iter().find(|skin| skin.active)
    }

    /// 正在生效的那件披风；没穿就是 None。
    pub fn active_cape(&self) -> Option<&Cape> {
        self.capes.iter().find(|cape| cape.active)
    }
}

/// 解析 GET /minecraft/profile（以及所有写操作）的响应。
pub fn parse_profile(text: &str) -> Result<Wardrobe, String> {
    let root = json::parse(text).map_err(|e| format!("档案不是 JSON：{}", e))?;
    let uuid = root
        .get("id")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string();
    let name = root
        .get("name")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string();

    let mut skins = Vec::new();
    if let Some(items) = root.get("skins").and_then(Json::as_array) {
        for item in items {
            let Some(id) = item.get("id").and_then(Json::as_str) else {
                continue;
            };
            skins.push(Skin {
                id: id.to_string(),
                active: is_active(item),
                url: item
                    .get("url")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_string(),
                variant: item
                    .get("variant")
                    .and_then(Json::as_str)
                    .and_then(Variant::parse),
            });
        }
    }

    let mut capes = Vec::new();
    if let Some(items) = root.get("capes").and_then(Json::as_array) {
        for item in items {
            let Some(id) = item.get("id").and_then(Json::as_str) else {
                continue;
            };
            capes.push(Cape {
                id: id.to_string(),
                active: is_active(item),
                url: item
                    .get("url")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_string(),
                alias: item
                    .get("alias")
                    .and_then(Json::as_str)
                    .map(|text| text.to_string()),
            });
        }
    }

    Ok(Wardrobe {
        name,
        uuid,
        skins,
        capes,
    })
}

fn is_active(value: &Json) -> bool {
    value
        .get("state")
        .and_then(Json::as_str)
        .map(|state| state.eq_ignore_ascii_case("ACTIVE"))
        .unwrap_or(false)
}

/// 皮肤图的形状：原版只认这两种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkinShape {
    pub width: usize,
    pub height: usize,
}

/// 检查一张皮肤图能不能上传。
///
/// 除了尺寸，也顺手确认它是真的 PNG —— 把一张 jpg 改名成 .png 传上去，
/// 服务端也只会回一个含糊的错误。
pub fn check_skin_png(bytes: &[u8]) -> Result<SkinShape, String> {
    let image = png::decode(bytes).map_err(|e| format!("这张图不是能用的 PNG：{}", e))?;
    let shape = SkinShape {
        width: image.width,
        height: image.height,
    };
    if shape.width != 64 || (shape.height != 64 && shape.height != 32) {
        return Err(format!(
            "皮肤图要是 64×64 或 64×32，这张是 {}×{}",
            shape.width, shape.height
        ));
    }
    Ok(shape)
}

/// 这个账号能不能用这里的接口。
///
/// 第三方皮肤站（authlib-injector / Blessing Skin 那一系）用的是另一套端点，
/// 这里刻意不碰：地址、字段名、权限判定都跟官方不同，猜错一次就可能把
/// 别人的皮肤站写坏。界面该在动手之前先问这一句。
pub fn supports(api_root: Option<&str>) -> bool {
    api_root.is_none()
}

/// 读自己现在穿着什么。
pub fn fetch_wardrobe(http: &dyn Http, token: &str) -> Result<Wardrobe, String> {
    let url = format!("{}/minecraft/profile", API_ROOT);
    let response = http
        .get(&url, &[("Authorization", &bearer(token))])
        .map_err(|e| format!("读取档案失败：{}", e))?;
    read_wardrobe(response.status, &response.body)
}

/// 上传一张皮肤。
///
/// variant 决定手臂粗细；同一张图配不同模型，在原版眼里是不同的皮肤。
pub fn upload_skin(
    http: &dyn Http,
    token: &str,
    png_bytes: &[u8],
    variant: Variant,
) -> Result<Wardrobe, String> {
    check_skin_png(png_bytes)?;
    let url = format!("{}/minecraft/profile/skins", API_ROOT);
    let boundary = "aicraft-skin-boundary";
    let body = multipart_skin(boundary, png_bytes, variant);
    let content_type = format!("multipart/form-data; boundary={}", boundary);
    let response = http
        .send(
            "POST",
            &url,
            Some(&content_type),
            &[("Authorization", &bearer(token))],
            Some(&body),
        )
        .map_err(|e| format!("上传皮肤失败：{}", e))?;
    read_wardrobe(response.status, &response.body)
}

/// 删掉自己的皮肤，回到默认那张。
pub fn reset_skin(http: &dyn Http, token: &str) -> Result<Wardrobe, String> {
    let url = format!("{}/minecraft/profile/skins/active", API_ROOT);
    let response = http
        .send("DELETE", &url, None, &[("Authorization", &bearer(token))], None)
        .map_err(|e| format!("重置皮肤失败：{}", e))?;
    read_wardrobe(response.status, &response.body)
}

/// 换上一件已拥有的披风。cape_id 必须在这账号的 capes 里。
pub fn set_cape(http: &dyn Http, token: &str, cape_id: &str) -> Result<Wardrobe, String> {
    let cape_id = cape_id.trim();
    if cape_id.is_empty() {
        return Err("没选披风".to_string());
    }
    let url = format!("{}/minecraft/profile/capes/active", API_ROOT);
    let body = format!("{{\"capeId\":\"{}\"}}", escape(cape_id));
    let response = http
        .send(
            "PUT",
            &url,
            Some("application/json"),
            &[("Authorization", &bearer(token))],
            Some(body.as_bytes()),
        )
        .map_err(|e| format!("换披风失败：{}", e))?;
    read_wardrobe(response.status, &response.body)
}

/// 把披风摘掉（不删，只是不再显示）。
pub fn clear_cape(http: &dyn Http, token: &str) -> Result<Wardrobe, String> {
    let url = format!("{}/minecraft/profile/capes/active", API_ROOT);
    let response = http
        .send("DELETE", &url, None, &[("Authorization", &bearer(token))], None)
        .map_err(|e| format!("摘披风失败：{}", e))?;
    read_wardrobe(response.status, &response.body)
}

/// 把皮肤贴图本身下下来（给界面做预览用）。
///
/// 贴图地址是 textures.minecraft.net 上的原图，64×64 的 RGBA PNG，几 KB 而已。
/// 走的是二进制通道 —— 档案里的 url 只保证能下，不保证别人替你缓存过。
pub fn fetch_skin_png(url: &str) -> Result<Vec<u8>, String> {
    if url.trim().is_empty() {
        return Err("这张皮肤没有贴图地址".to_string());
    }
    let bytes = crate::transport::fetch_bytes(url.trim())
        .map_err(|e| format!("下载皮肤贴图失败：{}", e.message))?;
    // 顺手确认服务端回的是张图，而不是一节 404 的 HTML。
    png::decode(&bytes).map_err(|e| format!("皮肤贴图不是能用的 PNG：{}", e))?;
    Ok(bytes)
}

/// 拼 multipart 正文：一个 variant 文本字段 + 一个 file 文件字段。
///
/// 文件字段必须自报 Content-Type: image/png，服务端靠它判断这是不是张图；
/// filename 是给皮肤站做显示名用的，给一个好认的名字。
fn multipart_skin(boundary: &str, png_bytes: &[u8], variant: Variant) -> Vec<u8> {
    let mut body = Vec::with_capacity(png_bytes.len() + 512);
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"variant\"\r\n\r\n");
    body.extend_from_slice(variant.as_str().as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"skin.png\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
    body.extend_from_slice(png_bytes);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    body
}

/// 把各种失败折成一句人能读懂的话。
///
/// 401 是令牌过期（需要重新登录），400 多半是这张图原版不认，
/// 429 是短时间改太多次 —— 这三种分开说，比一个「失败了」有用得多。
fn read_wardrobe(status: u16, body: &str) -> Result<Wardrobe, String> {
    match status {
        200..=299 => parse_profile(body),
        400 => Err(format!("服务端不接受这张图{}", snippet(body))),
        401 => Err("登录已过期，请重新登录后再试".to_string()),
        403 => Err(format!("这个账号没有权限改材质{}", snippet(body))),
        429 => Err("改得太频繁了，等一会儿再试".to_string()),
        other => Err(format!("服务端返回 {}{}", other, snippet(body))),
    }
}

/// Authorization 头。借一个 String 给调用方 bind 住，免得临时值掉得太早。
fn bearer(token: &str) -> String {
    format!("Bearer {}", token)
}

/// 把错误正文压成一小段括号注解。
fn snippet(body: &str) -> String {
    let text: String = body
        .trim()
        .chars()
        .take(160)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.is_empty() {
        String::new()
    } else {
        format!("（{}）", text)
    }
}

/// 在 JSON 字符串里转义，别让披风 id 里跑出引号（正常不会，但便宜）。
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out
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

    /// 一份「穿着 slim 皮肤 + 两件披风、其中一件生效」的档案。
    const PROFILE: &str = r#"{
        "id": "069a79f444e94726a5befca90e38aaf5",
        "name": "Notch",
        "skins": [{
            "id": "7800ee13-f75d-40e5-a5b2-9197c3e0125a",
            "state": "ACTIVE",
            "url": "http://textures.minecraft.net/texture/aaa",
            "variant": "SLIM"
        }],
        "capes": [
            {"id": "cape-1", "state": "INACTIVE", "url": "http://textures.minecraft.net/texture/c1", "alias": "Migrator"},
            {"id": "cape-2", "state": "ACTIVE", "url": "http://textures.minecraft.net/texture/c2", "alias": "Minecon2012"}
        ]
    }"#;

    #[test]
    fn only_official_accounts_are_served_here() {
        assert!(supports(None));
        assert!(!supports(Some("https://littleskin.cn/api/yggdrasil")));
    }

    #[test]
    fn a_profile_tells_what_is_worn() {
        let wardrobe = parse_profile(PROFILE).unwrap();
        assert_eq!(wardrobe.name, "Notch");
        assert_eq!(wardrobe.uuid, "069a79f444e94726a5befca90e38aaf5");
        assert_eq!(wardrobe.skins.len(), 1);
        assert_eq!(wardrobe.active_skin().unwrap().variant, Some(Variant::Slim));
        assert_eq!(wardrobe.capes.len(), 2);
        let cape = wardrobe.active_cape().unwrap();
        assert_eq!(cape.id, "cape-2");
        assert_eq!(cape.alias.as_deref(), Some("Minecon2012"));
    }

    #[test]
    fn a_profile_without_textures_is_still_a_profile() {
        let wardrobe = parse_profile(r#"{"id":"abc","name":"Steve"}"#).unwrap();
        assert_eq!(wardrobe.name, "Steve");
        assert!(wardrobe.skins.is_empty());
        assert!(wardrobe.active_skin().is_none());
        assert!(wardrobe.active_cape().is_none());
    }

    /// 造一张 RGBA8、不滤波的 PNG（存储块，不压缩）。
    fn skin_png(width: usize, height: usize) -> Vec<u8> {
        let mut raw = Vec::new();
        for _ in 0..height {
            raw.push(0);
            raw.extend(std::iter::repeat_n(0u8, width * 4));
        }
        let mut zlib = vec![0x78, 0x01];
        let mut rest = raw.as_slice();
        while !rest.is_empty() {
            let take = rest.len().min(65535);
            let (chunk, tail) = rest.split_at(take);
            let last = if tail.is_empty() { 1u8 } else { 0u8 };
            zlib.push(last);
            let len = chunk.len() as u16;
            zlib.extend_from_slice(&len.to_le_bytes());
            zlib.extend_from_slice(&(!len).to_le_bytes());
            zlib.extend_from_slice(chunk);
            rest = tail;
        }
        zlib.extend_from_slice(&[0, 0, 0, 0]);

        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(width as u32).to_be_bytes());
        ihdr.extend_from_slice(&(height as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        crate::png::push_chunk(&mut out, b"IHDR", &ihdr);
        crate::png::push_chunk(&mut out, b"IDAT", &zlib);
        crate::png::push_chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn both_skin_shapes_are_accepted() {
        assert_eq!(check_skin_png(&skin_png(64, 64)).unwrap().height, 64);
        assert_eq!(check_skin_png(&skin_png(64, 32)).unwrap().height, 32);
    }

    #[test]
    fn an_odd_shaped_png_is_refused_before_it_is_sent() {
        let err = check_skin_png(&skin_png(128, 128)).unwrap_err();
        assert!(err.contains("64×64 或 64×32"), "{}", err);
        assert!(check_skin_png(b"not a png").is_err());
    }

    #[test]
    fn uploading_sends_a_multipart_with_both_fields() {
        let mock = MockHttp::new(vec![ok(PROFILE)]);
        let wardrobe = upload_skin(&mock, "mc-token", &skin_png(64, 64), Variant::Slim).unwrap();
        assert_eq!(wardrobe.name, "Notch");

        let log = mock.log.borrow();
        assert_eq!(log[0].0, "POST");
        assert!(log[0].1.ends_with("/minecraft/profile/skins"));
        let body = &log[0].2;
        assert!(body.starts_with("--aicraft-skin-boundary\r\n"), "{}", body);
        assert!(body.contains("name=\"variant\""));
        assert!(body.contains("\r\n\r\nslim\r\n"));
        assert!(body.contains("name=\"file\"; filename=\"skin.png\""));
        assert!(body.contains("Content-Type: image/png"));
        assert!(body.ends_with("--aicraft-skin-boundary--\r\n"));
    }

    #[test]
    fn a_skin_that_is_too_big_never_reaches_the_server() {
        let mock = MockHttp::new(vec![]);
        assert!(upload_skin(&mock, "t", &skin_png(128, 64), Variant::Classic).is_err());
        assert!(mock.log.borrow().is_empty());
    }

    #[test]
    fn resetting_and_clearing_both_use_delete() {
        let mock = MockHttp::new(vec![ok(PROFILE), ok(PROFILE)]);
        reset_skin(&mock, "mc-token").unwrap();
        clear_cape(&mock, "mc-token").unwrap();

        let log = mock.log.borrow();
        assert_eq!(log[0].0, "DELETE");
        assert!(log[0].1.ends_with("/minecraft/profile/skins/active"));
        assert_eq!(log[1].0, "DELETE");
        assert!(log[1].1.ends_with("/minecraft/profile/capes/active"));
    }

    #[test]
    fn setting_a_cape_puts_its_id_in_the_body() {
        let mock = MockHttp::new(vec![ok(PROFILE)]);
        set_cape(&mock, "mc-token", "cape-1").unwrap();

        let log = mock.log.borrow();
        assert_eq!(log[0].0, "PUT");
        assert!(log[0].1.ends_with("/minecraft/profile/capes/active"));
        assert!(log[0].2.contains(r#""capeId":"cape-1""#), "{}", log[0].2);
    }

    #[test]
    fn an_empty_cape_is_refused_locally() {
        let mock = MockHttp::new(vec![]);
        assert!(set_cape(&mock, "t", "  ").is_err());
        assert!(mock.log.borrow().is_empty());
    }

    #[test]
    fn an_expired_token_says_so() {
        let mock = MockHttp::new(vec![HttpResponse {
            status: 401,
            body: "{\"error\":\"Unauthorized\"}".to_string(),
        }]);
        let err = fetch_wardrobe(&mock, "old").unwrap_err();
        assert!(err.contains("重新登录"), "{}", err);
    }

    #[test]
    fn a_rejected_skin_repeats_what_the_server_said() {
        let mock = MockHttp::new(vec![HttpResponse {
            status: 400,
            body: "{\"error\":\"Invalid skin\"}".to_string(),
        }]);
        let err = upload_skin(&mock, "t", &skin_png(64, 64), Variant::Classic).unwrap_err();
        assert!(err.contains("Invalid skin"), "{}", err);
    }
}
