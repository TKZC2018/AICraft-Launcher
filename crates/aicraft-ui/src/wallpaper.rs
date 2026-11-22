//! 启动器背景图：把玩家挑的那张图铺满整个窗口。
//!
//! 只做三件事：把文件解成像素、按「铺满」而不是「装下」算取样区、
//! 画在一切内容下面。面板底色在 theme 里让出一点透明度，图就从底下
//! 透上来 —— 亮色主题的层次还在，底图也不至于被盖死。
//!
//! 解码分两条路：启动那一帧在界面线程上同步解（缓存命中只要二十毫秒，
//! 图不到位第一帧就是一块白底）；运行中换图走后台线程（见 main.rs 的
//! kick_off_wallpaper），因为玩家可能挑一张 4000×3000 的照片，解一次
//! 要几百毫秒，卡在界面上会僵住。

use std::path::{Path, PathBuf};

use eframe::egui;

/// 存进显存前的最长边。
///
/// 屏幕很少超过 2560 宽，再大的图只会白占显存。
pub const MAX_SIDE: u32 = 2560;

/// 出厂自带的壁纸：1920×1080 的 JPEG，编进 exe 里。
///
/// 不放进数据目录，也不依赖玩家挑图：新装第一次打开就该有底色，
/// 而不是一块纯白。玩家挑了别的图就以数据目录那张为准。
pub const DEFAULT_IMAGE: &[u8] = include_bytes!("../assets/wallpaper.jpg");

/// 内置壁纸在 [`Wallpaper::key`] 里的记号。
///
/// 和文件记号的形状一样是「长度:时间」，只是时间是固定的 0：
/// 内容跟着 exe 走，exe 没换就不用重解。
pub const DEFAULT_KEY: &str = "builtin:0";

/// 缓存文件的格式标记。改了解码方式就换一个，老缓存自动作废。
const CACHE_MAGIC: &str = "aicraft-wallpaper-cache 1";

/// 缓存 JPEG 的质量。背景图会被面板盖掉大半，82 已经看不出压缩痕迹。
const CACHE_QUALITY: u8 = 82;

/// 缩好之后那份缓存的文件名，和原图同放在数据目录里。
pub const CACHE_FILE: &str = "background.cache.jpg";

/// 一张已经装进显存的背景图。
#[derive(Default)]
pub struct Wallpaper {
    texture: Option<egui::TextureHandle>,
    /// 这张图是从哪个文件装进来的（长度:修改时间）。
    ///
    /// 换图之后这个记号对不上，主循环据此重读一次；只比文件名不够，
    /// 玩家把新图覆盖成同一个名字是常事。
    key: String,
}

impl Wallpaper {
    /// 现在有没有图。没有的话各大块底色保持不透明，跟没加这个功能时一样。
    pub fn has_image(&self) -> bool {
        self.texture.is_some()
    }

    /// 已经装进来的那张图来自哪个文件。
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 把解好的像素传上显存，记下它对应的文件。
    pub fn install(&mut self, ctx: &egui::Context, key: &str, image: egui::ColorImage) {
        self.texture = Some(ctx.load_texture(
            "launcher-wallpaper",
            image,
            egui::TextureOptions::LINEAR,
        ));
        self.key = key.to_string();
    }

    /// 摘掉背景图。显存里的纹理交给 egui 自己回收。
    pub fn clear(&mut self) {
        self.texture = None;
        self.key.clear();
    }

    /// 把图铺满 rect：等比放大到盖住整块，多出来的部分居中裁掉。
    ///
    /// 不做拉伸：背景图多是照片或插画，拉扁了比裁掉两边难看。
    pub fn paint(&self, painter: &egui::Painter, rect: egui::Rect) {
        let Some(texture) = &self.texture else {
            return;
        };
        if !rect.is_positive() {
            return;
        }
        let size = texture.size_vec2();
        if size.x <= 0.0 || size.y <= 0.0 {
            return;
        }
        let uv = cover_uv(size, rect.size());
        painter.image(texture.id(), rect, uv, egui::Color32::WHITE);
    }
}

