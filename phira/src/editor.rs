//! 制谱器（`ChartEditor.html`）的后端：读 / 写 / 新建谱面。
//!
//! 设计取舍：
//! * **支持官方 JSON 与 RPE 文本（PEC）两种格式**。RPE 文本（`bp`/`n1`/`cm`/`cv`/`cr`…）
//!   直接交给引擎的 `prpr::parse::parse_pec` 解析成内部 `Chart`，再转成编辑器的统一
//!   数据模型（见 `chart_to_json`）；保存时按来源格式写回（`json_to_pec`）。
//!   **不做「转成官方 JSON」的有损降级** —— RPE 的缓动（tween 0..29）官方 JSON
//!   表达不了，转了就是丢内容。
//! * RPE 的新版 JSON 导出格式（`META`/`eventLayers`）**只读**：引擎能解析但结构比
//!   编辑器能安全重写的复杂，保存会退回 PEC 文本，所以打开时就在界面上标注。
//! * 编辑器数据模型单位：**拍**（beat，offset 除外是秒）。事件值：alpha 0..255、
//!   rotate 度、move 归一化 -1..1（UI 显示时换算成 RPE 像素系）、positionX -1..1（0 = 线心，= 引擎 world x）。
//!   type 用官方语义：1=Tap 2=Drag 3=Hold 4=Flick
//!   （**PEC 命令是 1=Tap 2=Hold 3=Flick 4=Drag，保存时转换**）。

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;

use prpr::core::{AnimFloat, AnimVector, BpmList, Chart, Keyframe, NoteKind, StaticTween};
use prpr::parse::{parse_pec, RPE_TWEEN_MAP};

/// 谱面来源格式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChartFormat {
    /// 官方 JSON（`formatVersion` + `judgeLineList`）
    Pgr,
    /// RPE 文本（`bp` / `n1`..`n4` / `cp` / `cm` / `cv` / `cd` / `ca` / `cr` / `cf` / `#` / `&`）
    Pec,
    /// RPE 新版 JSON 导出（只读，保存退回 PEC）
    RpeJson,
}



fn info_path(dir: &str) -> std::path::PathBuf {
    Path::new(dir).join("info.yml")
}

/// 谱面目录归一化：曲库页传进来的是**相对 `data/charts/` 的路径**（`custom/<uuid>`、
/// `download/4083`，见 data.json 的 `local_path`），编辑器内部一律按绝对目录读写。
/// 不归一化的话 `Path::new("custom/x").join("info.yml")` 会相对进程 CWD 找 —— 必然打不开。
pub fn resolve_dir(dir: &str) -> String {
    if dir.is_empty() || Path::new(dir).is_absolute() {
        return dir.to_owned();
    }
    match crate::dir::charts() {
        Ok(root) => Path::new(&root).join(dir).to_string_lossy().into_owned(),
        Err(_) => dir.to_owned(),
    }
}

fn read_info(dir: &str) -> Result<Value> {
    let text = std::fs::read_to_string(info_path(dir)).with_context(|| format!("读取 {} 失败", info_path(dir).display()))?;
    serde_yaml::from_str::<Value>(&text).or_else(|_| serde_json::from_str::<Value>(&text)).context("info.yml 解析失败")
}

fn write_info(dir: &str, info: &Value) -> Result<()> {
    let text = serde_yaml::to_string(info).context("info.yml 序列化失败")?;
    std::fs::write(info_path(dir), text).with_context(|| format!("写入 {} 失败", info_path(dir).display()))
}

/// 按内容猜格式：`{` 开头是 JSON，否则是 RPE 文本。
fn detect_format(raw: &str) -> ChartFormat {
    let t = raw.trim_start();
    if !t.starts_with('{') {
        return ChartFormat::Pec;
    }
    if t.contains("\"RPEVersion\"") || t.contains("\"META\"") {
        ChartFormat::RpeJson
    } else {
        ChartFormat::Pgr
    }
}

/// 扫描 PEC 文本里的 bp 段，返回 `(beat, bpm)` 对（`BpmList::new` 的参数顺序）。
///
/// ⚠️ PEC 的 bp 命令是 **`bp <拍> <bpm>`**（拍在前）—— 官方谱面 `bp 0.000 234.000`
/// 就是「第 0 拍、234 BPM」。写反了会让 bpm 变成 0，`60. / bpm` = inf，
/// 时间换算全线 NaN（`Anim` 里 0 * inf）。
/// （`Chart.bpm_list` 的 elements 是私有的，只能自己扫；保存时原样写回。）
fn scan_pec_bpm(source: &str) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for line in source.lines().skip(1) {
        let mut it = line.split_whitespace();
        if it.next() == Some("bp") {
            if let (Some(beat), Some(bpm)) = (it.next(), it.next()) {
                if let (Ok(beat), Ok(bpm)) = (beat.parse::<f64>(), bpm.parse::<f64>()) {
                    out.push((beat, bpm));
                }
            }
        }
    }
    out
}

/// 给 PEC 文本补齐全套默认事件。
///
/// 引擎的 `parse_pec` 对**空事件轨**会直接 panic（`AnimFloat::new(vec![])` 的
/// assert：alpha/rotate/move/speed 任一为空都炸），而 TPE 允许导出「只有音符、
/// 没有事件」的谱面。这里按线补齐默认值（TPE 导出时也是这么干的）：
/// `ca <id> 0 255` / `cd <id> 0 0` / `cp <id> 0 1024 700` / `cv <id> 0 5.85`。
fn ensure_pec_defaults(source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    // 收集出现过的线 id（n* / c* 命令的第二个 token）
    let mut ids: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for l in lines.iter().skip(1) {
        let mut it = l.split_whitespace();
        let cmd = it.next().unwrap_or("");
        let is_note = cmd.len() == 2 && cmd.starts_with('n') && "1234".contains(&cmd[1..]);
        let is_ctrl = cmd.len() == 2 && cmd.starts_with('c');
        if (is_note || is_ctrl) && cmd != "cv" || cmd == "cv" {
            if let Some(id) = it.next() {
                if seen.insert(id.to_owned()) {
                    ids.push(id.to_owned());
                }
            }
        }
    }
    if ids.is_empty() {
        return source.to_owned();
    }
    let has = |prefix: &str| lines.iter().skip(1).any(|l| l.split_whitespace().next() == Some(prefix));
    let mut fillers: Vec<String> = Vec::new();
    for id in &ids {
        if !has("ca") && !has("cf") {
            fillers.push(format!("ca {id} 0 255"));
        }
        if !has("cd") && !has("cr") {
            fillers.push(format!("cd {id} 0 0"));
        }
        if !has("cp") && !has("cm") {
            fillers.push(format!("cp {id} 0 1024 700"));
        }
        if !has("cv") {
            fillers.push(format!("cv {id} 0 5.85"));
        }
    }
    if fillers.is_empty() {
        return source.to_owned();
    }
    // 插在最后一个 bp 之后（bp 必须在所有 n/c 之前）
    let mut insert_at = 1.min(lines.len());
    for (i, l) in lines.iter().enumerate().skip(1) {
        if l.split_whitespace().next() == Some("bp") {
            insert_at = i + 1;
        }
    }
    let mut out: Vec<&str> = lines[..insert_at].to_vec();
    out.extend(fillers.iter().map(|s| s.as_str()));
    out.extend(lines[insert_at..].iter());
    let mut text = out.join("\n");
    if source.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// 从 Keyframe 的 tween 对象反查 TweenId（只有 `StaticTween` 带 id）。
fn tween_id(kf: &Keyframe<f32>) -> u8 {
    kf.tween.as_any().downcast_ref::<StaticTween>().map(|t| t.0).unwrap_or(0)
}

/// TweenId → RPE 的 tween 编号（`RPE_TWEEN_MAP` 的索引；linear=2 命中 0）。
fn rpe_tween_index(id: u8) -> u32 {
    RPE_TWEEN_MAP.iter().position(|&t| t == id).unwrap_or(0) as u32
}

/// 用 bp 段构造 BpmList（空谱面给个默认值，否则 `beat()` 会越界）。
fn bpm_list_of(bp: &[(f64, f64)]) -> BpmList {
    if bp.is_empty() {
        BpmList::new(vec![(0., 120.)])
    } else {
        BpmList::new(bp.to_vec())
    }
}

// ============================================================
// Chart → 编辑器数据模型
// ============================================================

/// PEC 判定线默认缩放（`pec.rs` 的 `AnimFloat::fixed(3.91 / 6.)`）。
const PEC_DEFAULT_SCALE: f32 = 3.91 / 6.;

/// AnimFloat → 「点值」事件列表（官方 JSON 风格：每个 kf 是点，段内常量）。
fn anim_points(anim: &AnimFloat, r: &mut BpmList) -> Vec<Value> {
    let kfs = &anim.keyframes;
    let mut out = Vec::new();
    for (i, kf) in kfs.iter().enumerate() {
        let end_beat = if i + 1 < kfs.len() { r.beat(kfs[i + 1].time) } else { 1e9 };
        out.push(json!({
            "startTime": r.beat(kf.time),
            "endTime": end_beat,
            "start": kf.value,
            "end": kf.value,
            "tween": 0,
        }));
    }
    out
}

/// AnimFloat → 「插值」事件列表（PEC 风格：tween≠0 的 kf 与下一 kf 成对）。
fn anim_tweens(anim: &AnimFloat, r: &mut BpmList) -> Vec<Value> {
    let kfs = &anim.keyframes;
    let mut out = Vec::new();
    let mut i = 0;
    while i < kfs.len() {
        let kf = &kfs[i];
        if tween_id(kf) != 0 && i + 1 < kfs.len() {
            let prev = if i > 0 { kfs[i - 1].value } else { kf.value };
            let nxt = &kfs[i + 1];
            out.push(json!({
                "startTime": r.beat(kf.time),
                "endTime": r.beat(nxt.time),
                "start": prev,
                "end": nxt.value,
                "tween": rpe_tween_index(tween_id(kf)),
            }));
            i += 2;
        } else {
            let end_beat = if i + 1 < kfs.len() { r.beat(kfs[i + 1].time) } else { 1e9 };
            out.push(json!({
                "startTime": r.beat(kf.time),
                "endTime": end_beat,
                "start": kf.value,
                "end": kf.value,
                "tween": 0,
            }));
            i += 1;
        }
    }
    out
}

/// PEC 的 move：x/y 两轴严格同步（`cp`/`cm` 总是同时 push 两轴），按对反推。
fn move_tweens(av: &AnimVector, r: &mut BpmList) -> Vec<Value> {
    let (xs, ys) = (&av.0.keyframes, &av.1.keyframes);
    let n = xs.len().min(ys.len());
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let kf = &xs[i];
        if tween_id(kf) != 0 && i + 1 < n {
            let px = if i > 0 { xs[i - 1].value } else { kf.value };
            let py = if i > 0 { ys[i - 1].value } else { ys[i].value };
            let (nx, ny) = (&xs[i + 1], &ys[i + 1]);
            out.push(json!({
                "startTime": r.beat(kf.time),
                "endTime": r.beat(nx.time),
                "start": px, "start2": py,
                "end": nx.value, "end2": ny.value,
                "tween": rpe_tween_index(tween_id(kf)),
            }));
            i += 2;
        } else {
            let end_beat = if i + 1 < n { r.beat(xs[i + 1].time) } else { 1e9 };
            out.push(json!({
                "startTime": r.beat(kf.time),
                "endTime": end_beat,
                "start": kf.value, "start2": ys[i].value,
                "end": kf.value, "end2": ys[i].value,
                "tween": 0,
            }));
            i += 1;
        }
    }
    out
}

