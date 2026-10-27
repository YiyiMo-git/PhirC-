//! `.pic`（PhiC 二进制分块容器）读取。
//!
//! 由 `page/library.rs` 的「导入」流程触发：当用户选中一个 `.pic` 时走这里，
//! 把剧情包**完整**读出来（40B 文件头 + 18B×N 目录 + 各分块解码），
//! 落到 `<data>/chapters/<uuid>/`（`package.pic` 原包 + `chapter.json` 结构化信息），
//! 并把 uuid 登记进 [`crate::data::Data::chapters`]。
//!
//! 布局（与 `phic-core` 的 `container.rs` 对齐）：
//! ```text
//! 头 40B: magic"PHIC" | ver u16 | flags u16 | count u32 | lang 3B | reserved 9B | salt 16B
//! 目录项 18B: id[4] | codec u8 | flags u8 | raw_len u32 | stored_len u32 | crc u32
//! 载荷: 按目录顺序紧密排列
//! flags: bit0=zstd 压缩, bit1=加密, bit2=签名
//! ```

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{dir, get_data_mut, save_data};

const MAGIC: &[u8; 4] = b"PHIC";
const HEADER_LEN: usize = 40;
const TOC_ENTRY_LEN: usize = 18;

const FLAG_ENCRYPTED: u16 = 1 << 1;
const FLAG_SIGNED: u16 = 1 << 2;

/// `.pic` 的 `META` 块（对应 phic-core 的 `Meta`；未知字段忽略，缺失字段用默认值）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PicMeta {
    pub format_version: u16,
    pub language_version: String,
    pub app_version: String,
    pub entry: String,
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub audio_offset_ms: f64,
    pub title: String,
    pub author: String,
    pub scenes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PicChunk {
    pub id: String,
    /// 0 = 原样存储，1 = zstd。
    pub codec: u8,
    pub raw_len: usize,
    pub crc: u32,
    /// 解压后的原始字节。
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PicPackage {
    pub format_version: u16,
    pub flags: u16,
    pub encrypted: bool,
    pub signed: bool,
    pub chunks: Vec<PicChunk>,
    pub meta: PicMeta,
}

/// 解析 `.pic` 字节流。加密包在无口令下无法解开 → 明确报错。
pub fn parse(bytes: &[u8]) -> Result<PicPackage> {
    if bytes.len() < HEADER_LEN {
        bail!("文件太小，不是有效的 .pic");
    }
    if &bytes[0..4] != MAGIC {
        bail!("不是 .pic 文件（缺少 PHIC 魔数）");
    }
    let format_version = u16::from_le_bytes([bytes[4], bytes[5]]);
    let flags = u16::from_le_bytes([bytes[6], bytes[7]]);
    let chunk_count = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    let encrypted = flags & FLAG_ENCRYPTED != 0;
    let signed = flags & FLAG_SIGNED != 0;
    if encrypted {
        bail!("该 .pic 已加密，需要口令才能读取（当前不支持）");
    }
    let toc_end = HEADER_LEN + chunk_count * TOC_ENTRY_LEN;
    if toc_end > bytes.len() {
        bail!("目录越界：文件可能损坏");
    }

    let mut chunks = Vec::with_capacity(chunk_count);
    let mut offset = toc_end;
    for i in 0..chunk_count {
        let base = HEADER_LEN + i * TOC_ENTRY_LEN;
        let id = String::from_utf8_lossy(&bytes[base..base + 4]).to_string();
        let codec = bytes[base + 4];
        let raw_len =
            u32::from_le_bytes([bytes[base + 6], bytes[base + 7], bytes[base + 8], bytes[base + 9]]) as usize;
        let stored_len =
            u32::from_le_bytes([bytes[base + 10], bytes[base + 11], bytes[base + 12], bytes[base + 13]]) as usize;
        let crc = u32::from_le_bytes([bytes[base + 14], bytes[base + 15], bytes[base + 16], bytes[base + 17]]);
        if offset + stored_len > bytes.len() {
            bail!("分块 `{id}` 载荷越界");
        }
        let payload = &bytes[offset..offset + stored_len];
        let raw = match codec {
            0 => payload.to_vec(),
            1 => zstd::stream::decode_all(payload).with_context(|| format!("分块 `{id}` zstd 解压失败"))?,
            other => bail!("分块 `{id}` 使用了未知编码 {other}"),
        };
        if raw.len() != raw_len {
            bail!("分块 `{id}` 长度不符（期望 {raw_len}，实际 {}）", raw.len());
        }
        chunks.push(PicChunk {
            id,
            codec,
            raw_len,
            crc,
            raw,
        });
        offset += stored_len;
    }

    let meta = chunks
        .iter()
        .find(|c| c.id == "META")
        .map(|c| serde_json::from_slice::<PicMeta>(&c.raw))
        .transpose()
        .context("META 块不是合法 JSON")?
        .unwrap_or_default();

    Ok(PicPackage {
        format_version,
        flags,
        encrypted,
        signed,
        chunks,
        meta,
    })
}

/// 快速判断一个文件是不是 `.pic`（只看前 4 字节魔数）。
pub fn is_pic_file(path: &Path) -> bool {
    std::fs::read(path)
        .map(|b| b.len() >= 4 && &b[0..4] == MAGIC)
        .unwrap_or(false)
}

/// 导入一个 `.pic`：完整解析 → 落到 `<data>/chapters/<uuid>/` → 登记进 Data。
/// 返回 `(uuid, 标题, 场景数)`。
pub fn import_pic(path: &Path) -> Result<(String, String, usize)> {
    let bytes = std::fs::read(path).with_context(|| format!("读取 {} 失败", path.display()))?;
    let pkg = parse(&bytes)?;

    let root = dir::chapters()?;
    let base = Path::new(&root);
    let mut uuid = uuid::Uuid::new_v4();
    while base.join(uuid.to_string()).exists() {
        uuid = uuid::Uuid::new_v4();
    }
    let id = uuid.to_string();
    let cdir = base.join(&id);
    std::fs::create_dir_all(&cdir)?;

    // 原始 `.pic` 留一份，运行时 / 编辑器可直接用。
    std::fs::write(cdir.join("package.pic"), &bytes)?;

    // 结构化章节信息（META + 分块清单）。
    let chapter = serde_json::json!({
        "title": pkg.meta.title,
        "author": pkg.meta.author,
        "entry": pkg.meta.entry,
        "bpm": pkg.meta.bpm,
        "scenes": pkg.meta.scenes,
        "format_version": pkg.format_version,
        "language_version": pkg.meta.language_version,
        "app_version": pkg.meta.app_version,
        "flags": pkg.flags,
        "encrypted": pkg.encrypted,
        "signed": pkg.signed,
        // 每个分块：id / 编码 / 解压后长度 / crc
        "chunks": pkg.chunks.iter().map(|c| serde_json::json!({
            "id": c.id,
            "codec": c.codec,
            "raw_len": c.raw_len,
            "crc": c.crc,
        })).collect::<Vec<_>>(),
    });
    std::fs::write(cdir.join("chapter.json"), serde_json::to_vec_pretty(&chapter)?)?;

    get_data_mut().chapters.push(uuid);
    save_data()?;

    let title = if pkg.meta.title.trim().is_empty() {
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名章节".into())
    } else {
        pkg.meta.title.clone()
    };
    Ok((id, title, pkg.meta.scenes.len()))
}
