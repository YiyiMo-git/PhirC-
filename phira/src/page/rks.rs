//! 自定义 RKS（T7.4）：本地计算。
//!
//! ⚠️ 2026-10-23：**编辑界面已整个删掉**（用户要求「删掉rks界面」）。
//! 剩下的只是计算层：从曲库本地谱面算 RKS，喂给 B0 最佳谱面榜和曲库里的 RKS 显示。
//! 配置仍写在 `data/custom-rks.json`（已保存的设置继续生效，只是没有界面可以改了）。
//!
//! 移植自 PhiraiAd 的 `page/rks.rs`。计算逻辑（`custom_rks.rs`）原样保留，
//! 其中「AI 定数」这一支没有移植（`DifficultySource::Ai` 枚举值仍保留，
//! 避免旧配置文件解析失败）。
//!
//! 所有配置都写在 `data/custom-rks.json`，不碰用户存档里的任何既有字段。
use crate::{custom_rks::*, dir, get_data};
use anyhow::Result;
use std::{collections::HashMap, path::PathBuf};

pub struct RksPanel {
    settings: Settings,
    /// 当前曲库里的本地谱面（含已解析出的准确率）。
    locals: Vec<LocalInput>,
    summary: Summary,
}

impl RksPanel {
    pub fn new() -> Result<Self> {
        let settings = Settings::load(&PathBuf::from(dir::root()?).join("custom-rks.json"))?;
        Ok(Self {
            settings,
            locals: Vec::new(),
            summary: Summary::default(),
        })
    }


    /// 徽章 / 最佳谱面榜用：当前计入统计的成绩条数。
    pub fn best_board_count(&self) -> usize {
        self.summary.best_used + self.summary.ap_used
    }
    /// 徽章 / 最佳谱面榜用：排好序的条目 + 总 RKS。
    pub fn best_board(&self) -> (Vec<RankedEntry>, f64) {
        (ranked_entries(&self.settings, &self.locals), self.summary.rks)
    }
    /// 面板显示出来的 RKS（两位小数）。
    #[allow(dead_code)]
    pub fn rks(&self) -> f64 {
        self.summary.rks
    }

    /// 从曲库状态重建本地谱面输入。只在需要时调用（进编辑器 / 曲库变动）。
    pub fn refresh(&mut self, charts_local: &[crate::page::ChartItem]) {
        let data = get_data();
        let records: HashMap<_, _> = data.charts.iter().map(|c| (c.local_path.as_str(), c.record.as_ref())).collect();
        self.locals = charts_local
            .iter()
            .filter_map(|c| {
                let path = c.local_path.as_ref()?;
                // 普通本地记录 与 `local_records` 里取更高的那个
                let ordinary = records.get(path.as_str()).copied().flatten().map(|r| r.accuracy as f64 * 100.);
                let fallback = data.local_records.get(path).and_then(|r| r.as_ref()).map(|r| r.accuracy as f64 * 100.);
                Some(LocalInput {
                    path: path.clone(),
                    name: c.info.name.clone(),
                    level: c.info.level.clone(),
                    difficulty: c.info.difficulty as f64,
                    // 不接入 AI：永远没有 AI 定数
                    ai_difficulty: None,
                    accuracy: highest_record_accuracy(ordinary, fallback),
                })
            })
            .collect();
        self.recalculate();
    }

    fn recalculate(&mut self) {
        let (_, summary) = calculate(&self.settings, &self.locals);
        self.summary = summary;
    }
}
