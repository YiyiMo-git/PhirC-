//! 开始页（`phira/ui/index.html`）5 个按钮的**原生触摸命中** —— 完全不依赖 JS。
//!
//! # 为什么需要它
//!
//! iOS 上 `WKWebView` 里的注入脚本（[`crate::webui::START_PAGE_INIT_SCRIPT`]，无论走
//! `WKUserScript` 还是 `didFinishNavigation` 里再 `evaluateJavaScript`）都跑不起来，
//! 5 个按钮的 `addEventListener` 永远不绑定 → 点了没有任何反应。
//!
//! 与其继续和 WebKit 的注入时机搏斗，这里换一条**完全绕开 JS** 的路：
//!
//! 1. `phira/ios/webui.m` 把 WKWebView 设成 `userInteractionEnabled = NO`
//!    —— 触摸不再被 WebView 吃掉，而是穿透到它下面的游戏视图
//!    （miniquad 的 `QuadView`；WKWebView 就是 `addSubview` 到它**上面**的，
//!    见 `prpr-miniquad/src/native/ios.rs` 与 `webui.m` 的 `phira_root_view_controller`）；
//! 2. Rust 侧在 [`crate::scene::MainScene::touch`] 里拿**原始触摸坐标**去比对 5 个按钮的
//!    矩形（就是本模块），命中就执行与 JS `HANDLERS` **完全相同**的动作
//!    （[`crate::webui::invoke_button_action`] → 同一条 [`crate::webui::handle_ipc`] 通路）。
//!
//! 于是**注入脚本一行都不跑，按钮照样能用**。JS 注入保持原样作为「万一它能跑」的冗余，
//! 两条路都指向同一套动作。
//!
//! # 按钮矩形的推导（从 `ui/index.html` 的 CSS 到最终数字，可复核）
//!
//! 注入脚本把视口固定成 `width=1280`（`webui::START_PAGE_INIT_SCRIPT` 的
//! `installViewport` / `DESIGN_WIDTH`），整页按 1280 CSS px 设计宽排版，再由 WebView
//! 缩放到屏幕宽度。而 WebView 的 frame 就是全屏（`webui.m`: `root.view.bounds`），
//! 所以 **页面原点 = 屏幕原点**：只要在「页面坐标」里把矩形算出来，横竖都乘同一个
//! 缩放比 `屏幕宽 / 1280` 即可。
//!
//! ## 横向
//!
//! ```css
//! .right-panel { right: 60px; width: 460px; }
//! .btn-row     { display: flex; gap: 16px; }
//! .btn-row .glass-btn { flex: 1; }
//! * { box-sizing: border-box; }   /* 高度含 border / padding */
//! ```
//!
//! * 面板左 = 1280 − 60 − 460 = **760**，右 = **1220**（[`PANEL_LEFT`] / 1280 − 60）；
//! * 「设置 / 活动」并排：可用宽 460 − 16(gap) = 444，两个 `flex: 1` 各 **222**
//!   → `#btn-settings` x ∈ [760, 982)、`#btn-activity` x ∈ [998, 1220)。
//!
//! ## 纵向
//!
//! ```css
//! .right-panel { top: 50%; transform: translateY(-50%);
//!                display: flex; flex-direction: column; gap: 16px; }
//! .glass-btn.main     { height: 100px; }   /* #btn-start */
//! .glass-btn.sec      { height:  76px; }   /* #btn-res   */
//! .btn-row .glass-btn { height:  76px; }   /* 设置 / 活动 */
//! .glass-btn.white    { height:  76px; }   /* #btn-last  */
//! ```
//!
//! `top: 50%` + `translateY(-50%)` 就是**垂直居中**，所以面板内容高度决定一切：
//! 4 个按钮 100 + 76 + 76 + 76 = 328，3 条 gap 3 × 16 = 48 → 内容高 **376**
//! （[`PANEL_H`]），`panel_top = (page_h − 376) / 2`。
//!
//! 各按钮「相对面板顶」的偏移（累加，含 gap）：
//!
//! | 按钮 | 偏移 | 高度 |
//! |---|---|---|
//! | `#btn-start`    | 0                        | 100 |
//! | `#btn-res`      | 100 + 16 = **116**       | 76 |
//! | `.btn-row`      | 116 + 76 + 16 = **208**  | 76 |
//! | `#btn-last`     | 208 + 76 + 16 = **300**  | 76 |
//! | 面板底部        | 300 + 76 = **376** ✅ 与 `PANEL_H` 自洽 | |
//!
//! ## 与屏幕的换算
//!
//! 传给 [`crate::scene::Scene::touch`] 的 `Touch::position` 是 prpr **已经归一化**过的
//! 坐标（见 `prpr::judge::touch_transform`）：
//!
//! * `x ∈ [−1, 1]`：−1 = 视口左边缘，+1 = 右边缘；
//! * `y = (屏幕像素 y / 屏高 × 2 − 1) / aspect`，其中 `aspect = 屏宽 / 屏高`；
//!   即 **y = 0 落在屏幕垂直中线上，向下为正**，取值范围 `[−1/aspect, +1/aspect]`。
//!
//! 记 `k = 屏高 / 屏宽 = 1 / aspect`：
//!
//! * 横向屏幕比例 `fx = (x + 1) / 2`
//!   → `page_x = 1280 · fx = 640 · (x + 1)`
//! * 纵向屏幕比例 `fy = (y · aspect + 1) / 2`，页面高 `page_h = 1280 · k`
//!   → `page_y = page_h · fy = 640 · (y · aspect + 1) · k = 640 · (y + k)`
//!   （因为 `aspect · k = 1`）
//!
//! 自检：`x = y = 0`（屏幕正中）应落在页面正中 → `page = (640, 640k)`，
//! 而 `page_h / 2 = 640k` ✅。
//!
//! # 已知取舍
//!
//! * WebView 关掉交互后，`index.html` 自带的「双击左侧曲绘换图」也没了 —— 那是页面自己的
//!   功能，Rust 侧没有对应动作。需要的话可以在 [`BUTTONS`] 里再补一个矩形，让 Rust 去
//!   `prpr::scene::request_file`（本次不做）。
//! * 页面用了 `100vh` / `top: 50%`，若将来 WebView 不再铺满全屏（例如让开安全区），
//!   这里的映射要跟着改成 WebView 的实际 frame。

