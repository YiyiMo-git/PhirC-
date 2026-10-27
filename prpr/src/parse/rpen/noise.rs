//! RPEN 分层噪域（`noiseAreaLayers` + 新格式 `blockAreaList`）解析。
//!
//! 与旧版（从 `isSubtract0/1.png` 噪域线派生）的关键区别：
//! * 每个区域带 `layerId`，落到某一层；**每层独立统计普通/反转**，互不干扰。
//! * `isInvert` 是逐区域的，不再是全局「反转线」。
//! * `activeIntervals` 支持多段（由 [`crate::core::rpe_next_block::Area`] 完整保留）。
//!
//! 这些区域存到 `Chart::next_block_areas`，和旧的 `Chart::block_areas` **分开**，
//! 避免新旧两套互相污染。

use anyhow::{ensure, Context, Result};
use serde_json::Value;

use crate::core::rpe_next_block::{Area, AreaData, Tempo};

/// 解析 `noiseAreaLayers`，返回各层 id（保持声明顺序，下标即层号）。
///
/// 上游约束：1..=32 层、id 非空且不重复。
pub fn parse_layers(raw: &Value) -> Result<Vec<String>> {
    let Some(layers) = raw["noiseAreaLayers"].as_array() else {
        return Ok(Vec::new());
    };
    ensure!(!layers.is_empty() && layers.len() <= 32, "noise layer count must be 1..=32");
    let mut ids = Vec::with_capacity(layers.len());
    for layer in layers {
        let id = layer["id"].as_str().context("invalid noise layer ID")?.to_owned();
        ensure!(!id.is_empty(), "empty noise layer ID");
        ensure!(!ids.contains(&id), "duplicate noise layer ID: {id}");
        ids.push(id);
    }
    Ok(ids)
}

/// 解析新格式 `blockAreaList`（只取带 `bottomLeft` 的项），按 `layerId` 分配层。
///
/// `layer_ids` 为空时（谱面没声明 `noiseAreaLayers`）所有区域都落在第 0 层，
/// 且 `layerId` 不做校验 —— 这样「只用了新格式区域、没声明层」的谱面也能正常显示。
pub fn parse_next_areas(raw: &Value, bpm: &[(f64, f64)], layer_ids: &[String]) -> Result<Vec<Area>> {
    let Some(list) = raw["blockAreaList"].as_array() else {
        return Ok(Vec::new());
    };
    let tempo = Tempo::new(bpm.iter().copied())?;
    let mut out = Vec::new();
    for (index, area) in list.iter().enumerate() {
        if area.get("bottomLeft").is_none() {
            continue;
        }
        // 层分配：声明了层就按 `layerId` 找；找不到直接报错（上游也是 fail-fast）。
        let layer = if layer_ids.is_empty() {
            0
        } else {
            let id = area["layerId"].as_str().unwrap_or("default");
            layer_ids
                .iter()
                .position(|s| s == id)
                .with_context(|| format!("missing noise layer {id}"))?
        };
        let data: AreaData = serde_json::from_value(area.clone()).with_context(|| format!("blockAreaList[{index}]"))?;
        let mut parsed = Area::new(data, &tempo).with_context(|| format!("blockAreaList[{index}]"))?;
        parsed.layer = layer;
        out.push(parsed);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bpm() -> Vec<(f64, f64)> {
        vec![(0., 120.)] // 1 拍 = 0.5s
    }

    #[test]
    fn layers_are_validated() {
        let ids = parse_layers(&json!({
            "noiseAreaLayers": [{ "id": "default" }, { "id": "upper" }]
        }))
        .unwrap();
        assert_eq!(ids, vec!["default".to_string(), "upper".to_string()]);

        assert!(parse_layers(&json!({ "noiseAreaLayers": [] })).is_err(), "空层列表要报错");
        assert!(
            parse_layers(&json!({ "noiseAreaLayers": [{ "id": "a" }, { "id": "a" }] })).is_err(),
            "重复 id 要报错"
        );
        // 没有 noiseAreaLayers 时返回空（不是错误）
        assert!(parse_layers(&json!({})).unwrap().is_empty());
    }

    #[test]
    fn areas_get_layer_and_invert_from_json() {
        let ids = vec!["default".to_string(), "upper".to_string()];
        let areas = parse_next_areas(
            &json!({
                "blockAreaList": [
                    {
                        "bottomLeft": { "x": -675, "y": -450 },
                        "topRight": { "x": 675, "y": 450 },
                        "appearTime": [0, 0, 1],
                        "disappearTime": [20, 0, 1],
                        "activeIntervals": [{ "startTime": [0, 0, 1], "endTime": [20, 0, 1] }],
                        "layerId": "default",
                        "isInvert": false
                    },
                    {
                        "bottomLeft": { "x": -135, "y": -90 },
                        "topRight": { "x": 135, "y": 90 },
                        "appearTime": [0, 0, 1],
                        "disappearTime": [20, 0, 1],
                        "layerId": "upper",
                        "isInvert": true
                    }
                ]
            }),
            &bpm(),
            &ids,
        )
        .unwrap();
        assert_eq!(areas.len(), 2);
        assert_eq!(areas[0].layer, 0);
        assert_eq!(areas[1].layer, 1, "upper 是第 1 层");
        // 120BPM：20 拍 = 10s；0.5s（第 1 拍）时处于 active 区间
        let a = areas[0].sample(0.5, 1.5).expect("应该可见");
        let b = areas[1].sample(0.5, 1.5).expect("应该可见");
        assert_eq!(a.state, 2, "activeIntervals 内应为激活态");
        assert!(!a.zone.invert);
        assert!(b.zone.invert, "isInvert=true 应转成反转区");
        // 世界单位 → 谱面单位：x 半宽 675 → 1.0；y 半高 450 → 2/3（aspect=1.5 时正好是屏幕半高）
        assert!((a.zone.half.x - 1.).abs() < 1e-5, "实际 {}", a.zone.half.x);
        assert!((a.zone.half.y - 2. / 3.).abs() < 1e-5, "实际 {}", a.zone.half.y);
        // 10s 之后（第 20 拍 endTime，半开区间）已不可见
        assert!(areas[0].sample(10.0, 1.5).is_none());
    }

    #[test]
    fn unknown_layer_is_rejected() {
        let ids = vec!["default".to_string()];
        let err = parse_next_areas(
            &json!({
                "blockAreaList": [{ "bottomLeft": { "x": 0, "y": 0 }, "topRight": { "x": 1, "y": 1 }, "layerId": "nope" }]
            }),
            &bpm(),
            &ids,
        );
        assert!(err.is_err(), "引用了不存在的层应该报错");
    }

    #[test]
    fn legacy_areas_without_layers_all_go_to_layer_zero() {
        let areas = parse_next_areas(
            &json!({
                "blockAreaList": [{
                    "bottomLeft": { "x": 0, "y": 0 },
                    "topRight": { "x": 10, "y": 10 },
                    "appearTime": [0, 0, 1],
                    "disappearTime": [4, 0, 1]
                }]
            }),
            &bpm(),
            &[],
        )
        .unwrap();
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].layer, 0);
    }
}
