//! 皮肤头像。
//!
//! 从会话服务器拿到玩家的皮肤地址，下载那张 PNG，裁出头像。整条链路都是
//! 可选增强：拿不到就返回错误，界面回退成「首字母方块」，不该因此挡住启动。
//!
//! 皮肤贴图的位置（这是 Minecraft 的老规矩，64×64 的图）：
//! - 脸：左上角 `(8, 8)` 起 8×8；
//! - 帽子/头发那层：`(40, 8)` 起 8×8，叠在脸上面。
//!
//! 64×32 的老皮肤只有脸，没有叠层。

use std::path::{Path, PathBuf};

use crate::auth::Http;
use crate::transport::HttpResponse;
use crate::json::{self, Json};
use crate::png;

/// 头像的边长（像素）。皮肤里那张脸只有 8×8，放大到 32 才看得清。
pub const HEAD_PX: usize = 32;

const OFFICIAL_SESSION: &str = "https://sessionserver.mojang.com/session/minecraft/profile";

/// 会话服务器上查某个 UUID 的档案地址。
pub fn profile_url(api_root: Option<&str>, uuid: &str) -> String {
    match api_root {
        Some(root) => format!(
            "{}/sessionserver/session/minecraft/profile/{}",
            root.trim_end_matches('/'),
            uuid
        ),
        None => format!("{}/{}", OFFICIAL_SESSION, uuid),
    }
}

/// 皮肤 PNG 的缓存位置。
pub fn cache_path(data_dir: &Path, uuid: &str) -> PathBuf {
    data_dir.join("skins").join(format!("{}.png", uuid))
}

/// 只读缓存，不联网。界面启动时先用它把已经存过的头像画出来。
pub fn head_from_cache(data_dir: &Path, uuid: &str) -> Option<Vec<u8>> {
    let bytes = std::fs::read(cache_path(data_dir, uuid)).ok()?;
    head_from_skin_png(&bytes).ok()
}

/// 联网取皮肤并裁成头像，成功后把原图存进缓存。
pub fn head_for(
    http: &dyn Http,
    api_root: Option<&str>,
    uuid: &str,
    data_dir: &Path,
) -> Result<Vec<u8>, String> {
    if let Some(head) = head_from_cache(data_dir, uuid) {
        return Ok(head);
    }
    let url = profile_url(api_root, uuid);
    let response: HttpResponse = http
        .get(&url, &[("Accept", "application/json")])
        .map_err(|e| format!("查询皮肤失败: {}", e))?;
    if !response.is_success() {
        return Err(format!("会话服务器返回 {}", response.status));
    }
    let skin_url = skin_url_from_profile(&response.body)?;

    let png_bytes = crate::transport::fetch_bytes(&skin_url)
        .map_err(|e| format!("下载皮肤失败: {}", e))?;
    let head = head_from_skin_png(&png_bytes)?;

    // 缓存写失败不算错：下次再下一遍就是了。
    let path = cache_path(data_dir, uuid);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_ok() {
            let _ = std::fs::write(&path, &png_bytes);
        }
    }
    Ok(head)
}

/// 从档案 JSON 里挖出皮肤地址。
pub fn skin_url_from_profile(text: &str) -> Result<String, String> {
    let root = json::parse(text).map_err(|e| format!("档案不是 JSON: {}", e))?;
    let properties = root
        .get("properties")
        .and_then(Json::as_array)
        .ok_or("这个档案里没有签名属性")?;
    let textures = properties
        .iter()
        .find(|p| p.get("name").and_then(Json::as_str) == Some("textures"))
        .and_then(|p| p.get("value").and_then(Json::as_str))
        .ok_or("档案里没有 textures 属性")?;

    let decoded = base64_decode(textures)?;
    let decoded = String::from_utf8(decoded).map_err(|e| format!("皮肤数据不是文本: {}", e))?;
    let skin = json::parse(&decoded)
        .map_err(|e| format!("皮肤数据不是 JSON: {}", e))?
        .get("textures")
        .and_then(|t| t.get("SKIN"))
        .and_then(|s| s.get("url"))
        .and_then(Json::as_str)
        .ok_or("这个账号没有设置皮肤")?
        .to_string();
    Ok(skin)
}

