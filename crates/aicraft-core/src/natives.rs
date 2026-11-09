//! natives 解压。
//!
//! Minecraft 的平台本地库（`.dll` / `.so` / `.dylib`）打包在若干 jar 里，
//! 启动前必须解压到一个目录，并通过 `-Djava.library.path` 指过去。
//! 不解压就会出现：
//!
//! ```text
//! java.lang.UnsatisfiedLinkError: Failed to locate library: lwjgl.dll
//! ```
//!
//! 实现上不引入 zip crate（本机 cargo 拉不到依赖），而是自带一个
//! 只读的 ZIP 解析器。只需要支持「遍历中央目录 + stored/deflate 解压」，
//! 其中 deflate 用纯 Rust 实现。
//!
//! 注意 ZIP 的两处细节：
//! - 文件名编码可能是 CP437 或 UTF-8，这里只关心 `.dll` 后缀，按字节比即可。
//! - natives jar 里通常带 `META-INF/`，清单要求排除（`extract.exclude`）。

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// 解压结果。
#[derive(Debug, Default)]
pub struct ExtractReport {
    /// 成功写出的文件数。
    pub extracted: usize,
    /// 因排除规则跳过的条目数。
    pub skipped: usize,
    /// 覆盖的已有文件数。
    pub overwritten: usize,
}

/// 解压一组 natives jar 到目标目录。
///
/// `sources` 里每个元素是 (jar 路径, 需要排除的路径前缀)。
pub fn extract_natives(
    sources: &[(PathBuf, Vec<String>)],
    target_dir: &Path,
) -> io::Result<ExtractReport> {
    fs::create_dir_all(target_dir)?;
    let mut report = ExtractReport::default();

    for (jar, excludes) in sources {
        if !jar.is_file() {
            // 库文件缺失时跳过，由上层决定是否报错
            continue;
        }
        let entries = read_zip_entries(jar)?;

        for entry in entries {
            if entry.is_dir {
                continue;
            }
            // 平台过滤：只要本地库与常见配置文件
            if !is_native_payload(&entry.name) {
                continue;
            }
            if excludes
                .iter()
                .any(|prefix| entry.name.starts_with(prefix))
            {
                report.skipped += 1;
                continue;
            }
            // 只取文件名，防止路径穿越
            let file_name = match Path::new(&entry.name).file_name() {
                Some(f) => f,
                None => continue,
            };
            let dest = target_dir.join(file_name);

            let data = entry.read_data(jar)?;
            let existed = dest.exists();
            // 本地库常被多个 jar 重复携带（如 lwjgl 与 lwjgl-glfw），
            // 内容相同就跳过，避免无谓的写盘。
            if existed {
                if let Ok(meta) = fs::metadata(&dest) {
                    if meta.len() == data.len() as u64 {
                        continue;
                    }
                }
                report.overwritten += 1;
            }
            fs::write(&dest, &data)?;
            report.extracted += 1;
        }
    }

    Ok(report)
}

/// 判断 ZIP 条目是否是启动时需要的本地库文件。
fn is_native_payload(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    // 排除目录标记
    if lower.ends_with('/') {
        return false;
    }
    // 排除元数据
    if lower.starts_with("meta-inf/") {
        return false;
    }
    lower.ends_with(".dll")
        || lower.ends_with(".so")
        || lower.ends_with(".dylib")
        || lower.ends_with(".jnilib")
        // 部分 natives jar 里带 .exe / .dylib 依赖
        || lower.ends_with(".exe")
}

/// 一个 ZIP 条目（中央目录信息）。
#[derive(Debug, Clone)]
pub struct ZipEntry {
    pub name: String,
    pub compression: u16,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub local_header_offset: u64,
    pub is_dir: bool,
}

impl ZipEntry {
    /// 读取该条目的解压后内容。
    pub fn read_data(&self, jar: &Path) -> io::Result<Vec<u8>> {
        let mut file = fs::File::open(jar)?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(self.local_header_offset))?;

        // 本地文件头：签名(4) 版本(2) 标志(2) 方法(2) 时间(2) 日期(2)
        //             crc(4) 压缩大小(4) 未压缩大小(4) 文件名长度(2) 扩展长度(2)
        let mut header = [0u8; 30];
        file.read_exact(&mut header)?;
        if &header[0..4] != b"PK\x03\x04" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: 本地文件头签名不匹配", self.name),
            ));
        }
        let name_len = u16::from_le_bytes([header[26], header[27]]) as u64;
        let extra_len = u16::from_le_bytes([header[28], header[29]]) as u64;
        let data_start = self.local_header_offset + 30 + name_len + extra_len;

        file.seek(SeekFrom::Start(data_start))?;
        let mut compressed = vec![0u8; self.compressed_size as usize];
        file.read_exact(&mut compressed)?;

        match self.compression {
            0 => Ok(compressed), // stored
            8 => deflate::inflate(&compressed, self.uncompressed_size as usize),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: 不支持的压缩方法 {}", self.name, other),
            )),
        }
    }
}

