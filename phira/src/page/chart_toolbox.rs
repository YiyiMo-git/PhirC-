// 「谱面工具箱」页 —— PhiCraft 谱面批量编辑器的原生 Rust 版。
//
// 核心引擎在 `crate::chart_core`（移植自 `PhiCraft-CPP/src/chartcore.cpp`）；
// 本页只负责 UI：选谱面 → 选操作 → 填参数 → 执行 → 结果写回本地谱面（可直接游玩）。
//
// ⚠️ 用 crate 根的 `ttl!`（绑 `common.ftl`），不给本模块单独建 ftl。
// ⚠️ 页面标题必须排在**返回键右侧**（`newui::back_rect`），直接放左边缘会压上去。

use super::{Page, SharedState};
use crate::data::{BriefChartInfo, LocalChart};
use crate::newui::{self, Metrics};
use crate::{chart_core, dir, get_data, get_data_mut, save_data};
use anyhow::Result;
use inputbox::InputBox;
use macroquad::prelude::*;
use prpr::{
    scene::{request_file, request_input, return_file, return_input, take_file, take_input},
    ui::Ui,
};
use serde_json::Value as Json;
use serde_json::json;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

const TOOL_COUNT: usize = 17;
const TOOL_IMPORT: usize = 0;
const TOOL_INFO: usize = 1;
const TOOL_NOTE_TYPE: usize = 2;
const TOOL_SPEED: usize = 3;
const TOOL_SLICE: usize = 4;
const TOOL_OFFSET: usize = 5;
const TOOL_BPM: usize = 6;
const TOOL_MIRROR: usize = 7;
const TOOL_RANDOM: usize = 8;
const TOOL_DIFFICULTY: usize = 9;
const TOOL_KEYMODE: usize = 10;
const TOOL_FUSION: usize = 11;
const TOOL_ANALYZE: usize = 12;
const TOOL_EXPORT: usize = 13;
const TOOL_EVENT: usize = 14;
const TOOL_RPE2PHIRA: usize = 15;
const TOOL_BATCH: usize = 16;

/// 「批量处理」可选的子操作（都是单谱面级的转换）。
const BATCH_OPS: [usize; 9] = [
    TOOL_MIRROR,
    TOOL_RANDOM,
    TOOL_NOTE_TYPE,
    TOOL_DIFFICULTY,
    TOOL_SPEED,
    TOOL_BPM,
    TOOL_SLICE,
    TOOL_OFFSET,
    TOOL_EVENT,
];

/// 一个可编辑参数。
struct Param {
    key: &'static str,
    label: Cow<'static, str>,
    value: String,
}

#[derive(Clone, PartialEq)]
enum View {
    /// 功能列表
    Menu,
    /// 选谱面（`usize` = 工具编号）
    Charts(usize),
    /// 填参数（工具编号, 谱面目录）
    Params(usize, PathBuf),
    /// 批量处理：选子操作
    BatchOps,
    /// 批量处理：填子操作的参数
    BatchParams,
    /// 结果 / 提示
    Info,
}

pub struct ChartToolboxPage {
    enter_t: f32,
    /// 当前视图的进入时间（切视图时重置 → 过渡动画）
    view_t: f32,
    /// 切视图方向（+1 前进 / -1 返回）→ 横向滑入方向
    view_dir: f32,
    /// 最近一帧的时间（`set_info` 里没拿到 `SharedState`，借它来重置过渡）
    now_t: f32,
    view: View,
    row_rects: Vec<Rect>,
    /// 列表滚动偏移 / 上限（UI 单位）
    scroll: f32,
    scroll_max: f32,
    /// 触摸：按下 y、按下时的 scroll、是否已判定为拖动、按下的行下标
    touch_start_y: Option<f32>,
    touch_scroll: f32,
    dragging: bool,
    pending_hit: Option<usize>,
    charts: Vec<(String, PathBuf)>,
    /// 铺面融合 / 批量处理的多选（`charts` 的下标集合）
    fusion_sel: Vec<usize>,
    /// 批量处理选中的谱面目录
    batch_dirs: Vec<PathBuf>,
    /// 批量处理选中的子操作（工具编号）
    batch_op: usize,
    params: Vec<Param>,
    info_title: String,
    info_body: String,
}

fn p(key: &'static str, label: Cow<'static, str>, value: &str) -> Param {
    Param {
        key,
        label,
        value: value.to_owned(),
    }
}

impl ChartToolboxPage {
    pub fn new() -> Self {
        Self {
            enter_t: f32::NEG_INFINITY,
            view_t: f32::NEG_INFINITY,
            view_dir: 1.,
            now_t: 0.,
            view: View::Menu,
            row_rects: Vec::new(),
            scroll: 0.,
            scroll_max: 0.,
            touch_start_y: None,
            touch_scroll: 0.,
            dragging: false,
            pending_hit: None,
            charts: Vec::new(),
            fusion_sel: Vec::new(),
            batch_dirs: Vec::new(),
            batch_op: 0,
            params: Vec::new(),
            info_title: String::new(),
            info_body: String::new(),
        }
    }

