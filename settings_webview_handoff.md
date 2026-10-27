# 设置界面 WebView 化 — 接手文档

> 项目：PhiRa-Mod-Meew-11（Phigros PC 改版，Rust + macroquad + prpr 引擎）
> 生成日期：2026-10-11
> 状态：**代码实现已完成，尚未编译 / 发布**。下一步是 build + release + 运行时验证。

## 0. TL;DR（给接手者）
- 设置界面已经**完全重写为 WebView 加载 `D:\Phigros\kaichang\Setting.html`**，并在 HTML 里把 8 个标签页的
  所有开关 / 滑块 / 输入框**接上了真实游戏配置**（读写 `Config` + 触发即时副作用）。
- 三处原生入口（HUD 设置按钮、首页面板设置、首页活动设置、开始页设置按钮）现在都跳进 WebView 设置页。
- 触控 / 输入优化：滑块从 `mouse*` 事件换成 **Pointer Events**
  （`pointerdown / pointermove / pointerup / pointercancel` + `setPointerCapture`），鼠标和触摸统一。
- **还没做**：编译、发布 exe、运行时验证。
- 编译命令（Windows）：`cargo +stable build --release -p phira-main --no-default-features --features webui`
  （**必须带 `--features webui`**）。

## 1. 目标回顾
用户原始需求：「接下来把设置界面完全照搬 `D:\Phigros\kaichang\Setting.html` 并优化触控和输入」，
并选定实现方式「**WebView 加载 HTML**」（不重新用 Rust 自绘），且明确「**这次全接好**」——
即控件全部读写真实 `Config` 并触发生效（含日志页读真实 `phira-crash.log`、重置恢复默认）。

## 2. 架构与数据流
### 2.1 WebView 宿主
- 宿主代码：`phira/src/webui.rs`（wry 0.44）。
- 自定义协议 `phirc://web/<path>` 把 `D:\Phigros\kaichang` 当作 web 根目录
  （`index.html` = 开始页，`Setting.html` = 设置页，`tripsUide.html` = 开始页别称）。
- `with_initialization_script` 注入：BRIDGE_JS + ALERT_JS + DEBUG_JS + HOTSPOT_JS + 新增 `SETTINGS_NAV_JS`。
- `set_bounds` 做窗口大小同步，`zoom()` 做等比缩放适配（设计分辨率 → 窗口，避免 973×608 下布局重叠）。

### 2.2 IPC 桥
- 页面 → 宿主：`window.phirc.postMessage(JSON.stringify({ id, cmd, args }))`。
- 宿主命令（`handle_ipc` 里分发）：
  - `get_config` → 返回 `crate::get_data().config` 的 JSON（serde camelCase）。
  - `set_config` → 把 `args.update`（或整份 `args`）合并进 `PENDING_CONFIG`（仅暂存，**不**当场改存档）。
  - `get_log` → 读 `<crate::dir::root()>/phira-crash.log` 返回文本（日志页用）。
  - `reset_config` → `data.config = Config::default()` + 存档 + 触发副作用。
  - 历史命令：`listCharts` / `startGame` / `hide` / `openPage` / `openUrl` / `log` / `navigate`。
- 宿主 → 页面：`reply(json!({ ok, data / error }))` 通过 wry evaluate。

### 2.3 线程安全（重点）
- wry 的 IPC 回调跑在**独立线程**，不能直接碰游戏状态。沿用既有 `PENDING_*` 模式：
  - IPC 线程只把更新写进 `static PENDING_CONFIG: Mutex<Option<serde_json::Value>>`。
  - 游戏线程每帧在 `main.rs` 的 `next_scene` 里调 `crate::webui::apply_pending_config()`：
    取出 `PENDING_CONFIG` → `merge_json` 合进当前 config → `save_data()` → 触发少数即时副作用。
- 这样滑块拖动时高频 `set_config` 也不会和数据竞争。

### 2.4 即时副作用（apply_config 里）
- `crate::newui::set_dark(cfg.ui_dark)` — 明暗主题。
- `prpr::ui::UI_SFX_VOLUME.store(cfg.volume_sfx.to_bits(), …)` — 音效音量
  （⚠️ 路径是 `prpr::ui`，**不是** `crate::ui`，这是编译坑，已修，见第 6 节）。
- `macroquad::window::set_fullscreen(cfg.fullscreen_mode)` — 全屏。
- `autoplay` 特殊：`Config::set_autoplay(on)` 会同时写 `mods` 和兼容字段；
  合并前把 `autoplay` 从更新里抽出来单独处理（否则「关掉重开又自己打开」）。

