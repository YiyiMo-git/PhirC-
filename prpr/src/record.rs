//! 成绩上传记录的编码器 —— 官方闭源 `encode_record` 的等价实现。
//!
//! 链路（完整调查记录见 `docs/上传成绩逆向笔记.md`）：
//!
//! ```text
//! record 92B
//!   --custom_enc--> 96B        补 (len & 0xf) 到 (len & !0xf) + 0x10；
//!                              每 16 字节块：zeta 子集和变换 + 4-bit 位反转置换
//!   --|| HMAC-SHA256(key)--> 128B   尾部 32 字节
//!   --raw deflate(level 0)--> token 字节，上层再 base64 放进 /play/upload
//! ```
//!
//! 92 字节布局：
//!
//! | offset | 内容 |
//! |---|---|
//! | 0 | 版本常量 `3` |
//! | 1..4 | player id（**LE**） |
//! | 5..8 | chart id（**BE**，官方用 `rev` 指令写） |
//! | 9..16 | 判定误差 variance（f64 **LE**）= Σdiff² / 判定次数 |
//! | 17..20 | 保留（9 个真实样本恒为 0） |
//! | 21..23 | 官方是三个「7 位值 ×2 + 布尔标志」的字节；**服务端不校验**，实测传 0 被接受 |
//! | 24..27 | 保留（官方取自一个全局 atomic，样本恒为 0） |
//! | 28 | `0xd8` |
//! | 29..32 | max_combo（LE） |
//! | 33..48 | counts[4]：Perfect / Good / Bad / Miss（各 LE u32） |
//! | 49 | 速度 × 100 |
//! | 50..81 | 官方谱面文件**原始字节**的 SHA-256 |
//! | 82 / 83 | `0x74` / `0xd2` |
//! | 84..87 | num_of_notes（LE） |
//! | 88..91 | 保留（样本恒为 0） |
//! | 92..95 | `0x0c` × 4（`custom_enc` 自己补的填充） |
//!
//! ⚠️ 两个坑：
//! 1. `chart_sha256` 必须对**原始 chart 文件字节**求，不要用解析后重新序列化的结果，
//!    否则服务端判谱面不匹配直接拒绝；
//! 2. deflate 用 **level 0**（官方就是 level 0，128B → 135B → 180 字符 base64）。
//!    用别的等级服务端也能解，但没必要引入变量。

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::judge::PlayResult;

/// 官方客户端内嵌的 HMAC 密钥：libphira.so 里 26 字节混淆常量经固定 XOR 还原。
const HMAC_KEY: &[u8] = b"KTCfLeu6d6gq957WOEyj-light";

const VERSION: u8 = 3;
const CONST_28: u8 = 0xd8;
const CONST_82: u8 = 0x74;
const CONST_83: u8 = 0xd2;

/// 官方客户端用的 deflate 等级（0 = 不压缩，stored blocks）。
const DEFLATE_LEVEL: u8 = 0;

/// 4 bit 位反转。
fn bitrev4(i: usize) -> usize {
    let mut r = 0;
    for k in 0..4 {
        r = (r << 1) | ((i >> k) & 1);
    }
    r
}

/// 4 维子集和：`out[i] = Σ_{j ⊆ i} in[j] (mod 256)`。
fn zeta_transform(block: &mut [u8; 16]) {
    for bit in 0..4 {
        let step = 1usize << bit;
        for i in 0..16 {
            if i & step != 0 {
                block[i] = block[i].wrapping_add(block[i ^ step]);
            }
        }
    }
}

/// 块内 4-bit 位反转置换（`i <-> bitrev4(i)`）。
fn bitrev_permute(block: &mut [u8; 16]) {
    for i in 0..16 {
        let j = bitrev4(i);
        if i < j {
            block.swap(i, j);
        }
    }
}