/// AnimFloat → 分段常量点列（合并相邻同值 kf，时间单位：秒）。
fn anim_const_segments(anim: &AnimFloat) -> Vec<(f64, f32)> {
    let mut segs: Vec<(f64, f32)> = Vec::new();
    for kf in anim.keyframes.iter() {
        if segs.last().map(|it| it.1 == kf.value).unwrap_or(false) {
            continue;
        }
        segs.push((kf.time, kf.value));
    }
    segs
}

fn val_at(segs: &[(f64, f32)], t: f64) -> f32 {
    let mut v = segs.first().map(|it| it.1).unwrap_or(0.);
    for (st, val) in segs {
        if *st <= t + 1e-9 {
            v = *val;
        } else {
            break;
        }
    }
    v
}

/// 官方 JSON 的 move：x/y 两轴**独立**点值（值变化才 push），按时间并集切常量段。
fn move_points(av: &AnimVector, r: &mut BpmList) -> Vec<Value> {
    let xs = anim_const_segments(&av.0);
    let ys = anim_const_segments(&av.1);
    let mut times: Vec<f64> = xs.iter().chain(ys.iter()).map(|it| it.0).collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    times.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let mut out = Vec::new();
    for w in times.windows(2) {
        let (t0, t1) = (w[0], w[1]);
        let (x0, y0) = (val_at(&xs, t0), val_at(&ys, t0));
        let (x1, y1) = (val_at(&xs, t1), val_at(&ys, t1));
        if (x0 - x1).abs() < 1e-6 && (y0 - y1).abs() < 1e-6 {
            continue;
        }
        out.push(json!({
            "startTime": r.beat(t0), "endTime": r.beat(t1),
            "start": x0, "start2": y0, "end": x1, "end2": y1, "tween": 0,
        }));
    }
    if let Some(&last) = times.last() {
        let (x, y) = (val_at(&xs, last), val_at(&ys, last));
        out.push(json!({
            "startTime": r.beat(last), "endTime": 1e9,
            "start": x, "start2": y, "end": x, "end2": y, "tween": 0,
        }));
    }
    out
}

/// speedEvents：Chart 只存积分后的 `height`，反推 = 对 height 求导。
/// PEC 语义直接可用；官方 JSON 的 height 除了 `HEIGHT_RATIO`，要乘回去。
fn speed_events(line: &prpr::core::JudgeLine, r: &mut BpmList, format: ChartFormat) -> Vec<Value> {
    let kfs = &line.height.keyframes;
    let mut out = Vec::new();
    for w in kfs.windows(2) {
        let dt = w[1].time - w[0].time;
        if dt <= 1e-9 {
            continue;
        }
        let mut speed = (w[1].value - w[0].value) / dt as f32;
        if format == ChartFormat::Pgr {
            speed *= prpr::core::HEIGHT_RATIO as f32;
        }
        out.push(json!({
            "startTime": r.beat(w[0].time),
            "endTime": r.beat(w[1].time),
            "value": speed,
        }));
    }
    out
}

/// 单条音符 → 编辑器 JSON（type 用官方语义）。
fn note_json(note: &prpr::core::Note, r: &mut BpmList) -> Value {
    let (ty, hold) = match note.kind {
        NoteKind::Click => (1, 0.0),
        NoteKind::Drag => (2, 0.0),
        NoteKind::Hold { end_time, .. } => (3, r.beat(end_time) - r.beat(note.time)),
        NoteKind::Flick => (4, 0.0),
    };
    let x = note.object.translation.0.keyframes.first().map(|k| k.value).unwrap_or(0.5);
    let scale = note.object.scale.0.keyframes.first().map(|k| k.value).unwrap_or(PEC_DEFAULT_SCALE);
    json!({
        "type": ty,
        "time": r.beat(note.time),
        "positionX": x,
        "holdTime": hold,
        "speed": note.speed,
        "floorPosition": 0.0,
        "above": note.above,
        "fake": note.fake,
        "size": if (scale - PEC_DEFAULT_SCALE).abs() > 1e-4 { scale } else { 1.0 },
    })
}

/// 一条判定线 → 编辑器 JSON。
fn line_json(line: &prpr::core::JudgeLine, r: &mut BpmList, format: ChartFormat, bpm_hint: f64) -> Value {
    let (alpha_ev, rotate_ev, move_ev);
    match format {
        ChartFormat::Pgr => {
            // 官方 JSON：alpha 0..1 → 0..255；rotate 直传；move 0..1 → -1..1
            let scale255 = |evs: Vec<Value>| -> Vec<Value> {
                evs.into_iter()
                    .map(|e| {
                        json!({
                            "startTime": e["startTime"], "endTime": e["endTime"],
                            "start": e["start"].as_f64().unwrap_or(0.) * 255.,
                            "end": e["end"].as_f64().unwrap_or(0.) * 255.,
                            "tween": 0,
                        })
                    })
                    .collect()
            };
            let norm = |evs: Vec<Value>| -> Vec<Value> {
                evs.into_iter()
                    .map(|e| {
                        json!({
                            "startTime": e["startTime"], "endTime": e["endTime"],
                            "start": e["start"].as_f64().unwrap_or(0.) * 2. - 1.,
                            "start2": e["start2"].as_f64().unwrap_or(0.) * 2. - 1.,
                            "end": e["end"].as_f64().unwrap_or(0.) * 2. - 1.,
                            "end2": e["end2"].as_f64().unwrap_or(0.) * 2. - 1.,
                            "tween": 0,
                        })
                    })
                    .collect()
            };
            alpha_ev = scale255(anim_points(&line.object.alpha, r));
            rotate_ev = anim_points(&line.object.rotation, r);
            move_ev = norm(move_points(&line.object.translation, r));
        }
        _ => {
            // PEC：alpha 已是 0..1（/255 过）→ ×255；rotate 取负回角度；move 归一化已是 -1..1
            let scale255 = |evs: Vec<Value>| -> Vec<Value> {
                evs.into_iter()
                    .map(|e| {
                        json!({
                            "startTime": e["startTime"], "endTime": e["endTime"],
                            "start": e["start"].as_f64().unwrap_or(0.) * 255.,
                            "end": e["end"].as_f64().unwrap_or(0.) * 255.,
                            "tween": e["tween"],
                        })
                    })
                    .collect()
            };
            let neg = |evs: Vec<Value>| -> Vec<Value> {
                evs.into_iter()
                    .map(|e| {
                        json!({
                            "startTime": e["startTime"], "endTime": e["endTime"],
                            "start": -e["start"].as_f64().unwrap_or(0.),
                            "end": -e["end"].as_f64().unwrap_or(0.),
                            "tween": e["tween"],
                        })
                    })
                    .collect()
            };
            alpha_ev = scale255(anim_tweens(&line.object.alpha, r));
            rotate_ev = neg(anim_tweens(&line.object.rotation, r));
            move_ev = move_tweens(&line.object.translation, r);
        }
    }
    let mut notes_above = Vec::new();
    let mut notes_below = Vec::new();
    for n in &line.notes {
        if n.above {
            notes_above.push(note_json(n, r));
        } else {
            notes_below.push(note_json(n, r));
        }
    }
    json!({
        "bpm": bpm_hint,
        "judgeLineDisappearEvents": alpha_ev,
        "judgeLineRotateEvents": rotate_ev,
        "judgeLineMoveEvents": move_ev,
        "speedEvents": speed_events(line, r, format),
        "notesAbove": notes_above,
        "notesBelow": notes_below,
    })
}

/// Chart → 编辑器数据模型。
fn chart_to_json(chart: &Chart, format: ChartFormat, bp: &[(f64, f64)]) -> Value {
    let mut r = bpm_list_of(bp);
    let bpm_hint = bp.first().map(|it| it.1).unwrap_or(120.);
    let lines: Vec<Value> = chart.lines.iter().map(|l| line_json(l, &mut r, format, bpm_hint)).collect();
    json!({
        "formatVersion": 3,
        "offset": chart.offset,
        "bpmList": if bp.is_empty() { Value::Null } else { json!(bp) },
        "judgeLineList": lines,
    })
}

// ============================================================
// 编辑器数据模型 → PEC 文本
// ============================================================

/// 数字格式化：拍/坐标统一最多 4 位小数，去掉多余的 0。
fn num(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_owned();
    }
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" { "0".to_owned() } else { s.to_owned() }
}

fn ev_num(e: &Value, key: &str) -> f64 {
    e.get(key).and_then(|v| v.as_f64()).unwrap_or(0.)
}

fn ev_u32(e: &Value, key: &str) -> u32 {
    e.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as u32
}

