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

/// 原版默认皮肤里那张 8×8 的脸。
///
/// 直接从客户端 jar 的 `assets/minecraft/textures/entity/player/{slim,wide}/*.png`
/// 的 `(8, 8)` 起 8×8 抄下来（每像素 RGBA 四字节）。帽子层 `(40, 8)` 那一片在
/// 这些默认皮肤里是**全透明**的，所以不必存 —— 少一份数据，也少一处会错的地方。
///
/// 1.19.4 起原版有 18 张默认皮肤：9 个名字 × 宽/细两种模型。`slim/` 与 `wide/`
/// 只有身体模型不同，脸是一样的，所以每个名字只存一份脸。
const FACE_ALEX: &str = "\
e58d3ffff3a858ffeb983ffff3a858ffe58d3fffeb983ffff3a858ffe58d3fff\
eb983ffff3a858ffe58d3fffeb983fffe58d3fffe58d3fffeb983ffff3a858ff\
eb983fffe58d3fffeb983fffe58d3fffdfc4a2ffdfc4a2ffe58d3fffeb983fff\
eb983fffeb983fffe58d3fffebd0b0ffefdabfffebd0b0ffebd0b0ffe58d3fff\
ebd0b0ffffffffff236224ffefdabfffefdabfff236224ffffffffffebd0b0ff\
efdabfffefdabfffefdabfffefdabfffefdabfffefdabfffefdabfffefdabfff\
efdabfffefdabfffefdabfffefbbb1ffefbbb1ffefdabfffefdabfffebd0b0ff\
ebd0b0ffefdabfffefdabfffefdabfffefdabfffefdabfffebd0b0ffdfc4a2ff";

const FACE_ARI: &str = "\
803012ff9d4522ff933f1eff933f1effe57f61ff873718ff933f1eff873718ff\
873718ff933f1effe57f61fff1936efff1936effe57f61ff933f1eff803012ff\
933f1efff1936efff9a786fff9a786fff9a786fff9a786fff1936eff933f1eff\
933f1efff9a786fff9a786fff9a786fff9a786fff9a786fff9a786ff933f1eff\
873718ffffffffff240303fff9a786fff9a786ff240303ffffffffff873718ff\
f1816efff1816efff9a786fff9a786fff9a786fff9a786fff1816efff1816eff\
f9a786fff9a786fff9a786ffe57f61ffe57f61fff9a786fff9a786fff9a786ff\
f1936efff9a786fff9a786fff9a786fff9a786fff9a786fff9a786fff1936eff";

const FACE_EFE: &str = "\
865c8eff8a6294ff8a6294ff956b9eff8a6294ff8a6294ff784f7eff8a6294ff\
865c8eff815889ff93699cff815889ff8a6294ff8a6294ff663f69ff8a6294ff\
8a6294ff815889ffa46840ffab724cff8a6294ff784f7effa46840ff8a6294ff\
865c8effa46840ffab724cffab724cffab724cffab724cffab724cff815889ff\
8a6294ffffffffff692b34ffab724cffab724cff692b34ffffffffff8a6294ff\
956b9effab724cffab724cffab724cffab724cffab724cffab724cff956b9eff\
956b9effa66849ffab724cff774320ff774320ffab724cffa66849ff9c73a9ff\
8a6294ffab724cffab724cffab724cffab724cffab724cffab724cff8a6294ff";

const FACE_KAI: &str = "\
fbed91fffbed91fffbe28afffbed91fffbed91fffbed91fffbed91fffbed91ff\
fbed91fffbe28afffbed91fffbed91fffbed91ffe8ba6dffe8ba6dfffbed91ff\
fbed91fffbed91fffbed91fffbed91ffce884effdf9658ffdf9658fffcf4aaff\
fbed91ffce884effdf9658ffdf9658ffdf9658ffdf9658ffdf9658fffcf4aaff\
fcf4aaffffffffff436753ffdf9658ffdf9658ff436753fffffffffffcf4aaff\
fcf4aaffdf9658ffdf9658ffdf9658ffdf9658ffdf9658ffdf9658fffbed91ff\
fbed91ffdf9658ffdf9658ff774320ff774320ffdf9658ffdf9658fffbed91ff\
fbed91fffbed91ffdf9658ffdf9658ffdf9658ffdf9658fffbed91fffbed91ff";

