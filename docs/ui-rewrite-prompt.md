# 提示词：把 PhiRa-Mod-Meew-11 的界面整体重写为纯 Rust 原生实现（零 WebView）

> 用法：把下面 `====` 之间的全部内容整段复制给编码 AI（或新开一个会话），即可开工。
> 里面所有路径、命令、数值都是从当前代码里核实过的真实值，不是示意。

---

====

## 0. 你的角色

你是一名 Rust + 图形界面工程师。任务是把 **PhiRa-Mod-Meew-11**（Phigros 的 Phira 改版客户端）
的**整个应用界面**重写为**纯 Rust 原生绘制**的实现，视觉风格是「磨砂玻璃卡片」。

界面已经完全没有 WebView 了（2026-10-09 拆掉的），**这次重写不允许以任何形式把 web 技术引回来**。
你的工作是：在现有引擎（macroquad fork + `prpr` 自绘 UI 框架）之上，用一套统一的卡片设计系统，
把外壳 + 全部页面重新实现一遍，做到「深/浅主题都好看、鼠标和触摸都能用、任意分辨率比例一致」。

先通读代码再动手。**不要凭想象发明 API** —— 这个工程有一套已经成型的 `crate::newui` 设计系统，
它就是唯一真源。

## 1. 仓库与构建（硬事实）

- 主工程根：`E:\Code\Rust\Phigros\PhiRa-Mod-Meew-11`
- 工作区成员：`phira`（库，UI 全在这里）、`phira-main`（桌面 exe 入口）、`prpr`（自绘 UI 引擎）、`prpr-l10n`
- UI 代码位置：`phira/src/newui.rs`（设计系统，2710 行）、`phira/src/page/*.rs`（13 个页面）、
  `phira/src/scene/*.rs`（独立场景）、`phira/src/hud.rs`、`phira/src/ui/`（自绘对话框等）

**构建命令（必须带这两个环境变量，否则会报 `os error 5 拒绝访问` 或撞 rustc ICE）：**

```bash
export CARGO_TARGET_DIR='D:\meew11-target'
export CARGO_INCREMENTAL=0
cargo +stable check -p phira-main --no-default-features      # 桌面
cargo +stable check -p prpr --lib                            # 引擎
cargo +stable build --release -p phira-main --no-default-features   # 出 exe
```

**验收标准：编译告警必须恰好是这 5 条，一条都不许多**（都是历史遗留，`dead_code` / `unused` 类）：
`hud.rs` 的 `unused_mut`、`newui.rs` 的 `unused_assignments`(`rx`)、`page/history.rs` 的 `CARD_BG`、
`page/library.rs` 的 `rgb`、`threed.rs` 的未用关联项。
**你新增的任何告警都要自己修掉**，不要用 `#[allow]` 掩盖（除非该处确实是设计需要的死代码，
那也要写注释说明）。

> 注：`f32::NAN` / `f32::INFINITY` 在部分上下文会被判成已弃用的模块常量，
> 那种地方要写成 `<f32>::NAN` / `<f32>::INFINITY`。

## 2. 绝对禁止 / 硬约束