/// 「铺满」时该从原图的哪一块取样（uv 坐标，0..=1）。
///
/// 原图比目标更宽就左右裁，更高就上下裁，裁掉的那部分居中。
pub fn cover_uv(image: egui::Vec2, target: egui::Vec2) -> egui::Rect {
    if image.x <= 0.0 || image.y <= 0.0 || target.x <= 0.0 || target.y <= 0.0 {
        return egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    }
    let image_ratio = image.x / image.y;
    let target_ratio = target.x / target.y;
    if image_ratio > target_ratio {
        // 图更宽：两边裁掉，留下的宽度是目标比例。
        let keep = target_ratio / image_ratio;
        let pad = (1.0 - keep) * 0.5;
        egui::Rect::from_min_max(egui::pos2(pad, 0.0), egui::pos2(pad + keep, 1.0))
    } else {
        // 图更高：上下裁。
        let keep = image_ratio / target_ratio;
        let pad = (1.0 - keep) * 0.5;
        egui::Rect::from_min_max(egui::pos2(0.0, pad), egui::pos2(1.0, pad + keep))
    }
}

/// 等比缩进 max 见方的框里。本来就够小的话原样返回。
fn fit_within(width: u32, height: u32, max: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= max || longest == 0 {
        return (width.max(1), height.max(1));
    }
    let scale = max as f64 / longest as f64;
    (
        ((width as f64 * scale).round() as u32).max(1),
        ((height as f64 * scale).round() as u32).max(1),
    )
}

/// 把图片文件解成可以直接上传的像素。认不出的格式、坏文件都返回 None。
pub fn decode(bytes: &[u8]) -> Option<egui::ColorImage> {
    let image = ::image::load_from_memory(bytes).ok()?;
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return None;
    }
    let (to_w, to_h) = fit_within(width, height, MAX_SIDE);
    let scaled = if (to_w, to_h) == (width, height) {
        image.to_rgba8()
    } else {
        // 箱式重采样：4K 缩到 2560 只要九十毫秒，三角形插值要八百多毫秒。
        // 这张图本来就要被面板盖掉大半，两者放到全屏上看不出差别。
        ::image::imageops::thumbnail(&image, to_w, to_h)
    };
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [to_w as usize, to_h as usize],
        scaled.as_raw(),
    ))
}

/// 文件记号：(长度):(修改时间的毫秒)。文件不在就是空串。
///
/// 换图时对不上就重读一次。用长度加时间戳而不是只看名字：玩家完全可能
/// 把新图覆盖成同名文件，那时候名字没变，内容已经换了。
pub fn file_key(path: &Path) -> String {
    let Ok(meta) = std::fs::metadata(path) else {
        return String::new();
    };
    if !meta.is_file() {
        return String::new();
    }
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{}:{}", meta.len(), stamp)
}

/// 缓存文件的位置：跟原图一样落在数据目录里。
pub fn cache_path(dir: &Path) -> PathBuf {
    dir.join(CACHE_FILE)
}

/// 把缩好的图存成 JPEG，连同它对应的原图记号一起写进同一个文件。
///
/// 存的是「缩到最长边 2560 之后」的样子：下次启动直接解这份小的，
/// 省掉读四兆原图和重采样这两步，实测从九百毫秒降到二十毫秒。
///
/// 写失败不报错：缓存只是加速手段，没有它下次多花几百毫秒而已，
/// 不该因为它让整张背景图装不上。
fn remember(dir: &Path, stamp: &str, image: &egui::ColorImage) {
    let (w, h) = (image.size[0], image.size[1]);
    if w == 0 || h == 0 {
        return;
    }
    let Some(raw) = ::image::RgbaImage::from_raw(
        w as u32,
        h as u32,
        image.pixels.iter().flat_map(|p| p.to_array()).collect(),
    ) else {
        return;
    };
    let mut buf: Vec<u8> = Vec::new();
    if ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, CACHE_QUALITY)
        .encode_image(&raw)
        .is_err()
    {
        return;
    }
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let mut text = Vec::with_capacity(buf.len() + stamp.len() + 40);
    text.extend_from_slice(CACHE_MAGIC.as_bytes());
    text.push(b'\n');
    text.extend_from_slice(stamp.as_bytes());
    text.push(b'\n');
    text.extend_from_slice(&buf);
    let _ = std::fs::write(cache_path(dir), text);
}

