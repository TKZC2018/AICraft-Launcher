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

/// 图标存进显存时的边长。列表里画 44px，留一点余量给高分屏。
pub const ICON_PX: u32 = 64;

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
    pub fn put(&mut self, ctx: &egui::Context, id: &str, image: &ColorImage) {
        self.requested.remove(id);
        let texture = ctx.load_texture(
            format!("mod-icon-{}", id),
            image.clone(),
            TextureOptions::LINEAR,
        );
        self.textures.insert(id.to_string(), texture);
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

/// 把字节解成可以直接上传的方形图。
///
/// 先等比缩到 [`ICON_PX`]，再补成正方（透明边），这样列表里的格子不用管
/// 原始比例，也不会因为某张图特别长而把排版顶歪。
pub fn decode(bytes: &[u8]) -> Option<ColorImage> {
    let image = ::image::load_from_memory(bytes).ok()?;
    let scaled = image.thumbnail(ICON_PX, ICON_PX).to_rgba8();
    let (w, h) = scaled.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let mut pixels = Vec::with_capacity((ICON_PX * ICON_PX * 4) as usize);
    let offset_x = (ICON_PX - w) / 2;
    let offset_y = (ICON_PX - h) / 2;
    for y in 0..ICON_PX {
        for x in 0..ICON_PX {
            let inside = x >= offset_x && x < offset_x + w && y >= offset_y && y < offset_y + h;
            if inside {
                let px = scaled.get_pixel(x - offset_x, y - offset_y);
                pixels.extend_from_slice(&px.0);
            } else {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    Some(ColorImage::from_rgba_unmultiplied(
        [ICON_PX as usize, ICON_PX as usize],
        &pixels,
    ))
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
}