// 本模块的命中表在非 iOS 平台不会被真正触发（见 [`ENABLED`]），属正常情况。
#![allow(dead_code)]

/// 注入脚本固定的设计宽度（CSS px）。
///
/// **必须**与 [`crate::webui::START_PAGE_INIT_SCRIPT`] 里的 `DESIGN_WIDTH` 一致，
/// 否则整张命中表都会偏。
pub const DESIGN_WIDTH: f32 = 1280.;

/// 原生触摸命中是否启用 —— **只在 iOS 上启用**。
///
/// 理由：
///
/// * 问题只出在 iOS：Windows 的 WebView2 注入是好的，Android 的
///   `onPageFinished` 注入也是好的，只有 iOS 的 `WKWebView` 注入不生效；
/// * Windows 上 WebView 是覆盖窗口的子 HWND，点击本来就会被它收走 —— 若两边都能触发，
///   同一个按钮会走两次（JS 一次 + 这里一次），属于凭空制造的双击；
/// * Android 上 WebView 是叠在 SurfaceView 之上的**真 View**，除非同时改 Java 侧
///   （`setClickable(false)` / `setFocusable(false)` 之类）让触摸穿透，否则触摸根本
///   到不了游戏视图，这条 Rust 路径就是死代码；而 Android 的注入本来就是好的，
///   没必要为了「多一条路」去动 Java 侧并承担回归风险。
///
/// 之所以用 `const bool` 而不是把调用点 `#[cfg]` 掉：这样**非 iOS 平台也会编译整条
/// iOS 命中链路**（`cargo check` 就能发现类型 / 借用错误），只是运行时永远不进这个分支。
pub const ENABLED: bool = cfg!(target_os = "ios");

