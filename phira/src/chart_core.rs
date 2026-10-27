//! 「谱面工具箱」的核心引擎。
//!
//! 从 `PhiCraft-CPP/src/chartcore.cpp`（纯 C++、无 Qt 依赖）移植成 Rust。
//! 只做**数据层**：谱面文件解析 / 信息读写 / 音符与事件操作 / 分析 / 融合。
//! UI（开始页入口 + 工具箱页面）在别处，本模块不碰任何渲染。
//!
//! 谱面两种格式（⚠️ **音符时间单位都是「1/32 拍」，不是毫秒**；秒数 = tick × 60 / 32 / bpm）：
//!   * **phira**：音符类型 Tap=1 / Drag=2 / 长条(Hold)=3 / Flick=4；
//!     音符在 `judgeLineList[i].notesAbove / notesBelow`，时间字段是数字 `time`。
//!   * **RPE**：音符类型 Tap=1 / 长条(Hold)=2 / Flick=3 / Drag=4；
//!     音符在 `judgeLineList[i].notes`，时间用 `startTime` / `endTime` = `[bar, beat, tick]`。
//!
use anyhow::Result;
use regex::Regex;
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};

// ============================================================
//  音符类型
// ============================================================

/// Phira 音符类型：Tap=1, Drag=2, 长条(Hold)=3, Flick=4。
pub const PHIRA_TAP: i64 = 1;
pub const PHIRA_DRAG: i64 = 2;
pub const PHIRA_HOLD: i64 = 3;
pub const PHIRA_FLICK: i64 = 4;

/// RPE 音符类型：Tap=1, Drag=4, 长条(Hold)=2, Flick=3。
pub const RPE_TAP: i64 = 1;
pub const RPE_DRAG: i64 = 4;
pub const RPE_HOLD: i64 = 2;
pub const RPE_FLICK: i64 = 3;

// ============================================================
//  基础工具
// ============================================================

/// 递归收集目录下所有 `.json` / `.pec` 文件。
pub fn collect_chart_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(dir).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = entry.path().extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("json") || ext.eq_ignore_ascii_case("pec") {
            out.push(entry.path().to_path_buf());
        }
    }
    out
}

// ============================================================
//  解压层定位（PEZ 里常常再套一层文件夹）
// ============================================================

fn is_archive_junk_name(name: &str) -> bool {
    name == "__MACOSX" || name == ".DS_Store" || name.starts_with("._")
}

/// 删掉归档垃圾：`__MACOSX` / `.DS_Store` / `._xxx`。
pub fn clean_archive_junk(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if name == "__MACOSX" {
                let _ = std::fs::remove_dir_all(&p);
            } else {
                clean_archive_junk(&p);
            }
        } else if is_archive_junk_name(&name) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

fn has_info_here(dir: &Path) -> bool {
    const K: [&str; 6] = ["info.json", "Info.txt", "info.yml", "info.yaml", "info.txt", "Info.json"];
    K.iter().any(|n| dir.join(n).exists())
}

fn has_chart_json_here(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    rd.flatten().any(|e| {
        let p = e.path();
        if !p.is_file() {
            return false;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        ext.eq_ignore_ascii_case("json") || ext.eq_ignore_ascii_case("pec")
    })
}

fn resolve_layer(dir: &Path, depth: u32) -> PathBuf {
    if depth > 12 || has_info_here(dir) || has_chart_json_here(dir) {
        return dir.to_path_buf();
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return dir.to_path_buf() };
    let subs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| !is_archive_junk_name(&p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
        .collect();
    if subs.is_empty() {
        return dir.to_path_buf();
    }
    let mut hits = Vec::new();
    for s in &subs {
        let r = resolve_layer(s, depth + 1);
        if r != *s || has_info_here(s) || has_chart_json_here(s) {
            hits.push(r);
        }
    }
    if hits.len() == 1 {
        hits.pop().unwrap()
    } else {
        dir.to_path_buf()
    }
}

/// 定位「谱面层」：直接含 info 文件（或谱面 json）的那一层。
pub fn resolve_chart_layer(dir: &Path) -> PathBuf {
    if !dir.is_dir() {
        return dir.to_path_buf();
    }
    resolve_layer(dir, 0)
}

/// 把谱面层的内容提到根目录，并删掉搬空的中间目录（返回是否真的搬过）。
pub fn flatten_to_chart_layer(root: &Path) -> bool {
    let layer = resolve_chart_layer(root);
    if layer.as_os_str().is_empty() || layer == root {
        return false;
    }
    let Ok(rd) = std::fs::read_dir(&layer) else { return false };
    let entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    let mut moved = false;
    for src in entries {
        let Some(name) = src.file_name() else { continue };
        let dst = root.join(name);
        if dst.exists() {
            continue;
        }
        if std::fs::rename(&src, &dst).is_ok() {
            moved = true;
        }
    }
    if !moved {
        return false;
    }
    let mut cur = layer;
    while cur != *root && !cur.as_os_str().is_empty() {
        if std::fs::read_dir(&cur).map(|mut it| it.next().is_some()).unwrap_or(true) {
            break; // 非空
        }
        if std::fs::remove_dir(&cur).is_err() {
            break;
        }
        cur = match cur.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
    }
    true
}

// ============================================================
//  RPE 时间工具
// ============================================================

/// 解析形如 `"[1, 2, 3]"` 或 `"1, 2, 3"` 的 RPE 时间字符串。
#[allow(dead_code)] // 目前仅单元测试使用
pub fn parse_rpe_time_str(s: &str) -> Option<[i64; 3]> {
    let t = s.trim().trim_start_matches('[').trim_end_matches(']');
    let mut r = [0i64; 3];
    let mut idx = 0;
    for tok in t.split(',') {
        if idx >= 3 {
            break;
        }
        let tok = tok.trim();
        if tok.is_empty() {
            return None;
        }
        r[idx] = tok.parse().ok()?;
        idx += 1;
    }
    if idx < 3 {
        None
    } else {
        Some(r)
    }
}

/// RPE 时间 → 32 分音符数。
pub fn rpe_time_to_32nd(t: &Json) -> f64 {
    if let Some(a) = t.as_array() {
        if a.len() >= 3 {
            let beats = a[0].as_f64().unwrap_or(0.) + a[1].as_f64().unwrap_or(0.) / a[2].as_f64().unwrap_or(1.);
            return beats * 32.0;
        }
    }
    0.0
}

/// 32 分音符时间 → 毫秒。
#[allow(dead_code)] // 目前仅单元测试使用
pub fn time32_to_ms(t32: f64, bpm: f64) -> f64 {
    t32 / 32.0 * 60.0 / bpm * 1000.0
}

// ============================================================
//  谱面信息（Info）
// ============================================================

fn strip_quotes(s: &str) -> String {
    s.trim_matches(|c| c == '\'' || c == '"').to_owned()
}

/// 解析 `title: xxx` 这种纯文本 info。
pub fn parse_info_text(raw: &str) -> Json {
    const PATS: [(&str, &str); 7] = [
        ("title", r"(?im)^title\s*:\s*(.+?)\s*$"),
        ("artist", r"(?im)^artist\s*:\s*(.+?)\s*$"),
        ("charter", r"(?im)^charter\s*:\s*(.+?)\s*$"),
        ("difficulty", r"(?im)^difficulty\s*:\s*(.+?)\s*$"),
        ("level", r"(?im)^level\s*:\s*(.+?)\s*$"),
        ("bpm", r"(?im)^bpm\s*:\s*(.+?)\s*$"),
        ("offset", r"(?im)^offset\s*:\s*(.+?)\s*$"),
    ];
    let mut data = json!({});
    for (key, pat) in PATS {
        let Ok(re) = Regex::new(pat) else { continue };
        if let Some(c) = re.captures(raw) {
            if let Some(m) = c.get(1) {
                data[key] = Json::String(strip_quotes(m.as_str().trim()));
            }
        }
    }
    data
}

/// 解析 RPE 的 `Info.txt`（Name/Composer/Charter/Level）。
pub fn parse_rpe_info_text(raw: &str) -> Json {
    let mut data = json!({
        "title": "未命名谱面",
        "artist": "未知",
        "charter": "未知",
        "difficulty": "?",
        "level": "?",
        "bpm": 140,
        "offset": 0,
    });
    for line in raw.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(colon) = line.find(':') else { continue };
        let key = line[..colon].trim_end();
        let value = line[colon + 1..].trim();
        match key {
            "Name" => data["title"] = Json::String(value.to_owned()),
            "Composer" => data["artist"] = Json::String(value.to_owned()),
            "Charter" => data["charter"] = Json::String(value.to_owned()),
            "Level" => data["level"] = Json::String(value.to_owned()),
            _ => {}
        }
    }
    data
}

/// 读目录里的 info 文件（`info.json` 优先于 `Info.txt`，见 PhiCraft 的修正说明）。
/// 只返回解析后的数据；要写回时用 `serde_yaml`（phira 自己的 `info.yml` 格式）。
pub fn load_info_file(dir: &Path) -> Option<Json> {
    const CANDS: [(&str, &str); 5] = [
        ("info.json", "json"),
        ("Info.txt", "rpe_text"),
        ("info.yml", "yaml"),
        ("info.yaml", "yaml"),
        ("info.txt", "yaml"),
    ];
    for (name, kind) in CANDS {
        let p = dir.join(name);
        if !p.exists() {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&p) else { return None };
        let data = match kind {
            "json" => match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => return None,
            },
            "rpe_text" => parse_rpe_info_text(&raw),
            _ => parse_info_text(&raw),
        };
        return Some(data);
    }
    None
}

/// 判断一份谱面 json 是不是 RPE 格式。
pub fn is_rpe_chart(data: &Json) -> bool {
    data.get("BPMList").is_some() || data.get("META").is_some()
}
/// 把 JSON 写成漂亮格式（2 空格缩进），和 PhiCraft 的 `dump(2)` 一致。
pub fn dump_json(v: &Json) -> Result<String> {
    Ok(serde_json::to_string_pretty(v)?)
}