fn events_of(line: &Value, key: &str) -> Vec<Value> {
    line.get(key).and_then(|v| v.as_array()).cloned().unwrap_or_default()
}

fn notes_of(line: &Value, key: &str) -> Vec<Value> {
    line.get(key).and_then(|v| v.as_array()).cloned().unwrap_or_default()
}

/// 编辑器数据模型 → PEC 文本（`bp`/`n*`/`cv`/`cp`/`cm`/`cd`/`ca`/`cr`/`cf`/`#`/`&`）。
///
/// 坐标反算（对齐 `pec.rs` 的解析）：
/// * move：编辑器 -1..1 → `cp`/`cm` 像素 x=(v+1)*1024（0..2048）、y=(v+1)*700（0..1400）
/// * alpha：编辑器 0..255 直写（`ca`/`cf`）
/// * rotate：编辑器角度 → PEC 取负（`cd`/`cr`）
/// * note x：positionX -1..1（0 中心）→ ×1024（PEC x 同为 0 中心）
/// * speed：`cv` 值 = speed × 5.85
/// * type：编辑器官方语义 → PEC 命令 1→1、2→4、3→2、4→3
pub fn json_to_pec(chart: &Value) -> Result<String> {
    let offset = chart.get("offset").and_then(|v| v.as_f64()).unwrap_or(0.);
    let mut out = String::new();
    // 第一行是 offset（毫秒）；引擎解析时 `- 0.15`
    out.push_str(&format!("{}\n", ((offset + 0.15) * 1000.).round() as i64));

    // bp 段（必须在任何 n/c 命令之前）
    if let Some(list) = chart.get("bpmList").and_then(|v| v.as_array()) {
        for item in list {
            if let Some(pair) = item.as_array() {
                if pair.len() >= 2 {
                    // PEC 的 bp 是「拍 在前、bpm 在后」
                    out.push_str(&format!("bp {} {}\n", num(pair[0].as_f64().unwrap_or(0.)), num(pair[1].as_f64().unwrap_or(120.))));
                }
            }
        }
    }

    let lines = chart.get("judgeLineList").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for (id, line) in lines.iter().enumerate() {
        // (start_beat, 命令文本)，同束按时间排序后输出
        let mut cmds: Vec<(f64, String)> = Vec::new();

        // PEC 事件语义：`ca`/`cd`/`cp` 是单点（值保持到下一事件），`cf`/`cr`/`cm` 是插值
        // —— **只带终点值**，起点继承前一事件（引擎 `parse_events` 就是这么重建的）。
        // 所以：常量段（start==end，含常驻 1e9）→ 单点命令；插值段 → 插值命令。
        // 另外插值事件前必须先有常量点，否则引擎 bail；这里统一兜底补齐。
        let mut alpha_first = true;
        for e in events_of(line, "judgeLineDisappearEvents") {
            let (st, en, s, e_) = (ev_num(&e, "startTime"), ev_num(&e, "endTime"), ev_num(&e, "start"), ev_num(&e, "end"));
            if en > st + 1e-6 && alpha_first {
                // 引擎要求插值前有常量点：用这个插值的 start 值补一条 ca
                cmds.push((st, format!("ca {id} {} {}", num(st), num(s))));
            }
            alpha_first = false;
            if en > st + 1e-6 {
                // cf 的 tween 在 PEC 里硬编码 linear，忽略编辑器里的值
                cmds.push((st, format!("cf {id} {} {} {}", num(st), num(en), num(e_))));
            } else {
                cmds.push((st, format!("ca {id} {} {}", num(st), num(s))));
            }
        }
        let mut rotate_first = true;
        for e in events_of(line, "judgeLineRotateEvents") {
            let (st, en, s, e_) = (ev_num(&e, "startTime"), ev_num(&e, "endTime"), ev_num(&e, "start"), ev_num(&e, "end"));
            if en > st + 1e-6 && rotate_first {
                cmds.push((st, format!("cd {id} {} {}", num(st), num(s))));
            }
            rotate_first = false;
            if en > st + 1e-6 {
                // 注意：pec 解析时会对角度取负存引擎（`-it.take_f32()`），
                // 而 editorJSON 存的已经是「用户视角角度」，所以这里直写、不再取负。
                cmds.push((st, format!("cr {id} {} {} {} {}", num(st), num(en), num(e_), ev_u32(&e, "tween"))));
            } else {
                cmds.push((st, format!("cd {id} {} {}", num(st), num(s))));
            }
        }
        let mut move_first = true;
        for e in events_of(line, "judgeLineMoveEvents") {
            let st = ev_num(&e, "startTime");
            let en = ev_num(&e, "endTime");
            if en > st + 1e-6 && move_first {
                let (x0, y0) = ((ev_num(&e, "start") + 1.) * 1024., (ev_num(&e, "start2") + 1.) * 700.);
                cmds.push((st, format!("cp {id} {} {} {}", num(st), num(x0), num(y0))));
            }
            move_first = false;
            if en > st + 1e-6 {
                let (x1, y1) = ((ev_num(&e, "end") + 1.) * 1024., (ev_num(&e, "end2") + 1.) * 700.);
                cmds.push((st, format!("cm {id} {} {} {} {} {}", num(st), num(en), num(x1), num(y1), ev_u32(&e, "tween"))));
            } else {
                let (x0, y0) = ((ev_num(&e, "start") + 1.) * 1024., (ev_num(&e, "start2") + 1.) * 700.);
                cmds.push((st, format!("cp {id} {} {} {}", num(st), num(x0), num(y0))));
            }
        }
        for e in events_of(line, "speedEvents") {
            // cv 的第三参是「速度 × 5.85」
            cmds.push((ev_num(&e, "startTime"), format!("cv {id} {} {}", num(ev_num(&e, "startTime")), num(ev_num(&e, "value") * 5.85))));
        }

        for (side, above) in [("notesAbove", 1u8), ("notesBelow", 0u8)] {
            for n in notes_of(line, side) {
                let ty = n.get("type").and_then(|v| v.as_u64()).unwrap_or(1);
                let cmd = match ty {
                    1 => '1',
                    2 => '4', // Drag
                    3 => '2', // Hold
                    4 => '3', // Flick
                    _ => '1',
                };
                let time = n.get("time").and_then(|v| v.as_f64()).unwrap_or(0.);
                let x = n.get("positionX").and_then(|v| v.as_f64()).unwrap_or(0.0) * 1024.;
                let fake = if n.get("fake").and_then(|v| v.as_bool()).unwrap_or(false) { 1u8 } else { 0u8 };
                // n2（Hold）的 end_time 在 x 之前：`n2 <线> <起拍> <止拍> <x> <上/下> <假>`；
                // 其余类型没有止拍：`n1/n3/n4 <线> <拍> <x> <上/下> <假>`
                let mut s = if ty == 3 {
                    let hold = n.get("holdTime").and_then(|v| v.as_f64()).unwrap_or(0.);
                    format!("n{cmd} {id} {} {} {} {above} {fake}", num(time), num(time + hold), num(x))
                } else {
                    format!("n{cmd} {id} {} {} {above} {fake}", num(time), num(x))
                };
                let speed = n.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.);
                if (speed - 1.).abs() > 1e-4 {
                    s += &format!(" # {}", num(speed));
                }
                let size = n.get("size").and_then(|v| v.as_f64()).unwrap_or(1.);
                if (size - 1.).abs() > 1e-4 {
                    s += &format!(" & {}", num(size));
                }
                cmds.push((time, s));
            }
        }

        cmds.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        // 引擎对**空事件轨**会直接 panic（`AnimFloat::new(vec![])` 的 assert），
        // 所以每条线至少给四条轨各写一条默认常量点（TPE 导出时也是这么干的）。
        let miss_alpha = !cmds.iter().any(|(_, c)| c.starts_with("ca ") || c.starts_with("cf "));
        let miss_rotate = !cmds.iter().any(|(_, c)| c.starts_with("cd ") || c.starts_with("cr "));
        let miss_move = !cmds.iter().any(|(_, c)| c.starts_with("cp ") || c.starts_with("cm "));
        let miss_speed = !cmds.iter().any(|(_, c)| c.starts_with("cv "));
        if miss_alpha {
            cmds.push((0., format!("ca {id} 0 255")));
        }
        if miss_rotate {
            cmds.push((0., format!("cd {id} 0 0")));
        }
        if miss_move {
            cmds.push((0., format!("cp {id} 0 1024 700")));
        }
        if miss_speed {
            cmds.push((0., format!("cv {id} 0 5.85")));
        }
        cmds.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, c) in cmds {
            out.push_str(&c);
            out.push('\n');
        }
    }
    Ok(out)
}

// ============================================================
// 入口：new / load / save / info
// ============================================================

/// 空谱面模板：一条判定线、常驻中央、无音符。
fn empty_chart() -> Value {
    json!({
        "formatVersion": 3,
        "offset": 0.0,
        "bpmList": null,
        "judgeLineList": [
            {
                "bpm": 120.0,
                "judgeLineDisappearEvents": [{ "startTime": 0.0, "endTime": 1e9, "start": 255.0, "end": 255.0, "tween": 0 }],
                "judgeLineRotateEvents":     [{ "startTime": 0.0, "endTime": 1e9, "start": 0.0, "end": 0.0, "tween": 0 }],
                "judgeLineMoveEvents":       [{ "startTime": 0.0, "endTime": 1e9, "start": 0.0, "start2": 0.0, "end": 0.0, "end2": 0.0, "tween": 0 }],
                "speedEvents":               [{ "startTime": 0.0, "endTime": 1e9, "value": 1.0 }],
                "notesAbove": [],
                "notesBelow": []
            }
        ]
    })
}