/* ============================ 几何常量（见模块文档的推导） ============================ */

/// `.right-panel { right: 60px }`：面板右边缘距视口右边 60。
const PANEL_RIGHT_GAP: f32 = 60.;

/// `.right-panel { width: 460px }`。
const PANEL_WIDTH: f32 = 460.;

/// `.right-panel` 左边：1280 − 60 − 460 = **760**。
const PANEL_LEFT: f32 = DESIGN_WIDTH - PANEL_RIGHT_GAP - PANEL_WIDTH;

/// `#btn-start { height: 100px }`。
const BTN_MAIN_H: f32 = 100.;

/// `.glass-btn.sec` / `.btn-row .glass-btn` / `.glass-btn.white` 的 `height: 76px`。
const BTN_H: f32 = 76.;

/// `.right-panel { gap: 16px }`（也等于 `.btn-row { gap: 16px }`）。
const GAP: f32 = 16.;

/// 并排按钮单宽：`(460 − 16) / 2 = 222`。
const ROW_BUTTON_W: f32 = (PANEL_WIDTH - GAP) / 2.;

/// `#btn-res` 相对面板顶的偏移：`100 + 16 = 116`。
const Y_RES: f32 = BTN_MAIN_H + GAP;

/// `.btn-row`（设置 / 活动）相对面板顶的偏移：`116 + 76 + 16 = 208`。
const Y_ROW: f32 = Y_RES + BTN_H + GAP;

/// `#btn-last` 相对面板顶的偏移：`208 + 76 + 16 = 300`。
const Y_LAST: f32 = Y_ROW + BTN_H + GAP;

/// 面板内容总高：`300 + 76 = 376`（`#btn-start` 之外还有 3 个 76 与 3 条 16 的 gap）。
const PANEL_H: f32 = Y_LAST + BTN_H;

/// 命中表：`(按钮 id, 相对面板左边的 x, 相对面板顶的 y, 宽, 高)`，单位都是**页面 CSS px**。
///
/// 顺序与 `ui/index.html` 的 DOM 顺序一致，也是 [`crate::webui::START_PAGE_BUTTON_IDS`]
/// 的顺序（有单元测试焊死）。
const BUTTONS: [(&str, f32, f32, f32, f32); 5] = [
    ("btn-start", PANEL_LEFT, 0., PANEL_WIDTH, BTN_MAIN_H),
    ("btn-res", PANEL_LEFT, Y_RES, PANEL_WIDTH, BTN_H),
    ("btn-settings", PANEL_LEFT, Y_ROW, ROW_BUTTON_W, BTN_H),
    ("btn-activity", PANEL_LEFT + ROW_BUTTON_W + GAP, Y_ROW, ROW_BUTTON_W, BTN_H),
    ("btn-last", PANEL_LEFT, Y_LAST, PANEL_WIDTH, BTN_H),
];

/* ============================ 命中 ============================ */

/// 页面坐标（原点左上，宽 [`DESIGN_WIDTH`]，高 `page_h`）→ 命中的按钮 id。
///
/// 矩形取**左闭右开**（`x ∈ [left, right)`、`y ∈ [top, bottom)`）：这样相邻矩形之间
/// 那条 16px 缝不会同时命中两个，边界值也有确定答案（正好落在左边/上边算命中，
/// 正好落在右边/下边不算）。
fn hit_in_page(x: f32, y: f32, page_h: f32) -> Option<&'static str> {
    // `top: 50%` + `translateY(-50%)` ⇒ 面板垂直居中
    let panel_top = (page_h - PANEL_H) * 0.5;
    BUTTONS.iter().find_map(|&(id, bx, by, bw, bh)| {
        let top = panel_top + by;
        (x >= bx && x < bx + bw && y >= top && y < top + bh).then_some(id)
    })
}