/// 读一个谱面 json 文件。
pub fn read_chart_json(path: &Path) -> Result<Json> {
    let txt = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&txt)?)
}

/// 写一个谱面 json 文件。
pub fn write_chart_json(path: &Path, v: &Json) -> Result<()> {
    std::fs::write(path, dump_json(v)?)?;
    Ok(())
}

// ============================================================
//  谱面操作：辅助
// ============================================================

/// 遍历 `judgeLineList[i]` 下的音符数组（`notesAbove` / `notesBelow` / `notes`），
/// 把每个数组整体交给 `f`（`f` 收到的是那个数组的 `&mut Json`）。
pub fn for_each_note_array<F: FnMut(&mut Json)>(root: &mut Json, mut f: F) {
    let Some(lines) = root.get_mut("judgeLineList").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for line in lines.iter_mut() {
        for key in ["notesAbove", "notesBelow", "notes"] {
            if let Some(arr) = line.get_mut(key) {
                if arr.is_array() {
                    f(arr);
                }
            }
        }
    }
}

/// 从一组速度事件（`speedEvents` 数组）算 floorPosition。
pub fn recompute_floor_from_events(events: &Json, bpm: f64, time32: f64) -> f64 {
    let mut floor = 0.0;
    let mut prev_end = 0.0;
    let Some(evts) = events.as_array() else {
        return 0.0;
    };
    for evt in evts {
        let st = evt.get("startTime").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let et = evt.get("endTime").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let value = evt.get("value").and_then(|v| v.as_f64()).unwrap_or(1.0);
        if et <= prev_end || st >= time32 {
            continue;
        }
        let seg_start = st.max(prev_end);
        let seg_end = et.min(time32);
        if seg_end <= seg_start {
            continue;
        }
        let beats = (seg_end - seg_start) / 32.0;
        let seconds = beats * 60.0 / bpm;
        floor += seconds * value;
        prev_end = et;
        if seg_end >= time32 - 1e-9 {
            break;
        }
    }
    floor
}

/// 32 分音符时间 → RPE Beat `[n, m, den]`（`n + m/den` 拍）。
pub fn time32_to_rpe_time_array_ex(t32: f64, den: i64) -> Json {
    let den = den.max(1);
    let scaled = (t32 * den as f64 / 32.0).round() as i64;
    let mut beats = scaled / den;
    let mut rem = scaled % den;
    if rem < 0 {
        rem += den;
        beats -= 1;
    }
    json!([beats, rem, den])
}

/// 32 分音符时间 → RPE Beat（分母 32）。
pub fn time32_to_rpe_time_array(t32: f64) -> Json {
    time32_to_rpe_time_array_ex(t32, 32)
}

/// 按每拍片数选 RPE 时间分母：32 * 2^n >= needed。
pub fn rpe_den_for_density(per_beat: f64) -> i64 {
    let mut den = 32i64;
    let needed = per_beat.ceil() as i64;
    while den < needed {
        den *= 2;
    }
    den
}

/// 用户填的「切割数」= 每个长条固定切这么多份 → 需要的每拍密度。
pub fn rpe_den_for_total(pieces: i64, beats: f64) -> i64 {
    if beats <= 0.0 {
        rpe_den_for_density(pieces as f64)
    } else {
        rpe_den_for_density(pieces as f64 / beats)
    }
}

// ============================================================
//  [功能 1] 音符类型转换
// ============================================================

/// Phira 编号 → RPE 编号（Tap1→1, Drag2→4, Hold3→2, Flick4→3）。
fn phira_to_rpe_type(t: i64) -> i64 {
    match t {
        PHIRA_TAP => RPE_TAP,
        PHIRA_DRAG => RPE_DRAG,
        PHIRA_HOLD => RPE_HOLD,
        PHIRA_FLICK => RPE_FLICK,
        _ => t,
    }
}

/// 在内存里做音符类型转换（`from`/`to` 用 **Phira 编号** 交互）。
/// 返回是否改动过。
pub fn apply_note_changes_json(root: &mut Json, from_type: i64, to_type: i64, is_rpe: bool) -> bool {
    let src = if is_rpe { phira_to_rpe_type(from_type) } else { from_type };
    let dst = if is_rpe { phira_to_rpe_type(to_type) } else { to_type };
    let mut modified = false;
    for_each_note_array(root, |notes| {
        if let Some(arr) = notes.as_array_mut() {
            for note in arr.iter_mut() {
                if note.get("type").and_then(|v| v.as_i64()) == Some(src) {
                    note["type"] = json!(dst);
                    modified = true;
                }
            }
        }
    });
    modified
}

// ============================================================
//  [功能 2] 流速修改
// ============================================================

/// 在内存里改流速：`mode` 0=倍率缩放，1=直接替换。返回是否改动过。
pub fn apply_speed_json(root: &mut Json, mode: i32, value: f64) -> bool {
    let mut modified = false;
    fn apply(v: &mut Json, mode: i32, value: f64, modified: &mut bool) {
        if !v.is_number() {
            return;
        }
        let n = v.as_f64().unwrap_or(0.0);
        *v = json!(if mode == 0 { n * value } else { value });
        *modified = true;
    }
    if let Some(lines) = root.get_mut("judgeLineList").and_then(|v| v.as_array_mut()) {
        for line in lines.iter_mut() {
            if let Some(evts) = line.get_mut("speedEvents").and_then(|v| v.as_array_mut()) {
                for evt in evts.iter_mut() {
                    if let Some(val) = evt.get_mut("value") {
                        apply(val, mode, value, &mut modified);
                    }
                }
            }
            for key in ["notesAbove", "notesBelow", "notes"] {
                if let Some(arr) = line.get_mut(key).and_then(|v| v.as_array_mut()) {
                    for note in arr.iter_mut() {
                        if let Some(sp) = note.get_mut("speed") {
                            apply(sp, mode, value, &mut modified);
                        }
                    }
                }
            }
        }
    }
    if let Some(evts) = root.get_mut("speedEvents").and_then(|v| v.as_array_mut()) {
        for evt in evts.iter_mut() {
            if let Some(val) = evt.get_mut("value") {
                apply(val, mode, value, &mut modified);
            }
        }
    }
    modified
}

// ============================================================
//  [功能 2.5] 事件编辑
// ============================================================

/// 对单个数值应用 start/end 操作（op: 0=不变 1=设为 2=加 3=乘），再乘整体倍率。
fn apply_event_op(v: &mut Json, op: i32, val: f64, multiply: f64) -> bool {
    if !v.is_number() {
        return false;
    }
    let mut cur = v.as_f64().unwrap_or(0.0);
    match op {
        1 => cur = val,
        2 => cur += val,
        3 => cur *= val,
        _ => {}
    }
    if multiply != 1.0 {
        cur *= multiply;
    }
    *v = json!(cur);
    true
}

/// 在内存里批量修改事件数值与缓动。
/// `event_type`: 0=全部 1=moveX 2=moveY 3=rotate 4=alpha 5=speed。
/// `easing`: -1=不变，0..=29=替换缓动类型（仅 RPE 非 speed 事件）。
#[allow(clippy::too_many_arguments)]
pub fn edit_events_json(
    root: &mut Json,
    is_rpe: bool,
    event_type: i32,
    start_op: i32,
    start_val: f64,
    end_op: i32,
    end_val: f64,
    multiply: f64,
    easing: i32,
) -> bool {
    let mut modified = false;

    // 嵌套 fn：不捕获任何变量 → 不会和 `modified` 的可变借用打架。
    #[allow(clippy::too_many_arguments)]
    fn apply_event_obj(evt: &mut Json, is_speed: bool, start_op: i32, start_val: f64, end_op: i32, end_val: f64, multiply: f64, easing: i32) -> bool {
        if !evt.is_object() {
            return false;
        }
        let mut m = false;
        if is_speed {
            if let Some(val) = evt.get_mut("value") {
                if val.is_number() {
                    return apply_event_op(val, start_op, start_val, multiply);
                }
            }
            if let Some(st) = evt.get_mut("start") {
                m |= apply_event_op(st, start_op, start_val, multiply);
            }
            if let Some(en) = evt.get_mut("end") {
                m |= apply_event_op(en, end_op, end_val, multiply);
            }
            return m;
        }
        if let Some(st) = evt.get_mut("start") {
            m |= apply_event_op(st, start_op, start_val, multiply);
        }
        if let Some(en) = evt.get_mut("end") {
            m |= apply_event_op(en, end_op, end_val, multiply);
        }
        if easing >= 0 && evt.get("easingType").is_some() {
            evt["easingType"] = json!(easing);
            m = true;
        }
        m
    }

    #[allow(clippy::too_many_arguments)]
    fn edit_arr(arr: &mut Json, is_speed: bool, start_op: i32, start_val: f64, end_op: i32, end_val: f64, multiply: f64, easing: i32) -> bool {
        let mut m = false;
        if let Some(list) = arr.as_array_mut() {
            for evt in list.iter_mut() {
                m |= apply_event_obj(evt, is_speed, start_op, start_val, end_op, end_val, multiply, easing);
            }
        }
        m
    }

    if let Some(lines) = root.get_mut("judgeLineList").and_then(|v| v.as_array_mut()) {
        for line in lines.iter_mut() {
            if !is_rpe {
                let keys: [(&str, i32, bool); 4] = [
                    ("judgeLineMoveEvents", 1, false),
                    ("judgeLineRotateEvents", 3, false),
                    ("judgeLineDisappearEvents", 4, false),
                    ("speedEvents", 5, true),
                ];
                for (key, t, is_speed) in keys {
                    if event_type == 0 || event_type == t {
                        if let Some(arr) = line.get_mut(key) {
                            modified |= edit_arr(arr, is_speed, start_op, start_val, end_op, end_val, multiply, easing);
                        }
                    }
                }
                // moveY：改 start2 / end2
                if event_type == 0 || event_type == 2 {
                    if let Some(arr) = line.get_mut("judgeLineMoveEvents").and_then(|v| v.as_array_mut()) {
                        for evt in arr.iter_mut() {
                            if let Some(v) = evt.get_mut("start2") {
                                modified |= apply_event_op(v, start_op, start_val, multiply);
                            }
                            if let Some(v) = evt.get_mut("end2") {
                                modified |= apply_event_op(v, end_op, end_val, multiply);
                            }
                        }
                    }
                }
            } else if let Some(layers) = line.get_mut("eventLayers").and_then(|v| v.as_array_mut()) {
                for layer in layers.iter_mut() {
                    if !layer.is_object() {
                        continue;
                    }
                    let keys: [(&str, i32, bool); 5] = [
                        ("moveXEvents", 1, false),
                        ("moveYEvents", 2, false),
                        ("rotateEvents", 3, false),
                        ("alphaEvents", 4, false),
                        ("speedEvents", 5, true),
                    ];
                    for (key, t, is_speed) in keys {
                        if event_type == 0 || event_type == t {
                            if let Some(arr) = layer.get_mut(key) {
                                modified |= edit_arr(arr, is_speed, start_op, start_val, end_op, end_val, multiply, easing);
                            }
                        }
                    }
                }
            }
        }
    }
    // 顶层速度事件（仅非 RPE）
    if !is_rpe && (event_type == 0 || event_type == 5) {
        if let Some(arr) = root.get_mut("speedEvents") {
            modified |= edit_arr(arr, true, start_op, start_val, end_op, end_val, multiply, easing);
        }
    }
    modified
}