    fn tool_label(i: usize) -> Cow<'static, str> {
        match i {
            0 => ttl!("ct-import"),
            1 => ttl!("ct-info"),
            2 => ttl!("ct-note-type"),
            3 => ttl!("ct-speed"),
            4 => ttl!("ct-slice"),
            5 => ttl!("ct-offset"),
            6 => ttl!("ct-bpm"),
            7 => ttl!("ct-mirror"),
            8 => ttl!("ct-random"),
            9 => ttl!("ct-difficulty"),
            10 => ttl!("ct-keymode"),
            11 => ttl!("ct-fusion"),
            12 => ttl!("ct-analyze"),
            13 => ttl!("ct-export"),
            14 => ttl!("ct-event"),
            15 => ttl!("ct-rpe2phira"),
            16 => ttl!("ct-batch"),
            _ => Cow::Borrowed("?"),
        }
    }

    /// 某工具的初始参数（空 = 无参数，选完谱面直接执行）。
    fn tool_params(tool: usize) -> Vec<Param> {
        let mut v = Self::tool_params_inner(tool);
        if matches!(
            tool,
            TOOL_NOTE_TYPE
                | TOOL_SPEED
                | TOOL_SLICE
                | TOOL_OFFSET
                | TOOL_BPM
                | TOOL_MIRROR
                | TOOL_RANDOM
                | TOOL_DIFFICULTY
                | TOOL_KEYMODE
                | TOOL_EVENT
                | TOOL_RPE2PHIRA
        ) {
            // 可变操作都带一个「另存为」（空 = 就地修改原谱面；填名字 = 复制成新谱面再改）
            v.push(p("saveas", ttl!("ct-p-saveas"), ""));
        }
        v
    }

    fn tool_params_inner(tool: usize) -> Vec<Param> {
        match tool {
            TOOL_NOTE_TYPE => vec![
                p("from", ttl!("ct-p-from"), "1"),
                p("to", ttl!("ct-p-to"), "2"),
            ],
            TOOL_SPEED => vec![
                p("mode", ttl!("ct-p-mode"), "0"),
                p("value", ttl!("ct-p-value"), "1.0"),
            ],
            TOOL_SLICE => vec![
                p("beats", ttl!("ct-p-beats"), "1"),
                p("target", ttl!("ct-p-target"), "1"),
            ],
            TOOL_OFFSET => vec![
                p("repeat", ttl!("ct-p-repeat"), "1"),
                p("offset", ttl!("ct-p-offset"), "4"),
                p("type", ttl!("ct-p-type"), "0"),
            ],
            TOOL_BPM => vec![
                p("from", ttl!("ct-p-from-bpm"), "140"),
                p("to", ttl!("ct-p-to-bpm"), "180"),
            ],
            TOOL_DIFFICULTY => vec![p("target", ttl!("ct-p-diff"), "EZ")],
            TOOL_KEYMODE => vec![
                p("k", ttl!("ct-p-k"), "4"),
                p("lineY", ttl!("ct-p-liney"), "-300"),
                p("speed", ttl!("ct-p-speed"), "11.8"),
                p("spacing", ttl!("ct-p-spacing"), "175"),
            ],
            TOOL_EVENT => vec![
                p("etype", ttl!("ct-p-etype"), "0"),
                p("sop", ttl!("ct-p-sop"), "0"),
                p("sval", ttl!("ct-p-sval"), "0"),
                p("eop", ttl!("ct-p-eop"), "0"),
                p("eval", ttl!("ct-p-eval"), "0"),
                p("mul", ttl!("ct-p-mul"), "1"),
                p("easing", ttl!("ct-p-easing"), "-1"),
            ],
            _ => vec![],
        }
    }

    /// 选中某个谱面后，按工具生成参数表（信息编辑要用谱面当前值预填）。
    fn tool_params_for(tool: usize, chart_dir: &Path) -> Vec<Param> {
        if tool == TOOL_INFO {
            info_params(chart_dir)
        } else {
            Self::tool_params(tool)
        }
    }

    fn load_charts(&mut self) {
        self.charts.clear();
        let Ok(root) = dir::charts() else { return };
        for c in &get_data().charts {
            self.charts.push((c.info.name.clone(), PathBuf::from(&root).join(&c.local_path)));
        }
    }

    fn set_info(&mut self, title: String, body: String) {
        self.info_title = title;
        self.info_body = body;
        let t = self.now_t;
        self.goto(View::Info, 1., t);
    }

    /// 切视图（带过渡动画：重置 `view_t`、记录方向）。
    fn goto(&mut self, view: View, dir: f32, t: f32) {
        self.view = view;
        self.view_t = t;
        self.view_dir = dir;
        // 换页回到顶部（与设置页切 tab 一致）
        self.scroll = 0.;
        self.scroll_max = 0.;
        self.touch_start_y = None;
        self.dragging = false;
        self.pending_hit = None;
    }

    /// 执行一个工具。
    fn run_tool(&mut self, tool: usize, chart_dir: &Path) {
        let title = Self::tool_label(tool).into_owned();
        if tool == TOOL_ANALYZE {
            let body = analyze_dir(chart_dir);
            self.set_info(title, body);
            return;
        }
        // 把参数挪出来（避免闭包捕获 `self.params` 与后面的 `self.set_info` 借用打架）
        let params = std::mem::take(&mut self.params);

        // 谱面信息编辑：就地改 info.yml
        if tool == TOOL_INFO {
            let body = match edit_chart_info(chart_dir, &params) {
                Ok(msg) => msg,
                Err(e) => format!("{e}"),
            };
            self.set_info(title, body);
            return;
        }

        // 导出 PEZ：把谱面目录打包成 zip 写到 data/ 下
        if tool == TOOL_EXPORT {
            let name = chart_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "chart".into());
            let out = match dir::root() {
                Ok(r) => PathBuf::from(r).join(format!("{name}.pez")),
                Err(e) => {
                    self.set_info(title, format!("{e}"));
                    return;
                }
            };
            let body = match chart_core::export_to_pez(chart_dir, &out) {
                Ok(()) => format!("已导出: {}", out.display()),
                Err(e) => format!("导出失败: {e}"),
            };
            self.set_info(title, body);
            return;
        }

        // 「另存为」：先把谱面复制成新谱面（并登记进曲库），再在新目录上操作
        let saveas = pval(&params, "saveas").unwrap_or_default();
        let work_dir: PathBuf = if saveas.trim().is_empty() {
            chart_dir.to_path_buf()
        } else {
            match save_as_new_chart(chart_dir, saveas.trim()) {
                Ok(d) => d,
                Err(e) => {
                    self.set_info(title, format!("另存失败: {e}"));
                    return;
                }
            }
        };

        let files = chart_json_files(&work_dir);
        if files.is_empty() {
            self.set_info(title, ttl!("ct-no-chart-json").into_owned());
            return;
        }
        let mut changed = 0usize;
        let mut err: Option<String> = None;
        for path in &files {
            let Ok(mut json) = chart_core::read_chart_json(path) else { continue };
            let is_rpe = chart_core::is_rpe_chart(&json);
            if apply_tool_to_json(tool, &mut json, &params, is_rpe) {
                if let Err(e) = chart_core::write_chart_json(path, &json) {
                    err = Some(format!("{e}"));
                    break;
                }
                changed += 1;
            }
        }
        let body = if let Some(e) = err {
            e
        } else {
            format!("{}: {} / {}", ttl!("ct-done").as_ref(), changed, files.len())
        };
        self.set_info(title, body);
    }

    /// 铺面融合：把多选的谱面合并成一份新谱面，登记进曲库。
    fn run_fusion(&mut self) {
        let title = Self::tool_label(TOOL_FUSION).into_owned();
        let mut sel = self.fusion_sel.clone();
        sel.sort_unstable();
        let dirs: Vec<PathBuf> = sel.iter().filter_map(|&i| self.charts.get(i).map(|c| c.1.clone())).collect();
        if dirs.len() < 2 {
            self.set_info(title, ttl!("ct-fusion-need2").into_owned());
            return;
        }
        // 读每个谱面目录的主谱面文件（带 `__name__` 供融合命名）
        let mut files = Vec::new();
        for d in &dirs {
            let Some(f) = chart_json_files(d).into_iter().next() else { continue };
            let Ok(mut j) = chart_core::read_chart_json(&f) else { continue };
            let nm = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "chart".into());
            j["__name__"] = json!(nm);
            files.push(j);
        }
        if files.len() < 2 {
            self.set_info(title, ttl!("ct-no-chart-json").into_owned());
            return;
        }
        let res = chart_core::fusion_merge(&files, 0);
        if !res.ok {
            self.set_info(title, res.error);
            return;
        }
        // 以第一个选中的谱面为模板复制成新谱面，再把合并结果写进去
        let base_name = dirs[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "chart".into());
        let new_name = unique_name(&format!("{base_name}-fusion"));
        let new_dir = match save_as_new_chart(&dirs[0], &new_name) {
            Ok(d) => d,
            Err(e) => {
                self.set_info(title, format!("{e}"));
                return;
            }
        };
        let Some(cf) = chart_json_files(&new_dir).into_iter().next() else {
            self.set_info(title, ttl!("ct-no-chart-json").into_owned());
            return;
        };
        let body = match chart_core::write_chart_json(&cf, &res.data) {
            Ok(()) => format!("{}: {} ({} 谱面)", ttl!("ct-fusion-done").as_ref(), new_name, files.len()),
            Err(e) => format!("{e}"),
        };
        self.fusion_sel.clear();
        self.set_info(title, body);
    }

    /// 批量处理：把选中的子操作并行应用到多选谱面（`run_batch`）。
    fn run_batch_tool(&mut self) {
        let title = Self::tool_label(TOOL_BATCH).into_owned();
        let op = self.batch_op;
        let params = std::mem::take(&mut self.params);
        let mut files = Vec::new();
        for d in &self.batch_dirs {
            files.extend(chart_json_files(d));
        }
        if files.is_empty() {
            self.set_info(title, ttl!("ct-no-chart-json").into_owned());
            return;
        }
        let res = chart_core::run_batch(&files, 4, |p| {
            let mut json = chart_core::read_chart_json(p)?;
            let is_rpe = chart_core::is_rpe_chart(&json);
            if apply_tool_to_json(op, &mut json, &params, is_rpe) {
                chart_core::write_chart_json(p, &json)?;
            }
            Ok(())
        });
        let body = format!(
            "{}: {} / {}（{} {}）",
            ttl!("ct-done").as_ref(),
            res.ok,
            files.len(),
            res.failed,
            ttl!("ct-failed").as_ref()
        );
        self.batch_dirs.clear();
        self.fusion_sel.clear();
        self.set_info(title, body);
    }

    /// 单列分组列表（与设置页同一套排版：整组一个圆角容器、行内缩发丝线、
    /// 左侧标签 + 右侧值/`›` 箭头）。
    ///
    /// `values`：与 `labels` 等长的右侧值（参数页的当前值）；`None` = 不显示值。
    /// `chevron`：是否画右侧 `›`（执行行 `▶` 不画，多选行也不画）。
    fn draw_rows(
        &mut self,
        ui: &mut Ui,
        m: &Metrics,
        th: &newui::Theme,
        area: Rect,
        labels: &[String],
        values: Option<&[String]>,
        chevron: bool,
        top: f32,
    ) {
        // 行高固定（比旧版 46 更大），放不下就滚动 —— 不再压缩到 37px。
        let row_h = m.u(56.);
        let n = labels.len().max(1);
        let total = row_h * n as f32;
        let view_h = (area.bottom() - top).max(m.u(40.));
        self.scroll_max = (total - view_h).max(0.);
        self.scroll = self.scroll.clamp(0., self.scroll_max);

        let clip = Rect::new(area.x, top, area.w, view_h);
        let (mx, my) = newui::mouse_world();
        let mouse = vec2(mx, my);
        self.row_rects.clear();

        ui.scissor(clip, |ui| {
            let group = Rect::new(area.x, top - self.scroll, area.w, total);
            newui::group_plate(ui, th, group, m.u(12.));

            let mut y = top - self.scroll;
            for (i, label) in labels.iter().enumerate() {
                let r = Rect::new(area.x, y, area.w, row_h);
                self.row_rects.push(r);
                let hp = if r.contains(mouse) { 1. } else { 0. };
                newui::group_row(ui, th, r, hp, i == 0, i + 1 == n, m.u(12.), m.u(16.), Color::new(0., 0., 0., 0.));
                // 左：标签
                newui::text(ui, m, label.as_str(), r.x + m.u(16.), r.center().y, 16., th.text, (0., 0.5));

                // 右：先放 `›`，再往左排值 —— 设置页的「标签 …… 值 ›」
                let mut right = r.right() - m.u(16.);
                if chevron && !label.starts_with('▶') {
                    let cr = newui::text(ui, m, "›", right, r.center().y, 19., th.text_dim, (1., 0.5));
                    right = cr.x - m.u(8.);
                }
                if let Some(vals) = values {
                    if let Some(v) = vals.get(i) {
                        if !v.is_empty() {
                            newui::text(ui, m, v.as_str(), right, r.center().y, 15., th.text_dim, (1., 0.5));
                        }
                    }
                }
                y += row_h;
            }
        });
    }
}