/// 原版默认皮肤里那张 8×8 的脸（classic，Steve）。
///
/// 取自客户端 jar 的 `assets/minecraft/textures/entity/player/wide/steve.png`：
/// 区域 `(8, 8)` 起 8×8，每 4 个字节一个像素的 RGBA。帽子层 `(40, 8)` 那一片
/// 在默认皮肤里是**全透明**的，所以这里不必存它 —— 少一份数据，也少一处会错的地方。
const STEVE_FACE: &str = "\
332411ff332411ff3f2a15ff3f2a15ff3f2a15ff3f2a15ff332411ff2b1e0dff\
241808ff332411ff332411ff3f2a15ff3f2a15ff332411ff3f2a15ff332411ff\
2b1e0dff9b6349ffb3795effb7836bffb3795effaa7259ff9b6349ff342512ff\
9b6349ffaa7259ffb3795effb3795effaa7259ffaa7259ffaa7259ff9b6349ff\
aa7259ffffffffff523d89ffaa7259ff9b6349ff523d89ffffffffffaa7259ff\
9b6349ffaa7259ffaa7259ff6a4030ff6a4030ffaa7259ffaa7259ff9b6349ff\
90593fff8f5e3eff492510ff774235ff774235ff421d0aff8f5e3eff815339ff\
94603eff815339ff421d0aff492510ff421d0aff492510ff815339ff8f5e3eff";

/// 原版默认皮肤里那张 8×8 的脸（slim，Alex）。
///
/// 取值同上，来自 `assets/minecraft/textures/entity/player/wide/alex.png`：
/// 外层的 `wide/` 与 `slim/` 只有身体模型不同，脸这一块是一样的。
const ALEX_FACE: &str = "\
e58d3ffff3a858ffeb983ffff3a858ffe58d3fffeb983ffff3a858ffe58d3ff\
feb983ffff3a858ffe58d3fffeb983fffe58d3fffe58d3fffeb983ffff3a858ff\
eb983fffe58d3fffeb983fffe58d3fffdfc4a2ffdfc4a2ffe58d3fffeb983ff\
feb983fffeb983fffe58d3fffebd0b0ffefdabfffebd0b0ffebd0b0ffe58d3ff\
febd0b0fffffffff236224ffefdabfffefdabfff236224ffffffffffebd0b0ff\
efdabfffefdabfffefdabfffefdabfffefdabfffefdabfffefdabfffefdabff\
fefdabfffefdabfffefdabfffefdabfffefbbb1ffefbbb1ffefdabfffefdabff\
febd0b0ffebd0b0ffefdabfffefdabfffefdabfffefdabfffefdabfffebd0b0ffdfc4a2ff";

/// 离线账号的头像：原版会按 UUID 在 Steve 与 Alex 之间挑一张。
///
/// 说清楚一件事：原版**不会为每个 UUID 生成独一无二的皮肤**，
/// `DefaultPlayerSkin.getSkinType(uuid)` 只是 `uuid.hashCode() & 1` 决定用哪张默认皮肤。
/// 所以这里也一样 —— 偶数拿 Steve，奇数拿 Alex。
pub fn default_head(uuid: &str) -> Vec<u8> {
    let face = if is_slim(uuid) { ALEX_FACE } else { STEVE_FACE };
    upscale_face(face)
}

/// 复刻 `java.util.UUID.hashCode()` 的最低位：奇数说明原版会给 Alex。
fn is_slim(uuid: &str) -> bool {
    match uuid_bits(uuid) {
        Some((most, least)) => {
            let hash = ((most >> 32) ^ most ^ (least >> 32) ^ least) as u32 as i32;
            hash & 1 != 0
        }
        // 认不出来的 UUID 就按 Steve 来：这是大多数皮肤站的默认观感
        None => false,
    }
}

/// 把 `8-4-4-4-12` 形式的 UUID 拆成两个 64 位整数。
fn uuid_bits(uuid: &str) -> Option<(u64, u64)> {
    let digits: String = uuid.chars().filter(|c| *c != '-').collect();
    if digits.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[i * 2..i * 2 + 2], 16).ok()?;
    }
    let mut most = [0u8; 8];
    let mut least = [0u8; 8];
    most.copy_from_slice(&bytes[..8]);
    least.copy_from_slice(&bytes[8..]);
    Some((u64::from_be_bytes(most), u64::from_be_bytes(least)))
}