1. **禁止任何 web 技术**：不引入 `wry` / `tao` / `webview2` / `servo` / `ultralight` /
   任何内嵌浏览器；不加载 HTML/CSS/JS；不用远程页面当界面；不新增 `webui` 之类的 feature。
   `phira/src/webui*.rs`、`phira/ios/webui.m`、`android/.../WebUIView.java`、`phira/ui/` 都已删除，
   **不要恢复**（备份在 `E:\Code\Rust\Phigros\_meew11-src-backup\webui-removed-2026-10-09\`，仅供查阅）。
2. **`phira/ios/ime.m` 不许删** —— 那是 iOS 软键盘通道（隐藏 `UITextField`），
   删了 iOS 上自绘输入框一个字也敲不进去。
3. **`phira/art/assets/3.png` 是开始页左立绘**（`home.rs` 用 `include_bytes!` 编译期嵌入），不是 web 素材。
4. 不 commit、不 push。
5. 不改用户存档：`rev/phira-pc/data/`、`PhiRa-Mod-Meew-11/rev/phira-pc/data/`（两处是**独立**的两套存档）。
6. 不要启动程序、不要截图（本机没有可用的图形验证手段）。
7. 不要动游戏内渲染（`prpr/src/scene/game.rs` 的判定线 / 音符）和 `hud.rs` 的打歌 HUD，
   除非任务明确要求 —— 这次重写的范围是**应用界面**。

## 3. 设计系统（唯一真源：`phira/src/newui.rs`）

### 3.1 坐标系与换算（必须理解，否则版式全错）

引擎 UI 的世界坐标里 **屏幕宽恒等于 2 个单位**（即 x ∈ [-1, 1]）。
设计稿按「横屏 1440px 宽 / 竖屏 860px 宽」绘制，通过 `Metrics` 映射：

```
长度：u(px) = px * k * 2 / viewport_w
字号：f(px) = px * k / (0.04 * viewport_w)     // 喂给 ui.text(..).size(..)
```

`k = (viewport_w / ref_w).clamp(k_min, k_max)`，其中：

- `REF_WIDE = 1440.`、`REF_NARROW = 860.`
- 桌面：`k ∈ [SCALE_MIN=1.0, SCALE_MAX=1.8]`
- 移动端（`GL 物理视口宽 / 逻辑宽 ≥ 1.5` 判定）：`k ∈ [1.5, 2.2]`
- `compact = aspect < 1.15`（窄屏/竖屏，开始页把立绘铺满当背景）
- `tiny = aspect < 0.75`（进一步压缩，状态栏收起次要项）

**关键点**：换算与真实视口尺寸无关，所以窗口怎么缩放版式比例都不变。
页面里所有尺寸/字号都必须走 `m.u(px)` / `m.f(px)`，**不许写死世界坐标数字**（除极少数刻度线）。

### 3.2 主题

`Theme`（`Copy`）有两个常量：`Theme::DARK`、`Theme::LIGHT`，字段与设计稿 CSS 变量一一对应。
每帧用 `newui::theme()` 取当前主题，`newui::set_dark(bool)` 切换。全局开关存在 `Config.ui_dark`。

深色（`.DARK`）关键值：

| 字段 | 值 | 说明 |
|---|---|---|
| `bg` | `#0d0d0d` | 页面底色 |
| `bg_alt` | `#151619` | |
| `bg_elev` | `rgba(0.125,0.141,0.169, 0.82)` | **半透明**，卡片底板（背景光斑要透上来） |
| `bg_hover` | `#2b3440` | |
| `border` / `border_soft` | `#3a404b` / `#262b34` | |
| `text` / `text_hi` / `text_dim` / `text_mute` | 白 0.98 / 白 1.0 / 白 0.80 / 白 0.58 | 主文字纯白 |
| `accent` / `accent_hi` / `glow` | `#0096ff` / `#5fd1ff` / `#4cc2ff` | |
| `blue`/`teal`/`yellow`/`orange`/`red` | `#7cc4ff` / `#55f0cf` / `#f2e19a` / `#ffb171` / `#ff6b7d` | |
| 难度色 `ez`/`hd`/`insane`/`at` | `#66ff9c` / `#5dd4ff` / `#ff7a89` / `#0a0c10` | EZ 绿 / HD 蓝 / IN 红 / AT 黑 |

浅色（`.LIGHT`，Win11 亚克力风）：`bg #f3f3f3`、`bg_alt #fafafa`、
`bg_elev = 白 0.72`、`border #d9d9d9`、`text #1b1b1b`、`text_hi #000`、
`text_dim #5d5d5d`、`text_mute #919191`、`accent #0067c0`、`accent_hi #0078d4`、`glow #0f6cd0`、
`teal #007f6e`、`orange #b45a00`、`red #c42b1c`、
难度色加深一档：`ez #0f7b0f` / `hd #0067c0` / `insane #c42b1c` / `at #171717`。

辅助方法：`th.with_a(c, a)`、`th.black(a)`、`th.white(a)`、`th.wash(a)`
（`wash` = 深色主题加白 / 浅色主题加黑，悬停、边框、分隔线都用它）、
`th.diff_kind(level)` → `Neon(Color)` / `Black` / `Rainbow`、`th.diff_color(level)`。

### 3.3 毛玻璃配方（**照抄，不要自创**）

工程里**没有真·背景模糊**（曾经做过抓 framebuffer + 高斯模糊，会吞掉元素、代价极高，已删除）。
磨砂观感是纯几何叠层，零渲染状态：

**`acrylic(ui, r, radius, th)`** —— 面板/卡片底板：
1. `frosted_halo`：边缘柔光 = 同色圆角块向外层层扩散（`glow_rect`），
   深色 `white(0.10)` / 浅色 `white(0.42)`，扩散 `radius*0.9 + 0.030`，6 层；
   `r.w < 0.02 || r.h < 0.02` 时跳过（省顶点）。