/// 谱面目录里的**真正的谱面文件**（排除 info.json / info.yml 之类）。
fn chart_json_files(dir: &Path) -> Vec<PathBuf> {
    chart_core::collect_chart_files(dir)
        .into_iter()
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
            !name.starts_with("info.")
        })
        .filter(|p| {
            chart_core::read_chart_json(p)
                .map(|j| chart_core::is_rpe_chart(&j) || j.get("judgeLineList").is_some() || j.get("notes").is_some())
                .unwrap_or(false)
        })
        .collect()
}

/// 把一个「单谱面级」工具的改动应用到一份谱面 json（返回是否有改动）。
/// 分析 / 信息编辑 / 导出 / 融合 / 批量不走这里（它们在 `run_tool` 里单独处理）。
fn apply_tool_to_json(tool: usize, json: &mut Json, params: &[Param], is_rpe: bool) -> bool {
    match tool {
        TOOL_NOTE_TYPE => chart_core::apply_note_changes_json(json, pint(params, "from", 1), pint(params, "to", 2), is_rpe),
        TOOL_SPEED => chart_core::apply_speed_json(json, pint(params, "mode", 0) as i32, pnum(params, "value", 1.0)),
        TOOL_SLICE => chart_core::slice_holds_json(json, pnum(params, "beats", 1.0), pint(params, "target", 1)),
        TOOL_OFFSET => chart_core::apply_offset_json(json, pint(params, "repeat", 1) as i32, pnum(params, "offset", 4.0), pint(params, "type", 0)),
        TOOL_BPM => chart_core::resample_bpm(json, pnum(params, "from", 140.0), pnum(params, "to", 180.0)),
        TOOL_MIRROR => chart_core::mirror_chart(json, true),
        TOOL_RANDOM => chart_core::randomize_notes(json, (get_time() * 1000.0) as u64),
        TOOL_DIFFICULTY => chart_core::convert_difficulty(json, pval(params, "target").unwrap_or_else(|| "EZ".into()).trim()),
        TOOL_KEYMODE => chart_core::keymode_json(json, pint(params, "k", 4) as i32, pnum(params, "lineY", -300.0), pnum(params, "speed", 11.8), pnum(params, "spacing", 175.0)),
        TOOL_EVENT => chart_core::edit_events_json(
            json,
            is_rpe,
            pint(params, "etype", 0) as i32,
            pint(params, "sop", 0) as i32,
            pnum(params, "sval", 0.0),
            pint(params, "eop", 0) as i32,
            pnum(params, "eval", 0.0),
            pnum(params, "mul", 1.0),
            pint(params, "easing", -1) as i32,
        ),
        TOOL_RPE2PHIRA => {
            if is_rpe {
                let converted = chart_core::convert_rpe_to_phira(json);
                *json = converted;
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

/// 分析一个谱面目录（读第一个可解析的谱面文件）。
fn analyze_dir(dir: &Path) -> String {
    for f in chart_json_files(dir) {
        let Ok(json) = chart_core::read_chart_json(&f) else { continue };
        let s = chart_core::analyze_chart(&json);
        return format!(
            "音符总数: {}\nTap: {} ({:.1}%)\nDrag: {} ({:.1}%)\n长条: {}\nFlick: {} ({:.1}%)\n峰值密度(1s): {}",
            s.total, s.tap, s.tap_pct, s.drag, s.drag_pct, s.hold, s.flick, s.flick_pct, s.peak_density
        );
    }
    ttl!("ct-no-chart-json").into_owned()
}

/// 取一个字符串参数。
fn pval(params: &[Param], k: &str) -> Option<String> {
    params.iter().find(|p| p.key == k).map(|p| p.value.clone())
}

/// 取一个 f64 参数。
fn pnum(params: &[Param], k: &str, d: f64) -> f64 {
    pval(params, k).and_then(|v| v.trim().parse().ok()).unwrap_or(d)
}

/// 取一个 i64 参数。
fn pint(params: &[Param], k: &str, d: i64) -> i64 {
    pval(params, k).and_then(|v| v.trim().parse().ok()).unwrap_or(d)
}

/// 取一个可选的 f64 参数（空 / 非法 = None）。
fn pnum_opt(params: &[Param], k: &str) -> Option<f64> {
    pval(params, k).and_then(|v| v.trim().parse().ok())
}

/// 数字 → 文本（整数不带小数点）。
fn fmt_num(v: f64) -> String {
    if v.fract() == 0. {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// 读谱面目录的 info：优先 `info.yml`（phira 自己的格式），否则从 Info.txt / info.json 兜底。
fn read_chart_info(dir: &Path) -> prpr::info::ChartInfo {
    let yml = dir.join("info.yml");
    if let Ok(f) = std::fs::File::open(&yml) {
        if let Ok(i) = serde_yaml::from_reader::<_, prpr::info::ChartInfo>(f) {
            return i;
        }
    }
    let mut info = prpr::info::ChartInfo::default();
    if let Some(data) = chart_core::load_info_file(dir) {
        apply_json_to_info(&mut info, &data);
    }
    if info.name.trim().is_empty() || info.name == "UK" {
        if let Some(n) = dir.file_name() {
            info.name = n.to_string_lossy().into_owned();
        }
    }
    info
}

/// 把通用 info json（title/artist/charter/level/difficulty/offset）映射到 `ChartInfo`。
fn apply_json_to_info(info: &mut prpr::info::ChartInfo, data: &Json) {
    let s = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| data.get(*k).and_then(|v| v.as_str()).map(|s| s.trim().to_owned()))
            .filter(|s| !s.is_empty())
    };
    let num = |k: &str| -> Option<f64> {
        data.get(k)
            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok())))
    };
    if let Some(v) = s(&["title", "name"]) {
        info.name = v;
    }
    if let Some(v) = s(&["artist", "composer"]) {
        info.composer = v;
    }
    if let Some(v) = s(&["charter"]) {
        info.charter = v;
    }
    if let Some(v) = s(&["illustrator"]) {
        info.illustrator = v;
    }
    if let Some(v) = s(&["level"]) {
        info.level = v;
    }
    if let Some(v) = num("difficulty") {
        info.difficulty = v as f32;
    }
    if let Some(v) = num("offset") {
        info.offset = v as f32;
    }
}

/// 信息编辑的参数表（用谱面当前值预填）。
fn info_params(dir: &Path) -> Vec<Param> {
    let info = read_chart_info(dir);
    vec![
        p("name", ttl!("ct-p-name"), &info.name),
        p("composer", ttl!("ct-p-composer"), &info.composer),
        p("charter", ttl!("ct-p-charter"), &info.charter),
        p("illustrator", ttl!("ct-p-illustrator"), &info.illustrator),
        p("level", ttl!("ct-p-level"), &info.level),
        p("difficulty", ttl!("ct-p-difficulty"), &fmt_num(info.difficulty as f64)),
        p("offset", ttl!("ct-p-offset-val"), &fmt_num(info.offset as f64)),
    ]
}

/// 就地改谱面 `info.yml`。
fn edit_chart_info(dir: &Path, params: &[Param]) -> Result<String> {
    let mut info = read_chart_info(dir);
    let s = |k: &str| pval(params, k).unwrap_or_default();
    let set = |dst: &mut String, v: String| {
        let t = v.trim().to_owned();
        if !t.is_empty() {
            *dst = t;
        }
    };
    set(&mut info.name, s("name"));
    set(&mut info.composer, s("composer"));
    set(&mut info.charter, s("charter"));
    set(&mut info.illustrator, s("illustrator"));
    set(&mut info.level, s("level"));
    if let Some(d) = pnum_opt(params, "difficulty") {
        info.difficulty = d as f32;
    }
    if let Some(o) = pnum_opt(params, "offset") {
        info.offset = o as f32;
    }
    std::fs::write(dir.join("info.yml"), serde_yaml::to_string(&info)?)?;
    Ok(format!("{}: {}", ttl!("ct-info-done").as_ref(), info.name))
}

/// 文件名净化（去掉 Windows 非法字符）。
fn sanitize_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    let t = cleaned.trim().trim_matches('.').to_owned();
    if t.is_empty() {
        "imported".into()
    } else {
        t
    }
}