/// 十六进制写死的 8×8 脸放大成 `HEAD_PX` 见方。
fn upscale_face(face_hex: &str) -> Vec<u8> {
    let pixels = face_hex.len() / 8;
    let scale = HEAD_PX / 8;
    let mut out = vec![0u8; HEAD_PX * HEAD_PX * 4];
    for y in 0..HEAD_PX {
        for x in 0..HEAD_PX {
            let src_pixel = (y / scale) * 8 + (x / scale);
            if src_pixel >= pixels {
                continue;
            }
            let base = src_pixel * 8;
            let dst = (y * HEAD_PX + x) * 4;
            for (i, slot) in out[dst..dst + 4].iter_mut().enumerate() {
                let pair = &face_hex[base + i * 2..base + i * 2 + 2];
                *slot = u8::from_str_radix(pair, 16).unwrap_or(0);
            }
        }
    }
    out
}

/// 把整张皮肤图变成头像（`HEAD_PX`×`HEAD_PX` 的 RGBA）。
///
/// 有外层（也就是大家说的「3D 层」，帽子、头发、面具那一圈）时画成一个
/// 斜看着的小方块，能看出厚度；没有外层的皮肤还是平着裁一张脸，保持原样。
pub fn head_from_skin_png(data: &[u8]) -> Result<Vec<u8>, String> {
    let image = png::decode(data)?;
    if image.width < 16 || image.height < 16 {
        return Err(format!("皮肤尺寸不对: {}×{}", image.width, image.height));
    }
    if has_outer_layer(&image) {
        Ok(layered_head(&image))
    } else {
        Ok(flat_head(&image))
    }
}

/// 这张皮肤有没有真的用上外层。
///
/// 老皮肤（64×32）根本没有这块；新皮肤有这块但通常整片透明，
/// 所以不能只看尺寸，得看有没有一个不透明的像素。
fn has_outer_layer(image: &png::Image) -> bool {
    if image.width < 64 || image.height < 64 {
        return false;
    }
    (0..8).any(|y| (0..8).any(|x| image.pixel(40 + x, 8 + y)[3] > 0))
}

/// 3D 头像在脸的外面留多宽的边。
///
/// 脸必须和 2D 皮肤一样大（32px 铺满），外壳才能叠到脸的外面 ——
/// 所以画布本身加大，多出来的一圈就是给外壳探出去用的。
pub const HEAD_PAD: usize = 4;

/// 有 3D 层时的画布边长：32 的脸 + 两侧各 4 的边。
pub const HEAD_3D_PX: usize = HEAD_PX + HEAD_PAD * 2;

/// 外壳的边长：比脸大 6px，正好在留白里探出 3px。
const LAYER_SPAN: usize = HEAD_PX + 6;

/// 外壳下沿那道断面的亮度倍数。
const OUTER_RIM: f32 = 1.45;

/// 正面视图 + 叠在脸外头的壳。
///
/// 视角不动，脸也不缩小：脸在 `HEAD_3D_PX` 画布正中铺满 32px，和 2D 皮肤
/// 一模一样。外壳画得比脸大一圈（38px），于是它的轮廓从脸的四周探出来 ——
/// 那圈「探出去的边」就是厚度。
///
/// 再补一笔：壳的下沿提亮一线（切开的断面）。
///
/// 不给壳画阴影：深色的头发压暗之后就是一团黑边，看着像描了一圈粗线，
/// 反而把头像弄脏了。厚度交给「壳比脸大」这件事本身说。
///
/// 不做等距方块：那要转视角，脸会变小、像素会糊。
fn layered_head(image: &png::Image) -> Vec<u8> {
    let size = HEAD_3D_PX;
    let layer_at = ((size - LAYER_SPAN) / 2) as f32;

    let mut out = vec![0u8; size * size * 4];

    // 脸：8×8 铺满中间那 32px，和 2D 皮肤同样大小
    for y in 0..HEAD_PX {
        for x in 0..HEAD_PX {
            let px = image.pixel(8 + x / CELL, 8 + y / CELL);
            let i = ((y + HEAD_PAD) * size + (x + HEAD_PAD)) * 4;
            out[i..i + 4].copy_from_slice(&px);
        }
    }

    // 外壳：把画布像素反解回它那张 8×8 上再取样
    let cell_size = LAYER_SPAN as f32 / 8.0;
    let layer = |x: usize, y: usize| -> Option<[u8; 4]> {
        let u = x as f32 + 0.5 - layer_at;
        let v = y as f32 + 0.5 - layer_at;
        if !(0.0..LAYER_SPAN as f32).contains(&u) || !(0.0..LAYER_SPAN as f32).contains(&v) {
            return None;
        }
        let cx = (u / cell_size) as usize;
        let cy = (v / cell_size) as usize;
        let px = image.pixel(40 + cx.min(7), 8 + cy.min(7));
        (px[3] > 0).then_some(px)
    };

    // 把壳盖上去，下沿提亮一线
    for y in 0..size {
        for x in 0..size {
            let Some(mut src) = layer(x, y) else { continue };
            if y + 1 >= size || layer(x, y + 1).is_none() {
                for channel in src.iter_mut().take(3) {
                    *channel = (*channel as f32 * OUTER_RIM).min(255.0) as u8;
                }
            }
            let i = (y * size + x) * 4;
            let mut dst = [0u8; 4];
            dst.copy_from_slice(&out[i..i + 4]);
            let blended = over(src, dst);
            out[i..i + 4].copy_from_slice(&blended);
        }
    }
    out
}