const FACE_MAKENA: &str = "\
261216ff1f0a0eff32191eff32191eff1e0e11ff261216ff32191eff261216ff\
32191eff261216ff32191eff1f0a0eff443528ff443528ff32191eff32191eff\
261216ff32191eff38281bff261216ff1f0a0eff443528ff38281bff261216ff\
38281bff443528ff443528ff443528ff443528ff443528ff443528ff38281bff\
443528ffd3cbc6ff2e1800ff443528ff443528ff2e1800ffd3cbc6ff443528ff\
5a4534ff443528ff443528ff443528ff443528ff443528ff443528ff5a4534ff\
38281bff443528ff443528ff302218ff302218ff443528ff443528ff38281bff\
2a1e0fff38281bff443528ff443528ff443528ff443528ff38281bff2a1e0fff";

const FACE_NOOR: &str = "\
2e180eff381e11ff2e180eff593420ff2e180eff381e11ff2e180eff381e11ff\
381e11ffa75c43ffb9674affa75c43ffb9674affa75c43ffb9674aff381e11ff\
a75c43ffb9674affcd795bffb9674affb9674affb9674affb9674affa75c43ff\
b9674affa9563affa9563affb9674affb9674affa9563affa9563affb9674aff\
a9563affffffffff582c1dffb9674affb9674aff582c1dffffffffffa9563aff\
b9674affb9674affb9674affb9674affb9674affb9674affb9674affb9674aff\
a75c43ffb9674affb9674aff733b27ff733b27ffb9674affb9674affa75c43ff\
914731ffa75c43ffb9674affb9674affb9674affb9674affa75c43ff914731ff";

const FACE_STEVE: &str = "\
332411ff332411ff3f2a15ff3f2a15ff3f2a15ff3f2a15ff332411ff2b1e0dff\
241808ff332411ff332411ff3f2a15ff3f2a15ff332411ff3f2a15ff332411ff\
2b1e0dff9b6349ffb3795effb7836bffb3795effaa7259ff9b6349ff342512ff\
9b6349ffaa7259ffb3795effb3795effaa7259ffaa7259ffaa7259ff9b6349ff\
aa7259ffffffffff523d89ffaa7259ff9b6349ff523d89ffffffffffaa7259ff\
9b6349ffaa7259ffaa7259ff6a4030ff6a4030ffaa7259ffaa7259ff9b6349ff\
90593fff8f5e3eff492510ff774235ff774235ff421d0aff8f5e3eff815339ff\
94603eff815339ff421d0aff492510ff421d0aff492510ff815339ff8f5e3eff";

const FACE_SUNNY: &str = "\
2f2f2fff383838ff2f2f2fff383838ff494949ff2f2f2fff383838ff2f2f2fff\
2f2f2fff383838ff494949ff383838ff494949ff2f2f2fff383838ff2f2f2fff\
2f2f2fff2f2f2fff494949ff2f2f2fff383838ff383838ff2f2f2fff2f2f2fff\
222222ff2f2f2fff383838ff222222ff2f2f2fff2f2f2fff2f2f2fff222222ff\
d8844bffffffffff000000ffd8844bffd8844bff000000ffffffffffd8844bff\
f29f5ffff29f5ffff29f5ffff29f5ffff29f5ffff29f5ffff29f5ffff29f5fff\
d8844bfff29f5ffff29f5fff774320ff774320fff29f5ffff29f5fffd8844bff\
c1723fffd8844bfff29f5ffff29f5ffff29f5ffff29f5fffd8844bffc1723fff";

const FACE_ZURI: &str = "\
0e0401ff0e0401ff1b110dff221611ff221611ff1b110dff0e0401ff0e0401ff\
0e0401ff5f3e29ff6d4830ff7e5337ff7e5337ff6d4830ff5f3e29ff0e0401ff\
6d4830ff7e5337ff7e5337ff7e5337ff7e5337ff7e5337ff7e5337ff6d4830ff\
7e5337ff7e5337ff7e5337ff7e5337ff7e5337ff7e5337ff7e5337ff7e5337ff\
7e5337ffffffffff411e02ff7e5337ff7e5337ff411e02ffffffffff6d4830ff\
92674cff7e5337ff7e5337ff7e5337ff7e5337ff7e5337ff7e5337ff92674cff\
7e5337ff7e5337ff7e5337ff753c28ff753c28ff7e5337ff7e5337ff6d4830ff\
5f3e29ff6d4830ff7e5337ff7e5337ff7e5337ff7e5337ff6d4830ff5f3e29ff";

/// 原版的一张默认皮肤。
pub struct DefaultSkin {
    /// 客户端 jar 里的文件名，比如 `steve`、`ari`。
    pub name: &'static str,
    /// 这张皮是不是细手臂（`slim`）。
    pub slim: bool,
    /// 8×8 的脸，按行写成 RGBA 十六进制。
    face: &'static str,
    /// 整张 64×64 的展开图，原样嵌着客户端 jar 里那张 PNG。
    ///
    /// 界面要画「完整展开图」时直接用这张；只画头像时用上面那份 8×8
    /// 就够了，省得为了一格脸去解一整张图。
    pub png: &'static [u8],
}