/// 在 `data/charts/custom/` 下找一个不冲突的目录名。
fn unique_name(base: &str) -> String {
    let Ok(custom) = dir::custom_charts() else { return base.to_owned() };
    let d = PathBuf::from(custom);
    if !d.join(base).exists() {
        return base.to_owned();
    }
    for i in 2..10000 {
        let n = format!("{base}-{i}");
        if !d.join(&n).exists() {
            return n;
        }
    }
    format!("{base}-{}", get_time() as i64)
}

/// 给 info 补上实际存在的 chart / music / illustration 文件名。
fn fill_asset_names(info: &mut prpr::info::ChartInfo, dir: &Path) {
    if let Some(f) = chart_json_files(dir).into_iter().next() {
        if let Some(n) = f.file_name() {
            info.chart = n.to_string_lossy().into_owned();
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut music = None;
        let mut illu = None;
        for e in rd.flatten() {
            let raw = e.file_name().to_string_lossy().into_owned();
            let l = raw.to_ascii_lowercase();
            if music.is_none() && (l.ends_with(".mp3") || l.ends_with(".ogg") || l.ends_with(".wav") || l.ends_with(".m4a") || l.ends_with(".flac")) {
                music = Some(raw.clone());
            }
            if illu.is_none() && (l.ends_with(".png") || l.ends_with(".jpg") || l.ends_with(".jpeg")) {
                illu = Some(raw);
            }
        }
        if let Some(m) = music {
            info.music = m;
        }
        if let Some(i) = illu {
            info.illustration = i;
        }
    }
}

/// 导入 PEZ / ZIP 谱面包到 `data/charts/custom/` 并登记进曲库。
fn import_pez(file_path: &Path) -> Result<String> {
    let custom = dir::custom_charts()?;
    let stem = file_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "imported".into());
    let name = unique_name(&sanitize_name(&stem));
    let dst = PathBuf::from(&custom).join(&name);
    std::fs::create_dir_all(&dst)?;
    let unpack = (|| -> Result<()> {
        let f = std::fs::File::open(file_path)?;
        let d = prpr::dir::Dir::new(&dst)?;
        prpr::ext::unzip_into(std::io::BufReader::new(f), &d, true)?;
        Ok(())
    })();
    if let Err(e) = unpack {
        let _ = std::fs::remove_dir_all(&dst);
        return Err(e);
    }
    chart_core::flatten_to_chart_layer(&dst);
    chart_core::clean_archive_junk(&dst);
    // 确保有 info.yml
    if !dst.join("info.yml").exists() {
        let mut info = prpr::info::ChartInfo::default();
        if let Some(data) = chart_core::load_info_file(&dst) {
            apply_json_to_info(&mut info, &data);
        }
        if info.name.trim().is_empty() || info.name == "UK" {
            info.name = name.clone();
        }
        fill_asset_names(&mut info, &dst);
        std::fs::write(dst.join("info.yml"), serde_yaml::to_string(&info)?)?;
    }
    let brief = load_brief_info(&dst);
    get_data_mut().charts.push(LocalChart {
        info: brief,
        local_path: format!("custom/{name}"),
        record: None,
        mods: Default::default(),
        played_unlock: false,
    });
    save_data()?;
    Ok(name)
}

