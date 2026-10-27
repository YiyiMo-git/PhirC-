//! `.phc`… 不，这是**回放**模块（`phira` 侧）。
//!
//! 格式与引擎侧实现在 `prpr::replay`（自包含：触摸磁带 + 判定事件 + 元数据）。
//! 这里负责**本地库管理**：索引、列出、导入、导出、删除、关联谱面，
//! 以及录制闭环（开始 / 结束 / 落盘）。
//!
//! 目录约定：`<data>/replays/`
//! * `*.ppr`                    回放文件（`PHIRAREC` + zstd(JSON)）
//! * `index.json`               导入绑定（`Binding` 数组，原子写）
//! 文件名即身份：自录制 `<谱面key>_<毫秒时间戳>.ppr`，导入 `import-<n>.ppr`。

pub mod library;

pub use prpr::replay::{
    finish_recording, is_recording, load as load_replay, save as save_replay, start_recording, ChartRef, Replay,
};

/// 给一个谱面算录制用的引用：在线谱用 id，本地谱用路径，都拿不到就不记。
pub fn chart_ref_of(id: Option<i32>, local_path: Option<&str>) -> Option<ChartRef> {
    if let Some(id) = id {
        return Some(ChartRef::Id(id));
    }
    local_path.map(|p| ChartRef::Local(p.to_owned()))
}

/// 谱面的稳定 key（用来归组 / 生成文件名），口径与 `history` 一致。
pub fn chart_key(id: Option<i32>, local_path: Option<&str>, name: &str, level: &str) -> String {
    if let Some(id) = id {
        format!("id:{id}")
    } else if let Some(p) = local_path {
        format!("local:{p}")
    } else {
        format!("name:{name}|{level}")
    }
}