/// 读取 ZIP 的中央目录。
pub fn read_zip_entries(jar: &Path) -> io::Result<Vec<ZipEntry>> {
    let data = fs::read(jar)?;
    parse_zip(&data)
}

/// 从内存中的完整 ZIP 数据解析条目表。
pub fn parse_zip(data: &[u8]) -> io::Result<Vec<ZipEntry>> {
    if data.len() < 22 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "文件太小，不是 ZIP"));
    }

    // 从尾部向前找 EOCD 签名 PK\x05\x06
    let mut eocd_pos = None;
    let min_pos = data.len().saturating_sub(22 + 65535);
    let mut i = data.len() - 22;
    loop {
        if &data[i..i + 4] == b"PK\x05\x06" {
            eocd_pos = Some(i);
            break;
        }
        if i == min_pos {
            break;
        }
        i -= 1;
    }
    let eocd = eocd_pos.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "找不到 ZIP 中央目录结尾标记")
    })?;

    let total_entries = u16::from_le_bytes([data[eocd + 10], data[eocd + 11]]) as usize;
    let cd_offset = u32::from_le_bytes([
        data[eocd + 16],
        data[eocd + 17],
        data[eocd + 18],
        data[eocd + 19],
    ]) as usize;

    let mut entries = Vec::with_capacity(total_entries);
    let mut pos = cd_offset;

    for _ in 0..total_entries {
        if pos + 46 > data.len() {
            break;
        }
        if &data[pos..pos + 4] != b"PK\x01\x02" {
            break;
        }
        let compression = u16::from_le_bytes([data[pos + 10], data[pos + 11]]);
        let compressed_size = u32::from_le_bytes([
            data[pos + 20],
            data[pos + 21],
            data[pos + 22],
            data[pos + 23],
        ]) as u64;
        let uncompressed_size = u32::from_le_bytes([
            data[pos + 24],
            data[pos + 25],
            data[pos + 26],
            data[pos + 27],
        ]) as u64;
        let name_len = u16::from_le_bytes([data[pos + 28], data[pos + 29]]) as usize;
        let extra_len = u16::from_le_bytes([data[pos + 30], data[pos + 31]]) as usize;
        let comment_len = u16::from_le_bytes([data[pos + 32], data[pos + 33]]) as usize;
        let local_header_offset = u32::from_le_bytes([
            data[pos + 42],
            data[pos + 43],
            data[pos + 44],
            data[pos + 45],
        ]) as u64;

        let name_start = pos + 46;
        let name_end = name_start + name_len;
        if name_end > data.len() {
            break;
        }
        let name = String::from_utf8_lossy(&data[name_start..name_end]).into_owned();
        let is_dir = name.ends_with('/');

        entries.push(ZipEntry {
            name,
            compression,
            compressed_size,
            uncompressed_size,
            local_header_offset,
            is_dir,
        });

        pos = name_end + extra_len + comment_len;
    }

    Ok(entries)
}

/// 判断某个 natives 目标目录里是否已经有可用文件。
pub fn natives_present(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.path()
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| matches!(x, "dll" | "so" | "dylib" | "jnilib"))
            .unwrap_or(false)
    })
}

/// 最小的 DEFLATE 解压实现（RFC 1951）。
pub mod deflate {
    use std::io;