/// 归一化触摸坐标 → 页面坐标（CSS px）。
///
/// 拟合公式的推导见模块文档「与屏幕的换算」。`k = 屏高 / 屏宽`。
fn ui_to_page(x: f32, y: f32, k: f32) -> (f32, f32) {
    let half = DESIGN_WIDTH * 0.5; // 640
    (half * (x + 1.), half * (y + k))
}

/// 归一化触摸坐标 `pos` + 屏幕尺寸 `screen`（像素，宽高）→ 命中的按钮 id。
///
/// `pos` 就是 `Touch::position`（`prpr` 已归一化，语义见模块文档）。
/// 屏幕宽高**只用到比值**，所以传逻辑像素还是物理像素都一样。
pub fn hit(pos: (f32, f32), screen: (f32, f32)) -> Option<&'static str> {
    let (w, h) = screen;
    if !(w > 0. && h > 0.) || !pos.0.is_finite() || !pos.1.is_finite() {
        return None;
    }
    let k = h / w; // = 1 / aspect
    let (px, py) = ui_to_page(pos.0, pos.1, k);
    hit_in_page(px, py, DESIGN_WIDTH * k)
}

/// 命中入口：命中开始页按钮就执行对应动作，返回是否命中。
///
/// 动作分发完全复用 [`crate::webui::invoke_button_action`]，
/// 也就是 JS `invoke(...)` 走的那条 `handle_ipc` 通路。
pub fn handle_touch(pos: (f32, f32), screen: (f32, f32)) -> bool {
    let Some(btn) = hit(pos, screen) else {
        return false;
    };
    crate::webui::invoke_button_action(btn)
}

/* ============================ 测试 ============================ */

#[cfg(test)]
mod tests {
    use super::*;

    /// 页面坐标 → 归一化触摸坐标。是 [`hit`] 里那套换算的**逆运算**，
    /// 用来把「页面里的某个点」搬到「屏幕坐标」，再喂回 `hit` 验证整条链路。
    fn page_to_touch(px: f32, py: f32, screen: (f32, f32)) -> (f32, f32) {
        let (w, h) = screen;
        let page_h = DESIGN_WIDTH * h / w;
        let x = px / DESIGN_WIDTH * 2. - 1.;
        let y = (py / page_h * 2. - 1.) * h / w;
        (x, y)
    }

    fn page_h_of(screen: (f32, f32)) -> f32 {
        DESIGN_WIDTH * screen.1 / screen.0
    }

    fn panel_top_of(screen: (f32, f32)) -> f32 {
        (page_h_of(screen) - PANEL_H) * 0.5
    }

    /// 常用屏幕尺寸（宽, 高）。宽高比覆盖 16:9 / 4:3 / 1:1 / 手机竖屏 / 长条。
    const SCREENS: [(f32, f32); 6] = [
        (1920., 1080.), // 桌面 16:9 横屏
        (1080., 1920.), // 竖屏 9:16
        (1170., 2532.), // iPhone 13 物理像素（≈19.5:9）
        (2048., 1536.), // iPad 4:3
        (1000., 1000.), // 正方形
        (800., 2400.),  // 极窄长条
    ];

    #[test]
    fn geometry_constants_match_the_css() {
        // 这几个数字就是模块文档里那条推导的落点，任何一个变了都说明 CSS 或推导被改过。
        assert!((PANEL_LEFT - 760.).abs() < 1e-6, "面板左边应为 760，实际 {PANEL_LEFT}");
        assert!((ROW_BUTTON_W - 222.).abs() < 1e-6, "并排按钮宽应为 222，实际 {ROW_BUTTON_W}");
        assert!((Y_RES - 116.).abs() < 1e-6);
        assert!((Y_ROW - 208.).abs() < 1e-6);
        assert!((Y_LAST - 300.).abs() < 1e-6);
        assert!((PANEL_H - 376.).abs() < 1e-6, "面板内容高应为 376，实际 {PANEL_H}");
        // 面板底 = 最后一个按钮的下沿，自洽性检查
        assert!((PANEL_H - (Y_LAST + BTN_H)).abs() < 1e-6);
        // 活动按钮右沿正好贴面板右边
        assert!((PANEL_LEFT + ROW_BUTTON_W + GAP + ROW_BUTTON_W - (DESIGN_WIDTH - PANEL_RIGHT_GAP)).abs() < 1e-6);
    }

