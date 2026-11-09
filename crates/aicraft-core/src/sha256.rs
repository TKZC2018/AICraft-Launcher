//! SHA-256 实现。
//!
//! 和 [`crate::sha1`] 不同，这里算哈希不是为了校验游戏文件，而是给
//! OAuth 2.0 的 PKCE 用：`code_challenge = BASE64URL_NOPAD(SHA256(verifier))`。
//!
//! 微软的 `/token` 端点只保证接受 `S256` 这种 challenge 方式，`plain`
//! 在公共客户端上并不可靠，所以这一步省不掉。
//!
//! 零依赖自实现，理由同其他模块：本机 cargo 拉不到 crates.io。
//! 结构与 [`crate::sha1::Sha1Stream`] 保持一致，方便对照阅读。

/// SHA-256 轮常量。
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 初始状态。
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// 计算一段字节的 SHA-256，返回小写十六进制字符串。
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = sha256(data);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// 计算一段字节的 SHA-256。
pub fn sha256(data: &[u8]) -> [u8; 32] {
    // 填充：0x80，然后补零到 56 mod 64，最后 8 字节大端位长度
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    let mut h = H0;
    for chunk in msg.chunks_exact(64) {
        compress(&mut h, chunk);
    }

    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// 流式 SHA-256，支持分块喂数据。
///
/// 目前 PKCE 的 verifier 只有几十字节，一次性算就够了；但登录流程以后
/// 可能要算更大的东西，先按流式写，和 `Sha1Stream` 对齐。
pub struct Sha256Stream {
    h: [u32; 8],
    buffer: Vec<u8>,
    total_len: u64,
}

impl Sha256Stream {
    pub fn new() -> Self {
        Sha256Stream {
            h: H0,
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
                compress(&mut self.h, &block);
            }
        }

        // 整块处理
        while data.len() >= 64 {
            compress(&mut self.h, &data[..64]);
            data = &data[64..];
        }

        // 余下留到下次
        self.buffer.extend_from_slice(data);
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bit_len = self.total_len.wrapping_mul(8);
        self.buffer.push(0x80);
        while self.buffer.len() % 64 != 56 {
            self.buffer.push(0);
        }
        self.buffer.extend_from_slice(&bit_len.to_be_bytes());
        // 先取出再处理，避免同时借用 self.buffer 与 self.h
        let blocks: Vec<Vec<u8>> = self
            .buffer
            .chunks_exact(64)
            .map(|c| c.to_vec())
            .collect();
        for block in blocks {
            compress(&mut self.h, &block);
        }

        let mut out = [0u8; 32];
        for (i, v) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }

    pub fn finish_hex(self) -> String {
        let digest = self.finish();
        let mut out = String::with_capacity(64);
        for b in digest {
            out.push_str(&format!("{:02x}", b));
        }
        out
    }
}

impl Default for Sha256Stream {
    fn default() -> Self {
        Self::new()
    }
}

/// 压缩一个 64 字节块到状态里。
fn compress(h: &mut [u32; 8], chunk: &[u8]) {
    debug_assert_eq!(chunk.len(), 64);
    let mut w = [0u32; 64];
    for (i, word) in w.iter_mut().take(16).enumerate() {
        *word = u32::from_be_bytes([
            chunk[i * 4],
            chunk[i * 4 + 1],
            chunk[i * 4 + 2],
            chunk[i * 4 + 3],
        ]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let mut a = h[0];
    let mut b = h[1];
    let mut c = h[2];
    let mut d = h[3];
    let mut e = h[4];
    let mut f = h[5];
    let mut g = h[6];
    let mut hh = h[7];

    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);

        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
    h[5] = h[5].wrapping_add(f);
    h[6] = h[6].wrapping_add(g);
    h[7] = h[7].wrapping_add(hh);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn abc() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn two_block_vector() {
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn million_a() {
        let data = vec![b'a'; 1_000_000];
        assert_eq!(
            sha256_hex(&data),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn streaming_matches_one_shot() {
        // 覆盖各种分块边界：块内、恰好一块、跨块、长度 55/56/64 这类填充临界点
        let data: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
        let expect = sha256_hex(&data);
        for chunk in [1usize, 7, 55, 56, 63, 64, 65, 127, 128, 300] {
            let mut hs = Sha256Stream::new();
            for part in data.chunks(chunk) {
                hs.update(part);
            }
            assert_eq!(hs.finish_hex(), expect, "分块大小 {} 结果不一致", chunk);
        }
    }

    #[test]
    fn sha256_is_stable_across_calls() {
        assert_eq!(sha256(b"x"), sha256(b"x"));
        assert_ne!(sha256(b"x"), sha256(b"y"));
    }
}
