//! 把抓下来的整屏画面磨成一片模糊底图。
//!
//! 启动游戏那几秒，玩家能看见的只剩一张「正在启动」的卡片。卡片后面
//! 原本是清清楚楚的首页，眼神会被上面的字勾走；磨掉之后视线自然落在
//! 卡片上，又不至于像纯色遮罩那样突兀。
//!
//! 做法很朴素：把画面缩到八分之一（这一步本身就丢掉了细节），在那张小图上
//! 反复做盒式模糊，再交给显卡拉回全屏 —— 双线性放大顺带又抹了一遍。
//! 真拿全分辨率做高斯，一次就要几百毫秒，而那几秒正是启动最要紧的时候。

use eframe::egui;

/// 缩到几分之一再做模糊。8 意味着 1125×775 的窗口只需要处理约 140×97。
pub const DOWNSCALE: usize = 8;

/// 盒式模糊跑几轮。
///
/// 每一轮都是「左右各取 radius 个邻居」，所以两轮的等效半径是 2×radius。
/// 别贪多：小图本来就只有百来像素宽，三轮 + 大半径会把整张图抹成一片纯色，
/// 糊得连「刚才那是首页」都看不出来。
pub const PASSES: usize = 2;

/// 原图缩到 `DOWNSCALE` 分之一之后的像素。窗口很小时至少留 1×1。
pub fn small_size(size: [usize; 2]) -> [usize; 2] {
    [
        size[0].div_ceil(DOWNSCALE).max(1),
        size[1].div_ceil(DOWNSCALE).max(1),
    ]
}

/// 一维盒式模糊：对每一行（或每一列）取左右 `radius` 个邻居的平均。
///
/// 用「先加再减」的滑动窗口，代价跟半径无关 —— 半径给到 12 也一样快。
fn blur_line(line: &mut [egui::Color32], radius: usize, scratch: &mut Vec<[f32; 4]>) {
    let n = line.len();
    if n == 0 || radius == 0 {
        return;
    }
    scratch.clear();
    scratch.extend(line.iter().map(|c| {
        let [r, g, b, a] = c.to_array();
        [r as f32, g as f32, b as f32, a as f32]
    }));

    let at = |i: isize| -> [f32; 4] {
        let clamped = i.clamp(0, n as isize - 1) as usize;
        scratch[clamped]
    };

    // 窗口是 [i-radius, i+radius]，越界的那部分按边缘像素算（夹边）。
    let mut sum = [0.0_f32; 4];
    for j in -(radius as isize)..=(radius as isize) {
        let p = at(j);
        for k in 0..4 {
            sum[k] += p[k];
        }
    }
    let span = (radius * 2 + 1) as f32;

    for (i, pixel) in line.iter_mut().enumerate() {
        let [r, g, b, a] = sum.map(|v| (v / span).round().clamp(0.0, 255.0) as u8);
        *pixel = egui::Color32::from_rgba_unmultiplied(r, g, b, a);

        // 窗口右移一格：右边进来一个，左边出去一个。
        let out = at(i as isize - radius as isize);
        let enter = at(i as isize + radius as isize + 1);
        for k in 0..4 {
            sum[k] += enter[k] - out[k];
        }
    }
}