/// 原版的默认皮肤表，顺序与客户端 `DefaultPlayerSkin.DEFAULT_SKINS` 逐项对应。
///
/// **顺序不能动。** 原版是 `Math.floorMod(uuid.hashCode(), DEFAULT_SKINS.length)`
/// 取下标，排错一位就会把别人的脸安到这个 UUID 头上。
pub const DEFAULT_SKINS: [DefaultSkin; 18] = [
    DefaultSkin {
        name: "alex",
        slim: true,
        face: FACE_ALEX,
        png: include_bytes!("../assets/skins/slim/alex.png"),
    },
    DefaultSkin {
        name: "ari",
        slim: true,
        face: FACE_ARI,
        png: include_bytes!("../assets/skins/slim/ari.png"),
    },
    DefaultSkin {
        name: "efe",
        slim: true,
        face: FACE_EFE,
        png: include_bytes!("../assets/skins/slim/efe.png"),
    },
    DefaultSkin {
        name: "kai",
        slim: true,
        face: FACE_KAI,
        png: include_bytes!("../assets/skins/slim/kai.png"),
    },
    DefaultSkin {
        name: "makena",
        slim: true,
        face: FACE_MAKENA,
        png: include_bytes!("../assets/skins/slim/makena.png"),
    },
    DefaultSkin {
        name: "noor",
        slim: true,
        face: FACE_NOOR,
        png: include_bytes!("../assets/skins/slim/noor.png"),
    },
    DefaultSkin {
        name: "steve",
        slim: true,
        face: FACE_STEVE,
        png: include_bytes!("../assets/skins/slim/steve.png"),
    },
    DefaultSkin {
        name: "sunny",
        slim: true,
        face: FACE_SUNNY,
        png: include_bytes!("../assets/skins/slim/sunny.png"),
    },
    DefaultSkin {
        name: "zuri",
        slim: true,
        face: FACE_ZURI,
        png: include_bytes!("../assets/skins/slim/zuri.png"),
    },
    DefaultSkin {
        name: "alex",
        slim: false,
        face: FACE_ALEX,
        png: include_bytes!("../assets/skins/wide/alex.png"),
    },
    DefaultSkin {
        name: "ari",
        slim: false,
        face: FACE_ARI,
        png: include_bytes!("../assets/skins/wide/ari.png"),
    },
    DefaultSkin {
        name: "efe",
        slim: false,
        face: FACE_EFE,
        png: include_bytes!("../assets/skins/wide/efe.png"),
    },
    DefaultSkin {
        name: "kai",
        slim: false,
        face: FACE_KAI,
        png: include_bytes!("../assets/skins/wide/kai.png"),
    },
    DefaultSkin {
        name: "makena",
        slim: false,
        face: FACE_MAKENA,
        png: include_bytes!("../assets/skins/wide/makena.png"),
    },
    DefaultSkin {
        name: "noor",
        slim: false,
        face: FACE_NOOR,
        png: include_bytes!("../assets/skins/wide/noor.png"),
    },
    DefaultSkin {
        name: "steve",
        slim: false,
        face: FACE_STEVE,
        png: include_bytes!("../assets/skins/wide/steve.png"),
    },
    DefaultSkin {
        name: "sunny",
        slim: false,
        face: FACE_SUNNY,
        png: include_bytes!("../assets/skins/wide/sunny.png"),
    },
    DefaultSkin {
        name: "zuri",
        slim: false,
        face: FACE_ZURI,
        png: include_bytes!("../assets/skins/wide/zuri.png"),
    },
];

/// 原版会给这个 UUID 挑哪一张默认皮肤：`floorMod(hashCode, 18)`。
///
/// 说清楚一件事：原版**不会为每个 UUID 生成独一无二的皮肤**，全服就这 18 张，
/// UUID 只是决定落在哪一张上。
pub fn default_skin(uuid: &str) -> &'static DefaultSkin {
    &DEFAULT_SKINS[default_index(uuid)]
}

/// 离线账号的头像：原版会按 UUID 从 18 张默认皮肤里挑一张，这里也画那一张。
pub fn default_head(uuid: &str) -> Vec<u8> {
    upscale_face(default_skin(uuid).face)
}

/// 原版会给这个 UUID 用哪一张默认皮肤（`alex`、`makena` 这些文件名）。
pub fn default_skin_name(uuid: &str) -> &'static str {
    default_skin(uuid).name
}