/// 新建谱面：`<data>/charts/custom/<uuid>/`，返回目录路径。
pub fn new_chart(name: &str) -> Result<String> {
    let dir = crate::dir::charts()?.to_string();
    let dir = std::path::Path::new(&dir).join("custom").join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).with_context(|| format!("创建 {} 失败", dir.display()))?;
    let info = json!({
        "name": if name.is_empty() { "未命名谱面" } else { name },
        "difficulty": 0.0,
        "level": "EZ Lv.0",
        "charter": "",
        "composer": "",
        "illustrator": "",
        "chart": "chart.json",
        "format": 3,
        "music": "music.mp3",
        "illustration": "illustration.png",
        "unlockVideo": null,
        "previewStart": 0.0,
        "previewEnd": null,
        "aspectRatio": 1.7777778,
        "backgroundDim": 0.6,
        "lineLength": 6.0,
        "offset": 0.0,
        "tip": null,
        "tags": [],
        "intro": "",
        "holdPartialCover": false,
        "noteUniformScale": false,
        "forceAspectRatio": false
    });
    write_info(dir.to_str().unwrap(), &info)?;
    std::fs::write(dir.join("chart.json"), serde_json::to_string_pretty(&empty_chart())?)?;
    Ok(dir.to_str().unwrap().to_owned())
}

/// 打开谱面：`{ ok, info, chart, chartFile, format }`。
/// `format` = `pgr`（官方 JSON）/ `pec`（RPE 文本）/ `rpe`（RPE JSON，只读）。
pub fn load_chart(dir: &str) -> Result<Value> {
    let dir = &resolve_dir(dir);
    if !Path::new(dir).is_dir() {
        bail!("目录不存在：{dir}");
    }
    let info = read_info(dir)?;
    let chart_file = info.get("chart").and_then(|v| v.as_str()).unwrap_or("chart.json").to_owned();
    let raw = std::fs::read_to_string(Path::new(dir).join(&chart_file))
        .with_context(|| format!("读取谱面文件 {chart_file} 失败"))?;
    let format = detect_format(&raw);
    match format {
        ChartFormat::Pec => {
            let bp = scan_pec_bpm(&raw);
            // 引擎对「缺事件轨」的谱面会直接 panic；TPE 允许导出这种谱面，先补齐再解析。
            let patched = ensure_pec_defaults(&raw);
            let chart = parse_pec(&patched, prpr::core::ChartExtra::default())
                .with_context(|| "RPE 谱面解析失败（可能是语法不兼容的命令）")?;
            let editor = chart_to_json(&chart, ChartFormat::Pec, &bp);
            Ok(json!({
                "ok": true, "info": info, "chart": editor,
                "chartFile": chart_file, "format": "pec",
            }))
        }
        ChartFormat::Pgr => {
            // 官方 JSON：time 本来就是拍，保留原结构；补上编辑器需要的 tween 字段
            let mut chart: Value = serde_json::from_str(&raw).context("谱面 JSON 解析失败")?;
            normalize_pgr(&mut chart);
            Ok(json!({
                "ok": true, "info": info, "chart": chart,
                "chartFile": chart_file, "format": "pgr",
            }))
        }
        ChartFormat::RpeJson => {
            // RPE JSON 导出：只读。保存时会退回 PEC 文本（界面上标注）。
            let chart: Value = serde_json::from_str(&raw).context("RPE JSON 解析失败")?;
            Ok(json!({
                "ok": true, "info": info, "chart": chart,
                "chartFile": chart_file, "format": "rpe", "readonly": true,
            }))
        }
    }
}

/// 官方 JSON → 编辑器统一 schema：
/// * move 0..1 → -1..1；alpha 0..1 → 0..255；事件补 `tween: 0`
/// * note 补 `above`/`fake`/`size`（官方 JSON 没有这些字段）
fn normalize_pgr(chart: &mut Value) {
    let scale255 = |e: &mut Value| {
        for k in ["start", "end"] {
            if let Some(v) = e.get(k).and_then(|v| v.as_f64()) {
                e[k] = json!(v * 255.);
            }
        }
    };
    let norm = |e: &mut Value| {
        for k in ["start", "start2", "end", "end2"] {
            if let Some(v) = e.get(k).and_then(|v| v.as_f64()) {
                e[k] = json!(v * 2. - 1.);
            }
        }
    };
    if let Some(lines) = chart.get_mut("judgeLineList").and_then(|v| v.as_array_mut()) {
        for line in lines.iter_mut() {
            for key in ["judgeLineDisappearEvents", "judgeLineRotateEvents", "judgeLineMoveEvents"] {
                if let Some(evs) = line.get_mut(key).and_then(|v| v.as_array_mut()) {
                    for e in evs.iter_mut() {
                        if key == "judgeLineDisappearEvents" {
                            scale255(e);
                        }
                        if key == "judgeLineMoveEvents" {
                            norm(e);
                        }
                        if e.get("tween").is_none() {
                            e.as_object_mut().unwrap().insert("tween".into(), json!(0));
                        }
                    }
                }
            }
            for key in ["notesAbove", "notesBelow"] {
                if let Some(notes) = line.get_mut(key).and_then(|v| v.as_array_mut()) {
                    for n in notes.iter_mut() {
                        let obj = n.as_object_mut().unwrap();
                        obj.entry("above").or_insert(json!(key == "notesAbove"));
                        obj.entry("fake").or_insert(json!(false));
                        obj.entry("size").or_insert(json!(1.0));
                    }
                }
            }
        }
    }
}

/// 把 rpe-next 导出的谱面文本**原地写回**谱面目录，并让 info.yml 指向它。
///
/// 和 `save_chart` 的区别：这里是「原样落盘」——rpe-next 导出的就是 RPE JSON 文本，
/// 不再经编辑器模型转换（转换会丢 Next 特有的曲线/噪域等字段）。旧文件先备份成
/// `<name>.bak`，`name` 只允许纯文件名（挡 `..` / 路径分隔符）。
pub fn write_chart_file(dir: &str, name: &str, text: &str) -> Result<Value> {
    let dir = &resolve_dir(dir);
    let name = name.trim();
    let bad = name.is_empty()
        || name.len() > 200
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.');
    if bad {
        bail!("文件名不合法：{name}");
    }
    let lower = name.to_ascii_lowercase();
    if !(lower.ends_with(".json") || lower.ends_with(".rpe") || lower.ends_with(".pec")) {
        bail!("只允许 .json / .rpe / .pec：{name}");
    }
    let path = Path::new(dir).join(name);
    if path.is_file() {
        let _ = std::fs::copy(&path, Path::new(dir).join(format!("{name}.bak")));
    }
    std::fs::write(&path, text).with_context(|| format!("写入 {} 失败", path.display()))?;
    let mut info = read_info(dir)?;
    if info.get("chart").and_then(|v| v.as_str()) != Some(name) {
        info.as_object_mut().context("info.yml 不是对象")?.insert("chart".into(), json!(name));
        write_info(dir, &info)?;
    }
    Ok(json!({ "ok": true, "file": name }))
}

/// 把谱面目录里的「谱面 + 音乐 + 曲绘」读成 base64，供照搬的 rpe-next 页面导入。
///
/// rpe-next 的导入入口是 `<input type=file>`（它只认 File 对象，不认路径），
/// 所以桥这边必须把字节递过去。谱面按原文件（`.json`/`.pec`）给，音乐/曲绘可选。
pub fn bundle(dir: &str) -> Result<Value> {
    let dir = &resolve_dir(dir);
    use base64::Engine as _;
    let info = read_info(dir)?;
    let enc = |name: &str| -> Result<Option<Value>> {
        if name.is_empty() {
            return Ok(None);
        }
        let path = Path::new(dir).join(name);
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path).with_context(|| format!("读取 {} 失败", path.display()))?;
        Ok(Some(json!({
            "name": name,
            "b64": base64::engine::general_purpose::STANDARD.encode(bytes),
        })))
    };
    let chart = info.get("chart").and_then(|v| v.as_str()).unwrap_or("").to_owned();
    if chart.is_empty() {
        bail!("info.yml 没有 chart 字段");
    }
    Ok(json!({
        "ok": true,
        "dir": dir,
        "chart": enc(&chart)?.context("谱面文件不存在")?,
        "music": enc(info.get("music").and_then(|v| v.as_str()).unwrap_or(""))?,
        "illustration": enc(info.get("illustration").and_then(|v| v.as_str()).unwrap_or(""))?,
    }))
}

/// 音乐波形峰值（时间轴用）：解码音频后按桶取 `max(|L|, |R|)`，返回 `{ ok, duration, peaks }`。
///
/// 页面只画波形、不做分析，所以直接给降采样峰值数组（默认 1600 桶 ≈ 一首 2 分钟歌
/// 每桶 75ms），比传 PCM 省几百倍带宽。解码走 `sasa`（和游戏内播放同一套 FFmpeg 后端）。
pub fn waveform(dir: &str, buckets: usize) -> Result<Value> {
    let dir = &resolve_dir(dir);
    let info = read_info(dir)?;
    let music = info.get("music").and_then(|v| v.as_str()).unwrap_or("").to_owned();
    if music.is_empty() {
        bail!("谱面没有配置音乐文件");
    }
    let path = Path::new(dir).join(&music);
    let bytes = std::fs::read(&path).with_context(|| format!("读取 {} 失败", path.display()))?;
    let (frames, rate) = sasa::AudioClip::decode(bytes).context("解码音频失败")?;
    let duration = frames.len() as f64 / rate.max(1) as f64;
    Ok(json!({
        "ok": true,
        "duration": duration,
        "peaks": peaks_from_frames(&frames, buckets.clamp(64, 8192)),
    }))
}

/// 帧 → 等分桶峰值（0..1）。单独拆出来方便单测（不需要真音频文件）。
fn peaks_from_frames(frames: &[sasa::Frame], buckets: usize) -> Vec<f32> {
    if frames.is_empty() || buckets == 0 {
        return Vec::new();
    }
    let mut out = vec![0f32; buckets];
    let step = frames.len() as f64 / buckets as f64;
    for (b, slot) in out.iter_mut().enumerate() {
        let a = (b as f64 * step) as usize;
        let z = (((b + 1) as f64 * step) as usize).clamp(a + 1, frames.len());
        let mut peak = 0f32;
        for f in &frames[a..z] {
            peak = peak.max(f.0.abs()).max(f.1.abs());
        }
        *slot = peak.min(1.);
    }
    out
}

