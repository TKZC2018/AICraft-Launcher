//! SHA-1 实现。
//!
//! Minecraft 的资源与库文件都以 SHA-1 作为完整性凭据（`assets` 索引里的
//! `hash` 字段、库下载信息里的 `sha1`）。校验必须能算出同样的值。
//!
//! 零依赖自实现，理由同其他模块：本机 cargo 拉不到 crates.io。

/// 计算一段字节的 SHA-1，返回小写十六进制字符串。
pub fn sha1_hex(data: &[u8]) -> String {
    let digest = sha1(data);
    let mut out = String::with_capacity(40);
    for b in digest {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// 计算一段字节的 SHA-1。
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0,
    ];

    // 填充：0x80，然后补零到 56 mod 64，最后 8 字节大端长度
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);

        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// 计算文件的 SHA-1，流式读取以免大文件占满内存。
pub fn sha1_file_hex(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha1Stream::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finish_hex())
}

/// 流式 SHA-1，支持分块喂数据。
pub struct Sha1Stream {
    h: [u32; 5],
    buffer: Vec<u8>,
    total_len: u64,
}

impl Sha1Stream {
    pub fn new() -> Self {
        Sha1Stream {
            h: [
                0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0,
            ],
            buffer: Vec::with_capacity(64),
            total_len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len += data.len() as u64;

        // 先填满已有缓冲
        if !self.buffer.is_empty() {
            let need = 64 - self.buffer.len();
            let take = need.min(data.len());
            self.buffer.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buffer.len() == 64 {
                let block = std::mem::take(&mut self.buffer);
                self.process_block(&block);
            }
        }

        // 整块处理
        while data.len() >= 64 {
            self.process_block(&data[..64]);
            data = &data[64..];
        }

        // 余下留到下次
        self.buffer.extend_from_slice(data);
    }

    pub fn finish(mut self) -> [u8; 20] {
        let bit_len = self.total_len.wrapping_mul(8);
        self.buffer.push(0x80);
        while self.buffer.len() % 64 != 56 {
            self.buffer.push(0);
        }
        self.buffer.extend_from_slice(&bit_len.to_be_bytes());
        let blocks: Vec<Vec<u8>> = self
            .buffer
            .chunks_exact(64)
            .map(|c| c.to_vec())
            .collect();
        for block in blocks {
            self.process_block(&block);
        }

        let mut out = [0u8; 20];
        for (i, v) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }

    pub fn finish_hex(self) -> String {
        let digest = self.finish();
        let mut out = String::with_capacity(40);
        for b in digest {
            out.push_str(&format!("{:02x}", b));
        }
        out
    }

    fn process_block(&mut self, chunk: &[u8]) {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (self.h[0], self.h[1], self.h[2], self.h[3], self.h[4]);

        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
    }
}

impl Default for Sha1Stream {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_vectors() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        assert_eq!(
            sha1_hex(b"The quick brown fox jumps over the lazy dog"),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
    }

    #[test]
    fn handles_exactly_64_bytes() {
        let data = vec![b'a'; 64];
        let expected = sha1_hex(&data);
        let mut s = Sha1Stream::new();
        s.update(&data);
        assert_eq!(s.finish_hex(), expected);
    }

    /// 分块喂数据必须和一次性算出来的一致。
    #[test]
    fn streaming_matches_oneshot() {
        let data: Vec<u8> = (0..1000).map(|i| (i % 251) as u8).collect();
        let expected = sha1_hex(&data);

        for chunk_size in [1usize, 7, 63, 64, 65, 100] {
            let mut s = Sha1Stream::new();
            for chunk in data.chunks(chunk_size) {
                s.update(chunk);
            }
            assert_eq!(s.finish_hex(), expected, "chunk_size={}", chunk_size);
        }
    }

    #[test]
    fn empty_stream_matches_empty_hash() {
        let s = Sha1Stream::new();
        assert_eq!(s.finish_hex(), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }
}