// ============================================================
//  一个极简确定性 RNG（避免为随机化引入新依赖）
// ============================================================

struct Rng64(u64);

impl Rng64 {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.unit() * (hi - lo)
    }
    fn range_i(&mut self, lo: i64, hi: i64) -> i64 {
        let span = (hi - lo + 1).max(1) as u64;
        lo + (self.next_u64() % span) as i64
    }
}

// ============================================================
//  [功能 2.7] 分析 / 创意工具
// ============================================================

/// 谱面统计。
#[derive(Default)]
pub struct ChartStats {
    pub total: i64,
    pub tap: i64,
    pub drag: i64,
    pub hold: i64,
    pub flick: i64,
    pub tap_pct: f64,
    pub drag_pct: f64,
    pub flick_pct: f64,
    pub peak_density: i64,
}

/// RPE 音符类型编号 → Phira 编号（`phira_to_rpe_type` 的逆）。
fn rpe_to_phira_type(t: i64) -> i64 {
    match t {
        RPE_TAP => PHIRA_TAP,
        RPE_DRAG => PHIRA_DRAG,
        RPE_HOLD => PHIRA_HOLD,
        RPE_FLICK => PHIRA_FLICK,
        _ => t,
    }
}

/// 从谱面 json 读 BPM：顶层 `bpm` → `BPMList[0].bpm` → `META.bpm` → 首条判定线 `bpm` → 140。
fn chart_bpm_of(data: &Json) -> f64 {
    data.get("bpm")
        .and_then(|v| v.as_f64())
        .or_else(|| {
            data.get("BPMList")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|b| b.get("bpm"))
                .and_then(|v| v.as_f64())
        })
        .or_else(|| data.get("META").and_then(|m| m.get("bpm")).and_then(|v| v.as_f64()))
        .or_else(|| {
            data.get("judgeLineList")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|l| l.get("bpm"))
                .and_then(|v| v.as_f64())
        })
        .filter(|b| *b > 0.0)
        .unwrap_or(140.0)
}

/// 统计音符数 / 占比 / 1 秒窗口峰值密度。
///
/// ⚠️ 两处容易搞错，都在这里统一处理：
/// * **音符时间单位是 1/32 拍**（不是毫秒）——phira 的 `time` 与 RPE 的 `startTime`
///   都是这个单位，「1 秒」要按 BPM 换算成 `32 * bpm / 60` 个 tick。
/// * **RPE 与 phira 的类型编号不同**（RPE: 1Tap/2长条/3Flick/4Drag）——统计前统一映射。
pub fn analyze_chart(data: &Json) -> ChartStats {
    let mut stats = ChartStats::default();
    let mut times: Vec<f64> = Vec::new();
    let is_rpe = is_rpe_chart(data);
    if let Some(lines) = data.get("judgeLineList").and_then(|v| v.as_array()) {
        for line in lines {
            for key in ["notesAbove", "notesBelow", "notes"] {
                if let Some(notes) = line.get(key).and_then(|v| v.as_array()) {
                    for note in notes {
                        let raw = note.get("type").and_then(|v| v.as_i64()).unwrap_or(0);
                        let ty = if is_rpe { rpe_to_phira_type(raw) } else { raw };
                        if !(1..=4).contains(&ty) {
                            continue;
                        }
                        stats.total += 1;
                        match ty {
                            PHIRA_TAP => stats.tap += 1,
                            PHIRA_DRAG => stats.drag += 1,
                            PHIRA_HOLD => stats.hold += 1,
                            _ => stats.flick += 1,
                        }
                        times.push(if is_rpe {
                            rpe_time_to_32nd(note.get("startTime").unwrap_or(&Json::Null))
                        } else {
                            note.get("time").and_then(|v| v.as_f64()).unwrap_or(0.0)
                        });
                    }
                }
            }
        }
    }
    if stats.total > 0 {
        let n = stats.total as f64;
        stats.tap_pct = stats.tap as f64 / n * 100.0;
        stats.drag_pct = stats.drag as f64 / n * 100.0;
        stats.flick_pct = stats.flick as f64 / n * 100.0;
    }
    if !times.is_empty() {
        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        // 1 秒 = 32 * bpm / 60 个「1/32 拍」
        let window = (32.0 * chart_bpm_of(data) / 60.0).max(1.0);
        let mut j = 0usize;
        for i in 0..times.len() {
            while j < times.len() && times[j] - times[i] <= window {
                j += 1;
            }
            let count = (j - i) as i64;
            if count > stats.peak_density {
                stats.peak_density = count;
            }
        }
    }
    stats
}

/// 难度转换（EZ/HD）：长条→Tap 并按步长抽稀。返回是否改动。
pub fn convert_difficulty(data: &mut Json, target: &str) -> bool {
    if target != "EZ" && target != "HD" {
        return false;
    }
    let keep_ratio = if target == "EZ" { 0.5 } else { 0.7 };
    let step = ((1.0 / keep_ratio) as i32).max(1);
    let is_rpe = is_rpe_chart(data);
    let hold_type = if is_rpe { 2 } else { 3 };
    let tap_type = 1;

    let mut modified = false;
    let Some(lines) = data.get_mut("judgeLineList").and_then(|v| v.as_array_mut()) else {
        return false;
    };
    for line in lines.iter_mut() {
        // 收集该线全部音符
        let mut all: Vec<Json> = Vec::new();
        for key in ["notesAbove", "notesBelow", "notes"] {
            if let Some(arr) = line.get(key).and_then(|v| v.as_array()) {
                all.extend(arr.iter().cloned());
            }
        }
        // 按时间稳定排序
        all.sort_by(|a, b| {
            let ta = if is_rpe {
                rpe_time_to_32nd(a.get("startTime").unwrap_or(&Json::Null))
            } else {
                a.get("time").and_then(|v| v.as_f64()).unwrap_or(0.0)
            };
            let tb = if is_rpe {
                rpe_time_to_32nd(b.get("startTime").unwrap_or(&Json::Null))
            } else {
                b.get("time").and_then(|v| v.as_f64()).unwrap_or(0.0)
            };
            ta.partial_cmp(&tb).unwrap_or(std::cmp::Ordering::Equal)
        });
        // 长条 → Tap
        for n in all.iter_mut() {
            if n.get("type").and_then(|v| v.as_i64()) == Some(hold_type) {
                n["type"] = json!(tap_type);
            }
        }
        // 按步长抽稀
        let filtered: Vec<Json> = all
            .into_iter()
            .enumerate()
            .filter(|(i, _)| (*i as i32) % step == 0)
            .map(|(_, n)| n)
            .collect();
        // 按原 key 的长度切回去
        let mut idx = 0usize;
        for key in ["notesAbove", "notesBelow", "notes"] {
            let Some(arr) = line.get(key).and_then(|v| v.as_array()) else { continue };
            let count = arr.len();
            let sliced: Vec<Json> = (idx..idx + count).filter_map(|i| filtered.get(i).cloned()).collect();
            line[key] = Json::Array(sliced);
            idx += count;
        }
        modified = true;
    }
    modified
}

/// 音符随机化（位置 + 类型）。
pub fn randomize_notes(data: &mut Json, seed: u64) -> bool {
    let is_rpe = is_rpe_chart(data);
    let mut rng = Rng64::new(seed);
    let mut modified = false;
    for_each_note_array(data, |notes| {
        if let Some(arr) = notes.as_array_mut() {
            for note in arr.iter_mut() {
                note["positionX"] = json!(if is_rpe { rng.range(-500.0, 500.0) } else { rng.range(0.0, 1.0) });
                note["type"] = json!(rng.range_i(1, 4));
                modified = true;
            }
        }
    });
    modified
}

/// 镜像（水平）：RPE 取反 positionX，Phira 用 `1 - positionX`。
pub fn mirror_chart(data: &mut Json, horizontal: bool) -> bool {
    let is_rpe = is_rpe_chart(data);
    let mut modified = false;
    for_each_note_array(data, |notes| {
        if let Some(arr) = notes.as_array_mut() {
            for note in arr.iter_mut() {
                if horizontal {
                    if let Some(px) = note.get("positionX").and_then(|v| v.as_f64()) {
                        note["positionX"] = json!(if is_rpe { -px } else { 1.0 - px });
                        modified = true;
                    }
                }
            }
        }
    });
    modified
}