/// 保存谱面。`format` 决定写回格式：`pec` → PEC 文本（原文件）；其它 → JSON。
/// 返回实际写出的文件名。
pub fn save_chart(dir: &str, chart: &Value, format: &str) -> Result<String> {
    let dir = &resolve_dir(dir);
    let mut info = read_info(dir)?;
    let cur = info.get("chart").and_then(|v| v.as_str()).unwrap_or("").to_owned();
    if format == "pec" {
        // RPE 文本：原地写回（文件名可能是 .rpe / .json 都行，引擎按内容分发）
        let target = if cur.is_empty() { "chart.pec".to_owned() } else { cur.clone() };
        let text = json_to_pec(chart)?;
        std::fs::write(Path::new(dir).join(&target), text).with_context(|| format!("写入 {target} 失败"))?;
        return Ok(target);
    }
    // 官方 JSON：当前文件是 JSON 就覆盖它；否则新写 editor.json 并改指向
    let reuse = !cur.is_empty() && cur.ends_with(".json");
    let target = if reuse { cur.clone() } else { "editor.json".to_owned() };
    std::fs::write(Path::new(dir).join(&target), serde_json::to_string_pretty(chart)?)
        .with_context(|| format!("写入 {target} 失败"))?;
    if target != cur {
        info.as_object_mut().context("info.yml 不是对象")?.insert("chart".into(), json!(target));
        write_info(dir, &info)?;
    }
    Ok(target)
}

/// 扩展名 → 素材类别
fn asset_kind(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension().map(|it| it.to_string_lossy().to_lowercase())?;
    match ext.as_str() {
        "mp3" | "ogg" | "wav" | "flac" | "m4a" | "aac" => Some("music"),
        "png" | "jpg" | "jpeg" | "webp" | "bmp" => Some("illustration"),
        _ => None,
    }
}

/// 创建谱面：把用户选的音乐 / 曲绘复制进 `custom/<uuid>/`，生成 info.yml + 空 chart.json。
/// `music` / `illustration` 传 `None` 就建空谱面（移动端没有文件选择器时的退路）。
pub fn create_chart(music: Option<&str>, illustration: Option<&str>, name: Option<&str>) -> Result<String> {
    let base = crate::dir::custom_charts()?;
    let dir = Path::new(&base).join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).with_context(|| format!("创建 {} 失败", dir.display()))?;

    let mut name_hint: Option<String> = None;
    // —— 音乐 ——
    let music_file = match music {
        Some(m) if asset_kind(m) == Some("music") && Path::new(m).is_file() => {
            let ext = Path::new(m).extension().map(|it| it.to_string_lossy().to_lowercase()).unwrap_or_else(|| "mp3".into());
            let fname = format!("music.{ext}");
            std::fs::copy(m, dir.join(&fname)).with_context(|| format!("复制音乐失败：{m}"))?;
            name_hint = Path::new(m).file_stem().map(|it| it.to_string_lossy().into_owned());
            fname
        }
        _ => "music.mp3".to_owned(),
    };
    // —— 曲绘 ——
    let illu_file = match illustration {
        Some(i) if asset_kind(i) == Some("illustration") && Path::new(i).is_file() => {
            let ext = Path::new(i).extension().map(|it| it.to_string_lossy().to_lowercase()).unwrap_or_else(|| "png".into());
            let fname = format!("illustration.{ext}");
            std::fs::copy(i, dir.join(&fname)).with_context(|| format!("复制曲绘失败：{i}"))?;
            fname
        }
        _ => "illustration.png".to_owned(),
    };

    // 曲名优先级：用户填的 > 音乐文件名 > 「未命名谱面」
    let name = name
        .map(|s| s.trim().to_owned())
        .into_iter()
        .chain(name_hint)
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| "未命名谱面".to_owned());
    let info = json!({
        "name": name,
        "difficulty": 0.0,
        "level": "EZ Lv.0",
        "charter": "",
        "composer": "",
        "illustrator": "",
        "chart": "chart.json",
        "format": 3,
        "music": music_file,
        "illustration": illu_file,
        "unlockVideo": null,
        "previewStart": 0.0,
        "previewEnd": null,
        "aspectRatio": 1.7777778,
        "backgroundDim": 0.6,
        "lineLength": 6.0,
        "offset": 0.0,
        "tip": null,
        "tags": [],
        "intro": "",
        "holdPartialCover": false,
        "noteUniformScale": false,
        "forceAspectRatio": false
    });
    write_info(dir.to_str().unwrap(), &info)?;
    std::fs::write(dir.join("chart.json"), serde_json::to_string_pretty(&empty_chart())?)?;
    Ok(dir.to_str().unwrap().to_owned())
}

/// 桌面端单选文件框：`kind` = `music` / `illustration`，按类型给过滤器。
/// 返回 `None` 表示用户取消。移动端没有文件选择器 → 也返回 `None`。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn pick_asset_file(kind: &str) -> Option<String> {
    let mut dlg = rfd::FileDialog::new();
    dlg = match kind {
        "music" => dlg.set_title("选择音乐文件").add_filter("音乐", &["mp3", "ogg", "wav", "flac", "m4a", "aac"]),
        _ => dlg.set_title("选择曲绘图片").add_filter("图片", &["png", "jpg", "jpeg", "webp", "bmp"]),
    };
    dlg.pick_file().map(|it| it.display().to_string())
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn pick_asset_file(_kind: &str) -> Option<String> {
    None
}

/// 列出本机谱面（制谱器「谱面管理」用）：`custom`（自建/导入）+ `download`（在线下载）。
/// 只读目录、读 info.yml，不碰谱面文件本身。
pub fn list_charts() -> Result<Value> {
    let mut out: Vec<Value> = Vec::new();
    let custom = crate::dir::custom_charts()?;
    scan_chart_dir(std::path::Path::new(&custom), "custom", &mut out)?;
    let dl = Path::new(&crate::dir::charts()?).join("download");
    if dl.is_dir() {
        scan_chart_dir(&dl, "download", &mut out)?;
    }
    // 自定义谱面排前面，同组按名称
    out.sort_by(|a, b| {
        let sa = a["source"].as_str().unwrap_or("") == "custom";
        let sb = b["source"].as_str().unwrap_or("") == "custom";
        sb.cmp(&sa).then_with(|| a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or("")))
    });
    Ok(json!({ "ok": true, "charts": out }))
}

fn scan_chart_dir(dir: &Path, source: &str, out: &mut Vec<Value>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return Ok(()),   // 目录不存在就当空
    };
    let mut items: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    items.sort_by_key(|e| e.file_name());
    for entry in items {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let info = read_info(path.to_str().unwrap_or("")).unwrap_or(json!({}));
        let modified = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        let chart_file = info.get("chart").and_then(|v| v.as_str()).unwrap_or("chart.json").to_owned();
        out.push(json!({
            "name": info.get("name").and_then(|v| v.as_str()).unwrap_or("(未命名)"),
            "charter": info.get("charter").and_then(|v| v.as_str()).unwrap_or(""),
            "composer": info.get("composer").and_then(|v| v.as_str()).unwrap_or(""),
            "level": info.get("level").and_then(|v| v.as_str()).unwrap_or(""),
            "path": path.to_str().unwrap_or(""),
            "chartFile": chart_file,
            "source": source,
            "modified": modified,
        }));
    }
    Ok(())
}

// ============================================================
// 谱面分享码（deflate + base64；二维码只对小谱面生成）
// ============================================================

/// 读谱面源文件（info.yml 的 chart 字段指向的 json / pec / rpe 文本）。
fn read_chart_source(dir: &str) -> Result<(String, String)> {
    let info = read_info(dir)?;
    let chart_file = info.get("chart").and_then(|v| v.as_str()).unwrap_or("chart.json").to_owned();
    let raw = std::fs::read_to_string(Path::new(dir).join(&chart_file)).with_context(|| format!("读取 {chart_file} 失败"))?;
    Ok((raw, chart_file))
}

/// deflate 压缩。
fn deflate_bytes(data: &[u8]) -> Vec<u8> {
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::best());
    let _ = enc.write_all(data);
    enc.finish().unwrap_or_default()
}

/// inflate 解压。
fn inflate_bytes(data: &[u8]) -> Result<Vec<u8>> {
    use flate2::read::DeflateDecoder;
    use std::io::Read;
    let mut dec = DeflateDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out).context("分享码解压失败")?;
    Ok(out)
}

/// 生成谱面分享码：`PHIRA1.<base64(deflate(源文本))>`。
/// 谱面源文件是纯文本（官方 JSON / RPE 文本），不含音乐/曲绘。
pub fn share_code(dir: &str) -> Result<Value> {
    let dir = &resolve_dir(dir);
    let (raw, chart_file) = read_chart_source(dir)?;
    let info = read_info(dir)?;
    let name = info.get("name").and_then(|v| v.as_str()).unwrap_or("谱面").to_owned();
    let compressed = deflate_bytes(raw.as_bytes());
    use base64::Engine;
    let code = format!("PHIRA1.{}", base64::engine::general_purpose::STANDARD.encode(&compressed));

    // 二维码由页面用本地 qrcode.js 生成（code ≤ ~2900 字节时才有意义；大谱面用分享码文本）
    Ok(json!({
        "ok": true,
        "code": code,
        "name": name,
        "chartFile": chart_file,
        "rawSize": raw.len(),
        "codeSize": code.len(),
    }))
}