    #[test]
    fn button_ids_match_the_shared_table() {
        let ids: Vec<&str> = BUTTONS.iter().map(|it| it.0).collect();
        assert_eq!(ids, crate::webui::START_PAGE_BUTTON_IDS.to_vec());
    }

    /// 16:9 横屏（1920×1080）：page_h = 1280 × 1080 / 1920 = 720，panel_top = (720 − 376)/2 = 172。
    #[test]
    fn page_height_and_panel_top_for_16_9() {
        let screen = (1920., 1080.);
        assert!((page_h_of(screen) - 720.).abs() < 1e-3);
        assert!((panel_top_of(screen) - 172.).abs() < 1e-3);
    }

    #[test]
    fn each_button_center_hits_itself_in_page_coords() {
        let page_h = 720.;
        let panel_top = (page_h - PANEL_H) * 0.5;
        for &(id, bx, by, bw, bh) in BUTTONS.iter() {
            let cx = bx + bw * 0.5;
            let cy = panel_top + by + bh * 0.5;
            assert_eq!(hit_in_page(cx, cy, page_h), Some(id), "{id} 的中心 ({cx}, {cy}) 没命中");
        }
    }

    /// 左闭右开：正好落在左 / 上边算命中，右 / 下边不算。
    #[test]
    fn edges_are_left_closed_right_open() {
        let page_h = 720.;
        let top = panel_top_of((1920., 1080.)); // 172，刚好等于 btn-start 的上沿
        let bottom = top + BTN_MAIN_H; // 272，btn-start 的下沿

        // #btn-start：x ∈ [760, 1220)、y ∈ [172, 272)
        assert_eq!(hit_in_page(760., 172., page_h), Some("btn-start"), "左上角（含）应命中");
        assert_eq!(hit_in_page(759.9, 200., page_h), None, "左边外 0.1px 不应命中");
        assert_eq!(hit_in_page(1219.9, 200., page_h), Some("btn-start"), "右内 0.1px 应命中");
        assert_eq!(hit_in_page(1220., 200., page_h), None, "右边（不含）不应命中");
        assert_eq!(hit_in_page(1000., top - 0.1, page_h), None, "上边外 0.1px 不应命中");
        assert_eq!(hit_in_page(1000., bottom - 0.1, page_h), Some("btn-start"), "下内 0.1px 应命中");
        assert_eq!(hit_in_page(1000., bottom, page_h), None, "下边（不含）不应命中");
    }

    /// 设置 / 活动之间那条 16px 缝（x ∈ [982, 998)）谁都不命中。
    #[test]
    fn the_row_gap_hits_nothing() {
        let page_h = 720.;
        let y = panel_top_of((1920., 1080.)) + Y_ROW + BTN_H * 0.5;
        assert_eq!(hit_in_page(981.9, y, page_h), Some("btn-settings"));
        assert_eq!(hit_in_page(982., y, page_h), None);
        assert_eq!(hit_in_page(997.9, y, page_h), None);
        assert_eq!(hit_in_page(998., y, page_h), Some("btn-activity"));
        assert_eq!(hit_in_page(1219.9, y, page_h), Some("btn-activity"));
        assert_eq!(hit_in_page(1220., y, page_h), None);
    }