## 3. 改动文件清单
| 文件 | 改动 |
|---|---|
| `phira/src/webui.rs` | 加 `WebCmd::Navigate`；加 `open_settings()` / `host_available()`；加 `PENDING_CONFIG` / `apply_pending_config` / `merge_json` / `apply_config`；加 `get_config` / `set_config` / `get_log` / `reset_config` IPC；加 `SETTINGS_NAV_JS`（返回键逻辑）；线程循环加 `Navigate` 分支 |
| `phira/src/scene/main.rs` | HUD 的 `PageId::Settings` 改走 `open_settings()`；`if p != Home && p != Settings`（不再压原生页）；`next_scene` 里调 `apply_pending_config()` |
| `phira/src/page/home.rs` | 首页面板 `Settings`、首页活动 `Settings` 两处入口改走 `open_settings()`（去掉旧的 `SettingsPage` 推送） |
| `D:\Phigros\kaichang\tripsUide.html` | 开始页设置按钮：`openPage('settings')` → `location.href='Setting.html'`（留在 WebView 内，返回回开始页） |
| `D:\Phigros\kaichang\Setting.html` | **整体重写**：保留原视觉（深色 `#08080b`、skewX 标签、左上角返回、滚动主体、8 标签页、`@YiyiMo` 页脚、背景图、`phirc.png` logo、`cmdysj.ttf` 字体）；每个控件加 `data-config="<camelCase 键>"`；滑块用 Pointer Events；日志页读真实 `phira-crash.log`；重置按钮调 `resetConfig` |

> `phira/src/page/settings.rs`（Rust `SettingsPage`）**保留不动**，作为「WebView2 缺失」时的原生回退界面。

## 4. data-config 键 → Config 字段映射（已逐项核对 config.rs 存在）
### 开关（checkbox，`data-config` 直接等于字段 camelCase 名）
`uiDark` `fullscreenMode` `offlineMode` `mpEnabled` `disableEffect` `adjustTime`
`autoplay`(特殊) `showAcc` `apFcIndicator` `showAvgFps` `doubleClickToPause` `doubleHint`
`aggressive` `useKeyboard` `dragProtect` `flickProtect` `lineRefYAxis`
`uploadRecord`(需 `uploadAgreed` 才能开) `phiRecorderFont` `judgeText` `missMarker`
`noteTrail` `lineAfterimage` `lineGlow` `hud` `hudAcc` `hudCounts` `hudMaxCombo`
`timingBar.enabled`(嵌套) `chartDebug` `touchDebug`

### 滑块（`data-config` + `data-min` / `data-max` / `data-display`）
- `volumeMusic` / `volumeSfx` / `volumeBgm`：0–1，显示成百分比。
- `inGameTextScale`：0.6–2.2（Config 侧再 clamp 到 TEXT_SCALE_MIN/MAX）。
- `judgeTextSize`：0.2–1.5（Config clamp 0.2–1.5）。
- `noteTrailLen`：0–1；`noteTrailAlpha`：0–1。
- `lineAfterimageAlpha`：0–1。
- `musicSpectrumGain`：0.2–4（Config clamp 0.2–4）。
- `lineGlowStrength`：0–1。
- `hudSize`：0.5–2（Config clamp 0.5–2）。
- `touchMarkerAlpha`：0.05–1（Config clamp 0.05–1）。

### 数字输入
- `challengeRank`（u32）。

> 注意：滑块的 `data-min` / `data-max` 只是 UI 拖拽边界；Config 在读取时还会再 clamp 一次，
> 所以即使两端略有出入也不会写坏存档。

## 5. 编译 / 发布（接手必做）
### 环境变量（沿用项目约定，本机已设用户级；工具进程需显式 export）
```
CARGO_TARGET_DIR=D:\meew11-target
CARGO_INCREMENTAL=0          # 不设这个 D 盘增量会报「拒绝访问 os error 5」还会撞 rustc ICE
CARGO_HOME=D:\Environment\Rust\cargo
RUSTUP_HOME=D:\Environment\Rust\rustup
```
（git 连 github 需 `http.schannelCheckRevoke false` + `http.sslBackend schannel`；
cargo 侧 `config.toml` 已 `git-fetch-with-cli=true`。）

### 编译（仅 PC exe，不需要 rustix patch）
```
cargo build --release -p phira-main --no-default-features --features webui
```
> 注意：**不要**加 `cargo +stable`。`rust-toolchain.toml` 已钉 `nightly-2026-01-01`，
> 本工程所有成功构建都走这个 nightly（日常日志可查）；强行 `+stable` 反而可能编不过。
> `--no-default-features` 不会自动开 webui，所以 `--features webui` 必带。

产物：`D:\meew11-target\release\phira-main.exe`（全量约 14 分钟）。

⚠️ **`--features webui` 必带**：`main.rs` / `home.rs` 无条件调用
`crate::webui::{open_settings, host_available, apply_pending_config}`，而这些只在
`target_os=windows && feature=webui` 的真模块里（否则是空桩，编译失败）。

