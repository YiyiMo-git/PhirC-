//! 回放库：`<data>/replays/` 下的文件枚举、索引与导入 / 导出 / 删除 / 关联。
//!
//! 设计口径（对齐上游 Phira-Pro，按本工程简化）：
//! * **导入的回放不写历史、不计 PB / RKS、不上传** —— 它属于「看别人打」；
//! * 索引：`index.json` 里只存「导入绑定」（谁导入的、关联到哪个本地谱面）；
//! * 自录制的回放文件名 = `<谱面key>_<结束毫秒时间戳>.ppr`，可直接从历史反查。

use super::{load_replay, save_replay, ChartRef, Replay};
use prpr::replay::{Metadata, ReplayResult};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[allow(dead_code)]
static REVISION: AtomicU64 = AtomicU64::new(0);

/// 库变更计数（增了什么都会 +1，UI 可用来判断要不要刷新）。
#[allow(dead_code)]
pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

fn root() -> Result<PathBuf> {
    Ok(PathBuf::from(crate::dir::root()?).join("replays"))
}

/// 导入绑定：一个文件一条。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Binding {
    pub file: String,
    pub imported_at: i64,
    /// 关联到的本地谱面路径（缺少谱面时给用户的「关联 / 下载」入口）。
    pub local_path: Option<String>,
    pub verified: bool,
    pub player: Option<String>,
    pub player_id: Option<i32>,
}

/// 列表里的一项。
#[derive(Clone, Debug, Default)]
pub struct Item {
    pub path: PathBuf,
    pub binding: Option<Binding>,
    pub meta: Metadata,
    pub result: Option<ReplayResult>,
    pub chart: Option<ChartRef>,
    pub duration: f64,
    pub speed: f32,
    pub offset: f32,
    pub imported: bool,
}

impl Item {
    /// 从磁盘读到的 `Replay` 造一条列表项。
    pub fn from_replay(path: PathBuf, binding: Option<Binding>, replay: Replay, imported: bool) -> Self {
        let last_frame = replay.frames.last().copied();
        let Replay { meta, chart, speed, offset, .. } = replay;
        let result = meta.result.clone();
        // 本地谱关联：绑定优先，否则用磁带里记的路径
        let chart = match (&binding, &chart) {
            (Some(b), Some(ChartRef::Local(_))) if b.local_path.is_some() => {
                Some(ChartRef::Local(b.local_path.clone().unwrap()))
            }
            (None, Some(ChartRef::Local(p))) => Some(ChartRef::Local(p.clone())),
            (_, c) => c.clone(),
        };
        Self {
            path,
            binding,
            meta,
            result,
            chart,
            duration: last_frame.unwrap_or_default(),
            speed,
            offset,
            imported,
        }
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    }
    pub fn player(&self) -> &str {
        self.meta.player.as_deref().filter(|v: &&str| !v.is_empty()).unwrap_or("本机")
    }
    /// 关联谱面是否就绪（能播）。
    pub fn playable(&self) -> bool {
        match &self.chart {
            Some(ChartRef::Local(p)) => self.binding.as_ref().and_then(|b| b.local_path.clone()).unwrap_or_else(|| p.clone()) != "",
            _ => true,
        }
    }
    pub fn duration_text(&self) -> String {
        let s = self.duration.max(0.);
        format!("{}:{:02}", (s / 60.) as u32, (s % 60.) as u32)
    }
}

fn index_path() -> Result<PathBuf> {
    Ok(root()?.join("index.json"))
}

fn read_index() -> Result<Vec<Binding>> {
    let path = index_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&std::fs::read(path)?).context("读取回放索引失败")
}

fn write_index(bindings: &[Binding]) -> Result<()> {
    let path = index_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(bindings)?)?;
    std::fs::rename(&tmp, &path)?;
    REVISION.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// 列出全部回放（按录制时间倒序；读不了的文件跳过并记日志）。