    /// 按钮之间那三条 16px 纵向缝谁都不命中。
    #[test]
    fn the_vertical_gaps_hit_nothing() {
        let page_h = 720.;
        let top = panel_top_of((1920., 1080.));
        let x = 900.; // 落在整宽按钮内、也在设置按钮内，避开设置 / 活动那条竖缝
        // (缝起点相对面板顶, 缝上面那个按钮, 缝下面那个按钮)
        //   100 → 116（开始游玩 | 资源包）
        //   192 → 208（资源包   | 设置）
        //   284 → 300（设置     | 上次游玩）
        let cases = [
            (BTN_MAIN_H, "btn-start", "btn-res"),
            (Y_RES + BTN_H, "btn-res", "btn-settings"),
            (Y_ROW + BTN_H, "btn-settings", "btn-last"),
        ];
        for (gap_off, above, below) in cases {
            let gap_start = top + gap_off;
            assert_eq!(hit_in_page(x, gap_start - 0.1, page_h), Some(above), "缝上方 0.1px 应命中 {above}");
            assert_eq!(hit_in_page(x, gap_start, page_h), None, "缝起点 {} 不应命中", gap_start);
            assert_eq!(hit_in_page(x, gap_start + GAP - 0.1, page_h), None, "缝末 0.1px 不应命中");
            assert_eq!(hit_in_page(x, gap_start + GAP, page_h), Some(below), "缝下方应命中 {below}");
        }
    }

    /// 面板外的一切都不命中（左上角、面板左侧、面板下方、页面下缘）。
    #[test]
    fn outside_the_panel_hits_nothing() {
        let page_h = 720.;
        let top = panel_top_of((1920., 1080.));
        for (x, y) in [
            (0., 0.),
            (640., page_h * 0.5),
            (759., top + 50.),
            (1230., top + 50.),
            (1000., top - 1.),
            (1000., top + PANEL_H + 1.),
            (1000., page_h),
        ] {
            assert_eq!(hit_in_page(x, y, page_h), None, "({x}, {y}) 不该命中任何按钮");
        }
    }

    /// 不同宽高比下，把每个按钮的页面中心换算成触摸坐标，再喂回 `hit` 必须命中同一个按钮。
    #[test]
    fn hit_round_trips_across_aspect_ratios() {
        for screen in SCREENS {
            let page_h = page_h_of(screen);
            let panel_top = (page_h - PANEL_H) * 0.5;
            for &(id, bx, by, bw, bh) in BUTTONS.iter() {
                let cx = bx + bw * 0.5;
                let cy = panel_top + by + bh * 0.5;
                let pos = page_to_touch(cx, cy, screen);
                assert_eq!(
                    hit(pos, screen),
                    Some(id),
                    "屏幕 {screen:?} 上 {id} 的中心（页面 ({cx}, {cy})、触摸 {pos:?}）没命中"
                );
            }
            // 屏幕左上角（页面原点）不该命中
            assert_eq!(hit(page_to_touch(1., 1., screen), screen), None, "屏幕 {screen:?} 的左上角不该命中");
            // 面板左侧空白处不该命中
            let mid = panel_top + PANEL_H * 0.5;
            assert_eq!(hit(page_to_touch(400., mid, screen), screen), None, "屏幕 {screen:?} 的面板左侧不该命中");
        }
    }

    /// 屏幕正中 (0, 0) 必须换算到页面正中：这是 `y` 那条公式最容易写错的地方。
    #[test]
    fn screen_center_is_page_center() {
        for screen in SCREENS {
            let page_h = page_h_of(screen);
            let k = screen.1 / screen.0;
            assert!((ui_to_page(0., 0., k).1 - page_h * 0.5).abs() < 1e-3, "屏幕 {screen:?}");
            assert!((ui_to_page(0., 0., k).0 - DESIGN_WIDTH * 0.5).abs() < 1e-3);
        }
    }

    /// `ui_to_page` 与测试里的 `page_to_touch` 必须互为逆运算。
    #[test]
    fn ui_to_page_is_the_inverse_of_page_to_touch() {
        for screen in SCREENS {
            let k = screen.1 / screen.0;
            let page_h = page_h_of(screen);
            for &(px, py) in &[
                (0., 0.),
                (640., page_h * 0.5),
                (760., page_h * 0.25),
                (1220., page_h),
                (DESIGN_WIDTH, page_h * 0.75),
            ] {
                let (x, y) = page_to_touch(px, py, screen);
                let (rx, ry) = ui_to_page(x, y, k);
                assert!((rx - px).abs() < 1e-2, "屏幕 {screen:?} x: {px} → {rx}");
                assert!((ry - py).abs() < 1e-1, "屏幕 {screen:?} y: {py} → {ry}");
            }
        }
    }