/// 官方的 `custom_enc`：无密钥的确定性变换。
///
/// 把 `plain` 补齐到 `(len & !0xf) + 0x10`（填充字节 = `len & 0xf`），
/// 然后对每个 16 字节块做 `zeta` 子集和 + 4-bit 位反转置换。
pub fn custom_enc(plain: &[u8]) -> Vec<u8> {
    let pad = (plain.len() & 0xf) as u8;
    let new_len = (plain.len() & !0xf) + 0x10;
    let mut data = Vec::with_capacity(new_len);
    data.extend_from_slice(plain);
    data.resize(new_len, pad);

    for block in data.chunks_exact_mut(16) {
        let block: &mut [u8; 16] = block.try_into().expect("chunks_exact_mut(16)");
        zeta_transform(block);
        bitrev_permute(block);
    }
    data
}

/// 把一局的结算数据编码成 `/play/upload` 的 `token` 原始字节。
///
/// 上层只需要再 `base64(token)` 即可（见 `phira/src/scene/song.rs`）。
///
/// * `player_id` / `chart_id` —— 账号 id 与**服务器端**的谱面 id；
/// * `speed` —— 游戏速度（倍速），记录里写的是 `round(speed * 100)`；
/// * `result` —— `Judge::result()` 的结果，用其中的 counts / max_combo /
///   num_of_notes / variance；
/// * `chart_bytes` —— **官方谱面文件的原始字节**（`GameScene::chart_bytes`）。
pub fn encode_record(
    player_id: i32,
    chart_id: i32,
    speed: f32,
    result: &PlayResult,
    chart_bytes: &[u8],
) -> Vec<u8> {
    let mut rec = Vec::with_capacity(96);
    rec.push(VERSION);
    rec.extend_from_slice(&player_id.to_le_bytes());
    rec.extend_from_slice(&chart_id.to_be_bytes());
    rec.extend_from_slice(&result.variance.to_le_bytes());
    // 17..20：9 个真实样本恒为 0。早期笔记猜过是 `config.mods`，但没有样本支持，
    // 先按观测值写 0；等抓到一局带 mod 的真实上传样本再定。
    rec.extend_from_slice(&0u32.to_le_bytes());
    // 21..23：官方这里放三个「7 位值 ×2 + 布尔标志」，服务端不校验（实测传 0 被接受）。
    rec.extend_from_slice(&[0u8; 3]);
    // 24..27：官方取自一个全局 atomic，样本恒为 0。
    rec.extend_from_slice(&0u32.to_le_bytes());
    rec.push(CONST_28);
    rec.extend_from_slice(&result.max_combo.to_le_bytes());
    for c in result.counts {
        rec.extend_from_slice(&c.to_le_bytes());
    }
    rec.push((speed * 100.).round().clamp(0., 255.) as u8);
    rec.extend_from_slice(&Sha256::digest(chart_bytes));
    rec.push(CONST_82);
    rec.push(CONST_83);
    rec.extend_from_slice(&result.num_of_notes.to_le_bytes());
    // 88..91：样本恒为 0。
    rec.extend_from_slice(&0u32.to_le_bytes());
    debug_assert_eq!(rec.len(), 92, "record layout must be 92 bytes");

    let cipher = custom_enc(&rec);
    let mut mac = Hmac::<Sha256>::new_from_slice(HMAC_KEY).expect("HMAC accepts any key length");
    mac.update(&cipher);
    let tag = mac.finalize().into_bytes();

    let mut payload = cipher;
    payload.extend_from_slice(&tag);
    debug_assert_eq!(payload.len(), 128);

    miniz_oxide::deflate::compress_to_vec(&payload, DEFLATE_LEVEL)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 合成测试向量，由 docs/rev/gen_test_vector.py 生成（不含任何真实账号数据）。
    // 输入：player_id=1000001, chart_id=12345, variance=0.006464875507526675,
    //       max_combo=135, counts=[182,4,0,19], speed=1.0, num_of_notes=205,
    //       chart_bytes=b"phira-test-chart"

    const RECORD_HEX: &[&str] = &[
        "0341420f0000003039c8650acee97a7a",
        "3f0000000000000000000000d8870000",
        "00b60000000400000000000000130000",
        "00647e2d770eee03c0f0a88493e7af85",
        "9f09926a5bb26e16b4c5143fed74b976",
        "575f74d2cd00000000000000",
    ];
    const CIPHER_HEX: &[&str] = &[
        "033c030a45e3452b444544fc9505c5e0",
        "3f3f3f173f3f3f173f3f3f9e3f3f3f9e",
        "0000000000000000b6b6bacdb6b6bacd",
        "00c077ca7ee6e38d6414e9130feb850f",
        "9f53fa9b31f9fa68a821b58fa4703591",
        "57572430cbcb98b0b6b6839bfcfcc9f9",
    ];
    const PAYLOAD_HEX: &[&str] = &[
        "033c030a45e3452b444544fc9505c5e0",
        "3f3f3f173f3f3f173f3f3f9e3f3f3f9e",
        "0000000000000000b6b6bacdb6b6bacd",
        "00c077ca7ee6e38d6414e9130feb850f",
        "9f53fa9b31f9fa68a821b58fa4703591",
        "57572430cbcb98b0b6b6839bfcfcc9f9",
        "7ba6e55e194b5efa4de16b314d53be0b",
        "1accc7c424cf69fb9e6ba9bae6c55775",
    ];

    fn unhex(parts: &[&str]) -> Vec<u8> {
        let s: String = parts.concat();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn sample_result() -> PlayResult {
        PlayResult {
            score: 876293,
            accuracy: (182. + 0.65 * 4.) / 205.,
            max_combo: 135,
            num_of_notes: 205,
            counts: [182, 4, 0, 19],
            variance: 0.006464875507526675,
            ..Default::default()
        }
    }

    #[test]
    fn record_layout_matches_official() {
        // 直接验证序列化：encode_record 内部不暴露 rec，这里用固定的期望字节
        // 反推 —— 通过 custom_enc 的输出间接校验（见下一个测试）。
        let cipher = custom_enc(&unhex(RECORD_HEX));
        assert_eq!(cipher, unhex(CIPHER_HEX), "custom_enc output changed");
    }

    #[test]
    fn payload_matches_official() {
        let token = encode_record(1000001, 12345, 1.0, &sample_result(), b"phira-test-chart");
        let payload = miniz_oxide::inflate::decompress_to_vec(&token).expect("raw inflate");
        assert_eq!(payload.len(), 128);
        assert_eq!(payload, unhex(PAYLOAD_HEX), "record / HMAC output changed");
    }

    /// 测试用逆变换（mobius 子集差分 + 同一个位反转置换）。
    fn custom_enc_inverse(cipher: &[u8]) -> Vec<u8> {
        fn mobius(block: &mut [u8; 16]) {
            for bit in (0..4).rev() {
                let step = 1usize << bit;
                for i in 0..16 {
                    if i & step != 0 {
                        block[i] = block[i].wrapping_sub(block[i ^ step]);
                    }
                }
            }
        }
        let mut out = Vec::with_capacity(cipher.len());
        for chunk in cipher.chunks_exact(16) {
            let mut block: [u8; 16] = chunk.try_into().unwrap();
            bitrev_permute(&mut block);
            mobius(&mut block);
            out.extend_from_slice(&block);
        }
        out
    }

    #[test]
    fn custom_enc_pads_like_official() {
        for len in [0usize, 1, 15, 16, 17, 92, 96] {
            let plain = vec![7u8; len];
            let cipher = custom_enc(&plain);
            // 补到 (len & !0xf) + 0x10
            assert_eq!(cipher.len(), (len & !0xf) + 0x10, "len={len}");
            let back = custom_enc_inverse(&cipher);
            assert_eq!(&back[..len], &plain[..], "round-trip body, len={len}");
            // 填充字节 = len & 0xf（92 -> 0x0c）
            let pad = (len & 0xf) as u8;
            assert!(
                back[len..].iter().all(|b| *b == pad),
                "padding must be len & 0xf, len={len}"
            );
        }
    }
}