/// 递归复制目录。
fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// 读谱面目录的 info（优先 `info.yml`），拿不到就用目录名兜底。
fn load_brief_info(dir: &Path) -> BriefChartInfo {
    if let Ok(f) = std::fs::File::open(dir.join("info.yml")) {
        if let Ok(info) = serde_yaml::from_reader::<_, prpr::info::ChartInfo>(f) {
            return info.into();
        }
    }
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    BriefChartInfo {
        id: None,
        uploader: None,
        name,
        level: String::new(),
        difficulty: 0.,
        intro: String::new(),
        charter: String::new(),
        composer: String::new(),
        illustrator: String::new(),
        created: None,
        updated: None,
        chart_updated: None,
        has_unlock: false,
    }
}

/// 把 `src_dir` 复制成 `data/charts/custom/<name>/` 并登记进曲库，返回新目录。
fn save_as_new_chart(src_dir: &Path, name: &str) -> Result<PathBuf> {
    let custom = dir::custom_charts()?;
    let dst = PathBuf::from(&custom).join(name);
    if dst.exists() {
        anyhow::bail!("目标已存在: {name}");
    }
    copy_dir(src_dir, &dst)?;
    let info = load_brief_info(&dst);
    get_data_mut().charts.push(LocalChart {
        info,
        local_path: format!("custom/{name}"),
        record: None,
        mods: Default::default(),
        played_unlock: false,
    });
    save_data()?;
    Ok(dst)
}