### 发布（覆盖前先确认 phira-main 没在跑）
```
if tasklist | grep -qi phira-main; then echo 跳过; else cp D:\meew11-target\release\phira-main.exe D:\Phigros\PhiRa-Mod-Meew-11\rev\phira-pc\phira-main.exe; fi
```
- 目标发布目录：`D:\Phigros\PhiRa-Mod-Meew-11\rev\phira-pc\phira-main.exe`。
- ⚠️ 发布时若 exe 正在运行，`cp` 会报 `Device or resource busy`，只会更新没在跑的那份，两份不一致。
  务必先停进程再 cp（用上面的 `if tasklist …` 守卫，别写成「grep 后照样 cp」）。
- 报告：新 exe 大小 + SHA256。
- 不 commit / push；不动用户存档 `rev\phira-pc\data\`。

## 6. 已知 / 已修问题
- ✅ **已修编译坑**：`webui.rs` 里原写的 `crate::ui::UI_SFX_VOLUME` 实际在 `prpr::ui`，
  已改成 `prpr::ui::UI_SFX_VOLUME`（两处：apply_config 与 reset_config）。
- 编译后仍需确认：`merge_json` / `from_value::<Config>` / `set_autoplay` 逻辑无误；
  `serde_json` 已在 phira 依赖里（既有 webui 桥就用 `json!`，应已可用）。
- `data-config` 键我已逐项对照 `config.rs` 确认字段存在；`timingBar.enabled` 对应
  `TimingBarConfig.enabled`（config.rs:402）也已确认。

## 7. 运行时验证清单
- [ ] 启动改版，开始页点设置 → 进入 `Setting.html`（WebView 显示，不是原生页）。
- [ ] HUD 设置按钮、首页面板设置、首页活动设置，三处都能打开 WebView 设置页。
- [ ] 控件初值 = 真实 `data.json` 里的 config（改之前先开一次确认初值对）。
- [ ] 拖滑块（用触摸 / 鼠标都试）→ 拖动时实时 `set_config`，松手后存档更新；读回 `data.json` 确认值变了。
- [ ] 明暗主题开关 → 立即生效（页面底变浅 / 深）。
- [ ] 音效音量滑块 → 立即生效。
- [ ] 全屏开关 → 立即生效。
- [ ] 自动游玩开关 → 写进 mods，重开不打回。
- [ ] 日志页 → 显示真实 `phira-crash.log` 内容；刷新按钮可用。
- [ ] 重置按钮 → `confirm` 后回到默认，存档变默认，页面刷新。
- [ ] 返回 / Esc：从原生入口进的（`?from=native`）→ 隐藏 WebView 回游戏；
      从开始页进的 → 回 `tripsUide.html`。
- [ ] 模拟「无 WebView2」环境（或临时把 `host_available` 恒返回 false）→ 走 Rust `SettingsPage` 回退，不崩。
- [ ] 告警：本仓库只允许 5 条既有告警；新增告警必须修掉。

## 8. 接手提示词（可直接复制给下一个 agent / 新会话）
```
你是 PhiRa-Mod-Meew-11（Phigros PC 改版，Rust+macroquad+prpr）的接手开发者。
项目主工程路径：D:\Phigros\PhiRa-Mod-Meew-11（工作目录 D:\Phigros；E 盘那份是同样内容拷贝，以后一律写 D 盘）。
构建约定：target 必须在 D 盘 CARGO_TARGET_DIR='D:\meew11-target'；必须 CARGO_INCREMENTAL=0；
工具链 stable 1.99.0，CARGO_HOME/RUSTUP_HOME 已设用户级。
代码改动已完成但还没编译：设置界面已整体重写为 WebView 加载 D:\Phigros\kaichang\Setting.html，
8 个标签页的开关/滑块/输入框都接上了真实 Config（读写 + 即时副作用），详见
D:\Phigros\PhiRa-Mod-Meew-11\settings_webview_handoff.md。
你的任务：
1) 用 `cargo build --release -p phira-main --no-default-features --features webui`
   编译（注意必须带 --features webui，否则 main.rs/home.rs 调用 webui 模块会编译失败；
   也别加 `+stable`，本工程靠 rust-toolchain.toml 钉的 nightly 编）；
   修复任何编译错误（已知已修一个坑：UI_SFX_VOLUME 在 prpr::ui 不在 crate::ui）。
2) 确认无新增编译告警（本仓库只允许 5 条既有告警）。
3) 若 phira-main 在运行先停掉，再把
   D:\meew11-target\release\phira-main.exe 发布到 D:\Phigros\PhiRa-Mod-Meew-11\rev\phira-pc\phira-main.exe
   （用 if tasklist|grep -qi phira-main 守卫，别在运行时 cp）。
4) 报告 exe 大小 + SHA256。
5) 按 handoff 文档第 7 节的清单做运行时验证（WebView 打开、控件读真实配置、滑块触摸拖动、
   返回/Esc 导航、日志页读真实 phira-crash.log、重置、WebView2 缺失回退到 Rust SettingsPage）。
不要 commit/push，不要动 rev\phira-pc\data\ 用户存档。
```