2. 半透明底板 `th.bg_elev`。
3. 雾面：上半段 `r.h * 0.55` 的白色渐变，深色 `white(0.055)` / 浅色 `white(0.38)` → 0。
4. 描边 `th.border`，宽 `0.001`。
5. 顶部高光：`Rect(r.x + radius*0.5, r.y + 0.0012, r.w - radius, 0.0018)`，
   深色 `white(0.22)` / 浅色 `white(0.85)`。
6. 磨砂颗粒：细密横线，`step = 0.0073`，线高 `0.0006`，左右内缩 `0.002`，
   深色 `white(0.032)` / 浅色 `white(0.22)`。

> ⚠️ 这几层的亮度/扩散是**被用户专门调大一档**过的（他反馈过「看不出模糊」）。
> **改小就会回到「看不见」**。要调整请往「更明显」的方向调。

**卡片底板两档（都用 `acrylic` 打底）：**

- `glass_plate_w(ui, r, radius, th, hp, tint, border)` —— **带投影**，列表 / 独立卡片用。
  额外：柔和阴影 `glow_rect` 黑 `dark(0.30+0.12hp)` / `light(0.16+0.08hp)`，
  扩散 `radius*0.55`，5 层；玻璃亮面 `dark white(0.05+0.06hp)` / `light white(0.24+0.22hp)`；
  `tint`（强调色，`a=0` 表示不叠）；顶部反光高 `0.0012`；
  描边 `rounded_stroke` 宽 `0.0013`。`radius` 会被夹到 `min(r.h/2, r.w/2)`。
- `glass_plate_flat(ui, r, radius, th, hp, tint, border, edge)` —— **不带投影**，
  「一堆按钮紧密竖排在同一面板里」用（每个都带投影会糊成一片，看不清卡片边界）。
  反光/描边宽度由 `edge` 传入（通常给 `m.u(1.)`）。
- `glass_card(ui, r, radius, th, hp)` —— 最常用的一行调用：
  `hp > 0.01` 时先 `glass_glow`，再 `glass_plate_w` 配 `glass_border(th, hp)`。

**配色辅助：**

- `glass_border(th, hp)`：深色 `white(0.16 + 0.18hp)` / 浅色 `black(0.09 + 0.08hp)`
- `glass_text(th, hp)`：深色 `mix(white(0.92), white(1.), hp)` / 浅色 `mix(text, text_hi, hp)`
- `glass_glow(ui, r, radius, th, hp)`：强调色外发光，`a = 0.10 + 0.14hp`，扩散 `radius*0.8`，6 层

### 3.4 圆角与设计令牌

`px` 模块（设计稿像素）：

```
TITLEBAR_H = 36.    STATUSBAR_H = 30.
MENU_ITEM_H = 58.   MENU_GAP = 10.   MENU_ICON = 20.   MENU_RADIUS = 6.
CARD_RADIUS = 10.   CHIP_RADIUS = 3.  PANEL_RADIUS = 7.
```

圆角矩形：`rounded(ui, r, radius, color)`、`rounded_stroke(ui, r, radius, width, color)`。
**不要自己写圆角路径**。

### 3.5 交互

- `hover_scale(r, hp)`：悬停缩放，`1 + 0.014 * hp`（锚点居中）。幅度刻意很小，
  是「卡片有生命感」和「抖动」的分界线，**别加大**。
- `hover_step(&mut Anim<f32>, on, t)`：0.16s 缓动的悬停进度，返回当前值 `hp ∈ [0,1]`。
  所有悬停态都必须走它（不要瞬切）。
- `press_scale(pr)`：按下回弹（0.985 ↔ 1.0）。
- `pulse(t, period, phase)`：呼吸脉冲（红点、光点用）。
- `enter_p(t, delay)`：入场淡入进度（各区块依次延迟出现）。
- `mouse_world() -> (f32, f32)`：当前鼠标的世界坐标（与 `Touch::position` 同一套坐标系）。
  判定悬停：`ui.rect_to_global(r).contains(vec2(mx, my))`。

### 3.6 控件库（已备好，直接用）

按钮（两种尺度，**不要自己画按钮**）：