/// 行点击行为（原 `touch` 里的 match，搬成独立方法便于「按下→松开」判定）。
impl ChartToolboxPage {
    /// 点中第 `i` 行要做什么（原 `touch` 里的那套 match 原样搬过来）。
    fn activate(&mut self, i: usize, t: f32) {
        match self.view.clone() {
            View::Menu => {
                if i == TOOL_IMPORT {
                    // 导入不需要先选谱面：直接开文件对话框
                    request_file("ct_import");
                } else {
                    self.load_charts();
                    self.fusion_sel.clear();
                    self.goto(View::Charts(i), 1., t);
                }
            }
            View::Charts(tool) => {
                if tool == TOOL_FUSION || tool == TOOL_BATCH {
                    // 融合 / 批量：点击行 = 多选切换；最后一行 = 下一步
                    if i < self.charts.len() {
                        if let Some(pos) = self.fusion_sel.iter().position(|&x| x == i) {
                            self.fusion_sel.remove(pos);
                        } else {
                            self.fusion_sel.push(i);
                        }
                    } else if tool == TOOL_FUSION {
                        self.run_fusion();
                    } else {
                        let mut sel = self.fusion_sel.clone();
                        sel.sort_unstable();
                        self.batch_dirs = sel.iter().filter_map(|&k| self.charts.get(k).map(|c| c.1.clone())).collect();
                        if self.batch_dirs.is_empty() {
                            self.set_info(Self::tool_label(TOOL_BATCH).into_owned(), ttl!("ct-batch-need1").into_owned());
                        } else {
                            self.goto(View::BatchOps, 1., t);
                        }
                    }
                } else {
                    let Some((_, path)) = self.charts.get(i).cloned() else { return };
                    self.params = Self::tool_params_for(tool, &path);
                    if self.params.is_empty() {
                        self.run_tool(tool, &path);
                    } else {
                        self.goto(View::Params(tool, path), 1., t);
                    }
                }
            }
            View::Params(tool, path) => {
                if i < self.params.len() {
                    let key = self.params[i].key;
                    let label = self.params[i].label.clone();
                    request_input(&format!("ct:{key}"), InputBox::new().title(label).prompt(ttl!("ct-param-hint")));
                } else {
                    self.run_tool(tool, &path);
                }
            }
            View::BatchOps => {
                if let Some(&op) = BATCH_OPS.get(i) {
                    self.batch_op = op;
                    // 批量不提供「另存为」（本来就是批量就地改）
                    self.params = Self::tool_params_inner(op);
                    self.goto(View::BatchParams, 1., t);
                }
            }
            View::BatchParams => {
                if i < self.params.len() {
                    let key = self.params[i].key;
                    let label = self.params[i].label.clone();
                    request_input(&format!("ct:{key}"), InputBox::new().title(label).prompt(ttl!("ct-param-hint")));
                } else {
                    self.run_batch_tool();
                }
            }
            View::Info => self.goto(View::Menu, -1., t),
        }
    }
}

