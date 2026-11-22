//! 模组图标：下载 → 解码 → 上传成纹理，按项目 id 缓存。
//!
//! Modrinth 的 `icon_url` 大多指向 **webp**（抽样约八成是无损 VP8L），少数
//! png / jpeg；所以解码统一交给 `image` crate，不自己写解码器。这边只负责
//! 三件事：
//!
//! 1. 记录「这张脸要不要去拿」（`want`），同一张不重复排队；
//! 2. 把拿回来的字节解成 [`egui::ColorImage`]（[`decode`]）；
//! 3. 上传成纹理后留住 [`egui::TextureHandle`]（`put`）。
//!
//! 纹理**每个项目只上传一次**：`load_texture` 每次都会往显存里拷贝一份，
//! 列表滚动时每帧重传会让显存一路涨上去。

use std::collections::{HashMap, HashSet};

use egui::{ColorImage, TextureHandle, TextureOptions};

/// 图标存进显存时的边长上限。
///
/// 列表里画 40px，但屏幕可能是 1.25× 甚至 2× 的缩放；留 128 给高分屏，
/// 缩下来才不会发虚。原图比这还小的就原样留着，不做无谓的放大。
pub const ICON_PX: u32 = 128;

/// 原图小于这个边长时，认为它是像素画，放大要按最近邻采样。
///
/// 40 是列表里那个格子的逻辑边长：比它还小的图一定是被放大着画的，
/// 双线性插值会把像素块抹成一片糊。
const NEAREST_BELOW: u32 = 40;

/// 一条待取回的图标请求：`(缓存键, 来源)`。
///
/// 「来源」通常是 http 地址；本机 jar 里的图标用 [`FILE_SCHEME`] 前缀，
/// 主循环看到这个前缀就改从磁盘读，不必绕一圈网络。
pub type Request = (String, String);

/// 本地 jar 的伪协议前缀。
///
/// 模组的图标不是独立文件，而是**压在 jar 里**的那张图（`fabric.mod.json`
/// 的 `icon` / `mods.toml` 的 `logoFile`）。所以请求里存 jar 的路径，
/// 取图时由 `aicraft_core::modicon::from_jar` 把它从 ZIP 里挖出来。
pub const JAR_SCHEME: &str = "jar+local://";

/// 资源包图标的伪协议前缀。
///
/// 资源包的脸是包根目录下的 `pack.png`：`.zip` 里压在 ZIP 中，解压开的
/// 目录就直接是个文件。两种情况都交给 `aicraft_core::resourcepack::icon_at`。
pub const PACK_SCHEME: &str = "pack+local://";

/// 存档图标的伪协议前缀。
///
/// 存档的脸是游戏存世界时截的那张 `icon.png`，就在存档目录里；
/// 备份是整个世界压成的 zip，同一张图埋在包内某处。
pub const SAVE_SCHEME: &str = "save+local://";

/// 图标缓存。挂在页面状态上，切走再回来不用重新下。
#[derive(Default)]
pub struct Cache {
    textures: HashMap<String, TextureHandle>,
    failed: HashSet<String>,
    /// 已经排进队列、还没回来的项目 id
    requested: HashSet<String>,
    /// 本帧新产生的请求，由主循环取走
    pending: Vec<Request>,
}

impl Cache {
    /// 要这张图。没下过、也没排队，就记一条请求。
    ///
    /// 返回 false 表示现在没图可画（还在路上或者已经放弃了），调用方画占位。
    pub fn want(&mut self, id: &str, url: Option<&str>) -> bool {
        if self.textures.contains_key(id) {
            return true;
        }
        let Some(url) = url.filter(|u| !u.is_empty()) else {
            return false;
        };
        if self.failed.contains(id) || self.requested.contains(id) {
            return false;
        }
        self.requested.insert(id.to_string());
        self.pending.push((id.to_string(), url.to_string()));
        false
    }

    /// 要一个模组 jar 里的图标。
    pub fn want_jar(&mut self, id: &str, jar: &std::path::Path) -> bool {
        let source = format!("{}{}", JAR_SCHEME, jar.display());
        self.want(id, Some(&source))
    }

    /// 要一个资源包的图标（`pack.png`）。
    ///
    /// 没有 `pack.png` 的包很常见（本机那个 3D Minecraft 就没有），
    /// 那种情况下线程会回一条失败，列表里画灰块，不会一直重试。
    pub fn want_pack(&mut self, id: &str, pack: &std::path::Path) -> bool {
        let source = format!("{}{}", PACK_SCHEME, pack.display());
        self.want(id, Some(&source))
    }

    /// 要一个存档（或备份）的图标。
    pub fn want_save(&mut self, id: &str, path: &std::path::Path) -> bool {
        let source = format!("{}{}", SAVE_SCHEME, path.display());
        self.want(id, Some(&source))
    }