/// 导入分享码：解码 → 建一份 custom 谱面（音乐/曲绘不包含在码里，之后补）。
pub fn import_share_code(code: &str) -> Result<String> {
    let code = code.trim();
    let payload = code.strip_prefix("PHIRA1.").context("不是有效的谱面分享码（应为 PHIRA1. 开头）")?;
    use base64::Engine;
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .context("分享码 base64 解码失败")?;
    let raw_bytes = inflate_bytes(&compressed)?;
    let raw = String::from_utf8(raw_bytes).context("分享码内容不是有效文本")?;
    // 建谱面：把源文本按原格式写入
    let base = crate::dir::custom_charts()?;
    let dir = Path::new(&base).join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).with_context(|| format!("创建 {} 失败", dir.display()))?;
    let is_json = raw.trim_start().starts_with('{');
    let chart_file = if is_json { "chart.json".to_owned() } else { "chart.pec".to_owned() };
    let info = json!({
        "name": "导入的谱面",
        "difficulty": 0.0,
        "level": "",
        "charter": "",
        "composer": "",
        "illustrator": "",
        "chart": chart_file,
        "format": 3,
        "music": "music.mp3",
        "illustration": "illustration.png",
        "unlockVideo": null,
        "previewStart": 0.0,
        "previewEnd": null,
        "aspectRatio": 1.7777778,
        "backgroundDim": 0.6,
        "lineLength": 6.0,
        "offset": 0.0,
        "tip": null,
        "tags": [],
        "intro": "由分享码导入（音乐/曲绘请自行补充）",
        "holdPartialCover": false,
        "noteUniformScale": false,
        "forceAspectRatio": false
    });
    write_info(dir.to_str().unwrap(), &info)?;
    std::fs::write(dir.join(&chart_file), raw)?;
    Ok(dir.to_str().unwrap().to_owned())
}

// ============================================================
// 谱面 diff（按索引对齐，面向「同一份谱面的两个版本」）
// ============================================================

/// 把编辑器 JSON 拍平成 `路径 -> 值` 的映射，逐路径比较。
/// 路径形如 `L0.notesAbove[3].time`；值只留标量（number/bool/string）。
fn flatten_chart(v: &Value, out: &mut Vec<(String, Value)>) {
    fn rec(prefix: &str, v: &Value, out: &mut Vec<(String, Value)>) {
        match v {
            Value::Object(map) => {
                for (k, val) in map {
                    rec(&format!("{prefix}.{k}"), val, out);
                }
            }
            Value::Array(arr) => {
                for (i, val) in arr.iter().enumerate() {
                    rec(&format!("{prefix}[{i}]"), val, out);
                }
            }
            scalar => out.push((prefix.to_owned(), scalar.clone())),
        }
    }
    rec("", v, out);
}

/// 结构化 diff：A → B 的差异列表。每条 `{ path, a, b }`（b 为 null = B 里删了）。
pub fn diff_charts(a: &Value, b: &Value) -> Value {
    let mut fa = Vec::new();
    let mut fb = Vec::new();
    flatten_chart(a, &mut fa);
    flatten_chart(b, &mut fb);
    let map_b: std::collections::HashMap<&str, &Value> = fb.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let map_a: std::collections::HashMap<&str, &Value> = fa.iter().map(|(k, v)| (k.as_str(), v)).collect();

    let mut diffs: Vec<Value> = Vec::new();
    // A 有 B 没有（删除/改动）
    for (path, va) in &fa {
        match map_b.get(path.as_str()) {
            Some(vb) => {
                if va != *vb && !(va.is_number() && vb.is_number() && va.as_f64() == vb.as_f64()) {
                    diffs.push(json!({ "path": path, "a": va, "b": vb, "op": "changed" }));
                }
            }
            None => diffs.push(json!({ "path": path, "a": va, "b": Value::Null, "op": "removed" })),
        }
    }
    // B 有 A 没有（新增）
    for (path, vb) in &fb {
        if !map_a.contains_key(path.as_str()) {
            diffs.push(json!({ "path": path, "a": Value::Null, "b": vb, "op": "added" }));
        }
    }
    diffs.sort_by(|x, y| x["path"].as_str().unwrap_or("").cmp(y["path"].as_str().unwrap_or("")));
    json!({ "ok": true, "diffs": diffs, "count": diffs.len() })
}

/// 递归打补丁：沿 comps 路径走到目标，last 时写入/删除。
fn patch_path(cur: &mut Value, comps: &[String], value: &Value, remove: bool) -> Result<()> {
    let Some(comp) = comps.first() else { return Ok(()) };
    let rest = &comps[1..];
    let last = rest.is_empty();
    if let Ok(idx) = comp.parse::<usize>() {
        let arr = cur.as_array_mut().with_context(|| format!("路径段 {comp} 处不是数组"))?;
        if last {
            if remove {
                if idx < arr.len() { arr.remove(idx); }
            } else if idx < arr.len() {
                arr[idx] = value.clone();
            } else {
                while arr.len() < idx { arr.push(Value::Null); }
                arr.push(value.clone());
            }
        } else {
            while arr.len() <= idx { arr.push(json!({})); }
            patch_path(&mut arr[idx], rest, value, remove)?;
        }
    } else {
        let obj = cur.as_object_mut().with_context(|| format!("路径段 {comp} 处不是对象"))?;
        if last {
            if remove {
                obj.remove(comp);
            } else {
                obj.insert(comp.clone(), value.clone());
            }
        } else {
            let slot = obj.entry(comp.clone()).or_insert_with(|| json!({}));
            patch_path(slot, rest, value, remove)?;
        }
    }
    Ok(())
}

/// 应用合并结果：`edits = [{ path, value }]`（value null = 删除该路径）。
/// 在 `base`（合并基底，一般是 A）上打补丁后返回新 JSON（不落盘，由前端走 save_chart）。
pub fn apply_merge(base: &Value, edits: &[Value]) -> Result<Value> {
    let mut out = base.clone();
    for e in edits {
        let path = e.get("path").and_then(|v| v.as_str()).context("edit 缺 path")?;
        let value = e.get("value").cloned().unwrap_or(Value::Null);
        let remove = value.is_null();
        let mut comps: Vec<String> = Vec::new();
        for t in path.split('.').filter(|t| !t.is_empty()) {
            if let Some(i) = t.find('[') {
                comps.push(t[..i].to_owned());
                for part in t[i + 1..].split(']') {
                    if !part.is_empty() {
                        comps.push(part.to_owned());
                    }
                }
            } else {
                comps.push(t.to_owned());
            }
        }
        if !comps.is_empty() {
            patch_path(&mut out, &comps, &value, remove).with_context(|| format!("打补丁失败：{path}"))?;
        }
    }
    Ok(out)
}

/// 从谱面源文本估算时长（秒）：所有音符 `time + hold` 的最大值 + 1s 余量。
/// 渲染时音乐文件缺失/解不动就用它（RPE 谱面经常不带 music）。
pub fn estimate_duration(source: &str) -> Option<f64> {
    let trimmed = source.trim_start();
    let chart = if trimmed.starts_with('{') {
        prpr::parse::parse_phigros(source, prpr::core::ChartExtra::default()).ok()?
    } else {
        let patched = ensure_pec_defaults(source);
        prpr::parse::parse_pec(&patched, prpr::core::ChartExtra::default()).ok()?
    };
    let mut max_t = 0f64;
    for l in &chart.lines {
        for n in &l.notes {
            let end = match &n.kind {
                prpr::core::NoteKind::Hold { end_time, .. } => *end_time,
                _ => n.time,
            };
            max_t = max_t.max(end).max(n.time);
        }
    }
    if max_t > 0. { Some(max_t + 1.0) } else { None }
}

/// 删除谱面目录。**只允许删 `custom` 下的**（在线下载的谱面归用户存档，不碰）。
pub fn delete_chart(path: &str) -> Result<()> {
    let path = &resolve_dir(path);
    let custom = crate::dir::custom_charts()?;
    let downloaded = crate::dir::downloaded_charts()?;
    let p = Path::new(path);
    // 必须是 custom / download 目录的直属子目录（路径安全检查，防止删到别处）
    let under = |root: &str| {
        p.starts_with(root) && p.parent().map(|par| par == Path::new(root)).unwrap_or(false)
    };
    if !under(&custom) && !under(&downloaded) {
        bail!("只能删除「自建谱面」或「已下载谱面」目录下的谱面");
    }
    if !p.is_dir() {
        bail!("目录不存在");
    }
    std::fs::remove_dir_all(p).with_context(|| format!("删除 {} 失败", p.display()))
}