/// 从缓存里取回上次缩好的图。缓存不在、坏了、或者对应的原图已经换了，都返回 None。
fn cached(dir: &Path, stamp: &str) -> Option<egui::ColorImage> {
    let bytes = std::fs::read(cache_path(dir)).ok()?;
    let mut rest = bytes.as_slice();
    for expected in [CACHE_MAGIC, stamp] {
        let end = rest.iter().position(|b| *b == b'\n')?;
        if &rest[..end] != expected.as_bytes() {
            return None;
        }
        rest = &rest[end + 1..];
    }
    decode(rest)
}

/// 把一张图弄成可以直接传显存的像素，并且尽量少花时间。
///
/// 顺序是「缓存 → 原图 → 内置壁纸」：命中缓存只要二十毫秒，是启动第一帧
/// 还能带图的唯一指望；没命中就老老实实解原图，顺便把结果留成缓存。
///
/// 启动那一帧直接调它（同步、会卡住界面），运行中换图则丢给后台线程。
/// 最坏情况是第一次遇到一张四兆的原图，几百毫秒的停顿换来「打开就有」，
/// 比先闪一块白底好；缓存命中之后通常只要二十毫秒。
pub fn prepare(dir: &Path, stamp: &str) -> Result<egui::ColorImage, String> {
    if stamp == DEFAULT_KEY {
        // 内置壁纸跟着 exe 走，每次都是同一张，缓存也帮不上忙。
        return decode(DEFAULT_IMAGE).ok_or_else(|| "这张图解不开，换一张试试".to_string());
    }
    if let Some(image) = cached(dir, stamp) {
        return Ok(image);
    }
    let path = dir.join(aicraft_core::settings::BACKGROUND_FILE);
    let bytes = std::fs::read(&path).map_err(|e| format!("读不了这张图：{}", e))?;
    let image = decode(&bytes).ok_or_else(|| "这张图解不开，换一张试试".to_string())?;
    remember(dir, stamp, &image);
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32) -> egui::Vec2 {
        egui::vec2(x, y)
    }

    /// 图比窗口宽：左右各裁一点，上下正好铺满。
    #[test]
    fn a_wide_image_is_cropped_at_the_sides() {
        let uv = cover_uv(v(2000.0, 1000.0), v(1000.0, 1000.0));
        assert!((uv.min.y - 0.0).abs() < 1e-6);
        assert!((uv.max.y - 1.0).abs() < 1e-6);
        assert!((uv.min.x - 0.25).abs() < 1e-6, "{:?}", uv);
        assert!((uv.max.x - 0.75).abs() < 1e-6, "{:?}", uv);
    }

    /// 图比窗口高：上下裁，左右铺满。
    #[test]
    fn a_tall_image_is_cropped_at_the_top_and_bottom() {
        let uv = cover_uv(v(1000.0, 2000.0), v(1000.0, 500.0));
        assert!((uv.min.x - 0.0).abs() < 1e-6);
        assert!((uv.max.x - 1.0).abs() < 1e-6);
        assert!((uv.min.y - 0.375).abs() < 1e-6, "{:?}", uv);
        assert!((uv.max.y - 0.625).abs() < 1e-6, "{:?}", uv);
    }

    /// 比例对得上时整张都用，一点不裁。
    #[test]
    fn a_matching_ratio_uses_the_whole_image() {
        let same = cover_uv(v(1000.0, 500.0), v(2000.0, 1000.0));
        assert!((same.width() - 1.0).abs() < 1e-6);
        assert!((same.height() - 1.0).abs() < 1e-6);
    }

    /// 超过最长边才缩，没超过原样留着。
    #[test]
    fn only_oversized_images_are_scaled_down() {
        assert_eq!(fit_within(800, 600, MAX_SIDE), (800, 600));
        let (w, h) = fit_within(5120, 2880, MAX_SIDE);
        assert_eq!(w, MAX_SIDE);
        assert_eq!(h, 1440);
    }

    /// 不是图片的字节直接拒绝，不要拿半张糊图去铺满屏幕。
    #[test]
    fn garbage_is_not_an_image() {
        assert!(decode(b"not a picture").is_none());
        assert!(decode(&[]).is_none());
    }

    /// 一张纯色的小图，够用来验证缓存能原样转一圈。
    fn swatch(w: usize, h: usize, color: egui::Color32) -> egui::ColorImage {
        egui::ColorImage::filled([w, h], color)
    }

    /// 一张能当原图用的小 JPEG。
    fn small_jpeg(w: u32, h: u32) -> Vec<u8> {
        let raw = ::image::RgbImage::from_pixel(w, h, ::image::Rgb([40, 90, 160]));
        let mut buf = Vec::new();
        ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90)
            .encode_image(&raw)
            .expect("能编出 JPEG");
        buf
    }

    /// 存进去的图能原样取回来：尺寸一样，颜色也大致对得上。
    #[test]
    fn a_remembered_picture_comes_back() {
        let dir = aicraft_core::testutil::fresh_dir("wp-cache-roundtrip");
        let image = swatch(8, 6, egui::Color32::from_rgb(40, 90, 160));
        remember(&dir, "1000:7", &image);
        assert!(cache_path(&dir).is_file(), "缓存该落盘");

        let back = cached(&dir, "1000:7").expect("缓存该能用");
        assert_eq!(back.size, [8, 6]);
        let px = back.pixels[0].to_array();
        assert!(
            (px[0] as i32 - 40).abs() <= 6 && (px[1] as i32 - 90).abs() <= 6,
            "颜色不该跑偏太多：{:?}",
            px
        );
    }

    /// 缓存里记着它对应哪张原图：图换过了就不能拿旧缓存顶上。
    #[test]
    fn a_cache_from_another_file_is_ignored() {
        let dir = aicraft_core::testutil::fresh_dir("wp-cache-stale");
        remember(&dir, "1000:7", &swatch(4, 4, egui::Color32::RED));
        assert!(cached(&dir, "1000:8").is_none(), "原图换了，缓存该作废");
        assert!(cached(&dir, "2000:7").is_none(), "大小变了也算换图");
        assert!(cached(&dir, "1000:7").is_some());
    }

    /// 缓存没有时退回读原图，并顺手把缓存写下来。
    #[test]
    fn a_cold_cache_is_filled_from_the_original() {
        let dir = aicraft_core::testutil::fresh_dir("wp-cache-cold");
        std::fs::write(
            dir.join(aicraft_core::settings::BACKGROUND_FILE),
            small_jpeg(32, 24),
        )
        .expect("能写原图");
        assert!(!cache_path(&dir).is_file(), "还没解过，不该有缓存");

        let image = prepare(&dir, "1000:7").expect("第一次该从原图解出来");
        assert_eq!(image.size, [32, 24]);
        assert!(cache_path(&dir).is_file(), "解完该把缓存留下");

        // 第二次走的就是缓存这条路，尺寸和原图一致。
        let again = prepare(&dir, "1000:7").expect("第二次该读缓存");
        assert_eq!(again.size, [32, 24]);
    }

    /// 原图不见了要说清楚是「读不了」，而不是丢一张黑图出来。
    #[test]
    fn a_missing_original_is_reported() {
        let dir = aicraft_core::testutil::fresh_dir("wp-cache-missing");
        let message = prepare(&dir, "1000:7").expect_err("没有原图就该报错");
        assert!(message.contains("读不了"), "{}", message);
    }

    /// 出厂壁纸编在 exe 里，没有文件也解得开。
    #[test]
    fn the_built_in_wallpaper_needs_no_file() {
        let dir = aicraft_core::testutil::fresh_dir("wp-cache-builtin");
        let image = prepare(&dir, DEFAULT_KEY).expect("内置壁纸该能解出来");
        assert!(image.size[0] > 100 && image.size[1] > 100, "{:?}", image.size);
    }

    /// 文件不在了就返回空记号，主循环据此把背景图摘掉。
    #[test]
    fn a_missing_file_has_no_key() {
        assert!(file_key(Path::new("E:/definitely/not/here.png")).is_empty());
    }
}