- 紧凑尺度（自绘输入对话框等小面积）：`draw_main_btn` / `draw_sec_btn` / `draw_quit_btn` / `draw_white_btn`
- 大尺度（开始页那列）：`draw_main_btn_lg` / `draw_sec_btn_lg` / `draw_row_btn_lg` / `draw_quit_btn_lg`
  - 大尺度标签 31px / 内边距 44px / 图标 20px；紧凑尺度 24 / 24 / 18

其它：

- `draw_list_row(ui, th, r, hp, icon, title, sub, value, size)` —— 列表行（设置项 / 消息行 / 历史行）
- `page_title(ui, m, th, x, cy, title, sub, size_px)` —— 左侧强调条 + 带字距主标题 + 弱化副标题
- `accent_bar(ui, r, from, to, radius)` —— 左侧竖向渐变强调条
- `draw_icon(ui, m, Icon, r, color, stroke_px)` —— 34 个矢量图标：
  `Logo, Sun, Moon, Minimize, Maximize, Close, Bolt, Chat, Chart, Gear, User, Play, Info,
  Logout, Music, Search, ChevronLeft, ChevronRight, Sort, Wave, Clock, Layers, List, Star,
  Heart, Cloud, Plus, Menu, Download, Trophy, Edit, Delete, Filter, X`
- 文字：`text` / `text_with_font` / `text_center` / `measure` / `text_ellipsis` / `text_multiline` /
  `text_tracked`（带字距，CSS `letter-spacing`）/ `tracked_width` / `page_title` /
  `rainbow_text`（SP 难度）/ `text_diff`（难度色文字）/ `text_aura`（文字外发光）
- 渐变：`vgrad` / `hgrad` / `radial`；发光：`glow_rect` / `glow_icon`；
  几何：`scaled_rect` / `rect_key` / `contain_bottom` / `cover_left_bottom` / `draw_image_feathered`
- 透视：`css_perspective` / `with_perspective` / `unproject`（倾斜/视差用）

### 3.7 背景

`draw_background(ui, &m, &th, t, base)`，每页在最底层调用一次：

1. **恒定纯深色底** `Color::new(0.051, 0.055, 0.063, 1.)` —— 与主题无关（光波是加色辉光，
   浅色主题下白底会把它吃掉）。
2. **TriGrid 光波**（`prpr::core::draw_trigrid`，着色器实现；`TEX_SIZE = [0.30, 0.36]`）。
   失败时回退到 `draw_grid` 的移动网格。
3. **环境光**：三团径向光斑漂浮，周期 26s / 32s / 38s。
   深色 `rgba(0,122,204,0.30)` 等；浅色换成 `rgba(11,107,203,0.16)` 等。
4. **暗角（vignette）**。

### 3.8 外壳现状（重要）

- `SHOW_TITLEBAR = false`、`SHOW_STATUSBAR = false` —— **用户明确要求把顶部标题栏、
  底部状态栏、最左侧全局竖向活动栏全部删掉**（`px::ACTIVITY_W` 等常量也已删除）。
  现在 `Shell::render` 提前 return，实际不画任何东西。
- 页面栈顶层的返回键由 `scene/main.rs` 画：`newui::back_rect` / `back_hit_rect`，
  方形圆角 + `ChevronLeft`，跟着页面过渡滑入滑出。
- **不要再把活动栏/标题栏/状态栏画回来**，除非任务里明确要求。
- `newui::current_activity()` / `Activity` 枚举（`Home/Library/Messages/Ranking/Settings/Account`）
  仍保留为页面状态，但已无绘制。

## 4. 页面与场景清单

### 4.1 页面（`impl Page for`，13 个）

`Page` trait（`phira/src/page.rs`）：

```rust
fn label(&self) -> Cow<'static, str>;                      // 标题（会被外壳用作场景名）
fn update(&mut self, s: &mut SharedState) -> Result<()>;
fn touch(&mut self, touch: &Touch, s: &mut SharedState) -> Result<bool>;
fn render(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()>;
// 可选：enter / render_top / on_result / pause / resume / exit / can_play_bgm /
//        next_page -> NextPage{None|Overlay(Box<dyn Page>)|Pop} / next_scene / on_back_pressed
```