/// 更新 info.yml 的字段（编辑器左侧表单保存）。
pub fn save_info(dir: &str, patch: &Value) -> Result<()> {
    let dir = &resolve_dir(dir);
    let mut info = read_info(dir)?;
    let obj = info.as_object_mut().context("info.yml 不是对象")?;
    let patch_obj = patch.as_object().context("patch 不是对象")?;
    for (k, v) in patch_obj {
        obj.insert(k.clone(), v.clone());
    }
    write_info(dir, &info)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把编辑器 JSON 序列化成 PEC，再解析回 Chart，再转回编辑器 JSON。
    fn pec_roundtrip(editor: &Value) -> Value {
        let text = json_to_pec(editor).unwrap();
        eprintln!("[rt] text://n{text}");
        let bp = scan_pec_bpm(&text);
        let chart = parse_pec(&text, prpr::core::ChartExtra::default())
            .unwrap_or_else(|e| panic!("往返后的 PEC 解析失败: {e}\n---\n{text}"));
        eprintln!("[rt] parse ok, {} lines", chart.lines.len());
        let v = chart_to_json(&chart, ChartFormat::Pec, &bp);
        eprintln!("[rt] to_json ok");
        v
    }

    /// 用真实 RPE 谱面做端到端往返（可选，需要 `PHIRA_REAL_CHART` 指向谱面文件）。
    /// 例：`PHIRA_REAL_CHART=D:/Phigros/PhiRa-Mod-Meew-11/rev/phira-pc/data/charts/download/4083/5238997.json`
    #[test]
    fn real_pec_roundtrip() {
        let Ok(path) = std::env::var("PHIRA_REAL_CHART") else { return };
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(detect_format(&raw), ChartFormat::Pec, "应是 RPE 文本");
        let bp = scan_pec_bpm(&raw);
        let patched = ensure_pec_defaults(&raw);
        let chart = parse_pec(&patched, prpr::core::ChartExtra::default()).unwrap();
        let notes: usize = chart.lines.iter().map(|l| l.notes.len()).sum();
        let first = chart_to_json(&chart, ChartFormat::Pec, &bp);

        // 序列化写回 → 再解析 → 再转 JSON，比较总量
        let text = json_to_pec(&first).unwrap();
        let bp2 = scan_pec_bpm(&text);
        let patched2 = ensure_pec_defaults(&text);
        let chart2 = parse_pec(&patched2, prpr::core::ChartExtra::default()).unwrap();
        let notes2: usize = chart2.lines.iter().map(|l| l.notes.len()).sum();
        let second = chart_to_json(&chart2, ChartFormat::Pec, &bp2);

        assert_eq!(chart.lines.len(), chart2.lines.len(), "判定线数");
        assert_eq!(notes, notes2, "音符总数");
        let ev = |c: &Value, k: &str| -> usize {
            c["judgeLineList"].as_array().unwrap().iter().map(|l| l[k].as_array().unwrap().len()).sum()
        };
        for k in ["judgeLineDisappearEvents", "judgeLineRotateEvents", "judgeLineMoveEvents", "speedEvents"] {
            assert_eq!(ev(&first, k), ev(&second, k), "{k} 数量");
        }
        // offset / bpm 段
        assert!((first["offset"].as_f64().unwrap() - second["offset"].as_f64().unwrap()).abs() < 1e-6, "offset");
        assert_eq!(first["bpmList"], second["bpmList"], "bp 段");
        eprintln!(
            "[real] {} 线 / {} 音符 / move {} 事件 / bp {:?} 往返一致",
            chart.lines.len(),
            notes,
            ev(&first, "judgeLineMoveEvents"),
            first["bpmList"]
        );
    }

    #[test]
    fn asset_kind_classifies() {
        assert_eq!(asset_kind("a.mp3"), Some("music"));
        assert_eq!(asset_kind("B.OGG"), Some("music"));
        assert_eq!(asset_kind("c.flac"), Some("music"));
        assert_eq!(asset_kind("d.png"), Some("illustration"));
        assert_eq!(asset_kind("E.JPEG"), Some("illustration"));
        assert_eq!(asset_kind("f.webp"), Some("illustration"));
        assert_eq!(asset_kind("g.txt"), None);
        assert_eq!(asset_kind("noext"), None);
        assert_eq!(asset_kind(""), None);
    }

    #[test]
    fn detect_format_works() {
        assert_eq!(detect_format("185\nbp 0 174\nn1 0 0 0 1 0 1\n"), ChartFormat::Pec);
        assert_eq!(detect_format("{\"formatVersion\":3}"), ChartFormat::Pgr);
        assert_eq!(detect_format("{\"META\":{},\"RPEVersion\":180}"), ChartFormat::RpeJson);
    }

    #[test]
    fn pec_roundtrip_preserves_notes_and_events() {
        // 一条线：bpm 174，起点居中，4 拍后移到右侧，两个音符（Tap + Hold）
        let editor = json!({
            "formatVersion": 3,
            "offset": 0.0,
            "bpmList": [[0.0, 174.0]],
            "judgeLineList": [{
                "bpm": 174.0,
                "judgeLineDisappearEvents": [
                    { "startTime": 0.0, "endTime": 1e9, "start": 255.0, "end": 255.0, "tween": 0 }
                ],
                "judgeLineRotateEvents": [
                    { "startTime": 0.0, "endTime": 4.0, "start": 0.0, "end": 90.0, "tween": 3 }
                ],
                "judgeLineMoveEvents": [
                    { "startTime": 0.0, "endTime": 4.0, "start": 0.0, "start2": 0.6, "end": 0.5, "end2": 0.6, "tween": 0 }
                ],
                "speedEvents": [
                    { "startTime": 0.0, "endTime": 1e9, "value": 1.0 }
                ],
                "notesAbove": [
                    { "type": 1, "time": 1.0, "positionX": 0.25, "holdTime": 0.0, "speed": 1.0, "floorPosition": 0.0, "above": true, "fake": false, "size": 1.0 },
                    { "type": 3, "time": 2.0, "positionX": 0.75, "holdTime": 1.5, "speed": 1.5, "floorPosition": 0.0, "above": true, "fake": false, "size": 1.2 }
                ],
                "notesBelow": [
                    { "type": 4, "time": 3.0, "positionX": 0.5, "holdTime": 0.0, "speed": 1.0, "floorPosition": 0.0, "above": false, "fake": true, "size": 1.0 }
                ]
            }]
        });

        let back = pec_roundtrip(&editor);
        let line = &back["judgeLineList"][0];

        // --- 音符 ---
        let above = line["notesAbove"].as_array().unwrap();
        assert_eq!(above.len(), 2, "notesAbove 数量");
        // 引擎的 JudgeLineCache::new 会按 (plain, above, speed, height) 重排 notes，
        // 所以这里按 type 找，不假定顺序。
        let tap = above.iter().find(|n| n["type"] == 1).expect("应有一条 Tap");
        assert!((tap["positionX"].as_f64().unwrap() - 0.25).abs() < 1e-3, "positionX {}", tap["positionX"]);
        assert!((tap["time"].as_f64().unwrap() - 1.0).abs() < 1e-3, "time {}", tap["time"]);
        // Hold：holdTime 保留；speed # 1.5 保留；size & 1.2 保留
        let hold = above.iter().find(|n| n["type"] == 3).expect("应有一条 Hold");
        assert!((hold["holdTime"].as_f64().unwrap() - 1.5).abs() < 1e-3, "holdTime {}", hold["holdTime"]);
        assert!((hold["speed"].as_f64().unwrap() - 1.5).abs() < 1e-3, "speed {}", hold["speed"]);
        assert!((hold["size"].as_f64().unwrap() - 1.2).abs() < 1e-3, "size {}", hold["size"]);
        // Flick 在 notesBelow、fake
        let below = line["notesBelow"].as_array().unwrap();
        assert_eq!(below.len(), 1);
        assert_eq!(below[0]["type"], 4, "PEC n3=Flick → 编辑器 type 4");
        assert_eq!(below[0]["fake"], true);

        // --- 事件 ---
        // rotate 插值 0→90，tween 3（RPE_TWEEN_MAP[3] = Sine Out）
        let rot = line["judgeLineRotateEvents"].as_array().unwrap();
        let interp = rot.iter().find(|e| e["endTime"].as_f64().unwrap_or(0.) > e["startTime"].as_f64().unwrap_or(0.) + 1e-6);
        assert!(interp.is_some(), "rotate 应有插值事件: {rot:?}");
        let interp = interp.unwrap();
        assert!((interp["start"].as_f64().unwrap() - 0.0).abs() < 1e-3);
        assert!((interp["end"].as_f64().unwrap() - 90.0).abs() < 1e-3, "rotate end {}", interp["end"]);
        assert_eq!(interp["tween"], 3, "tween 编号应保留");
        // move 插值 0→0.5（x）
        let mv = line["judgeLineMoveEvents"].as_array().unwrap();
        let mi = mv.iter().find(|e| e["endTime"].as_f64().unwrap_or(0.) > e["startTime"].as_f64().unwrap_or(0.) + 1e-6);
        assert!(mi.is_some(), "move 应有插值事件: {mv:?}");
        let mi = mi.unwrap();
        assert!((mi["start"].as_f64().unwrap() - 0.0).abs() < 1e-3);
        assert!((mi["end"].as_f64().unwrap() - 0.5).abs() < 1e-3, "move end x {}", mi["end"]);
        assert!((mi["start2"].as_f64().unwrap() - 0.6).abs() < 1e-3, "move start2 y {}", mi["start2"]);

        // --- offset / bpmList ---
        assert!((back["offset"].as_f64().unwrap() - 0.0).abs() < 1e-6);
        let bl = back["bpmList"].as_array().unwrap();
        assert_eq!(bl.len(), 1);
        assert!((bl[0][1].as_f64().unwrap() - 174.0).abs() < 1e-6);
    }

    #[test]
    fn pec_text_shape() {
        // 直接检查序列化出的命令文本（人眼可读的格式）
        let editor = json!({
            "offset": 0.15,
            "bpmList": [[0.0, 120.0]],
            "judgeLineList": [{
                "judgeLineDisappearEvents": [],
                "judgeLineRotateEvents": [],
                "judgeLineMoveEvents": [
                    { "startTime": 0.0, "endTime": 1e9, "start": 0.0, "start2": 0.0, "end": 0.0, "end2": 0.0, "tween": 0 },
                    { "startTime": 2.0, "endTime": 4.0, "start": 0.0, "start2": 0.0, "end": 0.5, "end2": 0.0, "tween": 0 }
                ],
                "speedEvents": [],
                "notesAbove": [
                    { "type": 2, "time": 1.0, "positionX": 0.5, "holdTime": 0.0, "speed": 1.0, "floorPosition": 0.0, "above": true, "fake": false, "size": 1.0 }
                ],
                "notesBelow": []
            }]
        });
        let text = json_to_pec(&editor).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "300", "offset 0.15s → 300ms（+0.15 补偿）");
        assert_eq!(lines[1], "bp 0 120", "PEC 的 bp 是「拍 bpm」");
        // move 常量段 → cp（中心 1024,700）；插值段 → cm
        assert!(lines.contains(&"cp 0 0 1024 700"), "缺 cp: {text}");
        assert!(lines.contains(&"cm 0 2 4 1536 700 0"), "缺 cm（只带终点 x=1536）: {text}");
        // 编辑器 type 2 = Drag → PEC n4
        assert!(lines.contains(&"n4 0 1 512 1 0"), "缺 n4: {text}");
    }

    #[test]
    fn resolve_dir_handles_relative() {
        assert_eq!(resolve_dir(""), "");
        // 绝对路径原样返回（Windows 盘符 / Unix 根）
        let abs = if cfg!(windows) { r"D://data//charts//custom//x" } else { "/data/charts/custom/x" };
        assert_eq!(resolve_dir(abs), abs);
        // 相对路径（曲库传的 local_path）必须拼到 charts 根下
        if let Ok(root) = crate::dir::charts() {
            let got = resolve_dir("custom/abc");
            assert!(got.starts_with(&root), "相对路径应拼到 charts 根：{got} vs {root}");
            assert!(got.ends_with("custom/abc") || got.ends_with("custom\\abc"), "尾部保留相对路径：{got}");
        }
    }

    #[test]
    fn write_chart_file_in_place() {
        let dir = std::env::temp_dir().join(format!("phira_ed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("info.yml"), "chart: level.pec\nmusic: a.mp3\n").unwrap();
        std::fs::write(dir.join("level.pec"), "old").unwrap();
        let d = dir.to_string_lossy().to_string();
        let r = write_chart_file(&d, "level.json", "{\"x\":1}").unwrap();
        assert_eq!(r["file"], "level.json");
        assert_eq!(std::fs::read_to_string(dir.join("level.json")).unwrap(), "{\"x\":1}");
        let info = std::fs::read_to_string(dir.join("info.yml")).unwrap();
        assert!(info.contains("level.json"), "info.yml 应指向新文件：{info}");
        assert!(write_chart_file(&d, "../evil.json", "{}").is_err(), "必须挡路径穿越");
        assert!(write_chart_file(&d, "evil.exe", "{}").is_err(), "必须挡非法扩展名");
        // 覆盖已有文件时留 .bak
        write_chart_file(&d, "level.json", "{\"x\":2}").unwrap();
        assert!(dir.join("level.json.bak").is_file(), "覆盖前应备份 .bak");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn waveform_peaks() {
        // 4 帧 → 2 桶：桶 0 = max(0.1, -0.5) = 0.5；桶 1 = max(0.25, 1.2 截断) = 1.0
        let frames = [
            sasa::Frame(0.1, -0.5),
            sasa::Frame(0.2, 0.05),
            sasa::Frame(-0.25, 0.3),
            sasa::Frame(0.0, 1.2),
        ];
        let peaks = peaks_from_frames(&frames, 2);
        assert_eq!(peaks.len(), 2);
        assert!((peaks[0] - 0.5).abs() < 1e-6, "bucket0 {}", peaks[0]);
        assert!((peaks[1] - 1.0).abs() < 1e-6, "bucket1 {}", peaks[1]);
        assert!(peaks_from_frames(&[], 4).is_empty());
    }

    #[test]
    fn pgr_normalize_units() {
        let mut chart: Value = serde_json::from_str(
            r#"{
                "formatVersion": 3,
                "offset": 0.0,
                "judgeLineList": [{
                    "bpm": 120,
                    "judgeLineDisappearEvents": [{ "startTime": 0, "endTime": 99999, "start": 0.5, "end": 1.0 }],
                    "judgeLineMoveEvents": [{ "startTime": 0, "endTime": 99999, "start": 0.5, "start2": 0.1, "end": 0.5, "end2": 0.1 }],
                    "judgeLineRotateEvents": [],
                    "speedEvents": [],
                    "notesAbove": [{ "type": 1, "time": 0, "positionX": 0.5, "holdTime": 0, "speed": 1 }],
                    "notesBelow": []
                }]
            }"#,
        )
        .unwrap();
        normalize_pgr(&mut chart);
        let line = &chart["judgeLineList"][0];
        // alpha 0..1 → 0..255
        let a = &line["judgeLineDisappearEvents"][0];
        assert!((a["start"].as_f64().unwrap() - 127.5).abs() < 1e-6);
        assert!((a["end"].as_f64().unwrap() - 255.0).abs() < 1e-6);
        assert_eq!(a["tween"], 0, "官方 JSON 事件补 tween 0");
        // move 0..1 → -1..1
        let m = &line["judgeLineMoveEvents"][0];
        assert!((m["start"].as_f64().unwrap() - 0.0).abs() < 1e-6);
        assert!((m["start2"].as_f64().unwrap() - (-0.8)).abs() < 1e-6);
        // note 补 above/fake/size
        let n = &line["notesAbove"][0];
        assert_eq!(n["above"], true);
        assert_eq!(n["fake"], false);
        assert_eq!(n["size"], 1.0);
    }

    #[test]
    fn share_code_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("phira-share-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("info.yml"), "name: 分享测试\nchart: chart.json\n").unwrap();
        let src = r#"{"formatVersion":3,"offset":0.5,"judgeLineList":[{"bpm":174}]}"#;
        std::fs::write(tmp.join("chart.json"), src).unwrap();

        let r = share_code(tmp.to_str().unwrap()).unwrap();
        assert_eq!(r["ok"], true);
        let code = r["code"].as_str().unwrap();
        assert!(code.starts_with("PHIRA1."), "码前缀");
        assert_eq!(r["name"], "分享测试");

        // 手动解码验证内容一致（不走 import_share_code，避免写真实存档）
        let payload = code.strip_prefix("PHIRA1.").unwrap();
        use base64::Engine;
        let compressed = base64::engine::general_purpose::STANDARD.decode(payload).unwrap();
        let raw = inflate_bytes(&compressed).unwrap();
        assert_eq!(String::from_utf8(raw).unwrap(), src, "deflate 往返应无损");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn diff_and_merge_roundtrip() {
        let a = json!({
            "offset": 0.0,
            "judgeLineList": [{ "bpm": 120, "notesAbove": [
                { "type": 1, "time": 1.0, "positionX": 0.5 },
                { "type": 2, "time": 2.0, "positionX": 0.3 }
            ], "notesBelow": [] }]
        });
        let b = json!({
            "offset": 0.5,
            "judgeLineList": [{ "bpm": 120, "notesAbove": [
                { "type": 1, "time": 1.0, "positionX": 0.7 },
                { "type": 2, "time": 2.0, "positionX": 0.3 }
            ], "notesBelow": [] }]
        });
        let r = diff_charts(&a, &b);
        assert_eq!(r["count"], 2, "应有 2 处差异（offset + positionX）: {r}");
        let diffs = r["diffs"].as_array().unwrap();
        assert!(diffs.iter().any(|d| d["path"] == ".offset" && d["op"] == "changed"));
        assert!(diffs.iter().any(|d| d["path"].as_str().unwrap().contains("positionX")));

        // 合并：offset 用 A 的，positionX 用 B 的
        let edits = json!([
            { "path": ".offset", "value": 0.0 },
            { "path": ".judgeLineList[0].notesAbove[0].positionX", "value": 0.7 }
        ]);
        let merged = apply_merge(&a, edits.as_array().unwrap()).unwrap();
        assert_eq!(merged["offset"], 0.0);
        assert_eq!(merged["judgeLineList"][0]["notesAbove"][0]["positionX"], 0.7);
        assert_eq!(merged["judgeLineList"][0]["notesAbove"][1]["positionX"], 0.3, "未动的字段不受影响");

        // 数组删除语义
        let del = json!([{ "path": ".judgeLineList[0].notesAbove[0]", "value": null }]);
        let removed = apply_merge(&a, del.as_array().unwrap()).unwrap();
        assert_eq!(removed["judgeLineList"][0]["notesAbove"].as_array().unwrap().len(), 1);
        assert_eq!(removed["judgeLineList"][0]["notesAbove"][0]["time"], 2.0);
    }

    #[test]
    fn empty_chart_shape() {
        let c = empty_chart();
        assert_eq!(c["formatVersion"], 3);
        let lines = c["judgeLineList"].as_array().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["bpm"], 120.0);
        assert!(lines[0]["notesAbove"].as_array().unwrap().is_empty());
        // 每个事件轨都得有一条常驻事件，否则编辑器里没法插值
        for k in ["judgeLineDisappearEvents", "judgeLineRotateEvents", "judgeLineMoveEvents", "speedEvents"] {
            assert!(lines[0][k].as_array().unwrap().len() >= 1, "{k} 空");
        }
    }

    #[test]
    fn new_save_load_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("phira-editor-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let dir = tmp.join("chart1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("info.yml"), "name: 测试\nchart: chart.json\n").unwrap();
        std::fs::write(dir.join("chart.json"), "{\"formatVersion\":3}").unwrap();

        let loaded = load_chart(dir.to_str().unwrap()).unwrap();
        assert_eq!(loaded["ok"], true);
        assert_eq!(loaded["chartFile"], "chart.json");
        assert_eq!(loaded["format"], "pgr");

        // 保存（覆盖原 JSON）
        let mut chart = loaded["chart"].clone();
        chart["judgeLineList"] = json!([]);
        let f = save_chart(dir.to_str().unwrap(), &chart, "pgr").unwrap();
        assert_eq!(f, "chart.json");

        // info 补丁
        save_info(dir.to_str().unwrap(), &json!({ "charter": "meew" })).unwrap();
        let info = read_info(dir.to_str().unwrap()).unwrap();
        assert_eq!(info["charter"], "meew");

        // RPE 文本：现在能打开（format=pec），且保存写回原文件
        std::fs::write(dir.join("info.yml"), "name: RPE谱\nchart: level.pec\n").unwrap();
        std::fs::write(dir.join("level.pec"), "185\nbp 0 174\nn1 0 0 0 1 0 1\n").unwrap();
        let rpe = load_chart(dir.to_str().unwrap()).unwrap();
        assert_eq!(rpe["ok"], true, "RPE 文本应能打开: {rpe}");
        assert_eq!(rpe["format"], "pec");
        assert_eq!(rpe["readonly"], Value::Null, "PEC 不是只读");
        // 谱面里那条 Tap 应该在 notesAbove
        let notes = rpe["chart"]["judgeLineList"][0]["notesAbove"].as_array().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0]["type"], 1);

        // 用 pec 格式保存 → 原地写回 level.pec
        let mut chart = rpe["chart"].clone();
        chart["judgeLineList"][0]["notesAbove"][0]["positionX"] = json!(0.5);
        let f = save_chart(dir.to_str().unwrap(), &chart, "pec").unwrap();
        assert_eq!(f, "level.pec");
        let text = std::fs::read_to_string(dir.join("level.pec")).unwrap();
        assert!(text.contains("bp 0 174"), "bp 段丢失（拍在前）: {text}");
        assert!(text.contains("n1 0 0 512 1 0"), "音符丢失/错位: {text}");

        // 再打开一次，确认 positionX 改动生效
        let again = load_chart(dir.to_str().unwrap()).unwrap();
        let x = again["chart"]["judgeLineList"][0]["notesAbove"][0]["positionX"].as_f64().unwrap();
        assert!((x - 0.5).abs() < 1e-3, "positionX 未写回: {x}");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