/// 把一张图缩小 `DOWNSCALE` 倍（每个格子取平均），然后做几轮盒式模糊。
///
/// 半径按小图的尺寸取：图越小越不能磨得太狠，否则整块糊成一片死色。
pub fn shrink_and_blur(image: &egui::ColorImage) -> egui::ColorImage {
    let [width, height] = image.size;
    if width == 0 || height == 0 {
        return egui::ColorImage::filled([1, 1], egui::Color32::from_rgb(0xED, 0xF1, 0xF7));
    }

    let small = small_size([width, height]);
    let mut pixels = Vec::with_capacity(small[0] * small[1]);
    for y in 0..small[1] {
        let y0 = y * height / small[1];
        let y1 = ((y + 1) * height / small[1]).max(y0 + 1).min(height);
        for x in 0..small[0] {
            let x0 = x * width / small[0];
            let x1 = ((x + 1) * width / small[0]).max(x0 + 1).min(width);
            let mut sum = [0u32; 4];
            let mut count = 0u32;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let [r, g, b, a] = image.pixels[sy * width + sx].to_array();
                    sum[0] += r as u32;
                    sum[1] += g as u32;
                    sum[2] += b as u32;
                    sum[3] += a as u32;
                    count += 1;
                }
            }
            let count = count.max(1);
            pixels.push(egui::Color32::from_rgba_unmultiplied(
                (sum[0] / count) as u8,
                (sum[1] / count) as u8,
                (sum[2] / count) as u8,
                (sum[3] / count) as u8,
            ));
        }
    }

    // 半径跟着小图的短边走。按 97 高的窗口算，这里给到 1：
    // 两轮下来等效 ±2 个小图像素，铺回全屏大约 ±16 —— 字糊掉了，
    // 但底下是哪一页、列表排到哪儿还看得出来。再厚就成了一片雾。
    let radius = (small[0].min(small[1]) / 64).clamp(1, 3);
    let mut scratch = Vec::with_capacity(small[0].max(small[1]));
    let mut column = vec![egui::Color32::TRANSPARENT; small[1]];
    for _ in 0..PASSES {
        for y in 0..small[1] {
            let start = y * small[0];
            blur_line(&mut pixels[start..start + small[0]], radius, &mut scratch);
        }
        for x in 0..small[0] {
            for (y, slot) in column.iter_mut().enumerate() {
                *slot = pixels[y * small[0] + x];
            }
            blur_line(&mut column, radius, &mut scratch);
            for (y, value) in column.iter().enumerate() {
                pixels[y * small[0] + x] = *value;
            }
        }
    }

    egui::ColorImage::new(small, pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: usize, height: usize, color: egui::Color32) -> egui::ColorImage {
        egui::ColorImage::filled([width, height], color)
    }

    #[test]
    fn a_blurred_picture_keeps_some_shape() {
        // 左半黑右半白：磨完之后左右两边还得分得出来，不能糊成一色。
        let mut source = image(64, 64, egui::Color32::BLACK);
        for y in 0..64 {
            for x in 32..64 {
                source.pixels[y * 64 + x] = egui::Color32::WHITE;
            }
        }
        let blurred = shrink_and_blur(&source);
        let [width, height] = blurred.size;
        let left = blurred.pixels[height / 2 * width + 1].to_array()[0];
        let right = blurred.pixels[height / 2 * width + width - 2].to_array()[0];
        assert!(
            right as i32 - left as i32 > 120,
            "左右应当还分得开，实际 {left} / {right}"
        );
    }

    #[test]
    fn the_small_copy_keeps_the_shape() {
        assert_eq!(small_size([1125, 775]), [141, 97]);
        // 比八还小的窗口至少也得有一个像素，不然没东西可画
        assert_eq!(small_size([3, 1]), [1, 1]);
    }

    #[test]
    fn a_flat_picture_stays_flat() {
        // 全白图上磨一遍还是全白：没有颜色可以往外渗
        let blurred = shrink_and_blur(&image(80, 48, egui::Color32::WHITE));
        assert_eq!(blurred.size, [10, 6]);
        assert!(blurred.pixels.iter().all(|c| *c == egui::Color32::WHITE));
    }

    #[test]
    fn a_hard_edge_bleeds_sideways() {
        // 左黑右白：模糊之后交界处必须出现灰，而不是一刀切
        let mut source = image(64, 64, egui::Color32::BLACK);
        for y in 0..64 {
            for x in 32..64 {
                source.pixels[y * 64 + x] = egui::Color32::WHITE;
            }
        }
        let blurred = shrink_and_blur(&source);
        let [width, _] = blurred.size;
        let middle = blurred.pixels[blurred.size[1] / 2 * width + width / 2];
        let [r, _, _, _] = middle.to_array();
        assert!(r > 20 && r < 235, "交界处应当是一片灰，实际 {r}");
    }

    #[test]
    fn an_empty_image_still_gives_something_to_draw() {
        let blurred = shrink_and_blur(&image(0, 0, egui::Color32::TRANSPARENT));
        assert_eq!(blurred.size, [1, 1]);
    }
}