| 文件 | 页面 | 内容 |
|---|---|---|
| `page/home.rs` | 开始页 | 左立绘 + 右侧**一张卡片**装着品牌字标与 8 个入口 |
| `page/library.rs` | 曲库 | 谱面列表、搜索、排序、标签筛选、收藏、导入；底部状态信息 |
| `page/song_detail.rs` | 谱面详情 | 单曲信息、难度、成绩、开始游玩 |
| `page/favorites.rs` | 收藏 | 收藏夹列表 + 收藏项 |
| `page/history.rs` | 历史 | 游玩记录列表 |
| `page/message.rs` | 消息 | 消息列表 |
| `page/settings.rs` | 设置 | 分组设置项（含「账号管理」「日志」两个子页） |
| `page/respack.rs` | 资源包 | 资源包列表、启用/禁用、导入 |
| `page/event.rs` | 活动 | 大图活动卡片（纵向滚动 + 上下翻页箭头） |
| `page/coll.rs` | 合集 | 章节合集横向滚动卡片 |
| `page/blacklist.rs` | 黑名单 | 用户黑名单列表 + 添加 |
| `page/offset.rs` | 偏移校准 | 判定线 + 偏移调节 |
| `page/timing_editor.rs` | 准度条编辑器 | 准度条可视化编辑面板 |

### 4.2 被内嵌的子件（不是独立页面，但也属于界面）

- `page/rks.rs` —— RKS / 排行列表
- `page/challenge.rs` —— 课题模式三槽位选曲条（`challenge_ui.rs` 也是这套）
- `page/best_board.rs` + `page/best_board_export.rs` —— 最佳成绩榜与导出
  （⚠️ 这张榜**故意不用 `newui::acrylic`**，因为它要保持固定配色，注释里有说明）
- `page/user.rs` —— 账号管理**后端**（无 render，界面在 `settings.rs`）

### 4.3 独立场景（`phira/src/scene/`）

`main.rs`（主场景 / 页面栈 / 返回键 / 过渡）、`chapter.rs`、`event.rs`、`profile.rs`、
`song.rs`、`challenge.rs`、`unlock.rs`、`chart_order.rs`、`loading`。
这些也走 `newui` 的玻璃语言。**游戏内（`GameScene`）的判定线与 HUD 不在本次范围内。**

## 5. 开始页版式（用户有明确要求，必须遵守）

用户原话：**「保留左立绘，面板做成卡片」**。

- **左立绘**：`phira/art/assets/3.png`（`include_bytes!` 编译期嵌入），
  左边贴屏幕、上下顶满、等比 `cover` 裁切、**右边缘羽化渐隐**
  （逐顶点 alpha 着色，见 `home.rs` 的 `ArtFeather`，对应 CSS
  `mask-image: linear-gradient(to right, black 55%, transparent 100%)`）。
  ⚠️ **立绘必须保持平面，不要做倾斜/透视**（用户明确否掉了 `css_perspective` 那版）。
  宽度：横屏 `min(屏宽 * 0.52, u(780))`；窄屏铺满整屏当背景。
- **右侧面板 = 一张玻璃卡片**（`glass_card`），里面装：
  品牌字标（`SYSTEM READY` 呼吸点 + `Phirc Mod` + 橙色 `++`，右对齐 + 细分割线）、
  然后自上而下 8 个入口：
  `开始游玩/ GAME →`（主按钮）、`资源包`（次级）、`设置 | 活动`、
  `上次游玩`（白底深字卡片，带封面/难度 chip/分数/播放圆钮）、`消息 | 关于`、`退出`（弱化）。
- 卡片内边距 **18 / 16**（比按钮间距 16 大一点，否则卡片边界和按钮边界会糊在一起）；
  卡片淡入延迟 0.02，各按钮依次延迟 `[0.14, 0.20, 0.26, 0.26, 0.32, 0.38, 0.38, 0.44]`。
- 整列高度按可用高度等比收缩（下限 0.62），品牌区 + 8 个按钮 + 间距一起算。
- 右下角版本号 `Version: v X.Y.Z`；左下角提示。
- 点击时有 0.62s 的径向高光（`.flash`）。
- **视差**：跟设备姿态（陀螺仪/重力）走，桌面没有传感器时回退鼠标指针；
  开关 `Config.gyro_parallax` + 幅度 `Config.gyro_scale_clamped()`（默认开、1.0）。
  立绘位移 ±6~10px，按钮 ±4~6px。

## 6. 其它页面的统一版式要求

1. **背景**：每页先 `draw_background`。
2. **内容区**：`Metrics::content()` / `ui.content_rect()`（活动栏已删，内容区是整宽）。
3. **页面标题**：用 `page_title(...)`（左侧强调条 + 带字距标题 + 弱化副标题），
   不要自己排版标题。