/// 8×8 的脸在画布上每格占多少像素。
const CELL: usize = HEAD_PX / 8;

/// 平着裁一张脸（没有外层的皮肤走这条）。
fn flat_head(image: &png::Image) -> Vec<u8> {
    // 先把 8×8 的脸（含叠层）合出来
    let mut face = vec![0u8; 8 * 8 * 4];
    let has_hat = image.width >= 64 && image.height >= 64;
    for y in 0..8 {
        for x in 0..8 {
            let mut px = image.pixel(8 + x, 8 + y);
            if has_hat {
                px = over(image.pixel(40 + x, 8 + y), px);
            }
            let i = (y * 8 + x) * 4;
            face[i..i + 4].copy_from_slice(&px);
        }
    }

    // 最近邻放大到 HEAD_PX，皮肤是像素画，放大时不该被糊掉
    let scale = HEAD_PX / 8;
    let mut out = vec![0u8; HEAD_PX * HEAD_PX * 4];
    for y in 0..HEAD_PX {
        for x in 0..HEAD_PX {
            let src = ((y / scale) * 8 + (x / scale)) * 4;
            let dst = (y * HEAD_PX + x) * 4;
            out[dst..dst + 4].copy_from_slice(&face[src..src + 4]);
        }
    }
    out
}

/// 标准 src-over 合成：叠层压在被覆盖的那一层上。
fn over(top: [u8; 4], bottom: [u8; 4]) -> [u8; 4] {
    let a = top[3] as u32;
    if a == 0 {
        return bottom;
    }
    if a == 255 {
        return top;
    }
    let b = bottom[3] as u32;
    let blend = |t: u8, b: u8| -> u8 {
        let t = t as u32;
        let b = b as u32;
        // 两个都是预乘前的前提：这里按普通 alpha 混合算
        let out = (t * a + b * (255 - a)) / 255;
        out.min(255) as u8
    };
    let alpha = a + b * (255 - a) / 255;
    [
        blend(top[0], bottom[0]),
        blend(top[1], bottom[1]),
        blend(top[2], bottom[2]),
        alpha.min(255) as u8,
    ]
}