pub fn all() -> Result<Vec<Item>> {
    let root = root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let bindings = read_index().unwrap_or_default();
    let mut items = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        if !matches!(path.extension().and_then(|s| s.to_str()), Some("ppr" | "phirar" | "phirarec")) {
            continue;
        }
        let file = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        match load_replay(&path) {
            Ok(replay) => {
                let Replay { meta, frames, chart, speed, offset, .. } = replay;
                let result = meta.result.clone();
                let binding = bindings.iter().find(|b| b.file == file).cloned();
                let imported = binding.as_ref().is_some_and(|b| b.imported_at > 0);
                // 本地谱关联：绑定优先，否则用磁带里记的路径
                let chart = match (&binding, &chart) {
                    (Some(b), Some(ChartRef::Local(_))) if b.local_path.is_some() => Some(ChartRef::Local(b.local_path.clone().unwrap())),
                    (None, Some(ChartRef::Local(p))) => Some(ChartRef::Local(p.clone())),
                    (_, c) => c.clone(),
                };
                items.push(Item {
                    path,
                    binding,
                    meta,
                    result,
                    chart,
                    duration: frames.last().copied().unwrap_or(0.),
                    speed,
                    offset,
                    imported,
                });
            }
            Err(err) => {
                tracing::warn!("[replay] 跳过无法解析的回放 {file}: {err:#}");
            }
        }
    }
    items.sort_by(|a, b| b.meta.recorded_at.unwrap_or(0).cmp(&a.meta.recorded_at.unwrap_or(0)));
    Ok(items)
}

/// 导入外部文件 → `import-<n>.ppr`，返回新条目。
pub fn import(src: &Path, player: Option<String>) -> Result<Item> {
    // 先解析，失败就不落库（别留垃圾文件）
    let replay = load_replay(src).context("导入失败：不是有效的回放文件")?;
    let root = root()?;
    std::fs::create_dir_all(&root)?;
    let mut n = read_index()?.len() + 1;
    let mut dst = root.join(format!("import-{n}.ppr"));
    while dst.exists() {
        n += 1;
        dst = root.join(format!("import-{n}.ppr"));
    }
    save_replay(&replay, &dst)?;
    let mut bindings = read_index()?;
    let binding = Binding {
        file: dst.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        imported_at: now_ms(),
        local_path: None,
        verified: true,
        player,
        player_id: None,
    };
    bindings.push(binding.clone());
    write_index(&bindings)?;
    let duration = replay.duration();
    let Item {
        meta, result, chart, speed, offset, ..
    } = Item::from_replay(dst, Some(binding), replay, true);
    Ok(Item {
        meta,
        result,
        chart,
        duration,
        speed,
        offset,
        imported: true,
        ..Default::default()
    })
}

/// 按文件名读一个回放（播放 / 导出 / 删除都用它）。
pub fn load_by_file_name(file: &str) -> Result<Replay> {
    let path = root()?.join(file);
    anyhow::ensure!(path.is_file(), "找不到回放文件 {file}");
    load_replay(&path)
}

/// 导出到用户选的位置。
pub fn export(item: &Item, dest: &Path) -> Result<()> {
    let replay = load_replay(&item.path)?;
    save_replay(&replay, dest)
}

/// 删除（自录制与导入都能删；导入的会清掉索引）。
pub fn delete(item: &Item) -> Result<()> {
    if item.path.exists() {
        std::fs::remove_file(&item.path)?;
    }
    if item.imported {
        let file = item.file_name();
        let mut bindings = read_index()?;
        bindings.retain(|b| b.file != file);
        write_index(&bindings)?;
    }
    Ok(())
}

/// 给回放关联本地谱面（导入的「缺谱面」流程）。
pub fn associate(item: &Item, local_path: Option<String>) -> Result<()> {
    let file = item.file_name();
    let mut bindings = read_index()?;
    if let Some(b) = bindings.iter_mut().find(|b| b.file == file) {
        b.local_path = local_path;
        b.verified = true;
    } else {
        bindings.push(Binding {
            file,
            imported_at: now_ms(),
            local_path,
            verified: true,
            player: None,
            player_id: None,
        });
    }
    write_index(&bindings)
}

/// 自录制回放的落点：`<data>/replays/<key>_<ms>.ppr`
pub fn recording_path(chart_key: &str, time_ms: i64) -> Result<PathBuf> {
    Ok(root()?.join(format!("{}_{}.ppr", sanitize(chart_key), time_ms)))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .take(64)
        .collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}