// ============================================================
//  [功能 2.8] BPM 重采样
// ============================================================

fn scale_time_field(v: &mut Json, ratio: f64) {
    if let Some(n) = v.as_f64() {
        *v = json!((n * ratio) as i64);
    }
}

/// BPM 重采样（时间轴按比例缩放）。返回是否改动。
pub fn resample_bpm(data: &mut Json, original_bpm: f64, target_bpm: f64) -> bool {
    if target_bpm <= 0.0 || original_bpm == target_bpm {
        return false;
    }
    let ratio = original_bpm / target_bpm;
    data["bpm"] = json!(target_bpm);
    if let Some(lines) = data.get_mut("judgeLineList").and_then(|v| v.as_array_mut()) {
        for line in lines.iter_mut() {
            for key in ["notesAbove", "notesBelow", "notes"] {
                if let Some(arr) = line.get_mut(key).and_then(|v| v.as_array_mut()) {
                    for note in arr.iter_mut() {
                        if let Some(t) = note.get_mut("time") {
                            scale_time_field(t, ratio);
                        }
                        if let Some(t) = note.get_mut("holdTime") {
                            scale_time_field(t, ratio);
                        }
                    }
                }
            }
            for key in ["speedEvents", "judgeLineMoveEvents", "judgeLineRotateEvents", "judgeLineDisappearEvents"] {
                if let Some(arr) = line.get_mut(key).and_then(|v| v.as_array_mut()) {
                    for evt in arr.iter_mut() {
                        if let Some(t) = evt.get_mut("startTime") {
                            scale_time_field(t, ratio);
                        }
                        if let Some(t) = evt.get_mut("endTime") {
                            scale_time_field(t, ratio);
                        }
                    }
                }
            }
        }
    }
    if let Some(arr) = data.get_mut("speedEvents").and_then(|v| v.as_array_mut()) {
        for evt in arr.iter_mut() {
            if let Some(t) = evt.get_mut("startTime") {
                scale_time_field(t, ratio);
            }
            if let Some(t) = evt.get_mut("endTime") {
                scale_time_field(t, ratio);
            }
        }
    }
    true
}

// ============================================================
//  [功能 4] 铺面偏移复制（Phira 格式，时间字段为数字）
// ============================================================

fn offset_lines(root: &mut Json, repeat_count: i32, offset32: i64, note_type_override: i64) {
    let base = root.get("judgeLineList").cloned().unwrap_or(Json::Array(vec![]));
    let Some(base_lines) = base.as_array() else { return };
    for i in 1..=repeat_count {
        let shift = i as i64 * offset32;
        for line in base_lines {
            let mut copied = line.clone();
            for key in ["speedEvents", "judgeLineMoveEvents", "judgeLineRotateEvents", "judgeLineDisappearEvents"] {
                if let Some(arr) = copied.get_mut(key).and_then(|v| v.as_array_mut()) {
                    for evt in arr.iter_mut() {
                        if let Some(t) = evt.get_mut("startTime").and_then(|v| v.as_i64()) {
                            evt["startTime"] = json!(t + shift);
                        }
                        if let Some(t) = evt.get_mut("endTime").and_then(|v| v.as_i64()) {
                            evt["endTime"] = json!(t + shift);
                        }
                    }
                }
            }
            for key in ["notesAbove", "notesBelow", "notes"] {
                if let Some(arr) = copied.get_mut(key).and_then(|v| v.as_array_mut()) {
                    for note in arr.iter_mut() {
                        if let Some(t) = note.get_mut("time").and_then(|v| v.as_i64()) {
                            note["time"] = json!(t + shift);
                        }
                        if note_type_override > 0 {
                            note["type"] = json!(note_type_override);
                        }
                    }
                }
            }
            if let Some(list) = root.get_mut("judgeLineList").and_then(|v| v.as_array_mut()) {
                list.push(copied);
            }
        }
    }
}

/// 铺面偏移复制。返回是否改动。
pub fn apply_offset_json(root: &mut Json, repeat_count: i32, offset_beats: f64, note_type_override: i64) -> bool {
    if repeat_count <= 0 || offset_beats <= 0.0 {
        return false;
    }
    if root.get("judgeLineList").and_then(|v| v.as_array()).is_none() {
        return false;
    }
    let offset32 = (offset_beats * 32.0) as i64;
    offset_lines(root, repeat_count, offset32, note_type_override);
    true
}

// ============================================================
//  [功能 3] 长条切割
// ============================================================

/// 长条切割（内存版）。`target_type` 用 Phira 编号。返回是否改动。
pub fn slice_holds_json(data: &mut Json, beats_per_slice: f64, target_type: i64) -> bool {
    if beats_per_slice <= 0.0 {
        return false;
    }
    let is_rpe = is_rpe_chart(data);
    let root_bpm = {
        let b = data.get("bpm").and_then(|v| v.as_f64()).unwrap_or(140.0);
        if b <= 0.0 { 140.0 } else { b }
    };
    let out_type = if is_rpe { phira_to_rpe_type(target_type) } else { target_type };

    let mut modified = false;
    let Some(lines) = data.get_mut("judgeLineList").and_then(|v| v.as_array_mut()) else {
        return false;
    };
    for line in lines.iter_mut() {
        let bpm = {
            let b = line.get("bpm").and_then(|v| v.as_f64()).unwrap_or(root_bpm);
            if b <= 0.0 { root_bpm } else { b }
        };
        // 先把 speedEvents 克隆出来，避免后面可变借用冲突（Phira 重算 floorPosition 要用）
        let speed_events = line.get("speedEvents").cloned();
        for key in ["notesAbove", "notesBelow", "notes"] {
            let Some(arr) = line.get(key).and_then(|v| v.as_array()) else { continue };
            let notes = arr.clone();
            let mut new_notes: Vec<Json> = Vec::new();
            for note in &notes {
                let mut t_start = 0.0f64;
                let mut t_end = 0.0f64;
                let mut is_hold = false;
                if is_rpe {
                    if let Some(st) = note.get("startTime") {
                        if st.is_array() {
                            t_start = rpe_time_to_32nd(st);
                            t_end = rpe_time_to_32nd(note.get("endTime").unwrap_or(&Json::Null));
                            if note.get("type").and_then(|v| v.as_i64()) == Some(2) && t_end > t_start + 1e-6 {
                                is_hold = true;
                            }
                        }
                    }
                } else {
                    // 时间字段可能是整数或浮点，统一用 `as_f64`（`as_i64` 遇到浮点会返回 None）
                    let time = note.get("time").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let hold_time = note.get("holdTime").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let end_time = note.get("endTime").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    t_start = time;
                    if hold_time > 0.0 {
                        t_end = time + hold_time;
                        is_hold = true;
                    } else if end_time > time {
                        t_end = end_time;
                        is_hold = true;
                    }
                }
                if is_hold && t_end > t_start {
                    let duration = t_end - t_start;
                    let beats = duration / 32.0;
                    let fill_n = (beats_per_slice.ceil() as i64).max(1);
                    let mut total_pieces_ll = (beats * beats_per_slice).ceil() as i64;
                    if total_pieces_ll < fill_n {
                        total_pieces_ll = fill_n;
                    }
                    let mut den = 32i64;
                    let max_unique;
                    if is_rpe {
                        den = rpe_den_for_total(total_pieces_ll, beats);
                        let cap = (duration * den as f64 / 32.0).floor() as i64;
                        max_unique = cap.max(1);
                    } else {
                        max_unique = (duration.floor() as i64).max(1);
                    }
                    if total_pieces_ll > max_unique {
                        total_pieces_ll = max_unique;
                    }
                    let total_pieces = total_pieces_ll.max(1);
                    let has_floor = note.get("floorPosition").is_some();
                    for i in 0..total_pieces {
                        let mut new_note = note.clone();
                        let tt = t_start + i as f64 * duration / total_pieces as f64;
                        if is_rpe {
                            let arr = time32_to_rpe_time_array_ex(tt, den);
                            new_note["startTime"] = arr.clone();
                            new_note["endTime"] = arr;
                        } else {
                            new_note["time"] = json!(tt.round() as i64);
                            new_note["holdTime"] = json!(0);
                            new_note["endTime"] = json!(0);
                            if has_floor {
                                let evts = speed_events.as_ref().unwrap_or(&Json::Null);
                                new_note["floorPosition"] = json!(recompute_floor_from_events(evts, bpm, tt));
                            }
                        }
                        new_note["type"] = json!(out_type);
                        new_note["speed"] = json!(1.0);
                        new_notes.push(new_note);
                    }
                    modified = true;
                } else {
                    new_notes.push(note.clone());
                }
            }
            line[key] = Json::Array(new_notes);
        }
    }
    modified
}

// ============================================================
//  [功能 5] RPE → Phira 引擎格式转换
// ============================================================