/// 这个 UUID 对应的那张默认皮肤整图（64×64 的 PNG 字节）。
///
/// 画「完整展开图」用它；只要头像的话用 [`default_head`]，不必解一整张图。
pub fn default_skin_png(uuid: &str) -> &'static [u8] {
    default_skin(uuid).png
}

/// 复刻 `java.util.UUID.hashCode()`，再按原版的 `Math.floorMod` 折到表长上。
fn default_index(uuid: &str) -> usize {
    match uuid_bits(uuid) {
        Some((most, least)) => {
            let hash = ((most >> 32) ^ most ^ (least >> 32) ^ least) as u32 as i32;
            hash.rem_euclid(DEFAULT_SKINS.len() as i32) as usize
        }
        // 认不出来的 UUID 给第一张：多数皮肤站的默认观感
        None => 0,
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

    /// 把脸那串十六进制的最左上像素解成 RGBA。
    fn face_pixel(face: &str) -> [u8; 4] {
        let mut out = [0u8; 4];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&face[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

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
        png::push_chunk(&mut out, b"IHDR", &ihdr);
        png::push_chunk(&mut out, b"IDAT", &zlib);
        png::push_chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn a_default_head_is_a_scaled_face() {
        // Notch 的 UUID 落在表里第 0 张：alex。
        let head = default_head("069a79f444e94726a5befca90e38aaf5");
        assert_eq!(head.len(), HEAD_PX * HEAD_PX * 4);
        // 左上角那一点就是 alex 那张脸 (8,8) 的肤色
        assert_eq!(&head[0..4], &[0xe5, 0x8d, 0x3f, 0xff]);
        // 放大是最近邻：相邻两点、上下两行都应该完全一样
        assert_eq!(&head[0..4], &head[4..8]);
        assert_eq!(&head[0..4], &head[(HEAD_PX * 4)..(HEAD_PX * 4 + 4)]);
    }

    #[test]
    fn the_uuid_picks_one_of_eighteen_skins() {
        // 原版是 floorMod(hashCode, 18)，下标对着 DEFAULT_SKINS 走。
        // 这三个下标是拿 java.util.UUID.hashCode() 的公式手算出来的。
        assert_eq!(default_skin_name("069a79f444e94726a5befca90e38aaf5"), "alex");
        assert_eq!(
            default_skin_name("00000000-0000-0000-0000-000000000001"),
            "ari"
        );
        assert_eq!(
            default_skin_name("5627dd98e6be3c21b8a8e92344183641"),
            "efe"
        );
        assert_eq!(default_index("00000000-0000-0000-0000-000000000012"), 0);

        // 负数哈希也得折回正的下标里，不能越界
        for uuid in [
            "069a79f444e94726a5befca90e38aaf5",
            "ffffffffffffffffffffffffffffffff",
            "00000000000000000000000000000001",
        ] {
            assert!(default_index(uuid) < DEFAULT_SKINS.len(), "{uuid}");
        }
    }

    #[test]
    fn the_table_has_every_name_in_both_models() {
        // 9 个名字 × 宽/细两种模型；同名两张的脸必须一模一样，
        // 因为原版只是换了身体模型，脸是同一张。
        assert_eq!(DEFAULT_SKINS.len(), 18);
        for name in [
            "alex", "ari", "efe", "kai", "makena", "noor", "steve", "sunny", "zuri",
        ] {
            let rows: Vec<&DefaultSkin> =
                DEFAULT_SKINS.iter().filter(|s| s.name == name).collect();
            assert_eq!(rows.len(), 2, "{name}");
            assert_eq!(rows[0].face, rows[1].face, "{name} 的两张脸不一样");
            assert_eq!(rows[0].face.len(), 512, "{name} 的脸不是 8×8");
            assert!(rows[0].slim != rows[1].slim, "{name} 的模型重复了");
            // 整图必须是一张能解的 64×64 PNG，而且裁出来的脸和上面那 8×8 一致。
            for row in [rows[0], rows[1]] {
                let image = png::decode(row.png).expect("默认皮肤的 PNG 应该能解");
                assert_eq!((image.width, image.height), (64, 64), "{name}");
                let head = head_from_skin_png(row.png).expect("默认皮肤能裁出头像");
                assert_eq!(&head[0..4], &face_pixel(row.face), "{name} 的脸对不上");
            }
        }
    }

    #[test]
    fn a_broken_uuid_still_gets_a_head() {
        let head = default_head("这不是-uuid");
        assert_eq!(head.len(), HEAD_PX * HEAD_PX * 4);
        // 认不出来的 UUID 落在第一张：alex
        assert_eq!(&head[0..4], &[0xe5, 0x8d, 0x3f, 0xff]);
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