impl Page for ChartToolboxPage {
    fn label(&self) -> Cow<'static, str> {
        ttl!("chart-toolbox")
    }

    /// 工具箱固定深色（与 WebView 设置页一致），场景层的返回键也跟着变深。
    fn prefers_dark(&self) -> bool {
        true
    }

    fn enter(&mut self, s: &mut SharedState) -> Result<()> {
        self.enter_t = s.t;
        self.view_t = s.t;
        self.view_dir = 1.;
        self.view = View::Menu;
        Ok(())
    }

    fn update(&mut self, s: &mut SharedState) -> Result<()> {
        self.now_t = s.t;
        // 滚轮滚动（列表超出可视区时）；无论是否可滚都取走，避免污染别的页面
        let (_, wy) = prpr::judge::take_wheel();
        if self.scroll_max > 0. && wy.abs() > 1e-4 {
            self.scroll = (self.scroll - wy * 0.1).clamp(0., self.scroll_max);
        }
        if let Some((id, file)) = take_file() {
            if id == "ct_import" {
                let title = Self::tool_label(TOOL_IMPORT).into_owned();
                let body = match import_pez(Path::new(&file)) {
                    Ok(name) => format!("{}: {}", ttl!("ct-import-ok").as_ref(), name),
                    Err(e) => format!("{}: {e}", ttl!("ct-import-fail").as_ref()),
                };
                self.set_info(title, body);
                return Ok(());
            }
            return_file(id, file);
        }
        if let Some((id, text)) = take_input() {
            if let Some(key) = id.strip_prefix("ct:") {
                if let Some(p) = self.params.iter_mut().find(|p| p.key == key) {
                    p.value = text;
                    return Ok(());
                }
            }
            return_input(id, text);
        }
        Ok(())
    }

    fn on_back_pressed(&mut self, s: &mut SharedState) -> bool {
        match self.view {
            View::Menu => false,
            // 批量流程有 3 层，逐层退回更顺
            View::BatchParams => {
                self.goto(View::BatchOps, -1., s.t);
                true
            }
            View::BatchOps => {
                self.goto(View::Charts(TOOL_BATCH), -1., s.t);
                true
            }
            _ => {
                self.goto(View::Menu, -1., s.t);
                true
            }
        }
    }

    fn touch(&mut self, touch: &Touch, s: &mut SharedState) -> Result<bool> {
        let t = s.t;
        match touch.phase {
            TouchPhase::Started => {
                self.touch_start_y = Some(touch.position.y);
                self.touch_scroll = self.scroll;
                self.dragging = false;
                // 先记下按到的行；真正执行放到 Ended —— 拖动列表时不会误触
                self.pending_hit = self.row_rects.iter().position(|r| r.contains(touch.position));
                Ok(self.pending_hit.is_some())
            }
            TouchPhase::Stationary | TouchPhase::Moved => {
                if let Some(y0) = self.touch_start_y {
                    let dy = touch.position.y - y0;
                    if !self.dragging && dy.abs() > 0.02 {
                        self.dragging = true;
                        self.pending_hit = None;
                    }
                    if self.dragging {
                        self.scroll = (self.touch_scroll - dy).clamp(0., self.scroll_max);
                    }
                }
                Ok(self.dragging)
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let hit = self.pending_hit.take();
                let dragging = self.dragging;
                self.dragging = false;
                self.touch_start_y = None;
                if !dragging {
                    if let Some(i) = hit {
                        self.activate(i, t);
                    }
                }
                Ok(dragging || hit.is_some())
            }
        }
    }

    fn render(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()> {
        let m = Metrics::new(ui);
        // 强制深色：与 WebView 设置页一致的观感（不跟全局浅色主题走）
        let th = newui::Theme::DARK;
        ui.fill_rect(ui.screen_rect(), th.bg);
        let c = m.content(ui);
        let p = newui::enter_p((s.t - self.enter_t).max(0.), 0.04);
        // 切视图过渡：淡入 + 横向滑入（前进从右、返回从左）
        let vp = newui::enter_p((s.t - self.view_t).max(0.), 0.02);
        let slide = (1. - vp) * self.view_dir * m.u(30.);

        ui.alpha(p, |ui| {
            ui.scope(|ui| {
            ui.dx(slide);
            ui.alpha(vp, |ui| {
            let back = newui::back_rect(ui, &m);
            let title_x = back.right() + m.u(16.);
            newui::page_title(ui, &m, &th, title_x, back.center().y, ttl!("chart-toolbox").as_ref(), None, 22.);

            let sub_y = back.bottom() + m.u(18.);
            let sub = match &self.view {
                View::Menu => ttl!("ct-subtitle").into_owned(),
                View::Charts(tool) => format!("{} · {}", ttl!("chart-toolbox").as_ref(), Self::tool_label(*tool)),
                View::Params(tool, _) => Self::tool_label(*tool).into_owned(),
                View::BatchOps => format!("{} · {}", ttl!("ct-batch").as_ref(), ttl!("ct-batch-pick").as_ref()),
                View::BatchParams => format!("{} · {}", ttl!("ct-batch").as_ref(), Self::tool_label(self.batch_op)),
                View::Info => self.info_title.clone(),
            };
            newui::text(ui, &m, sub.as_str(), c.x + m.u(4.), sub_y, 13., th.text_dim, (0., 0.5));

            let top = sub_y + m.u(26.);
            // 设置页同款「扁平分组」：行直接落在页面底上，不铺卡片底、不画分隔线
            crate::newui::set_flat_groups(true);
            match self.view.clone() {
                View::Menu => {
                    let labels: Vec<String> = (0..TOOL_COUNT).map(|i| Self::tool_label(i).into_owned()).collect();
                    // 单列分组列表（与设置页同一排版）；行高按可用高度自动压缩
                    self.draw_rows(ui, &m, &th, c, &labels, None, true, top);
                }
                View::Charts(tool) => {
                    let mut labels: Vec<String> = self.charts.iter().map(|(n, _)| n.clone()).collect();
                    if labels.is_empty() {
                        newui::text(ui, &m, ttl!("ct-no-charts").as_ref(), c.x + m.u(4.), top + m.u(10.), 14., th.text_dim, (0., 0.5));
                        self.row_rects.clear();
                    } else {
                        if tool == TOOL_FUSION || tool == TOOL_BATCH {
                            // 选中项打勾 + 末尾一行（融合=执行 / 批量=下一步）
                            for (i, l) in labels.iter_mut().enumerate() {
                                if self.fusion_sel.contains(&i) {
                                    *l = format!("✓ {l}");
                                }
                            }
                            let tail = if tool == TOOL_FUSION {
                                ttl!("ct-run")
                            } else {
                                ttl!("ct-next")
                            };
                            labels.push(format!("▶ {} ({})", tail.as_ref(), self.fusion_sel.len()));
                        }
                        // 普通工具：点一行进参数页；融合 / 批量是多选，不画箭头
                        self.draw_rows(ui, &m, &th, c, &labels, None, tool != TOOL_FUSION && tool != TOOL_BATCH, top);
                    }
                }
                View::Params(_, _) | View::BatchParams => {
                    // 设置页排版：左「参数名」、右「当前值」、再右侧 `›`
                    let labels: Vec<String> = self
                        .params
                        .iter()
                        .map(|p| p.label.to_string())
                        .chain(std::iter::once(format!("▶ {}", ttl!("ct-run").as_ref())))
                        .collect();
                    let values: Vec<String> = self
                        .params
                        .iter()
                        .map(|p| p.value.clone())
                        .chain(std::iter::once(String::new()))
                        .collect();
                    self.draw_rows(ui, &m, &th, c, &labels, Some(&values), true, top);
                }
                View::BatchOps => {
                    let labels: Vec<String> = BATCH_OPS.iter().map(|&op| Self::tool_label(op).into_owned()).collect();
                    self.draw_rows(ui, &m, &th, c, &labels, None, true, top);
                }
                View::Info => {
                    self.row_rects.clear();
                    let body = self.info_body.clone();
                    let mut ty = top + m.u(6.);
                    for line in body.lines() {
                        newui::text(ui, &m, line, c.x + m.u(4.), ty, 15., th.text, (0., 0.5));
                        ty += m.u(26.);
                    }
                }
            }
            crate::newui::set_flat_groups(false);
            });
            });
        });

        Ok(())
    }
}