/// 把一份 RPE 谱面转成 Phira 引擎格式（供官谱引擎播放）。
pub fn convert_rpe_to_phira(rpe_data: &Json) -> Json {
    let main_bpm = rpe_data
        .get("BPMList")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|b| b.get("bpm"))
        .and_then(|v| v.as_f64())
        .unwrap_or(140.0);

    let mut phira = json!({
        "formatVersion": 3,
        "offset": rpe_data.get("META").and_then(|m| m.get("offset")).and_then(|v| v.as_f64()).unwrap_or(0.0),
        "bpm": main_bpm,
        "judgeLineList": [],
    });

    let Some(rpe_lines) = rpe_data.get("judgeLineList").and_then(|v| v.as_array()) else {
        return phira;
    };
    for rpe_line in rpe_lines {
        // 自适应线宽：|positionX| 超过 500 视为 1350 宽，否则 1000 宽
        let mut max_abs = 0.0f64;
        if let Some(notes) = rpe_line.get("notes").and_then(|v| v.as_array()) {
            for note in notes {
                let a = note.get("positionX").and_then(|v| v.as_f64()).unwrap_or(0.0).abs();
                max_abs = max_abs.max(a);
            }
        }
        let line_half = if max_abs > 500.0 { 675.0 } else { 500.0 };
        let line_w = line_half * 2.0;

        let mut line = json!({
            "bpm": main_bpm,
            "notesAbove": [],
            "notesBelow": [],
            "speedEvents": [],
            "judgeLineMoveEvents": [],
            "judgeLineRotateEvents": [],
            "judgeLineDisappearEvents": [],
        });

        if let Some(notes) = rpe_line.get("notes").and_then(|v| v.as_array()) {
            for note in notes {
                let start32 = rpe_time_to_32nd(note.get("startTime").unwrap_or(&json!([0, 0, 0])));
                let end32 = rpe_time_to_32nd(note.get("endTime").unwrap_or(&json!([0, 0, 0])));
                let hold32 = end32 - start32;
                let rpe_type = note.get("type").and_then(|v| v.as_i64()).unwrap_or(1);
                let phira_type = if hold32 <= 1e-6 {
                    match rpe_type {
                        RPE_TAP => PHIRA_TAP,
                        RPE_DRAG => PHIRA_DRAG,
                        RPE_FLICK => PHIRA_FLICK,
                        _ => PHIRA_TAP,
                    }
                } else {
                    PHIRA_HOLD
                };
                let pos_x = note.get("positionX").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let phira_pos = (pos_x + line_half) / line_w;
                let n = json!({
                    "type": phira_type,
                    "time": start32.round() as i64,
                    "positionX": phira_pos,
                    "holdTime": hold32.round() as i64,
                    "speed": note.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0),
                    "floorPosition": 0.0,
                });
                let above = note.get("above").and_then(|v| v.as_i64()).unwrap_or(1);
                let key = if above != 2 { "notesAbove" } else { "notesBelow" };
                if let Some(arr) = line.get_mut(key).and_then(|v| v.as_array_mut()) {
                    arr.push(n);
                }
            }
        }

        // 事件转换（第 0 层）
        if let Some(layers) = rpe_line.get("eventLayers").and_then(|v| v.as_array()) {
            for layer in layers {
                if layer.is_null() {
                    continue;
                }
                let empty = json!([]);
                let mx = layer.get("moveXEvents").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let my = layer.get("moveYEvents").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let move_count = mx.len().max(my.len());
                let _ = empty;
                for i in 0..move_count {
                    let has_x = i < mx.len();
                    let has_y = i < my.len();
                    let (st, et, s, e) = if has_x {
                        (
                            rpe_time_to_32nd(mx[i].get("startTime").unwrap_or(&json!([0, 0, 0]))),
                            rpe_time_to_32nd(mx[i].get("endTime").unwrap_or(&json!([0, 0, 0]))),
                            (mx[i].get("start").and_then(|v| v.as_f64()).unwrap_or(0.0) + line_half) / line_w,
                            (mx[i].get("end").and_then(|v| v.as_f64()).unwrap_or(0.0) + line_half) / line_w,
                        )
                    } else {
                        (
                            rpe_time_to_32nd(my[i].get("startTime").unwrap_or(&json!([0, 0, 0]))),
                            rpe_time_to_32nd(my[i].get("endTime").unwrap_or(&json!([0, 0, 0]))),
                            0.5,
                            0.5,
                        )
                    };
                    let (s2, e2) = if has_y {
                        (
                            (my[i].get("start").and_then(|v| v.as_f64()).unwrap_or(0.0) + 450.0) / 900.0,
                            (my[i].get("end").and_then(|v| v.as_f64()).unwrap_or(0.0) + 450.0) / 900.0,
                        )
                    } else {
                        (0.5, 0.5)
                    };
                    let e_obj = json!({ "startTime": st, "endTime": et, "start": s, "end": e, "start2": s2, "end2": e2 });
                    if let Some(arr) = line.get_mut("judgeLineMoveEvents").and_then(|v| v.as_array_mut()) {
                        arr.push(e_obj);
                    }
                }
                for evt in layer.get("rotateEvents").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                    let e = json!({
                        "startTime": rpe_time_to_32nd(evt.get("startTime").unwrap_or(&json!([0, 0, 0]))),
                        "endTime": rpe_time_to_32nd(evt.get("endTime").unwrap_or(&json!([0, 0, 0]))),
                        "start": evt.get("start").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "end": evt.get("end").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    });
                    if let Some(arr) = line.get_mut("judgeLineRotateEvents").and_then(|v| v.as_array_mut()) {
                        arr.push(e);
                    }
                }
                for evt in layer.get("alphaEvents").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                    let e = json!({
                        "startTime": rpe_time_to_32nd(evt.get("startTime").unwrap_or(&json!([0, 0, 0]))),
                        "endTime": rpe_time_to_32nd(evt.get("endTime").unwrap_or(&json!([0, 0, 0]))),
                        "start": evt.get("start").and_then(|v| v.as_f64()).unwrap_or(255.0) / 255.0,
                        "end": evt.get("end").and_then(|v| v.as_f64()).unwrap_or(255.0) / 255.0,
                    });
                    if let Some(arr) = line.get_mut("judgeLineDisappearEvents").and_then(|v| v.as_array_mut()) {
                        arr.push(e);
                    }
                }
                for evt in layer.get("speedEvents").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                    let val = evt
                        .get("start")
                        .and_then(|v| v.as_f64())
                        .or_else(|| evt.get("value").and_then(|v| v.as_f64()))
                        .unwrap_or(1.0);
                    let e = json!({
                        "startTime": rpe_time_to_32nd(evt.get("startTime").unwrap_or(&json!([0, 0, 0]))),
                        "endTime": rpe_time_to_32nd(evt.get("endTime").unwrap_or(&json!([0, 0, 0]))),
                        "value": val,
                    });
                    if let Some(arr) = line.get_mut("speedEvents").and_then(|v| v.as_array_mut()) {
                        arr.push(e);
                    }
                }
            }
        }
        if let Some(arr) = phira.get_mut("judgeLineList").and_then(|v| v.as_array_mut()) {
            arr.push(line);
        }
    }
    phira
}

// ============================================================
//  [功能 6] 铺面融合
// ============================================================

/// 融合结果。
pub struct FusionResult {
    pub data: Json,
    pub name: String,
    pub error: String,
    pub ok: bool,
}

/// 合并多个谱面（同格式；BPM 不同则重采样到基准 BPM）。
pub fn fusion_merge(files: &[Json], base_idx: i32) -> FusionResult {
    let mut result = FusionResult {
        data: Json::Null,
        name: String::new(),
        error: String::new(),
        ok: false,
    };
    if files.len() < 2 {
        result.error = "需要至少 2 个谱面文件才能合并".into();
        return result;
    }
    if base_idx < 0 || base_idx as usize >= files.len() {
        result.error = "请选择有效的基准谱面".into();
        return result;
    }
    let fmt_of = |d: &Json| -> &'static str {
        if d.get("BPMList").is_some() {
            "rpe"
        } else {
            "phira"
        }
    };
    let base_format = fmt_of(&files[base_idx as usize]);
    if files.iter().any(|f| fmt_of(f) != base_format) {
        result.error = "铺面融合仅支持同格式之间的合并（RPE 只能合并 RPE，Phira 只能合并 Phira）。".into();
        return result;
    }

    let mut base = files[base_idx as usize].clone();
    let base_bpm = base.get("bpm").and_then(|v| v.as_f64()).unwrap_or(140.0);
    let mut name_parts = vec![base.get("__name__").and_then(|v| v.as_str()).unwrap_or("chart").to_owned()];

    for (i, f) in files.iter().enumerate() {
        if i as i32 == base_idx {
            continue;
        }
        let mut add = f.clone();
        let add_bpm = add.get("bpm").and_then(|v| v.as_f64()).unwrap_or(140.0);
        if add_bpm != base_bpm {
            resample_bpm(&mut add, add_bpm, base_bpm);
        }
        if let Some(add_lines) = add.get_mut("judgeLineList").and_then(|v| v.as_array_mut()) {
            if base.get("judgeLineList").and_then(|v| v.as_array()).is_none() {
                base["judgeLineList"] = json!([]);
            }
            if let Some(base_lines) = base.get_mut("judgeLineList").and_then(|v| v.as_array_mut()) {
                base_lines.append(add_lines);
            }
        }
        if let Some(add_evts) = add.get_mut("speedEvents").and_then(|v| v.as_array_mut()) {
            if base.get("speedEvents").and_then(|v| v.as_array()).is_none() {
                base["speedEvents"] = json!([]);
            }
            if let Some(base_evts) = base.get_mut("speedEvents").and_then(|v| v.as_array_mut()) {
                base_evts.append(add_evts);
            }
        }
        let add_fv = add.get("formatVersion").and_then(|v| v.as_i64()).unwrap_or(0);
        let base_fv = base.get("formatVersion").and_then(|v| v.as_i64()).unwrap_or(0);
        if add_fv > base_fv {
            base["formatVersion"] = add["formatVersion"].clone();
        }
        name_parts.push(add.get("__name__").and_then(|v| v.as_str()).unwrap_or("chart").to_owned());
    }
    base["bpm"] = json!(base_bpm);
    if let Some(obj) = base.as_object_mut() {
        obj.remove("__name__");
    }
    result.name = format!("{}.json", name_parts.join("+"));
    result.data = base;
    result.ok = true;
    result
}

// ============================================================
//  [功能 7] 导出 PEZ（把谱面目录打成 zip）
// ============================================================

/// 把 `dir` 目录打包成 PEZ（zip）到 `out`。
pub fn export_to_pez(dir: &Path, out: &Path) -> Result<()> {
    use std::io::Write;
    let file = std::fs::File::create(out)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in walkdir::WalkDir::new(dir).into_iter().flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let name = p.strip_prefix(dir).unwrap_or(p).to_string_lossy().replace('\\', "/");
        zip.start_file(name, opts)?;
        let data = std::fs::read(p)?;
        zip.write_all(&data)?;
    }
    zip.finish()?;
    Ok(())
}