4. **列表行**：一律 `glass_card` + 悬停提亮（`hover_step`）+ 左侧 `accent_bar` 强调条 +
   右侧操作按钮。行高走 `px::MENU_ITEM_H` / 页面自己的 `hud::param` 可调参数。
5. **卡片**：内容必须落在玻璃卡片里，不要直接把文字铺在光波背景上（可读性差）。
   一屏里卡片之间留 `px::MENU_GAP` 级别的间距。
6. **过渡**：`SharedState::render_fader` 包住页面内容；**每个区块一个 `render_fader` 闭包**，
   不要合并成一个（上游 phira-0.8.2 的惯例）。
7. **滚动**：`prpr::ui::Scroll`，横向用 `.horizontal()`。
8. **响应式**：`m.compact`（窄屏）和 `m.tiny`（极窄）都要照顾到 ——
   横排改竖排、次要信息收起、立绘退成背景。
9. **深/浅主题都要真的可用**：浅色主题下不能出现「白底白字」「白描边」。
   所有压在图片/封面上的文字必须**写死白色 + 黑投影**（不能跟主题走）。
10. **本地化**：所有面向用户的文案走 `tl!` / `ttl!` / `rtl!`（`phira/locales/{zh-CN,en-US}/*.ftl`），
    新增文案要同时补两个语言文件。

## 7. 代码规范与已知陷阱（踩过的坑，务必避开）

1. **`ui.dx` / `ui.dy` 是持久修改变换**，只有 `ui.scope` / `ui.with` / `abs_scope` 才还原。
   同一闭包里先 `dx/dy` 再画别的东西 → 后者会被平移到错位置。
2. **`semi_white` 在浅色主题下是黑墨水**。给贴图着色必须用 `Color::new(1.,1.,1.,a)`。
3. **`DrawText` 默认色 = `theme::ink(1.)`**，浅色主题下是黑的。压在深色画面（封面、立绘）上
   必须显式 `.color(WHITE)`。
4. **不要在页面里重复实现玻璃/按钮/排版** —— 全部调 `newui`。
   （`home.rs` 里曾经各留了一份 292 行的副本，导致「改了 newui 的质感、开始页却没变」，已删。
   别再犯。）
5. **不要用 `#[allow(dead_code)]` 之外的 allow 掩盖告警**。
6. **不要写死世界坐标数字**：尺寸走 `m.u(px)`、字号走 `m.f(px)`。
7. 卡片圆角走 `px::CARD_RADIUS` 一档（10），小 chip 用 `CHIP_RADIUS`（3），面板用 `PANEL_RADIUS`（7）。
8. 难度文字用 `text_diff` + `th.diff_kind(level)`，不要自己判 `EZ/HD/IN/AT/SP`。
9. `prpr::ui::clip_rounded_rect` 可以把图片裁进圆角（引擎的 UV 着色不带圆角）。
10. 需要新 API 时**加到 `newui` 里**，并写清用途注释 —— 它是这个工程唯一的界面设计系统。

## 8. 交付方式与验收

1. **先出一份方案再动手**：列出你要改/新建的文件、每个页面的卡片结构草图（文字描述即可）、
   复用哪些 `newui` 控件。等我确认后再写代码。
2. 每完成一个页面就跑一次
   `CARGO_TARGET_DIR=D:\meew11-target CARGO_INCREMENTAL=0 cargo +stable check -p phira-main --no-default-features`，
   保证告警始终只有那 5 条。
3. 全部改完后出一份总结：改了哪些文件、每个页面变成了什么样、
   深/浅主题与窄屏下分别怎么处理、还剩哪些已知问题。
4. **不要 commit / push**；不要发布 exe / APK（发布由我另外说）。
5. 如果某个页面的现有实现已经很符合卡片风格（大部分都是），
   **不要为了「重写」而重写** —— 只做必要的统一与修正，并说明为什么保留。

## 9. 参考实现（可查阅，别照抄 web 部分）

- 上游官方界面：`E:\Code\Rust\Phigros\phira-0.8.2\phira-0.8.2\`（注意两层同名目录）
- 移植来源：`E:\Code\Rust\Phigros\PhiraiAd-1.1.0`
- 旧版源码备份：`E:\Code\Rust\Phigros\_meew11-src-backup\`（含 `phira-src/`、`prpr-src/` 全量）

====