/// 标准 base64（带 `+` `/` 和补位的 `=`）。
///
/// 不用 [`crate::codec::base64url_nopad_decode`]：Mojang 的签名属性用的是
/// 普通字母表，`+` 和 `/` 在 URL 版本里是非法字符。
fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for ch in text.bytes() {
        let value = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => return Err(format!("皮肤数据里有非法字符: {}", ch as char)),
        } as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一张假皮肤：背景透明，脸刷成红色，帽子层只盖左上 2×2。
    ///
    /// `legacy` 给出 64×32 的老皮肤（没有帽子层）。真实皮肤里帽子层
    /// 大部分是透明的，所以这里也必须留透明——用不透明色盖满会正好
    /// 验证不到「叠层只盖住它自己那一块」这件事。
    fn fake_skin(legacy: bool) -> Vec<u8> {
        let (w, h) = if legacy { (64usize, 32usize) } else { (64, 64) };
        let mut rgba = vec![0u8; w * h * 4];
        let mut put = |x: usize, y: usize, color: [u8; 4]| {
            let i = (y * w + x) * 4;
            rgba[i..i + 4].copy_from_slice(&color);
        };
        for y in 8..16 {
            for x in 8..16 {
                put(x, y, [200, 60, 60, 255]);
            }
        }
        if !legacy {
            for y in 8..10 {
                for x in 8..10 {
                    put(40 + (x - 8), y, [20, 120, 220, 255]);
                }
            }
        }
        encode_png(&rgba, w, h)
    }

    /// 极简 PNG 编码（测试用）：只写 RGBA8、不滤波、单块 IDAT。
    fn encode_png(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
        let mut raw = Vec::with_capacity((width * 4 + 1) * height);
        for y in 0..height {
            raw.push(0);
            raw.extend_from_slice(&rgba[y * width * 4..(y + 1) * width * 4]);
        }
        // zlib 头 + 存储块（不压缩）+ adler32（这里随便填，解码器不校验）
        let mut zlib = vec![0x78, 0x01];
        for (i, chunk) in raw.chunks(65535).enumerate() {
            let last = if (i + 1) * 65535 >= raw.len() { 1u8 } else { 0u8 };
            zlib.push(last);
            let len = chunk.len() as u16;
            zlib.extend_from_slice(&len.to_le_bytes());
            zlib.extend_from_slice(&(!len).to_le_bytes());
            zlib.extend_from_slice(chunk);
        }
        zlib.extend_from_slice(&[0, 0, 0, 0]);

        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(width as u32).to_be_bytes());
        ihdr.extend_from_slice(&(height as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        push_chunk(&mut out, b"IHDR", &ihdr);
        push_chunk(&mut out, b"IDAT", &zlib);
        push_chunk(&mut out, b"IEND", &[]);
        out
    }

    fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&[0, 0, 0, 0]); // CRC 校验位，解码器不验
    }

    #[test]
    fn a_default_head_is_a_scaled_face() {
        let head = default_head("069a79f444e94726a5befca90e38aaf5");
        assert_eq!(head.len(), HEAD_PX * HEAD_PX * 4);
        // 左上角那一点就是 Steve 皮肤 (8,8) 的肤色
        assert_eq!(&head[0..4], &[0x33, 0x24, 0x11, 0xff]);
        // 放大是最近邻：相邻两点、上下两行都应该完全一样
        assert_eq!(&head[0..4], &head[4..8]);
        assert_eq!(&head[0..4], &head[(HEAD_PX * 4)..(HEAD_PX * 4 + 4)]);
    }

    #[test]
    fn the_uuid_decides_between_steve_and_alex() {
        // 这个 UUID 的最低位是 0，原版给 Steve
        assert!(!is_slim("069a79f444e94726a5befca90e38aaf5"));
        assert_eq!(
            &default_head("069a79f444e94726a5befca90e38aaf5")[0..4],
            &[0x33, 0x24, 0x11, 0xff]
        );
        // 这个的最低位是 1，原版给 Alex（皮肤偏橙）
        assert!(is_slim("00000000-0000-0000-0000-000000000001"));
        assert_eq!(
            &default_head("00000000-0000-0000-0000-000000000001")[0..4],
            &[0xe5, 0x8d, 0x3f, 0xff]
        );
    }

    #[test]
    fn a_broken_uuid_still_gets_a_head() {
        let head = default_head("这不是-uuid");
        assert_eq!(head.len(), HEAD_PX * HEAD_PX * 4);
        assert_eq!(&head[0..4], &[0x33, 0x24, 0x11, 0xff]);
    }

    #[test]
    fn a_skin_with_an_outer_layer_lifts_the_layer_up() {
        // 老皮肤没有外层：脸铺满 32 的画布
        let flat = head_from_skin_png(&fake_skin(true)).unwrap();
        assert_eq!(flat.len(), HEAD_PX * HEAD_PX * 4);
        assert_eq!(&flat[0..4], &[200, 60, 60, 255]);

        // 有新层的皮肤：画布加大到 40，脸还是 32 铺在正中，壳从四周探出来
        let layered = head_from_skin_png(&fake_skin(false)).unwrap();
        assert_eq!(layered.len(), HEAD_3D_PX * HEAD_3D_PX * 4);
        assert_eq!(layered[3], 0, "画布角上应该是空的（留给外壳探出去）");

        let mid = (HEAD_3D_PX / 2 * HEAD_3D_PX + HEAD_3D_PX / 2) * 4;
        assert_eq!(layered[mid + 3], 255);

        // 壳必须探到脸框外面：左边那 4px 留白里得找得到不透明像素
        let overhang = (0..HEAD_3D_PX)
            .flat_map(|y| (0..HEAD_PAD).map(move |x| (x, y)))
            .any(|(x, y)| layered[(y * HEAD_3D_PX + x) * 4 + 3] > 0);
        assert!(overhang, "外壳应该从脸的四边探出来");
    }

    #[test]
    fn an_empty_outer_layer_keeps_the_flat_head() {
        // 新版皮肤有外层那一块，但整片透明 —— 这种还是平着裁
        assert!(has_outer_layer(&png::decode(&fake_skin(false)).unwrap()));
        assert!(!has_outer_layer(&png::decode(&fake_skin(true)).unwrap()));

        // 把外层擦干净，输出应该跟老皮肤一样铺满画布
        let mut rgba = vec![0u8; 64 * 64 * 4];
        for y in 8..16 {
            for x in 8..16 {
                let i = (y * 64 + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[200, 60, 60, 255]);
            }
        }
        let head = head_from_skin_png(&encode_png(&rgba, 64, 64)).unwrap();
        assert_ne!(head[3], 0, "没有外层就该是平铺的，左上角得有不透明像素");
    }

    #[test]
    fn a_head_is_the_face_scaled_up() {
        // 用没有外层的那张：平着裁的时候整张脸铺满画布
        let head = head_from_skin_png(&fake_skin(true)).expect("应当能裁出头像");
        assert_eq!(head.len(), HEAD_PX * HEAD_PX * 4);
        // 左上角就是脸的红（老皮肤没有帽子层可叠）
        assert_eq!(&head[0..4], &[200, 60, 60, 255]);
        assert_eq!(head.len(), HEAD_PX * HEAD_PX * 4);
        // 最近的采样点也是红
        let px = (16 * HEAD_PX + 16) * 4;
        assert_eq!(&head[px..px + 4], &[200, 60, 60, 255]);
    }

    #[test]
    fn a_legacy_skin_has_no_hat_layer() {
        let head = head_from_skin_png(&fake_skin(true)).expect("应当能裁出头像");
        assert_eq!(&head[0..4], &[200, 60, 60, 255]);
    }

    #[test]
    fn profile_urls_follow_the_account_kind() {
        assert_eq!(
            profile_url(None, "abc"),
            "https://sessionserver.mojang.com/session/minecraft/profile/abc"
        );
        assert_eq!(
            profile_url(Some("https://littleskin.cn/api/yggdrasil/"), "abc"),
            "https://littleskin.cn/api/yggdrasil/sessionserver/session/minecraft/profile/abc"
        );
    }

    #[test]
    fn the_skin_url_is_dug_out_of_the_properties() {
        let payload = "{\"textures\":{\"SKIN\":{\"url\":\"http://textures.minecraft.net/texture/abc\"}}}";
        let encoded = base64_encode(payload.as_bytes());
        let profile = format!(
            "{{\"id\":\"x\",\"name\":\"Steve\",\"properties\":[{{\"name\":\"textures\",\"value\":\"{}\"}}]}}",
            encoded
        );
        assert_eq!(
            skin_url_from_profile(&profile).unwrap(),
            "http://textures.minecraft.net/texture/abc"
        );
    }

    #[test]
    fn a_profile_without_a_skin_says_so() {
        let profile = "{\"id\":\"x\",\"name\":\"Steve\",\"properties\":[]}";
        let err = skin_url_from_profile(profile).unwrap_err();
        assert!(err.contains("textures"), "实际是: {}", err);
    }

    #[test]
    fn base64_round_trips_through_the_standard_alphabet() {
        let bytes: Vec<u8> = (0u8..=255).collect();
        let encoded = base64_encode(&bytes);
        assert!(encoded.contains('=') || encoded.len() % 4 == 0);
        assert_eq!(base64_decode(&encoded).unwrap(), bytes);
    }

    fn base64_encode(data: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(TABLE[(n >> 18) as usize & 63] as char);
            out.push(TABLE[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                TABLE[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                TABLE[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }
}
