//! 最小 PNG 解码器。
//!
//! 只为皮肤服务：Minecraft 的皮肤是 64×64 或 64×32 的 PNG，8 位通道、
//! 非隔行。把 PNG 规范做全（隔行、16 位通道、各种滤波组合）等于在这项目里
//! 再写一个图像库，而这里只有一个用途。遇到不支持的形状就明确报错，
//! 让调用方回退到「没有头像」，而不是猜着画出一张花屏。

use crate::natives::deflate;

/// 解出来的图片：RGBA8，行优先。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Image {
    /// 取某个像素；越界返回全透明。
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let i = (y * self.width + x) * 4;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }
}

/// 解码一张 PNG。
pub fn decode(data: &[u8]) -> Result<Image, String> {
    if data.len() < 8 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("这不是 PNG".to_string());
    }

    let mut pos = 8usize;
    let mut width = 0usize;
    let mut height = 0usize;
    let mut depth = 0u8;
    let mut color_type = 0u8;
    let mut saw_header = false;
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();

    while pos + 8 <= data.len() {
        let length = be32(&data[pos..pos + 4]) as usize;
        let kind = &data[pos + 4..pos + 8];
        let body_start = pos + 8;
        let body_end = body_start.checked_add(length).ok_or("PNG 块长度溢出")?;
        if body_end + 4 > data.len() {
            return Err("PNG 数据被截断".to_string());
        }
        let body = &data[body_start..body_end];

        match kind {
            b"IHDR" => {
                if body.len() < 13 {
                    return Err("PNG 头长度不对".to_string());
                }
                width = be32(&body[0..4]) as usize;
                height = be32(&body[4..8]) as usize;
                depth = body[8];
                color_type = body[9];
                if body[10] != 0 || body[11] != 0 {
                    return Err("PNG 用了不认识的压缩或滤波方式".to_string());
                }
                if body[12] != 0 {
                    return Err("隔行扫描的 PNG 不支持".to_string());
                }
                saw_header = true;
            }
            b"PLTE" => {
                palette = body.chunks(3).map(|c| [c[0], c[1], c[2], 255]).collect();
            }
            b"tRNS" => {
                // 只有调色板图用得上的透明表：给前面几项补 alpha
                for (i, alpha) in body.iter().enumerate() {
                    if let Some(entry) = palette.get_mut(i) {
                        entry[3] = *alpha;
                    }
                }
            }
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }

        pos = body_end + 4;
    }

    if !saw_header {
        return Err("PNG 缺少文件头".to_string());
    }
    if width == 0 || height == 0 {
        return Err("PNG 尺寸是 0".to_string());
    }
    if depth != 8 {
        return Err(format!("只支持 8 位通道的 PNG（这张是 {} 位）", depth));
    }

    let channels = match color_type {
        0 => 1, // 灰度
        2 => 3, // RGB
        3 => 1, // 调色板，一个字节是索引
        4 => 2, // 灰度 + alpha
        6 => 4, // RGBA
        other => return Err(format!("不支持的 PNG 颜色类型 {}", other)),
    };
    if color_type == 3 && palette.is_empty() {
        return Err("调色板 PNG 缺少调色板".to_string());
    }

    let raw = zlib_inflate(&idat, (width * channels + 1) * height)?;
    let stride = width * channels;
    let need = (stride + 1) * height;
    if raw.len() < need {
        return Err("PNG 像素数据不完整".to_string());
    }

    let mut pixels = unfilter(&raw, height, stride, channels)?;

    // 统一成 RGBA
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let i = y * stride + x * channels;
            let px = match color_type {
                0 => [pixels[i], pixels[i], pixels[i], 255],
                2 => [pixels[i], pixels[i + 1], pixels[i + 2], 255],
                3 => {
                    let index = pixels[i] as usize;
                    palette.get(index).copied().unwrap_or([0, 0, 0, 0])
                }
                4 => {
                    let v = pixels[i];
                    [v, v, v, pixels[i + 1]]
                }
                _ => [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]],
            };
            rgba.extend_from_slice(&px);
        }
    }
    pixels.clear();

    Ok(Image {
        width,
        height,
        rgba,
    })
}

/// 去掉每行开头的滤波字节，把数据摊平成连续的像素。
fn unfilter(raw: &[u8], height: usize, stride: usize, channels: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; height * stride];
    for y in 0..height {
        let src = y * (stride + 1);
        let filter = raw[src];
        let line = &raw[src + 1..src + 1 + stride];
        for x in 0..stride {
            let a = if x >= channels {
                out[y * stride + x - channels]
            } else {
                0
            };
            let b = if y > 0 { out[(y - 1) * stride + x] } else { 0 };
            let c = if y > 0 && x >= channels {
                out[(y - 1) * stride + x - channels]
            } else {
                0
            };
            let value = match filter {
                0 => line[x],
                1 => line[x].wrapping_add(a),
                2 => line[x].wrapping_add(b),
                3 => line[x].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => line[x].wrapping_add(paeth(a, b, c)),
                other => return Err(format!("不认识的 PNG 行滤波 {}", other)),
            };
            out[y * stride + x] = value;
        }
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let pa = (p - a as i16).abs();
    let pb = (p - b as i16).abs();
    let pc = (p - c as i16).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// zlib 包一层的 deflate：前两个字节是头和校验位，末尾 4 字节是 adler32。
/// 我们的 inflate 读到最后一个块就停，尾部校验不读也没关系。
fn zlib_inflate(data: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    if data.len() < 6 {
        return Err("PNG 的压缩数据太短".to_string());
    }
    let cmf = data[0];
    let flg = data[1];
    if (cmf as u16 * 256 + flg as u16) % 31 != 0 {
        return Err("PNG 的 zlib 头损坏".to_string());
    }
    if cmf & 0x0f != 8 {
        return Err("PNG 用了不认识的压缩方法".to_string());
    }
    deflate::inflate(&data[2..], expected).map_err(|e| format!("解压 PNG 失败: {}", e))
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