    /// 竖屏手机上：屏幕右中部 = 开始游玩，屏幕左侧 / 顶部空白不命中。
    ///
    /// 用的是具体数字（iPhone 13 的 1170×2532），确保「按设计宽缩放」这件事真的生效。
    #[test]
    fn portrait_phone_concrete_positions() {
        let screen = (1170., 2532.);
        // page_h = 1280 × 2532 / 1170 = 3240960 / 1170 = 2770.05128…；
        // panel_top = (page_h − 376) / 2 = 2394.05128 / 2 = 1197.02564…
        let page_h = page_h_of(screen);
        assert!((page_h - 2770.0513).abs() < 0.01, "page_h = {page_h}");
        let panel_top = (page_h - PANEL_H) * 0.5;
        assert!((panel_top - 1197.0256).abs() < 0.01, "panel_top = {panel_top}");

        // 开始游玩（页面 y ∈ [1197.13, 1297.13)）中心
        let pos = page_to_touch(990., panel_top + 50., screen);
        assert_eq!(hit(pos, screen), Some("btn-start"));
        // 上次游玩（页面 y ∈ [1309.13, 1385.13)）中心
        let pos = page_to_touch(990., panel_top + Y_LAST + 38., screen);
        assert_eq!(hit(pos, screen), Some("btn-last"));
        // 活动按钮（x ∈ [998, 1220)）
        let pos = page_to_touch(1100., panel_top + Y_ROW + 38., screen);
        assert_eq!(hit(pos, screen), Some("btn-activity"));
        // 设置按钮（x ∈ [760, 982)）
        let pos = page_to_touch(800., panel_top + Y_ROW + 38., screen);
        assert_eq!(hit(pos, screen), Some("btn-settings"));
        // 资源包（整宽）
        let pos = page_to_touch(990., panel_top + Y_RES + 38., screen);
        assert_eq!(hit(pos, screen), Some("btn-res"));

        // 左上角（人物立绘 / logo 那边）不命中
        assert_eq!(hit(page_to_touch(100., 100., screen), screen), None);
        // 面板下方不命中
        assert_eq!(hit(page_to_touch(990., panel_top + PANEL_H + 5., screen), screen), None);
    }

    /// 退化输入不该 panic，也不该命中。
    #[test]
    fn degenerate_inputs_are_rejected() {
        assert_eq!(hit((0., 0.), (0., 0.)), None);
        assert_eq!(hit((0., 0.), (0., 1080.)), None);
        assert_eq!(hit((0., 0.), (-1920., 1080.)), None);
        assert_eq!(hit((f32::NAN, 0.), (1920., 1080.)), None);
        assert_eq!(hit((0., f32::INFINITY), (1920., 1080.)), None);
    }

    /// 面板比页面还高（极端宽高比）时不该出乱子：`panel_top` 变负，面板整体上溢出页面，
    /// 但「区间之内命中、区间之外不命中」这条规则不变。
    #[test]
    fn panel_taller_than_page_does_not_panic_or_wrap() {
        let page_h = 300.; // 比 PANEL_H(376) 还小 ⇒ panel_top = −38
        let panel_top = (page_h - PANEL_H) * 0.5;
        assert!(panel_top < 0., "panel_top 应为负，实际 {panel_top}");
        assert_eq!(hit_in_page(990., panel_top - 1., page_h), None, "面板上方不该命中");
        assert_eq!(hit_in_page(990., panel_top + PANEL_H + 1., page_h), None, "面板下方不该命中");
        // 面板内部（虽然已经溢出页面）仍按同一套规则命中
        assert_eq!(hit_in_page(990., panel_top + BTN_MAIN_H * 0.5, page_h), Some("btn-start"));
    }
}