    /// 取走这一帧攒下的请求。
    pub fn take_pending(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.pending)
    }

    /// 有没有刚登记、还没被取走的请求。
    ///
    /// 界面在**绘制**时才登记请求，而取请求发生在每帧开头的 `logic` 里；
    /// 中间这一帧要是没别的理由重绘（列表是静止的），egui 就直接睡了，
    /// 那批请求永远等不到人取——表现就是图标一直是灰块。
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn texture(&self, id: &str) -> Option<&TextureHandle> {
        self.textures.get(id)
    }

    /// 上传一张解好的图。
    ///
    /// `nearest` 是给像素画用的：放大时按最近邻采样，别糊成一片。
    pub fn put(&mut self, ctx: &egui::Context, id: &str, decoded: &Decoded) {
        self.requested.remove(id);
        let options = if decoded.nearest {
            TextureOptions::NEAREST
        } else {
            TextureOptions::LINEAR
        };
        let texture = ctx.load_texture(format!("mod-icon-{}", id), decoded.image.clone(), options);
        self.textures.insert(id.to_string(), texture);
    }

    /// 这张图已经确认取不到了（包里压根没带）。
    pub fn is_failed(&self, id: &str) -> bool {
        self.failed.contains(id)
    }

    /// 这张图废了（404、格式不认识）。记住别再试，列表里画占位。
    pub fn mark_failed(&mut self, id: &str) {
        self.requested.remove(id);
        self.failed.insert(id.to_string());
    }

    /// 线程意外断了：把还在排队的一并放回来。
    ///
    /// 不清这些 id 的话，它们会一直躺在 `requested` 里，那几张脸就永远
    /// 停在占位灰块上，用户再也不会看到它们。
    pub fn abandon_requests(&mut self) {
        self.requested.clear();
    }
}

/// 解好的一张图，外加「放大时该怎么采样」。
pub struct Decoded {
    pub image: ColorImage,
    /// 原图比 [`ICON_PX`] 还小，列表里是把它**放大**着画的。
    ///
    /// 这种多半是像素画（资源包的 `pack.png` 常常只有 8×8、16×16），
    /// 用双线性放大就成了糊成一团的色块；换成最近邻，方块还是方块。
    pub nearest: bool,
}

/// 把字节解成可以直接上传的方形图。
///
/// 大的等比缩到 [`ICON_PX`]，**小的原样留着**——先放大再缩小只会白白糊一层；
/// 最后补成正方（透明边），这样列表里的格子不用管原始比例，也不会因为某张图
/// 特别长而把排版顶歪。
pub fn decode(bytes: &[u8]) -> Option<Decoded> {
    let image = ::image::load_from_memory(bytes).ok()?;
    let (sw, sh) = (image.width(), image.height());
    if sw == 0 || sh == 0 {
        return None;
    }
    let nearest = needs_nearest(sw, sh);
    let scaled = if sw.max(sh) <= ICON_PX {
        // 本来就不大，原样留着：先放大再缩小只会白糊一层。
        image.to_rgba8()
    } else {
        // `thumbnail` 用的是快速整数采样，大图缩下来边缘发虚；Lanczos3 慢一点，
        // 但 1024×1024 的包图标缩一次也就几毫秒。
        let (w, h) = fit(sw, sh, ICON_PX);
        ::image::imageops::resize(&image, w, h, ::image::imageops::FilterType::Lanczos3)
    };
    let (w, h) = scaled.dimensions();
    let side = w.max(h);
    let mut pixels = Vec::with_capacity((side * side * 4) as usize);
    let offset_x = (side - w) / 2;
    let offset_y = (side - h) / 2;
    for y in 0..side {
        for x in 0..side {
            let inside = x >= offset_x && x < offset_x + w && y >= offset_y && y < offset_y + h;
            if inside {
                let px = scaled.get_pixel(x - offset_x, y - offset_y);
                pixels.extend_from_slice(&px.0);
            } else {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    Some(Decoded {
        image: ColorImage::from_rgba_unmultiplied([side as usize, side as usize], &pixels),
        nearest,
    })
}

/// 这张原图放大着画的时候，要不要按最近邻采样。
///
/// 资源包的 `pack.png` 常常只有 8×8、16×16（像素画），双线性放大就成了
/// 一团模糊的色块；模组图标基本都是 128 以上，放大也用不着。
fn needs_nearest(w: u32, h: u32) -> bool {
    w.max(h) < NEAREST_BELOW
}

/// 等比缩进 `max×max` 里，至少一边顶到边。
fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w >= h {
        (max, (h * max / w).max(1))
    } else {
        ((w * max / h).max(1), max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn want_queues_only_once() {
        let mut cache = Cache::default();
        assert!(!cache.want("abc", Some("https://example.com/a.webp")));
        assert_eq!(cache.take_pending().len(), 1);
        // 第二次问：已经排过队，不再重复排
        assert!(!cache.want("abc", Some("https://example.com/a.webp")));
        assert!(cache.take_pending().is_empty());
    }

    #[test]
    fn want_without_url_never_queues() {
        let mut cache = Cache::default();
        assert!(!cache.want("abc", None));
        assert!(!cache.want("abc", Some("")));
        assert!(cache.take_pending().is_empty());
    }

    #[test]
    fn failed_icon_is_not_retried() {
        let mut cache = Cache::default();
        cache.want("abc", Some("https://example.com/a.webp"));
        let _ = cache.take_pending();
        cache.mark_failed("abc");
        assert!(!cache.want("abc", Some("https://example.com/a.webp")));
        assert!(cache.take_pending().is_empty());
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode(b"not an image").is_none());
    }

    #[test]
    fn shrinking_keeps_the_aspect_ratio() {
        assert_eq!(fit(1024, 1024, ICON_PX), (128, 128));
        assert_eq!(fit(1024, 512, ICON_PX), (128, 64));
        assert_eq!(fit(300, 900, ICON_PX), (42, 128));
        // 极细的图也不许缩成 0 像素，否则整张图会消失
        assert_eq!(fit(4000, 10, ICON_PX), (128, 1));
    }

    #[test]
    fn only_tiny_icons_are_drawn_with_nearest() {
        // 本机那两个包的 pack.png 就是 8×8 和 16×16，放大后要留住像素块。
        assert!(needs_nearest(8, 8));
        assert!(needs_nearest(16, 16));
        assert!(needs_nearest(32, 32));
        assert!(!needs_nearest(64, 64), "64 缩到 40 是缩小，用平滑采样");
        assert!(!needs_nearest(1024, 1024));
    }
}