// ============================================================
//  批量处理（std::thread::scope 并行）
// ============================================================

/// 批量处理结果。
pub struct BatchResult {
    pub ok: i32,
    pub failed: i32,
}

/// 对一组文件并行跑一个操作。`threads` 会被夹到 1..=16。
pub fn run_batch<F>(files: &[PathBuf], threads: i32, op: F) -> BatchResult
where
    F: Fn(&Path) -> Result<()> + Sync,
{
    use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
    let ok = AtomicI32::new(0);
    let failed = AtomicI32::new(0);
    let n_threads = threads.clamp(1, 16) as usize;
    if n_threads == 1 || files.len() <= 1 {
        for p in files {
            match op(p) {
                Ok(_) => {
                    ok.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    } else {
        let counter = AtomicUsize::new(0);
        let n = n_threads.min(files.len());
        std::thread::scope(|s| {
            for _ in 0..n {
                s.spawn(|| loop {
                    let idx = counter.fetch_add(1, Ordering::Relaxed);
                    if idx >= files.len() {
                        break;
                    }
                    match op(&files[idx]) {
                        Ok(_) => {
                            ok.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
    }
    BatchResult {
        ok: ok.load(Ordering::Relaxed),
        failed: failed.load(Ordering::Relaxed),
    }
}

// ============================================================
//  [功能 2.6] 轨道化（合并全部判定线到 0 号线，note 映射到 K 轨道）
// ============================================================

// ---- RPE 缓动（phire TWEEN_FUNCTIONS 语义，id 2..32） ----
fn e_sine(x: f64) -> f64 {
    1.0 - (x * std::f64::consts::PI / 2.0).cos()
}
fn e_quad(x: f64) -> f64 {
    x * x
}
fn e_cubic(x: f64) -> f64 {
    x * x * x
}
fn e_quart(x: f64) -> f64 {
    x * x * x * x
}
fn e_quint(x: f64) -> f64 {
    x * x * x * x * x
}
fn e_expo(x: f64) -> f64 {
    (2.0f64).powf(10.0 * (x - 1.0))
}
fn e_circ(x: f64) -> f64 {
    1.0 - (1.0 - x * x).sqrt()
}
fn e_back(x: f64) -> f64 {
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    (c3 * x - c1) * x * x
}
fn e_elastic(x: f64) -> f64 {
    let c4 = (2.0 * std::f64::consts::PI) / 3.0;
    -(2.0f64).powf(10.0 * x - 10.0) * ((x * 10.0 - 10.75) * c4).sin()
}
fn e_bounce(x: f64) -> f64 {
    let n1 = 7.5625;
    let d1 = 2.75;
    let x = 1.0 - x;
    let r = if x < 1.0 / d1 {
        n1 * x * x
    } else if x < 2.0 / d1 {
        n1 * (x - 1.5 / d1) * (x - 1.5 / d1) + 0.75
    } else if x < 2.5 / d1 {
        n1 * (x - 2.25 / d1) * (x - 2.25 / d1) + 0.9375
    } else {
        n1 * (x - 2.625 / d1) * (x - 2.625 / d1) + 0.984375
    };
    1.0 - r
}
fn ease_out(x: f64, f: fn(f64) -> f64) -> f64 {
    f(x)
}
fn ease_in(x: f64, f: fn(f64) -> f64) -> f64 {
    1.0 - f(1.0 - x)
}
fn ease_in_out(x: f64, f: fn(f64) -> f64) -> f64 {
    let x = x * 2.0;
    if x < 1.0 {
        f(x) / 2.0
    } else {
        1.0 - f(2.0 - x) / 2.0
    }
}

fn tween_by_id(id: i32, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    match id {
        2 => x,
        3 => ease_out(x, e_sine),
        4 => ease_in(x, e_sine),
        5 => ease_in_out(x, e_sine),
        6 => ease_out(x, e_quad),
        7 => ease_in(x, e_quad),
        8 => ease_in_out(x, e_quad),
        9 => ease_out(x, e_cubic),
        10 => ease_in(x, e_cubic),
        11 => ease_in_out(x, e_cubic),
        12 => ease_out(x, e_quart),
        13 => ease_in(x, e_quart),
        14 => ease_in_out(x, e_quart),
        15 => ease_out(x, e_quint),
        16 => ease_in(x, e_quint),
        17 => ease_in_out(x, e_quint),
        18 => ease_out(x, e_expo),
        19 => ease_in(x, e_expo),
        20 => ease_in_out(x, e_expo),
        21 => ease_out(x, e_circ),
        22 => ease_in(x, e_circ),
        23 => ease_in_out(x, e_circ),
        24 => ease_out(x, e_back),
        25 => ease_in(x, e_back),
        26 => ease_in_out(x, e_back),
        27 => ease_out(x, e_elastic),
        28 => ease_in(x, e_elastic),
        29 => ease_in_out(x, e_elastic),
        30 => ease_out(x, e_bounce),
        31 => ease_in(x, e_bounce),
        32 => ease_in_out(x, e_bounce),
        _ => x,
    }
}

fn rpe_easing_to_id(easing_type: i32) -> i32 {
    const MAP: [i32; 30] = [
        2, 2, 3, 4, 6, 7, 5, 8, 9, 10, 12, 13, 11, 14, 15, 16, 18, 19, 21, 22, 24, 25, 23, 26, 27, 28, 30, 31, 32, 29,
    ];
    if !(0..=29).contains(&easing_type) {
        return 2;
    }
    MAP[easing_type as usize]
}

struct KmKey {
    t: f64,
    v: f64,
    id: i32,
}

fn sample_keys(keys: &[KmKey], t: f64, default: f64) -> f64 {
    if keys.is_empty() {
        return default;
    }
    if t <= keys[0].t {
        return keys[0].v;
    }
    if t >= keys[keys.len() - 1].t {
        return keys[keys.len() - 1].v;
    }
    for i in 0..keys.len() - 1 {
        if t >= keys[i].t && t <= keys[i + 1].t {
            let span = keys[i + 1].t - keys[i].t;
            let p = if span > 1e-9 { (t - keys[i].t) / span } else { 0.0 };
            let e = tween_by_id(keys[i].id, p);
            return keys[i].v + (keys[i + 1].v - keys[i].v) * e;
        }
    }
    keys[keys.len() - 1].v
}

fn push_key(keys: &mut Vec<KmKey>, t: f64, v: f64, id: i32) {
    for k in keys.iter_mut() {
        if (k.t - t).abs() < 1e-9 {
            k.v = v;
            k.id = id;
            return;
        }
    }
    keys.push(KmKey { t, v, id });
    keys.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
}

fn collect_event_keys(evts: &Json, keys: &mut Vec<KmKey>, default_start: f64, default_end: f64) {
    let Some(arr) = evts.as_array() else { return };
    for e in arr {
        if !e.is_object() {
            continue;
        }
        let st = rpe_time_to_32nd(e.get("startTime").unwrap_or(&json!([0, 0, 0])));
        let et = rpe_time_to_32nd(e.get("endTime").unwrap_or(&json!([0, 0, 0])));
        let sv = e.get("start").and_then(|v| v.as_f64()).unwrap_or(default_start);
        let ev2 = e.get("end").and_then(|v| v.as_f64()).unwrap_or(default_end);
        let et_raw = e.get("easingType").and_then(|v| v.as_f64()).unwrap_or(0.0);
        push_key(keys, st, sv, rpe_easing_to_id(et_raw as i32));
        if et > st + 1e-9 {
            push_key(keys, et, ev2, 2);
        }
    }
}

struct OutNote {
    ty: i64,
    t32: f64,
    end32: f64,
    abs_x: f64,
    _orig_x: f64,
    _line_idx: usize,
    src: Json,
}

/// 轨道化：合并全部判定线到 0 号线，note 按绝对 X 映射到 K 条轨道（仅 RPE 谱面）。
pub fn keymode_json(root: &mut Json, k: i32, line_y: f64, speed: f64, spacing: f64) -> bool {
    let k = k.clamp(1, 8);
    if root.get("judgeLineList").and_then(|v| v.as_array()).is_none() {
        return false;
    }

    let mut notes: Vec<OutNote> = Vec::new();
    if let Some(lines) = root.get("judgeLineList").and_then(|v| v.as_array()) {
        let mut line_idx = 0usize;
        for line in lines {
            if !line.is_object() {
                line_idx += 1;
                continue;
            }
            let mut kx = Vec::new();
            let mut ky = Vec::new();
            let mut kr = Vec::new();
            if let Some(layers) = line.get("eventLayers").and_then(|v| v.as_array()) {
                for layer in layers {
                    if !layer.is_object() {
                        continue;
                    }
                    collect_event_keys(layer.get("moveXEvents").unwrap_or(&Json::Null), &mut kx, 0.0, 0.0);
                    collect_event_keys(layer.get("moveYEvents").unwrap_or(&Json::Null), &mut ky, 0.0, 0.0);
                    collect_event_keys(layer.get("rotateEvents").unwrap_or(&Json::Null), &mut kr, 0.0, 0.0);
                }
            }
            if let Some(ns) = line.get("notes").and_then(|v| v.as_array()) {
                for n in ns {
                    if !n.is_object() {
                        continue;
                    }
                    let t32 = rpe_time_to_32nd(n.get("startTime").unwrap_or(&json!([0, 0, 0])));
                    let end32 = rpe_time_to_32nd(n.get("endTime").unwrap_or(&json!([0, 0, 0])));
                    let lx = sample_keys(&kx, t32, 0.0);
                    let rot = sample_keys(&kr, t32, 0.0);
                    let rel_x = n.get("positionX").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let rad = rot * std::f64::consts::PI / 180.0;
                    let abs_x = lx + rel_x * rad.cos();
                    let mut ty = n.get("type").and_then(|v| v.as_i64()).unwrap_or(1);
                    if !(1..=4).contains(&ty) {
                        ty = 1;
                    }
                    notes.push(OutNote {
                        ty,
                        t32,
                        end32,
                        abs_x,
                        _orig_x: rel_x,
                        _line_idx: line_idx,
                        src: n.clone(),
                    });
                }
            }
            line_idx += 1;
        }
    }
    if notes.is_empty() {
        return false;
    }

    // ---- 轨道分配（防视觉重合）----
    let center0 = (k - 1) as f64 / 2.0;
    let lane_center = |i: i32| (i as f64 - center0) * spacing;
    let vis = 1.0f64;
    let same_x = spacing.abs() * 0.12;

    notes.sort_by(|a, b| a.t32.partial_cmp(&b.t32).unwrap_or(std::cmp::Ordering::Equal));

    // 聚类：同刻 + 相近 X 归一组
    struct NoteGroup {
        t32: f64,
        idx: Vec<usize>,
        rep_abs_x: f64,
    }
    let mut groups: Vec<NoteGroup> = Vec::new();
    for i in 0..notes.len() {
        let mut joined = false;
        for g in groups.iter_mut() {
            if (g.t32 - notes[i].t32).abs() <= 0.25 && (g.rep_abs_x - notes[i].abs_x).abs() <= same_x {
                g.idx.push(i);
                g.rep_abs_x = (g.rep_abs_x * (g.idx.len() - 1) as f64 + notes[i].abs_x) / g.idx.len() as f64;
                joined = true;
                break;
            }
        }
        if !joined {
            groups.push(NoteGroup {
                t32: notes[i].t32,
                rep_abs_x: notes[i].abs_x,
                idx: vec![i],
            });
        }
    }
    groups.sort_by(|a, b| a.t32.partial_cmp(&b.t32).unwrap_or(std::cmp::Ordering::Equal));

    let mut lane_occ = vec![-1e18f64; k as usize];
    let mut lane_src_x = vec![0.0f64; k as usize];
    for g in &groups {
        let mut best = 0i32;
        let mut bd = 1e300;
        for i in 0..k {
            let d = (g.rep_abs_x - lane_center(i)).abs();
            if d < bd {
                bd = d;
                best = i;
            }
        }
        let mut chosen = best;
        if k > 2 {
            if g.t32 >= lane_occ[best as usize] || (lane_src_x[best as usize] - g.rep_abs_x).abs() <= same_x {
                chosen = best;
            } else {
                for step in 1..k {
                    if chosen != best {
                        break;
                    }
                    if best - step >= 0 && g.t32 >= lane_occ[(best - step) as usize] {
                        chosen = best - step;
                    }
                    if chosen != best {
                        break;
                    }
                    if best + step < k && g.t32 >= lane_occ[(best + step) as usize] {
                        chosen = best + step;
                    }
                }
            }
        }
        let target_x = lane_center(chosen);
        let mut g_end = -1e18f64;
        for &ix in &g.idx {
            notes[ix].abs_x = target_x;
            let end = if notes[ix].end32 > notes[ix].t32 {
                notes[ix].end32
            } else {
                notes[ix].t32 + vis
            };
            g_end = g_end.max(end);
        }
        lane_occ[chosen as usize] = lane_occ[chosen as usize].max(g_end);
        lane_src_x[chosen as usize] = g.rep_abs_x;
    }

    // ---- 生成单线 RPE 谱面 ----
    let evt = |start: f64, end: f64| -> Json {
        json!({
            "startTime": time32_to_rpe_time_array(0.0),
            "endTime": time32_to_rpe_time_array(0.0),
            "start": start,
            "end": end,
            "easingType": 0,
            "linkgroup": 0,
            "easingLeft": 0,
            "easingRight": 1,
        })
    };
    let layer = json!({
        "moveXEvents": [evt(0.0, 0.0)],
        "moveYEvents": [evt(line_y, line_y)],
        "rotateEvents": [evt(0.0, 0.0)],
        "alphaEvents": [evt(255.0, 255.0)],
        "speedEvents": [{
            "startTime": time32_to_rpe_time_array(0.0),
            "endTime": time32_to_rpe_time_array(0.0),
            "start": speed,
            "end": speed,
        }],
    });

    let mut out = root.clone();
    let mut line = root
        .get("judgeLineList")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(obj) = line.as_object_mut() {
        obj.insert("id".into(), json!(0));
        obj.insert("Texture".into(), json!("line.png"));
        for key in ["posControl", "sizeControl", "yControl", "skewControl", "alphaControl", "extended"] {
            obj.remove(key);
        }
        obj.insert("eventLayers".into(), json!([layer]));
        obj.insert("notes".into(), json!([]));
    }
    // META.offset 保持整数
    let offset = root
        .get("META")
        .and_then(|m| m.get("offset"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if let Some(meta) = out.get_mut("META").and_then(|v| v.as_object_mut()) {
        meta.insert("offset".into(), json!(offset.round() as i64));
    }
    if let Some(obj) = out.as_object_mut() {
        for key in ["judgeLineGroup", "multiLineString", "multiScale", "xybind"] {
            obj.remove(key);
        }
    }

    let mut out_notes = Vec::new();
    for n in &notes {
        let mut note = json!({
            "type": n.ty,
            "above": 1,
            "startTime": n.src.get("startTime").cloned().unwrap_or_else(|| json!([0, 0, 1])),
            "positionX": (n.abs_x * 1000000.0).round() / 1000000.0,
            "yOffset": 0.0,
            "alpha": 255,
            "size": 1.0,
            "speed": 1.0,
            "isFake": n.src.get("isFake").cloned().unwrap_or(json!(0)),
            "visibleTime": 999999.0,
        });
        let end_time = n.src.get("endTime").cloned().unwrap_or_else(|| note["startTime"].clone());
        note["endTime"] = end_time;
        out_notes.push(note);
    }
    let n_count = out_notes.len();
    if let Some(obj) = line.as_object_mut() {
        obj.insert("notes".into(), Json::Array(out_notes));
        obj.insert("numOfNotes".into(), json!(n_count as i64));
    }
    out["judgeLineList"] = json!([line]);
    *root = out;
    true
}

// ============================================================
//  单元测试（验证从 C++ 移植过来的逻辑）
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 造一份最小 Phira 谱面：1 条判定线，3 个音符（Tap / Hold / Drag）。
    /// `time` 用 1/32 拍单位，`holdTime` 同理。
    fn phira_chart() -> Json {
        json!({
            "formatVersion": 3,
            "offset": 0.0,
            "bpm": 140.0,
            "judgeLineList": [{
                "bpm": 140.0,
                "notesAbove": [
                    { "type": 1, "time": 0,  "positionX": 0.25, "speed": 1.0, "floorPosition": 0.0 },
                    { "type": 3, "time": 32, "positionX": 0.75, "speed": 1.0, "holdTime": 64, "floorPosition": 0.5 }
                ],
                "notesBelow": [
                    { "type": 2, "time": 64, "positionX": 0.5, "speed": 1.0, "floorPosition": 1.0 }
                ],
                "speedEvents": [ { "startTime": 0, "endTime": 1000, "value": 1.0 } ]
            }]
        })
    }

    /// 造一份最小 RPE 谱面（含 eventLayers）。
    fn rpe_chart() -> Json {
        json!({
            "META": { "offset": 0.0 },
            "BPMList": [ { "bpm": 150.0 } ],
            "judgeLineList": [{
                "notes": [
                    { "type": 1, "startTime": [0, 0, 1], "endTime": [0, 0, 1], "positionX": 300.0, "above": 1 }
                ],
                "eventLayers": [{
                    "alphaEvents": [ { "startTime": [0, 0, 1], "endTime": [0, 1, 1], "start": 0.0, "end": 1.0, "easingType": 1 } ]
                }]
            }]
        })
    }

    #[test]
    fn is_rpe_detection() {
        assert!(!is_rpe_chart(&phira_chart()));
        assert!(is_rpe_chart(&rpe_chart()));
        assert!(is_rpe_chart(&json!({ "BPMList": [] })));
        assert!(is_rpe_chart(&json!({ "META": { "offset": 0 } })));
    }

    #[test]
    fn time_utils() {
        assert_eq!(parse_rpe_time_str("[1, 2, 4]"), Some([1, 2, 4]));
        assert_eq!(parse_rpe_time_str("1,2,4"), Some([1, 2, 4]));
        assert_eq!(parse_rpe_time_str("[1,2]"), None);
        // [0,1,4] = 0.25 拍 = 8 个 32 分
        assert!((rpe_time_to_32nd(&json!([0, 1, 4])) - 8.0).abs() < 1e-9);
        // 8 个 32 分 @ 140 BPM = 8/32 拍 = 0.25 拍 → 0.25*60/140 s
        let ms = time32_to_ms(8.0, 140.0);
        assert!((ms - 0.25 * 60.0 / 140.0 * 1000.0).abs() < 1e-6);
    }

    #[test]
    fn note_type_convert_phira() {
        let mut c = phira_chart();
        // Tap(1) → Flick(4)
        assert!(apply_note_changes_json(&mut c, 1, 4, false));
        assert_eq!(c["judgeLineList"][0]["notesAbove"][0]["type"], json!(4));
        // 已经没有 Tap 了 → 不再改动
        assert!(!apply_note_changes_json(&mut c, 1, 4, false));
    }

    #[test]
    fn note_type_convert_rpe_uses_phira_numbering() {
        // 交互用 Phira 编号：3(长条) 在 RPE 里对应 type 2
        let mut c = rpe_chart();
        assert!(apply_note_changes_json(&mut c, 1, 3, true));
        assert_eq!(c["judgeLineList"][0]["notes"][0]["type"], json!(2));
    }

    #[test]
    fn mirror_phira_and_rpe() {
        let mut c = phira_chart();
        assert!(mirror_chart(&mut c, true));
        // Phira：1 - x
        assert_eq!(c["judgeLineList"][0]["notesAbove"][0]["positionX"], json!(0.75));
        assert_eq!(c["judgeLineList"][0]["notesBelow"][0]["positionX"], json!(0.5));
        // RPE：取反
        let mut r = rpe_chart();
        assert!(mirror_chart(&mut r, true));
        assert_eq!(r["judgeLineList"][0]["notes"][0]["positionX"], json!(-300.0));
    }

    #[test]
    fn analyze_counts_and_peak() {
        let s = analyze_chart(&phira_chart());
        assert_eq!(s.total, 3);
        assert_eq!(s.tap, 1);
        assert_eq!(s.drag, 1);
        assert_eq!(s.hold, 1);
        assert_eq!(s.flick, 0);
        // 三个音符时间 0/32/64，BPM 140 → 1 秒 = 74.7 tick，三个都在窗口内
        assert_eq!(s.peak_density, 3);
        assert!((s.tap_pct - 100.0 / 3.0).abs() < 1e-6);
    }

    /// 回归：RPE 谱面的类型编号与时间都要正确处理。
    /// 旧实现按 phira 编号统计、且时间全读成 0 → 这里两条断言都会挂。
    #[test]
    fn analyze_rpe_maps_types_and_window() {
        let mut c = rpe_chart(); // BPM 150 → 1 秒 = 80 tick
        {
            let notes = c["judgeLineList"][0]["notes"].as_array_mut().unwrap();
            // RPE：2 = 长条
            notes.push(json!({ "type": 2, "startTime": [0, 1, 1], "endTime": [0, 2, 1] }));
            // RPE：3 = Flick（两个）
            notes.push(json!({ "type": 3, "startTime": [0, 2, 1], "endTime": [0, 2, 1] }));
            notes.push(json!({ "type": 3, "startTime": [0, 3, 1], "endTime": [0, 3, 1] }));
            // RPE：4 = Drag（三个）
            notes.push(json!({ "type": 4, "startTime": [0, 4, 1], "endTime": [0, 4, 1] }));
            notes.push(json!({ "type": 4, "startTime": [0, 5, 1], "endTime": [0, 5, 1] }));
            notes.push(json!({ "type": 4, "startTime": [0, 6, 1], "endTime": [0, 6, 1] }));
        }
        let s = analyze_chart(&c);
        assert_eq!(s.total, 7);
        assert_eq!(s.tap, 1);
        assert_eq!(s.hold, 1);
        assert_eq!(s.flick, 2);
        assert_eq!(s.drag, 3);
        // 时间 0/32/64/96/128/160/192，窗口 80 → 最多同时 3 个
        assert_eq!(s.peak_density, 3);
    }

    #[test]
    fn speed_scale_then_replace() {
        let mut c = phira_chart();
        assert!(apply_speed_json(&mut c, 0, 2.0)); // 倍率
        assert_eq!(c["judgeLineList"][0]["speedEvents"][0]["value"], json!(2.0));
        assert_eq!(c["judgeLineList"][0]["notesAbove"][0]["speed"], json!(2.0));

        let mut c2 = phira_chart();
        assert!(apply_speed_json(&mut c2, 1, 5.0)); // 替换
        assert_eq!(c2["judgeLineList"][0]["speedEvents"][0]["value"], json!(5.0));
        assert_eq!(c2["judgeLineList"][0]["notesAbove"][0]["speed"], json!(5.0));
    }

    #[test]
    fn event_edit_rpe_ops_and_easing() {
        let mut c = rpe_chart();
        // 全部事件：start +10、end *2、整体 *1、easing 替换为 5
        assert!(edit_events_json(&mut c, true, 0, 2, 10.0, 3, 2.0, 1.0, 5));
        let evt = &c["judgeLineList"][0]["eventLayers"][0]["alphaEvents"][0];
        assert_eq!(evt["start"], json!(10.0));
        assert_eq!(evt["end"], json!(2.0));
        assert_eq!(evt["easingType"], json!(5));
    }

    #[test]
    fn event_edit_phira_movey_start2_end2() {
        let mut c = json!({
            "bpm": 140.0,
            "judgeLineList": [{
                "judgeLineMoveEvents": [
                    { "startTime": 0, "endTime": 32, "start": 0.0, "end": 1.0, "start2": 0.2, "end2": 0.8 }
                ]
            }]
        });
        // 只改 moveY（event_type=2）：start2 +0.5、end2 *0.5
        assert!(edit_events_json(&mut c, false, 2, 2, 0.5, 3, 0.5, 1.0, -1));
        let evt = &c["judgeLineList"][0]["judgeLineMoveEvents"][0];
        assert_eq!(evt["start2"], json!(0.7));
        assert_eq!(evt["end2"], json!(0.4));
        // start/end 不该被 moveY 动到
        assert_eq!(evt["start"], json!(0.0));
    }

    #[test]
    fn resample_bpm_scales_times() {
        let mut c = phira_chart();
        // 140 → 280：ratio 0.5
        assert!(resample_bpm(&mut c, 140.0, 280.0));
        assert_eq!(c["bpm"], json!(280.0));
        assert_eq!(c["judgeLineList"][0]["notesAbove"][1]["time"], json!(16));
        assert_eq!(c["judgeLineList"][0]["notesAbove"][1]["holdTime"], json!(32));
        // 相同 BPM / 非法 BPM → 不改
        assert!(!resample_bpm(&mut c, 280.0, 280.0));
        assert!(!resample_bpm(&mut c, 280.0, 0.0));
    }

    #[test]
    fn offset_copy_repeats_lines() {
        let mut c = phira_chart();
        assert!(apply_offset_json(&mut c, 2, 4.0, 0));
        // 原 1 条 + 复制 2 次 = 3 条
        assert_eq!(c["judgeLineList"].as_array().unwrap().len(), 3);
        // 第一次复制：偏移 = 1 * (4 拍 * 32) = 128
        assert_eq!(c["judgeLineList"][1]["notesAbove"][0]["time"], json!(128));
        // 非法参数不改
        assert!(!apply_offset_json(&mut c, 0, 4.0, 0));
        assert!(!apply_offset_json(&mut c, 1, 0.0, 0));
    }

    #[test]
    fn slice_hold_into_taps() {
        let mut c = phira_chart();
        // 长条：time=32, holdTime=64 → duration 64（=2 拍）；每拍 1 段 → 切 2 段
        assert!(slice_holds_json(&mut c, 1.0, 1));
        let arr = c["judgeLineList"][0]["notesAbove"].as_array().unwrap();
        assert_eq!(arr.len(), 3); // 原 Tap + 切出的 2 个
        assert_eq!(arr[0]["type"], json!(1)); // 原 Tap 保持
        for n in &arr[1..] {
            assert_eq!(n["type"], json!(1));
            assert_eq!(n["holdTime"], json!(0));
        }
        // 整个谱面不该再剩长条
        let s = analyze_chart(&c);
        assert_eq!(s.hold, 0);
    }

    #[test]
    fn fusion_merges_lines_and_names() {
        let mut a = phira_chart();
        a["__name__"] = json!("A");
        let mut b = phira_chart();
        b["__name__"] = json!("B");
        let res = fusion_merge(&[a, b], 0);
        assert!(res.ok, "{}", res.error);
        assert_eq!(res.data["judgeLineList"].as_array().unwrap().len(), 2);
        assert!(res.name.starts_with("A+B"), "name = {}", res.name);
        // `__name__` 不该留在结果里
        assert!(res.data.get("__name__").is_none());
    }

    #[test]
    fn fusion_rejects_different_formats() {
        let a = phira_chart();
        let b = rpe_chart();
        let res = fusion_merge(&[a, b], 0);
        assert!(!res.ok);
        assert!(!res.error.is_empty());
    }

    #[test]
    fn rpe_to_phira_drops_rpe_markers() {
        let p = convert_rpe_to_phira(&rpe_chart());
        assert!(p.get("judgeLineList").is_some());
        assert_eq!(p["formatVersion"], json!(3));
        assert_eq!(p["bpm"], json!(150.0));
        // 转换后不再是 RPE 格式
        assert!(!is_rpe_chart(&p));
        // 音符被搬进 notesAbove / notesBelow
        let line = &p["judgeLineList"][0];
        let above = line["notesAbove"].as_array().unwrap().len();
        let below = line["notesBelow"].as_array().unwrap().len();
        assert_eq!(above + below, 1);
    }

    #[test]
    fn keymode_merges_into_single_line() {
        let mut c = rpe_chart();
        assert!(keymode_json(&mut c, 4, -300.0, 11.8, 175.0));
        assert_eq!(c["judgeLineList"].as_array().unwrap().len(), 1);
        // 轨道化后仍是 RPE 谱面（结构没被破坏）
        assert!(c["judgeLineList"][0].get("notes").is_some());
    }

    #[test]
    fn difficulty_convert_removes_holds() {
        let mut c = phira_chart();
        assert!(convert_difficulty(&mut c, "EZ"));
        let s = analyze_chart(&c);
        assert_eq!(s.hold, 0); // 长条全变 Tap
        assert!(s.total < 3); // 抽稀后变少
        // 非法难度不改
        let mut c2 = phira_chart();
        assert!(!convert_difficulty(&mut c2, "XX"));
    }

    #[test]
    fn randomize_is_deterministic_and_in_range() {
        let mut a = phira_chart();
        let mut b = phira_chart();
        assert!(randomize_notes(&mut a, 12345));
        assert!(randomize_notes(&mut b, 12345));
        assert_eq!(a, b); // 同种子 → 同结果

        for key in ["notesAbove", "notesBelow"] {
            for n in a["judgeLineList"][0][key].as_array().unwrap() {
                let ty = n["type"].as_i64().unwrap();
                assert!((1..=4).contains(&ty), "type out of range: {ty}");
                let px = n["positionX"].as_f64().unwrap();
                assert!((0.0..=1.0).contains(&px), "positionX out of range: {px}");
            }
        }
    }
}