    /// 解压原始 deflate 流。
    ///
    /// `expected` 用于预分配；实际输出长度以数据为准。
    pub fn inflate(input: &[u8], expected: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(expected.max(64));
        let mut br = BitReader::new(input);

        loop {
            let is_final = br.bits(1)?;
            let block_type = br.bits(2)?;
            match block_type {
                0 => inflate_stored(&mut br, &mut out)?,
                1 => inflate_fixed(&mut br, &mut out)?,
                2 => inflate_dynamic(&mut br, &mut out)?,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "非法的 deflate 块类型",
                    ))
                }
            }
            if is_final == 1 {
                break;
            }
        }
        Ok(out)
    }

    struct BitReader<'a> {
        data: &'a [u8],
        pos: usize,
        bit_buf: u32,
        bit_count: u32,
    }

    impl<'a> BitReader<'a> {
        fn new(data: &'a [u8]) -> Self {
            BitReader {
                data,
                pos: 0,
                bit_buf: 0,
                bit_count: 0,
            }
        }

        fn bits(&mut self, n: u32) -> io::Result<u32> {
            while self.bit_count < n {
                let byte = *self.data.get(self.pos).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "deflate 数据提前结束")
                })?;
                self.pos += 1;
                self.bit_buf |= (byte as u32) << self.bit_count;
                self.bit_count += 8;
            }
            let mask = if n == 32 { u32::MAX } else { (1u32 << n) - 1 };
            let value = self.bit_buf & mask;
            self.bit_buf >>= n;
            self.bit_count -= n;
            Ok(value)
        }

        /// 对齐到字节边界（stored 块要求）。
        ///
        /// 位缓冲里可能已经预读了若干整字节，它们对应的 `pos` 已经被推进，
        /// 因此这里要把这些字节「退回」给 `pos`，否则会跳过数据。
        fn align_byte(&mut self) {
            let drop = self.bit_count % 8;
            self.bit_buf >>= drop;
            self.bit_count -= drop;
            self.pos -= (self.bit_count / 8) as usize;
            self.bit_buf = 0;
            self.bit_count = 0;
        }

        /// 在字节边界上直接读一个小端 u16。
        ///
        /// stored 块里的 LEN / NLEN 是对齐后的整字，若走 `bits()` 会把
        /// 字节拆进位缓冲，后续再对齐就会错位。
        fn read_u16_aligned(&mut self) -> io::Result<u16> {
            debug_assert_eq!(self.bit_count, 0);
            let lo = *self
                .data
                .get(self.pos)
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "stored 块长度缺失"))?;
            let hi = *self
                .data
                .get(self.pos + 1)
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "stored 块长度缺失"))?;
            self.pos += 2;
            Ok(u16::from_le_bytes([lo, hi]))
        }

        fn read_bytes(&mut self, n: usize) -> io::Result<&'a [u8]> {
            self.align_byte();
            let start = self.pos;
            let end = start + n;
            if end > self.data.len() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stored 块数据不足",
                ));
            }
            self.pos = end;
            self.bit_buf = 0;
            self.bit_count = 0;
            Ok(&self.data[start..end])
        }
    }

    fn inflate_stored(br: &mut BitReader, out: &mut Vec<u8>) -> io::Result<()> {
        br.align_byte();
        let len = br.read_u16_aligned()? as usize;
        let nlen = br.read_u16_aligned()? as usize;
        if len != (!nlen & 0xFFFF) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "stored 块长度校验失败",
            ));
        }
        let bytes = br.read_bytes(len)?;
        out.extend_from_slice(bytes);
        Ok(())
    }

    /// 哈夫曼表：用「计数 + 符号序列」的规范形式。
    struct Huffman {
        counts: [u16; 16],
        symbols: Vec<u16>,
    }

    impl Huffman {
        fn from_lengths(lengths: &[u8]) -> Self {
            let mut counts = [0u16; 16];
            for &l in lengths {
                counts[l as usize] += 1;
            }
            counts[0] = 0;

            let mut offsets = [0u16; 16];
            for i in 1..16 {
                offsets[i] = offsets[i - 1] + counts[i - 1];
            }

            let mut symbols = vec![0u16; lengths.len()];
            for (symbol, &l) in lengths.iter().enumerate() {
                if l != 0 {
                    symbols[offsets[l as usize] as usize] = symbol as u16;
                    offsets[l as usize] += 1;
                }
            }

            Huffman { counts, symbols }
        }

        fn decode(&self, br: &mut BitReader) -> io::Result<u16> {
            let mut code: i32 = 0;
            let mut first: i32 = 0;
            let mut index: i32 = 0;
            for len in 1..16 {
                code |= br.bits(1)? as i32;
                let count = self.counts[len] as i32;
                if code - count < first {
                    let idx = (index + (code - first)) as usize;
                    return self
                        .symbols
                        .get(idx)
                        .copied()
                        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "哈夫曼符号越界"));
                }
                index += count;
                first += count;
                first <<= 1;
                code <<= 1;
            }
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "哈夫曼码无法解码",
            ))
        }
    }

    const LENGTH_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LENGTH_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DIST_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DIST_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    fn inflate_block(
        br: &mut BitReader,
        out: &mut Vec<u8>,
        lit: &Huffman,
        dist: &Huffman,
    ) -> io::Result<()> {
        loop {
            let symbol = lit.decode(br)?;
            if symbol < 256 {
                out.push(symbol as u8);
            } else if symbol == 256 {
                return Ok(());
            } else {
                let idx = (symbol - 257) as usize;
                if idx >= LENGTH_BASE.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "长度码越界",
                    ));
                }
                let mut length = LENGTH_BASE[idx] as usize;
                let extra = LENGTH_EXTRA[idx];
                if extra > 0 {
                    length += br.bits(extra as u32)? as usize;
                }

                let dist_symbol = dist.decode(br)? as usize;
                if dist_symbol >= DIST_BASE.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "距离码越界",
                    ));
                }
                let mut distance = DIST_BASE[dist_symbol] as usize;
                let extra = DIST_EXTRA[dist_symbol];
                if extra > 0 {
                    distance += br.bits(extra as u32)? as usize;
                }
                if distance > out.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "回溯距离超出已解压数据",
                    ));
                }
                // 逐字节复制（允许重叠）
                let start = out.len() - distance;
                for i in 0..length {
                    let byte = out[start + i];
                    out.push(byte);
                }
            }
        }
    }

    fn inflate_fixed(br: &mut BitReader, out: &mut Vec<u8>) -> io::Result<()> {
        let mut lit_lengths = [0u8; 288];
        for (i, l) in lit_lengths.iter_mut().enumerate() {
            *l = match i {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        let lit = Huffman::from_lengths(&lit_lengths);
        let dist_lengths = [5u8; 30];
        let dist = Huffman::from_lengths(&dist_lengths);
        inflate_block(br, out, &lit, &dist)
    }

    fn inflate_dynamic(br: &mut BitReader, out: &mut Vec<u8>) -> io::Result<()> {
        const ORDER: [usize; 19] = [
            16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
        ];

        let hlit = br.bits(5)? as usize + 257;
        let hdist = br.bits(5)? as usize + 1;
        let hclen = br.bits(4)? as usize + 4;

        let mut code_lengths = [0u8; 19];
        for &idx in ORDER.iter().take(hclen) {
            code_lengths[idx] = br.bits(3)? as u8;
        }
        let code_huff = Huffman::from_lengths(&code_lengths);

        let total = hlit + hdist;
        let mut lengths = vec![0u8; total];
        let mut i = 0;
        while i < total {
            let sym = code_huff.decode(br)?;
            match sym {
                0..=15 => {
                    lengths[i] = sym as u8;
                    i += 1;
                }
                16 => {
                    if i == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "重复码出现在第一个符号",
                        ));
                    }
                    let prev = lengths[i - 1];
                    let repeat = br.bits(2)? as usize + 3;
                    for _ in 0..repeat {
                        if i >= total {
                            break;
                        }
                        lengths[i] = prev;
                        i += 1;
                    }
                }
                17 => {
                    let repeat = br.bits(3)? as usize + 3;
                    i += repeat.min(total - i);
                }
                18 => {
                    let repeat = br.bits(7)? as usize + 11;
                    i += repeat.min(total - i);
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "码长符号越界",
                    ))
                }
            }
        }

        let lit = Huffman::from_lengths(&lengths[..hlit]);
        let dist = Huffman::from_lengths(&lengths[hlit..]);
        inflate_block(br, out, &lit, &dist)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 用 stored 块构造一个最小可解压流：原样字节。
        fn stored_block(data: &[u8], is_final: bool) -> Vec<u8> {
            let mut v = Vec::new();
            // 位序：先 1 位 final，再 2 位类型(00)
            // 组合成一个字节的低 3 位
            let first = if is_final { 1u8 } else { 0 };
            v.push(first);
            let len = data.len() as u16;
            v.extend_from_slice(&len.to_le_bytes());
            v.extend_from_slice(&(!len).to_le_bytes());
            v.extend_from_slice(data);
            v
        }

        #[test]
        fn inflates_stored_block() {
            let payload = b"hello world";
            let stream = stored_block(payload, true);
            let out = inflate(&stream, payload.len()).unwrap();
            assert_eq!(out, payload);
        }

        #[test]
        fn inflates_two_stored_blocks() {
            let mut stream = stored_block(b"abc", false);
            stream.extend_from_slice(&stored_block(b"def", true));
            let out = inflate(&stream, 6).unwrap();
            assert_eq!(out, b"abcdef");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个最小的 stored 模式 ZIP（含一个条目）。
    fn make_stored_zip(name: &str, content: &[u8]) -> Vec<u8> {
        let mut z = Vec::new();
        let crc = crc32(content);

        // 本地文件头
        let name_bytes = name.as_bytes();
        z.extend_from_slice(b"PK\x03\x04");
        z.extend_from_slice(&20u16.to_le_bytes()); // version needed
        z.extend_from_slice(&0u16.to_le_bytes()); // flags
        z.extend_from_slice(&0u16.to_le_bytes()); // method = stored
        z.extend_from_slice(&0u16.to_le_bytes()); // time
        z.extend_from_slice(&0u16.to_le_bytes()); // date
        z.extend_from_slice(&crc.to_le_bytes());
        z.extend_from_slice(&(content.len() as u32).to_le_bytes());
        z.extend_from_slice(&(content.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes()); // extra len
        z.extend_from_slice(name_bytes);
        z.extend_from_slice(content);

        let local_size = 30 + name_bytes.len() + content.len();

        // 中央目录
        let cd_start = z.len();
        z.extend_from_slice(b"PK\x01\x02");
        z.extend_from_slice(&20u16.to_le_bytes()); // version made by
        z.extend_from_slice(&20u16.to_le_bytes()); // version needed
        z.extend_from_slice(&0u16.to_le_bytes()); // flags
        z.extend_from_slice(&0u16.to_le_bytes()); // method
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&crc.to_le_bytes());
        z.extend_from_slice(&(content.len() as u32).to_le_bytes());
        z.extend_from_slice(&(content.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes()); // extra
        z.extend_from_slice(&0u16.to_le_bytes()); // comment
        z.extend_from_slice(&0u16.to_le_bytes()); // disk
        z.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        z.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        z.extend_from_slice(&0u32.to_le_bytes()); // local header offset
        z.extend_from_slice(name_bytes);

        let cd_size = z.len() - cd_start;

        // EOCD
        z.extend_from_slice(b"PK\x05\x06");
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(&1u16.to_le_bytes()); // entries on disk
        z.extend_from_slice(&1u16.to_le_bytes()); // total entries
        z.extend_from_slice(&(cd_size as u32).to_le_bytes());
        z.extend_from_slice(&(cd_start as u32).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());

        let _ = local_size;
        z
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFFFFFFu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB88320 & mask);
            }
        }
        !crc
    }

    #[test]
    fn parses_zip_central_directory() {
        let zip = make_stored_zip("dir/foo.dll", b"dll-bytes");
        let entries = parse_zip(&zip).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "dir/foo.dll");
        assert_eq!(entries[0].uncompressed_size, 9);
    }

    #[test]
    fn filters_native_payloads() {
        assert!(is_native_payload("org/lwjgl/lwjgl.dll"));
        assert!(is_native_payload("libfoo.so"));
        assert!(is_native_payload("libfoo.dylib"));
        assert!(!is_native_payload("META-INF/MANIFEST.MF"));
        assert!(!is_native_payload("meta-inf/LICENSE"));
        assert!(!is_native_payload("readme.txt"));
    }

    #[test]
    fn extracts_natives_to_directory() {
        let tmp = crate::testutil::fresh_dir("aicraft_test_natives");
        let _ = fs::remove_dir_all(&tmp);
        let zip = make_stored_zip("lwjgl.dll", b"FAKE-DLL-DATA");
        let jar = tmp.join("test-natives.jar");
        fs::create_dir_all(&tmp).unwrap();
        fs::write(&jar, &zip).unwrap();

        let out_dir = tmp.join("out");
        let report = extract_natives(&[(jar.clone(), vec![])], &out_dir).unwrap();
        assert_eq!(report.extracted, 1);
        let extracted = out_dir.join("lwjgl.dll");
        assert!(extracted.is_file());
        assert_eq!(fs::read(&extracted).unwrap(), b"FAKE-DLL-DATA");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn respects_exclude_prefixes() {
        let tmp = crate::testutil::fresh_dir("aicraft_test_natives_ex");
        let _ = fs::remove_dir_all(&tmp);
        let zip = make_stored_zip("META-INF/x.dll", b"should-skip");
        let jar = tmp.join("n.jar");
        fs::create_dir_all(&tmp).unwrap();
        fs::write(&jar, &zip).unwrap();

        let out_dir = tmp.join("out");
        // is_native_payload 已经过滤 META-INF，这里验证不会写出
        let report = extract_natives(&[(jar, vec!["META-INF/".into()])], &out_dir).unwrap();
        assert_eq!(report.extracted, 0);
        assert!(!out_dir.join("x.dll").exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn detects_present_natives() {
        let tmp = crate::testutil::fresh_dir("aicraft_test_present");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        assert!(!natives_present(&tmp));
        fs::write(tmp.join("a.dll"), b"x").unwrap();
        assert!(natives_present(&tmp));
        let _ = fs::remove_dir_all(&tmp);
    }
}
