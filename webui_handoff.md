# WebView 界面层（kaichang）— 交接文档

> 项目：PhiRa-Mod-Meew-11（Phigros PC 改版，Rust + macroquad + prpr）
> 更新日期：2026-10-25
> 范围：`phira/src/webui.rs` 宿主 + `D:\Phigros\kaichang\` 下的全部 Web 页面 + 页面间导航 + IPC 桥
> 本文**取代** `settings_webview_handoff.md`（那份只讲设置页 WebView 化，已过时；设置页现状见 §6.3）

---

## 0. TL;DR

- Web 层只承担 **开始页 + 收藏夹 + 工具面板 + 设置页 + 资源包页 + 曲库页**；游戏内 / HUD / 首页仍是 Rust 原生。
- **本次会话（10-14）主要产出**：新建曲库页 `library.html`（按用户设计稿照搬 + 接真数据），
  并把原生「曲库」入口从原生 `LibraryPage` 切到该 Web 页；新增 3 个桥命令。
- **10-25 会话新增**：phirc 内置 BP 连接入口——`BP_NAV_JS` 注入（所有 WebView 页左下浮动「BP」钮）→
  `bpHost.html`（iframe 全屏嵌 bp-host 赛房前端，地址簿 + ✕/Esc 返回）；主工程三页与 `tools.html` 一字未动（见 §1.1 / §2 / §9）。
- **设置页 `Setting.html` 已被用户要求回退到 10-14 会话前状态，不要再动**（见 §6.3）。
- HTML 改动**不需要重编 exe**（Web 根是开发目录 `D:\Phigros\kaichang`，`serve_asset` 带 no-cache）；
  只有 `.rs` 改动才需要重编 + 发布。

---

## 1. 架构

### 1.1 WebView 宿主
- 宿主代码：`phira/src/webui.rs`（wry 0.44 / WebView2，作为游戏窗口的 `WS_CHILD`）。
- 自定义协议 `phirc://web/<path>` → 把 Web 根目录喂给 WebView。
- **Web 根目录优先级**（`web_root()`，第 794 行起）：
  1. 环境变量 `PHIRC_WEBUI_ROOT`
  2. `D:\Phigros\kaichang`（**开发目录，优先 —— 改 HTML 重启即生效**）
  3. `<exe 同目录>/kaichang`（随包副本）
- 入口页：`PHIRC_WEBUI_ENTRY` 覆盖，否则 `index.html`（`entry_url()`）。
- 等位缩放：`PHIRC_WEBUI_FIT=fill` 可关闭；默认按设计分辨率等比缩放。
- 注入脚本（`with_initialization_script`，顺序即下文）：
  `BRIDGE_JS` → `ERROR_JS` → `ALERT_JS` → `DEBUG_JS`(仅 `PHIRC_WEBUI_DEBUG=1`) → `HOTSPOT_JS` → `SETTINGS_NAV_JS` → `RESPACK_NAV_JS` → `BP_NAV_JS`
- `BP_NAV_JS`（10-25）：左下 52px「BP」浮动圆钮 → `location.href="bpHost.html"`；守卫三件：
  `__phircBpNavInstalled` 幂等 / `location.protocol==='phirc:'` 才执行（防 iframe 二次执行）/ 排除 `bpHost.html` 自身；
  挂点在 `init_scripts_base` 的**两份** cfg 分支里都要 format 进去（`webui.rs`）。
- ⚠️ **F12 已开 DevTools**（`with_devtools(true)`），正式发布要关就把那行删掉。

### 1.2 ⚠️ 全屏白屏根因（2026-10-13 定位，花了很多时间，务必记住）
miniquad 全屏窗口是 `WS_POPUP`，WebView2 的 DComp 表面（`Intermediate D3D Window`）在 live 视图失活
→ 全屏一片白、Alt+Tab 缩略图里还能看到界面。
**解法**：`set_fullscreen(true)` 改用**无边框顶层窗口**（`WS_CLIPSIBLINGS|WS_CLIPCHILDREN|WS_VISIBLE`，**不要** `WS_POPUP`）；
`ensure_clip_children` 顺手剥 `WS_POPUP`；切全屏后发 `WebCmd::Refresh`（重设 bounds+可见+焦点）；
keepalive 每轮再 `ensure_clip_children` 兜底。窗口模式恢复 `WS_CAPTION|WS_SYSMENU|...`。

### 1.3 导航根因（同类大坑）
`WebCmd::Navigate` 曾是 `webview.load_url("phirc://web/...")` —— **对自定义协议 `phirc://` 在 `new_as_child` 子 WebView 下静默无效**
（不触发 serve_asset、不报错），表现为「返回键点了没反应」。
**已根治**：改成 `webview.evaluate_script("window.location.href = <剥掉 phirc://web/ 前缀的相对路径>")`，
与页面内跳转走同一条客户端导航路。

---

## 2. 页面清单与导航图

| 页面 | 文件 | 职责 | 出口 |
|---|---|---|---|
| 开始页 | `index.html` | 版权页 / WebGL 背景 | 全屏触点（`HOTSPOT_JS`，单击 260ms 消歧）→ `tripsUide.html` |
| 收藏夹 | `tripsUide.html` | 收藏卡片（localStorage `phigros_fav_cards`） | 左上角→`tools.html`；右下角→`Setting.html`；另有曲库入口 |
| 工具面板 | `tools.html` | 4 个工具入口（谱面工具箱/资源包/通知/活动） | 见 §4；关闭/Esc→`tripsUide.html` |
| 设置页 | `Setting.html` | 8 标签页设置（**已回退，勿动**） | 账号/历史/黑名单等 `data-action` |
| 资源包 | `ResPack.html` | 资源包列表/切换/导入/导出/删除 | 左上角返回 |
| 曲库 | `library.html` | 曲目网格 + 分类 + 搜索 + 分页 + 插件入口 | 返回→`history.back()`；收藏夹→`openPage('favorites')`；导入→`openTool('toolbox')` |
| 剧情编辑器 | `PhigrOS.html` | PhiC 剧情编排 | 独立工具页 |
| 资源页 | `zeyuanbao.html` | 静态资源展示 | — |
| BP 连接入口 | `bpHost.html`（10-25 新增） | 连 bp-host 赛房：地址簿（`phirc_bp_addrs` 留 5 展 3）+ `no-cors` 探测（失败不拦截跳转）+ iframe 全屏嵌 `http://<addr>/index.html`（与 bp-host 同源 → 房码自动回填、`/ws` 原生可用）；`normalize()` 只收 `host:port` | 左上 ✕ 常驻 → `history.back()` 兜底 `index.html`；Esc 分流：嵌入态退出嵌入、面板态放行宿主 |

```
index.html ──单击──> tripsUide.html ──左上角──> tools.html ──┐
                        │                                │
                        ├──右下角──> Setting.html         │
                        │                                │
                        └──曲库入口──> library.html <─────┘ (关闭/返回)
                                              │
                                              ├── RKS/课题/最佳榜 ──> 原生浮层（open_overlay）
                                              └── 返回 ──> history.back() → 上一页（无历史才回开始页）
```

### 2.1 原生入口怎么进 WebView 页
`phira/src/scene/main.rs::goto_hud_page`：
- `PageId::Home` → WebView 可用时 `show_start_page()`（**不再露出原生 Home 面板**）
- `PageId::Settings` → `open_settings()`（`Setting.html`）；不可用才回退原生 `SettingsPage`
- `PageId::Library` → **`open_library()`（`library.html`）**；不可用才回退原生 `LibraryPage` 〔本次改动〕
- 其余（Favorites/Message/History/Respack/Blacklist）仍是原生页，由 WebView 用 `openPage(name)` 触发

---

## 3. IPC 桥（完整清单）

页面 → 宿主：`window.phirc.postMessage(JSON.stringify({ id, cmd, args }))`
宿主 → 页面：`window.__phircResolve(id, payload)`，payload 形如 `{ ok, data | error }`

### 3.1 配置 / 语言 / 日志
| `window.phirc.xxx` | cmd | 说明 |
|---|---|---|
| `getConfig()` | `get_config` | 返回 `Config` JSON；Data 级键在 `__data` 子对象里 |
| `setConfig(obj)` | `set_config` | 增量合并。⚠️ 走 `serde_json::from_value::<Config>`，**未知字段被静默丢弃** → 新键必须先在 `prpr/src/config.rs` 加同名字段 |
| `resetConfig()` | `reset_config` | 全部恢复默认（不影响账号/谱面/存档） |
| `setData(key, value)` | `set_data` | Data 级设置（`language`/`enable_anys`/`anys_gateway`/`accept_invalid_cert`） |
| `listLanguages()` | `list_languages` | `{ current, list:[{name, code}] }`（字段是 `code`，不是 `id`） |
| `setLanguage(code)` | `set_language` | **只切游戏原生 UI 语言**，不翻译网页自身 |
| `log(msg)` | `log` | 写进 `phira-crash.log` |
| `getStats()` | `get_stats` | `{all, clear, fc, phi}`（本地谱面库口径） |

### 3.2 曲库（本次重点）
| 方法 | cmd | 说明 |
|---|---|---|
| `listCharts()` | `list_charts` | 本地曲库：`[{name,composer,charter,illustrator,level,difficulty,path,cover}]`，cover = `phirc://web/local-cover?p=<base64url(local_path)>` |
| `listOnlineCharts(type, page, search, order)` | `list_online_charts` | **新增**。`type` ∈ `ranked`(上架)/`special`(特殊)/`unstable`(未上架)/`popular`(热门)。返回 `{charts:[{id,name,level,difficulty,charter,composer,illustrator,intro,rating,ranked,reviewed,stable,cover,local:false}],count,total_page}`。未登录/离线/未接受条款 → `{ok:false,error:'must-login'\|'offline-mode'\|'must-accept-tos'}` |
| `getCustomRks()` | `get_custom_rks` | **新增**。`{rks,best_used,ap_used}`，口径 = `custom_rks::calculate`（本地成绩算的 RKS） |
| `startGame(path)` | `start_game` | 只对**本地**谱面有效（path 用 `listCharts` 返回的） |
| `startOnlineChart(id)` | `start_online_chart` | **新增**。按 id 从服务器取谱面信息 → 暂存 `PENDING_ONLINE_CHART` → 让位给原生 `SongScene`；**按 play 时才真正下载**（复用 `global_start_download`：下载→解压→登记进 `data.charts`→开局）。未登录/离线 → 同 §3.2 的错误码 |
| `openOverlay(name)` | `open_overlay` | **新增**。`name` ∈ `challenge`/`rks`/`board` → 展开原生曲库页内对应浮层 |
| `importPlugin()` | `import_plugin` | **插件**。弹系统文件框导入 `.phc`（ZIP 含 `manifest.json`，或单个 JSON）→ `{id,name}`。桌面端同步 rfd |
| `listPlugins()` | `list_plugins` | 已装插件 → `[{id,name,version,author,description,entry,hasScript,enabled}]` |
| `readPluginScript(id)` | `read_plugin_script` | 插件脚本正文（字符串） |
| `setPluginEnabled(id,on)` | `set_plugin_enabled` | 写 `plugins/state.json` |
| `deletePlugin(id)` | `delete_plugin` | 删除 `plugins/<id>/` 并清状态 |
| `listReplays()` | `list_replays` | **回放**。→ `[{file,name,level,difficulty,player,imported,chart:{id\|local},duration,durationText,playable,localPath,speed,offset,result:{score,accuracy,maxCombo,numOfNotes,counts},recordedAt}]` |
| `importReplay()` | `import_replay` | 弹系统文件框导入 `.ppr`（兼容 `.phirar`/`.phirarec` 扩展名） |
| `exportReplay(file)` | `export_replay` | 弹保存框导出 |
| `deleteReplay(file)` | `delete_replay` | 删除文件 + 索引 |
| `associateReplay(file, localPath)` | `associate_replay` | 给导入的回放关联本地谱面 |
| `watchReplay(file)` | `watch_replay` | 播放：读磁带 → `scene::replay::queue` → 隐藏 WebView → 进谱面信息页，按 play 开始 |

### 3.3 原生界面 / 工具 / 账号 / 文件
| 方法 | cmd | 说明 |
|---|---|---|
| `openPage(name)` | `open_page` | 白名单：`settings/favorites/library/message/history/respack/blacklist/home` → 让位给原生页 |
| `openTool(name)` | `open_tool` | `toolbox`/`event`/`timing` → 原生工具覆盖页 |
| `openUrl(url)` | `open_url` | 系统浏览器打开 |
| `hide()` | `hide` | 「我完事了」→ `show_start_page()` 回开始页。⚠️ **页面返回键不要用它**（用户视角=跳回开始界面）；返回上一页一律 `history.back()` |
| `pickFile(id)` / `resetFile(id)` | `pick_file` / `reset_file` | `id` = `ui_font`(→`{root}/font.ttf`) / `home_char`(→`{root}/char.img`)。桌面同步 rfd 框 |
| `listAccounts()` / `addAccount(e,p)` / `switchAccount(e)` / `forgetAccount(e)` / `logout()` | 对应 `*_account` / `account_logout` | 账号走网络，**单独线程 + current-thread tokio runtime `block_on`**，别堵 WebView 线程 |
| `listRespacks()` / `setRespack(i)` / `importRespack()` / `exportRespack(i)` / `deleteRespack(i)` / `respackInfo(i)` | 对应 | 资源包页（`ResPack.html`）用；内置包(index 0) 删除会被拒 |

---

## 4. 工具面板 `tools.html`（用户设计稿，已接通）

用户设计稿：磨砂玻璃**平行四边形**（`skewX(-6deg)` 面板 / `-8deg` 条目 / `-10deg` 关闭钮）+ 错峰入场动画
+ 选中态白底黑字光晕脉冲。**样式与结构照搬，一行不改。**

四个工具的落点（`runTool()`，桥不存在则降级提示）：

| 设计稿条目 | data-tool | 落到哪 | 是否真功能 |
|---|---|---|---|
| 谱面工具箱 | `chart` | `openTool('toolbox')` → 原生 `WebTool::Toolbox` 覆盖页 | ✅ 真（原生导入/转换/PEZ） |
| 资源包管理 | `respack` | 页内跳 `ResPack.html` | ✅ 真（list/set/import/export/delete/info 全接） |
| 通知 | `notice` | `openPage('message')` → 原生 `PageId::Message` | ✅ 真 |
| 活动 | `event` | `openTool('event')` → 原生 `WebTool::Event` 覆盖页 | ✅ 真 |

- 关闭 / Esc → `location.href = 'tripsUide.html'`（回收藏夹，不惊动原生界面）。
- 入口：`tripsUide.html` 左上角。

---

## 5. 曲库页 `library.html`（本次新建）

### 5.1 设计稿语义 → 原生概念的对应（**重要，别自己发明分类**）
翻 `phira/src/page/library.rs` 确认：设计稿就是原生曲库页的概念。
- 侧栏 5 项 = `enum ChartListType { Local, Ranked, Special, Unstable, Popular }`（源码注释原文：「底部翻页条（上架 / 特殊 / 未上架 / 热门）」）
- 顶部 3 钮 = 原生曲库右侧三个文字入口：`查看 B0`→**最佳谱面榜**(`best_board.rs`, T7.2)、`RKS`→**自定义 RKS 编辑器**(`rks.rs`, T7.4)、`课题模式`→**课题面板**(`challenge.rs`, T7.3)
- ⚠️ 这三个是**原生曲库页内的浮层**，不是独立 `PageId`，只能原生绘制 → 用 `open_overlay` 桥让位给原生展开
- 难度家族 `EZ < IN < HD < AT < SP`（`newui.rs`：EZ绿/HD蓝/IN红/AT黑/SP彩虹）→ 映射到设计稿 4 款卡：`EZ/IN→v1`、`HD→v2`、`AT→v3`、`SP→v4`

### 5.2 照搬设计的处理方式
- 用户设计稿的 `<style>` 块 + Tailwind 类名 + 4 张示例卡 markup **原样搬**；4 张卡移进 `#card-templates`，JS 克隆后只填 `tpl-*` 占位。
- **CDN 本地化**：`vendor/tailwind.js`（Tailwind Play CDN，451,131 B）+ `vendor/lucide.js`（UMD，454,082 B），
  `<script src>` 指向本地 → 断网可用，观感零差别。`serve_asset` 原生支持子目录 + `js` MIME，无需改 Rust。
- 额外只加：`#lib-status` 状态行、`#pager` 在线分页条（每页 28，与原生 `PAGE_NUM` 一致）。

### 5.3 在线列表查询口径（`list_online_charts`，逐字复刻原生 `load_online`）
- `ranked/special/unstable` → `type=0/1/2`；`popular` → `/popular` 后缀（不带 search/order/rating）
- 每页 `PAGE_NUM=28`；division 固定 `regular`（`tags.rs` 的 `DIVISION_TAGS[0]` 没公开，只能写字面量）
- rating 区间 `0,16.5`；排序默认 `updated`
- 前置校验：`offline_mode` / `me.is_none()` / `check_read_tos_and_policy`
- 曲绘 URL 走 `cover_url()`：开 Anys 时 `<API>/files/x` → `<API>/anys/x`（同 `File::request` 的转发规则）

### 5.4 RKS 口径（`get_custom_rks`）
完全复用 `page/rks.rs::refresh` + `custom_rks::calculate`：
每张谱取 `charts[].record` 与 `local_records` **更优的那一份**精度，难度用 `info.difficulty`，不接 AI 定数；
设置读 `{dir::root()}/custom-rks.json`，读失败回落 `Settings::default()`。

### 5.5 已知限制
- 在线分类**必须登录**；未登录/离线/未接受条款分别提示（没偷偷放行）
- 在线谱面点卡 → `startOnlineChart(id)` → 原生 `SongScene`（先看谱面信息，**按 play 才下载**，可取消）；下载完自动登记进本地曲库
- ⚠️ 下载复用 `scene/song.rs::Downloading::global_start_download`，**没有自己实现下载**；那条路要 `Ptr::<Chart>::new(id).fetch()`（返回 `Arc<Chart>`）

### 5.6 性能红线（2026-10-15 修「曲库卡死」时踩出来的）
1. **⚠️ 绝不能在 WebView 线程做重活/碰 UI** —— wry 的 IPC 回调（`handle_ipc`）跑在**持有消息泵的那个线程**上，一旦阻塞：keepalive 停止、页面不重绘、整个 App 像卡死。
   * 实测根因：`list_online_charts` 在 IPC 线程调了 `scene::check_read_tos_and_policy`，它在用户**未接受条款**时会 `Dialog::show()`（非主线程操作 prpr UI）+ `get_data_mut()/save_data()`（写盘）→ 消息泵死锁。
   * 现在：桥里只做**线程安全的只读**判断（`tos_accepted()` = `terms_modified.is_some() || TOS_VERIFIED`），需要弹窗就置 `PENDING_TOS` 交给游戏线程（`scene/main.rs::next_scene` 里 `take_pending_tos()` → 主线程跑检查）。
   * **WebView 盖在游戏窗口上，原生对话框会被挡住** → 走条款流程前先让位；接受后由 `TOS_AWAIT_ACCEPT` 把用户送回曲库（`open_library()`）。
   * 同理 `list_charts` / `get_custom_rks` 也**已改成 spawn 线程 + `reply_async`**（曲库可能很大 / 要读文件）。
1b. **⚠️ 隐藏 WebView 必须用 `webui::handoff_to_native()`，不要用 `webui::hide()`** —— keepalive 每 ~1s 会按 `WEBVIEW_VISIBLE` **幂等重断言 `set_visible(true)`**；`hide()` 只发命令、不改该标志，一秒后 WebView 又被显示回来、盖住原生弹窗（实测「一直跳条约、点不到接受」）。`handoff_to_native()` 同时改 `WEBVIEW_HANDOFF` + `WEBVIEW_VISIBLE` 再发命令。
1c. **诊断要点**：`phira-crash.log` 会记每个 `phirc://` 请求、页面 JS 错误、`keepalive`（**由 WebView 线程打印，它停了 = 消息泵被卡**）。在线查询失败必须落日志（用 `{e:#}` 打完整错误链），否则页面只显示第一层、无从诊断。
2. **动态拼 CSS 选择器必须闭合括号** —— `'[data-variant="' + v + '"'` 少了 `]` 会让 `querySelector` 抛 `SyntaxError`，`renderCards` 在**第一张卡就中断**，整个网格空白。
3. **必须分页**：本地列表也按 `PAGE_NUM=28` 切。一次性注入上百张卡 → 每张卡的曲绘都请求 `phirc://web/local-cover` → Rust 侧**解包 + 生成缩略图** → 协议线程排队 → 界面像卡死。全量缓存在 `ALL_LOCAL`，切页/搜索**纯客户端**。
4. **`lucide.createIcons()` 只在启动调一次**：每轮渲染都全量重扫 DOM（叠加 Tailwind Play CDN 的 MutationObserver 重建）会明显卡顿。
5. **桥请求要有超时兜底**：请求永不返回时 `state.loading` 会卡在 `true`，之后点分类/翻页全被静默忽略 → 「假死」。用 `withTimeout()`（本地 8s / 在线 20s）。
6. 每张卡 `try/catch`（单张出错不拖垮整轮）；封面 `loading="lazy"` + `decoding="async"`。
7. 未验证的次嫌疑：**Tailwind Play CDN 的运行时 JIT** 在动态内容多时较重（它在浏览器里重新编译 CSS）。若将来还卡，改用 Tailwind CLI **预编译成静态 `vendor/tailwind.css`**，去掉运行时 JS。
8. **`listLanguages()` 返回 `{ current, list:[{name, code}] }`** —— 不是 `{languages:[{id}]}`；字段是 `code` 不是 `id`（Setting.html 的语言下拉曾因此空白）。
9. **⚠️ 桥的 promise 是「解包」的**：`window.__phircResolve(id, payload)` → `ok:false` 时 **reject(new Error(payload.error))**，否则 **resolve(payload.data)**。
   * 页面里**必须写 `bridge.xxx().then(data => …)`**；写 `res.data` 会拿到 `undefined`（曲库「本地」列表曾因此永远为空，日志表现为「加载 library.html 后一张曲绘都没请求」）。
   * 失败**不会**走 `res.ok === false` 分支（那是死代码），错误码要在 **`.catch(e => e.message)`** 里翻译。
10. **TOS/条款流程是「拉取 → 弹窗 → 接受」三段式**：`load_tos_and_policy` 有防重复逻辑，条款**没拉到**时 `check_read_tos_and_policy` 只启动后台拉取、返回 false、**不弹窗**。所以调用方必须**反复重试**（`scene/main.rs` 用 `TOS_PENDING`/`TOS_DIALOG_SHOWN`/`TOS_RETRY` 状态机，每 30 帧推一次），且**只在条款已拉到时才隐藏 WebView 弹窗**（`scene::tos_terms_loaded()`），否则用户对着黑屏干等、条款永远接受不了（症状：一直 `must-accept-tos`）。
10e. **浮层用完必须把用户送回 WebView 曲库页**：`LibraryPage.overlay_return` 在 `enter()` 消费
    `PENDING_OVERLAY` 时置位；`next_scene()` 里当三个浮层全关（`!challenge.active && !rks.editing && !best_board.open`）
    就 `webui::mark_handoff()` + `NextScene::Pop`。否则用户关掉 B0/课题后会停在原生曲库这个「老界面」上。

10d. **⚠️ `goto_hud_page(PageId::Library)` 在 WebView 可用时会走 `open_library()`，不创建原生页** ——
    所以**原生独有的浮层**（B0 / RKS 编辑器 / 课题面板）必须先看 `PENDING_OVERLAY`：有请求就强制走原生 `LibraryPage`
    （否则 `enter()` 不跑，请求烂在 `PENDING_OVERLAY` 里，用户看到「点了没反应」）。2026-10-18 实测踩过。

10c. **⚠️ 改 `BRIDGE_JS` 后必须跑 `python scripts/check_injected_js.py`** ——
    注入脚本是 7 个 `*_JS` 常量**拼成一整段**注入的，**任何一处语法错误都会让整段解析失败**：
    桥不装、开始页触点不铺、`页面就绪` 不打日志，**6 秒后看门狗判定「WebView 不可用」→ 隐藏页面 →
    用户看到的就是「点不动」**（2026-10-18 实测：`associateReplay` 漏了个逗号，整个界面失联）。
    这个检查器会按 `init_js` 的顺序拼接并 `node --check`，几秒钟就能拦住这类事故。

10b. **回放注入必须走 `Judge` 的 thread-local，不要另开输入通道**：`on_new_frame()` 里 `take_playback_touches()` 有值就整帧替换 `TOUCHES`（含 `keys_down`/`key_delta` 归零），否则真实输入会和合成触摸叠加。录制同理：`raw_touches()` 取的是**未变换**坐标，注入也按未变换坐标送回，别混用 `get_touches()`（那个已经过 `touch_transform`）。

11. **页面返回键一律 `history.back()`（2026-10-18 统一）**：`if (history.length > 1) history.back(); else location.href='index.html';`
    * 所有页面间跳转（页面内 `location.href` / Rust `Navigate` 的 `evaluate_script`）都会**压入 WebView 历史栈**，所以回退一定是对的。
    * ⚠️ **绝不要用 `phirc.hide()` 做返回** —— 它是「让位回原生界面」，用户看到的就是「返回键跳回了开始界面」（用户实测报的 bug，`library`/`Setting`/`tripsUide` 三页全中）。
    * ⚠️ **Setting.html 的 `bindBack()` 与注入的 `SETTINGS_NAV_JS` 都绑 `.back-btn-wrapper`**：两边各跑一次 `history.back()` 会**连退两页**。用 `__phircBack` 标志去重（谁先绑定谁处理）。
    * `leaveToNative()` / 固定 `location.href` 只作**无历史时的兜底**；`phirc-library-demo.html` 是演示页不在此列。

---

## 5.8 回放系统（2026-10-18）

**三段式：录制（引擎内）→ 库管理（phira）→ 播放（合成触摸注入）。**

### 文件与职责
| 文件 | 职责 |
|---|---|
| `prpr/src/replay.rs` | 磁带格式（`PHIRAREC` + u32 版本 + zstd(JSON)，上限 128 MiB）、`Recorder`、`TouchPlayer`、触摸注入槽 |
| `prpr/src/judge.rs` | `raw_touches()`（录制取原始触摸）、`on_new_frame()` 优先消费注入触摸 |
| `prpr/src/scene/game.rs` | `judge.update` **之前** 调 `replay::record_frame(res.time, &judge)` |
| `phira/src/replay.rs` + `replay/library.rs` | `<data>/replays/` 索引（`index.json`）、列表/导入/导出/删除/关联 |
| `phira/src/scene/replay.rs` | 双槽（`PENDING_SCENE` → 建信息页 / `PENDING_LAUNCH` → 开局）+ `build_driver()` |
| `phira/src/scene/song.rs` | 开局取磁带（`take_pending`）→ 强制 `GameMode::Replay` + 挂驱动；`on_result` 收尾落盘 / 跳过历史 |
| `kaichang/Replays.html` | **曲库同款风格**的管理页（Tailwind + zinc），入口在曲库页头部「回放」 |

### 关键设计
- **录制不碰判定**：每帧抄 `Judge` 的**原始触摸**（未经谱面坐标变换）+ 新增 `feedback`（判定/误差/时刻）。
- **播放不碰输入**：`set_playback_touches()` 塞进 `Judge` 的 thread-local，`on_new_frame` 见到注入就跳过 macroquad 采集 → flick 追踪/判定/计分原样复用。
- **一帧延迟**：注入发生在第 N 帧 `update_fn`（在 `judge.update` 之后），第 N+1 帧 `on_new_frame` 消费；驱动按「下一帧时间窗」取触摸。
- **暂停安全**：谱面时间不前进 → 注入空 vec，恢复后不会重复 `Started`。
- **`GameMode::Replay`**：判定照常、结算照出，但 **不写历史 / 不计每日 / 不更新最佳 / 不上传**（`rated` 加了 `!replay_mode`）。
- **`SongScene::Drop`** 兜底：暂停菜单中途退出不会走 `on_result`，Drop 里清回放态 + 注入触摸，别漏给下一局。
- 文件名即身份：自录制 `<谱面key>_<毫秒>.ppr`，导入 `import-<n>.ppr`；导入的**永不进历史/PB/RKS**。

## 5.7 `.phc` 插件系统（2026-10-16）

- 完整规范见 **`phc_plugin_format.md`**（格式 / manifest / api / 事件 / 边界）。
- 一句话：`.phc` = **ZIP（含 `manifest.json` + `entry` 脚本）** 或 **单个 JSON（脚本在 `script` 字段）**；
  落盘 `<data>/plugins/<id>/`，启用状态同目录 `state.json`，新导入默认启用。
- 5 个桥命令见 §3.3；**全部 spawn 线程**（文件框同步 + 读写磁盘，别占消息泵）。
- `valid_plugin_id()` **兼作目录穿越防护**（id 直接当目录名用）。
- 前端：曲库**左上角** 🧩 按钮 → 插件管理面板（导入/列表/启停/删除/报错）；
  运行时 `PhiraPlugin.register({ id, name, onLoad(api) })`，`api` = `toast/log/on/addToolbarButton/cards/reload/bridge/storage`，
  钩子 `ready` / `charts(list,info)` / `cardRender(el,chart)`；脚本用 `new Function` 隔离作用域、异常被捕获。
- ⚠️ 插件行用 **`#plugin-row-template` 隐藏模板**克隆 —— 既当克隆源，又让 Tailwind 加载时就把类名生成好。
- ⚠️ **启动顺序**：先 `loadPlugins()` 再 `load()`（否则首屏卡片错过 `cardRender`），另加 1.5s 兜底定时器。
- **三种运行时**：`js`（默认，页面内执行）/ `python` / `rust`。后两者走**统一「旁挂进程」模型**（子进程 + JSON-lines，不用 FFI/DLL → 无 ABI 依赖、崩溃隔离）。
  - 桥：`startPlugin` / `stopPlugin` / `pluginEvent` / `pluginStorageSet`；`list_plugins` 返回 `runtime`/`hooks`/`running`。
  - 协议：宿主推 `init`/`ready`/`charts`/`cardRender`/`click`/`shutdown`；插件回 `log`/`toast`/`addToolbarButton`/`badge`/`storageSet`/`startGame`/`openPage`/`openUrl`/`reload`。
  - Rust 侧 stdout 逐行转发 `window.__phcPluginCommand(id, line)`；stderr 进诊断日志；`CREATE_NO_WINDOW` 启动；解释器按 `PHC_PYTHON`→`python`→`python3`→`py -3` 探测。
  - ⚠️ 写插件 stdin 可能阻塞 → 单独线程；`pagehide` 时 `stopAllSidecars()` 防孤儿进程。
- 示例：`random-pick.phc`(JS/JSON)、`hard-mark.phc`(JS/ZIP)、`py-stats.phc`(**Python**)、`rs-stats.phc`(**Rust**，内含 exe)。

---

## 5.9 谱面管理面板 `ChartPanel.html`（2026-10-18）

用户提供的设计稿（4 个 tab），从**原生谱面详情页的 Mod 按钮**进入。

| tab | 接的东西 |
|---|---|
| Mod 配置 | 12 个 Mod（位标志与 `prpr::config::Mods` 完全一致）+ 判定偏移 ±1ms；互斥组（上隐/下隐、AP/FC）在页面内处理 |
| 全局设置 | 血条参数：`healthMax` / `healthScale` / `healthBarLen` / `healthBarColor`（走 `setConfig`） |
| 编辑谱面 | `info.yml` 全字段（名称/谱师/作曲/画师/显示等级/定数/背景暗度/预览区间/偏移/宽高比/强制宽高比/三态 RPE1.7.0 与 AttachUI/解锁动画/四个文件/Tip/简介） |
| 谱面信息 | 只读：名称/曲师/谱师/难度/简介/评分/种类/标签/ID + 「在网站中打开」 |

**桥（5 个，全部 spawn 线程）**：`chart_panel` / `chart_panel_mods` / `chart_panel_save` / `chart_panel_pick` / `open_chart_web`。
**入口**：`webui::open_chart_panel(local_path)`（URL 带 `?path=`，`urlencode` 转义）→ 原生 `mod_btn` 点击时调用；没有 WebView2 时回退原生 Mod 侧栏。

⚠️ **两个坑**：
1. **面板改 Mods 后原生详情页不会自动刷新**（`self.mods` 在内存里）→ 加了 `PENDING_MODS` 静态量，`SongScene::update` 每帧 `take_pending_mods()` 同步。
2. **`BRIDGE_JS` 的方法必须插在 JS 字符串里** —— 这次我用 4 空格缩进的注释做锚点，结果命中了 `match cmd` 里 8 空格缩进的同名注释，**JS 被插进了 Rust 代码**（编译直接报错）。插完必须跑 `scripts/check_injected_js.py` 并确认 `BRIDGE_JS` 字符数变大。

---

## 5.10 移动端 WebView（安卓）—— 已实现（Rust 侧，2026-10-18）

**选型**：弃用 wry-android（要动 AppCompat + 主题迁移 + 9 个 JNI 生命周期，见旧调研的结论，
风险集中在「改完 APK 起不来」），改用**自写轻量 Java WebView 宿主**：零新依赖，
现有 `PhiraTheme` / `SurfaceView` 完全不受影响。

**架构（关键：业务层一条代码两个平台跑）**
- 安卓代码直接写在 `phira/src/webui.rs` 的 `#[cfg(target_os = "android")]` 段，与 Windows wry 段并列。
  `HOST` / `WebCmd` / `handle_ipc`（2000 行业务层）**零改动复用**——只是 `WebCmd` 的解释端
  从「wry 线程」换成「JNI 转发线程」（`forward_loop`）。
- `lib.rs` 模块门：`#[cfg(any(all(windows, feature = "webui"), target_os = "android"))]`。
  APK 构建是 `-p phira --no-default-features`（不带 webui feature → 不编 wry）✔。
- Java 侧 `android/.../modded/WebViewHost.java`：
  - `init()` 末尾 `nativeRegisterHost(this)`（Rust 存 `Global<JObject>`）；
  - `Bridge.post(json)`（`@JavascriptInterface`）→ `nativeIpc(json)` → Rust `handle_ipc`（丢线程，不堵 JNI 线程）；
  - `onPageStarted` / `onPageFinished` → `nativeReady()` → Rust 注入 `init_scripts()`
    （与 Windows `with_initialization_script` 同一份，`BRIDGE_JS` 幂等，三重注入无害）；
  - 供 Rust 调的实例方法：`eval(js)` / `setVisible(bool)` / `loadPage(url)` / `refresh()`；
  - 页面 = APK `assets/kaichang/`（`build_apk.ps1` 已打包），`file:///android_asset/` 加载；
    `respack/<idx>/<file>` 走 `shouldInterceptRequest` 直读 assets。
- `WebCmd::Navigate` 的 `phirc://web/x` 在转发线程翻成 `file:///android_asset/kaichang/x`（query 保留）。
- `export_replay` 的 `rfd`（桌面专属依赖）在安卓返回「不支持」，待接 MainActivity 的
  `EXPORT_FILE`（Storage Access Framework）。

**验证状态（2026-10-18）**
- ✅ 安卓编译：`cargo check --target aarch64-linux-android -p phira --no-default-features` 通过，
  告警 5 条 = 允许的既有项（本机可验安卓编译，命令见 §6）。
- ✅ Windows 回归：`cargo check -p phira-main --no-default-features --features webui` 通过，5 条既有告警。
- ✅ Java：`scripts/check_android_java.ps1` 通过（WebViewHost + MainActivity）。
- ⬜ **真机待验证**：WebView 显示 / 触摸 / 桥往返 / 页面注入时序（onPageStarted 双保险是否够）。
- ⬜ iOS 未动。

### iOS（2026-10-18 同轮实现，Rust 侧完成）

**选型**：复用当年被移除的旧实现（备份 `_meew11-src-backup/webui-removed-2026-10-09/phira-ios/webui.m`
+ `phira-src/webui_ios.rs`）的成熟骨架，但资源喂法升级：弃用旧的「内联单文件 HTML +
`loadHTMLString`」（多页/相对路径会崩，且当年因 JS 注入不生效禁了 `userInteractionEnabled`），
改用 **`WKURLSchemeHandler` 实现 `phirc://`** —— 与 Windows 协议层完全对齐：

- `WebCmd::Navigate` 的 `phirc://web/...` **原样直传**（`loadRequest:`），无需翻译；
- 页面相对路径原样解析成 `phirc://web/...`；`serve_asset` / `serve_respack` /
  `local-cover` 全部复用（这批函数 gate 放宽为 `#[cfg(any(windows, ios))]`；
  `web_root()` 的「exe 同目录/kaichang」分支在 iOS 上正好是 .app bundle 目录）；
- **不禁 `userInteractionEnabled`**——页面要完整交互（触点/输入框）。

**三层结构**（与安卓完全平行）：
- `phira/ios/webview.m`（新）：C 函数边界 `phira_webui_create/navigate/set_visible/eval/destroy`；
  `messageHandlers.phira` 消息 → Rust `phira_webui_ipc`；scheme handler → Rust `phira_webui_asset`
  （malloc 缓冲 + mime 枚举，ObjC 侧 `free`）。MRC、主线程 `dispatch_async`、无软键盘代码
  （那部分在 `ios/ime.m`，符号不能重复）。
- `webui.rs` iOS 段：`init_host`（cfg ios）+ `forward_loop_ios` + 两个 `#[no_mangle]` 回调。
- `build.rs`：`build_ios_webview()`（cc 编 webview.m + WebKit framework）。
- `lib.rs` 模块门 + ios。

**`window.ipc` 垫片（安卓/iOS 共用，重要）**：`BRIDGE_JS` 的 `call()` 依赖 wry 原生注入的
`window.ipc.postMessage`——移动端没有 wry！`IPC_SHIM_JS_TPL`（cfg android/ios）在桥脚本之前
把 `window.ipc` 接到 `phiraAndroid`（安卓）/`messageHandlers.phira`（iOS）。
**安卓侧缺这层就是真机必踩的 bug**（页面全线 `window.ipc is undefined`），本轮顺手补上。

**验证**：
- ✅ iOS 段 Rust 类型检查：临时包进 `mod` 在 Windows 上 `cargo check` 全绿后还原
  （iOS 段只依赖 std + extern 符号声明，不依赖 iOS API，此法可靠）；
- ✅ Windows 回归：5 条基线告警（无新增）；
- ⚠️ `webview.m` 无法在 Windows 编译验证（无 Xcode），需 macOS 首次构建确认；
- ⚠️ 本机 iOS 完整 `cargo check` 不可行：C 依赖（aws-lc-sys 等）需要 `xcrun`/苹果 SDK；
  iOS std 组件已装进 nightly-2026-01-01（TUNA 镜像 403，走 USTC）；
- **⚠️ `IPHONEOS_DEPLOYMENT_TARGET` 12.0 → 13.0（本次已改 4 处）**：`WKURLSchemeHandler`
  实现自定义 scheme 是 iOS 13+ 的硬约束（12 设备上 `phirc://` 完全不生效）——若不改，
  苹果官方规定不可用。iOS 12 设备 2026 年存量近零；如必须支持 12 需回退 `loadFileURL` 方案
  （代价：`texImage2D` 跨源 SecurityError，开始页背景光波会没）。
- Xcode Rust 脚本是 `cargo build --bin phira-main --no-default-features`（无 webui feature）
  → iOS 模块门按 `target_os = "ios"` 成立，不编 wry ✔。
- ⬜ Xcode 工程要把 `kaichang/` 打进 bundle（资源引用未加，真机阶段处理）；
- ⬜ 真机验证：WKWebView 创建/导航/注入时序、texImage2D（scheme 域下应无跨源问题）。

**改动清单**：`webui.rs`（19 处 `#[cfg(windows)]` + `init_scripts()` 共享构造 + 安卓段约 130 行）、
`lib.rs`（模块门 + `read_java_string` 改 pub(crate)）、`WebViewHost.java`、`MainActivity.java`
（QuadSurface 包进 FrameLayout，WebView 默认 GONE 懒创建，失败不拖垮游戏）。

---

## 6. 编译 / 发布 / 验证

### 6.1 命令
```bash
# 快速检查（nightly 工具链，别加 +stable）
cargo check -p phira-main --no-default-features --features webui

# 出 exe（依赖缓存后约 1 分钟）
CARGO_TARGET_DIR='D:\meew11-target' CARGO_INCREMENTAL=0 \
  cargo build --release -p phira-main --no-default-features --features webui
```
- ⚠️ `--features webui` **必带**（main.rs/home.rs 无条件调 webui 真模块）
- ⚠️ **必须 `CARGO_INCREMENTAL=0`**（D 盘增量报「拒绝访问」+ prpr 撞 rustc ICE）
- 产物在 `D:\meew11-target\release\phira-main.exe`，**发布 = 拷到 `PhiRa-Mod-Meew-11/rev/phira-pc/phira-main.exe`**
- ⚠️ **后台构建别接 `| Select-Object -Last N`**：会提前关闭管道让 cargo 退出码变 1（其实已 `Finished`）。
  用 `> log 2>&1; echo exit=$?` 再 `tail`。
- 构建前先 `Get-Process cargo,rustc` 看有没有野进程抢占（曾因此跑 15~20 分钟）

### 6.2 基线
- **允许的告警只有 5 条**：`hud.rs`(mut) / `newui.rs`(rx) / `history.rs`(CARD_BG) / `library.rs`(rgb) / `threed.rs`(未用项)。
  `warning: phira (lib) generated 5 warnings` 即正常；**多一条就要修**。
- 10-14 发布：`phira-main.exe` = **29,026,816 B**，sha256 `f70d97ab69290c02…`
- HTML 侧校验三件套：`<div>` 平衡、`node --check`（抽出 `<script>`）、JSON 解析（若是 JSON 风格）

### 6.3 设置页 `Setting.html` —— 已按用户设计稿重写（2026-10-15）
- 用户给的**设置页设计稿**：斜切玻璃面板（`skewX(-15deg)`）+ 8 标签（`导航栏.png` 底图）+ 楼梯式 `translateX(30-30i)` 偏移 + 滚动联动位移 + 模糊背景三层（`Rubbish Sorting.png` 模糊 / `背景.png` 叠加 / 暗罩）+ 账号卡 / 日志卡 / 关于页。**照搬**，含其自带动效 keyframes。
- 历史包袱：10-14 用户曾要求把设置页「全部回退到会话前」，随后又给了这份设计稿 → 现已**整页替换**为该设计稿并接线。
- **接线（全部真实功能）**：
  - 开关/滑块/数字/文本/下拉：`data-config` 走 `setConfig`（滑块用 Pointer Events）；`data-data-key` 走 `setData`（`enable_anys`/`anys_gateway`/`accept_invalid_cert`/`prefer_reduced_motion`）
  - 特殊控件：`sampleCount`（低画质 = 1 / 正常 = 2）、`mods` 位标志（自动游玩 = AUTOPLAY `0x0001`，`mods` 是 `#[serde(transparent)]` 的 i32）、`rksOverride`（留空 = 跟随账号）
  - 枚举下拉（camelCase）：`challengeColor` / `noteMap.tap|drag|flick|hold` / `touchMarkerStyle` / `touchMarkerColor`
  - `data-action`：`file`/`reset-file`（字体与立绘）、`page`（黑名单/成绩历史/删除账号）、`url`（服务器状态/关于页链接）、`tool`（编辑准度条 → `timing`）、`reset`（恢复默认）
  - 账号页：`listAccounts` 渲染当前状态 + 已保存列表（切换/删除）+ 登录表单 + 退出
  - 语言行 = `listLanguages` 填充 `<select>` → `setLanguage`
  - 日志页：`getLog` 刷新；「清空」只清显示（**无清空桥**，原生也没有对应命令）
- ⚠️ **10-14 那个「别再动 Setting.html」的约定已被用户的新设计稿取代**；改动前仍建议确认。
- ⚠️ 设计稿里 **`confirm()` 不会被 ALERT_JS 接管**（只接管 `alert`），所以「恢复默认」没走 `confirm`，改为直接执行 + toast。
- ⚠️ 设计稿没有、但原生有的设置项（**未加**，等用户定夺）：映射长条长度 `noteMapHoldMs`、血条上限/强弱/长度/颜色 `healthMax/healthScale/healthBarLen/healthBarColor`、音符流速 `noteFlowSpeed`、音符大小 `noteScale`、噪区精细度 `blockMaskDivisor`、静止判定时间 `motionStillTime`、摇动幅度 `motionThreshold`、HUD 透明度 `hudAlpha`、残影数量 `noteTrailCount`、准度条计数 `timingBar.counts`、成绩上传协议 `uploadAgreed`/`upload-profile`。
- ⚠️ 设计稿里的「跟随账号」**不是独立开关**（原生 `rks-follow` 只是 RKS 行在 `rks_override` 为空时显示的文案）→ 已改成真正的 **RKS 行**（文本输入，留空 = 跟随账号）。

---

## 7. 硬约束 / 禁令（踩过坑的）

1. **`kaichang/index.html` 逐字节不改** —— 全屏触点靠注入脚本，改了就废。
2. **别把更多界面搬去 WebView，也别删 webui**。当前范围：设置 + 开始页（+ 收藏夹/工具/资源包/曲库这些工艺品级页面）。
3. **不 commit / 不 push**（`git` 连 github 用 `http.sslBackend=schannel` + `http.schannelCheckRevoke=false`）。
4. **不改用户存档** `rev\phira-pc\data\`；**不启动程序、不截图**。
5. HTML 改 `D:\Phigros\kaichang` 后**必须同步** `PhiRa-Mod-Meew-11/rev/phira-pc/kaichang/`（随包副本）。
6. ⚠️ **整块替换 HTML 里 `var I18N = {...}` 这类对象时**：`html[:start] + "var I18N = " + …` 会把前缀写重
   （`start` 已越过前缀）→ JS 语法错。**必须先剥旧前缀，且写完必须 `node --check`**。
   （这个错 10-14 一天犯了两次。）
7. `windows-sys` 只开了 `Win32_UI_WindowsAndMessaging`/`Win32_UI_HiDpi`/`Win32_Foundation`
   → 别用 `RedrawWindow`/`InvalidateRect`。
8. 枚举 `<option value>` 必须 camelCase（`Config` 容器 `rename_all="camelCase"`，大写首字母值会被 `setConfig` 静默丢弃）。
9. `QueryBuilder::search/order` 形参是 `impl Into<Cow<'static, str>>` → **必须传 `to_owned()`**，不能传借用的 `&str`。
10. 桌面（Windows）打不进 BMP 外字符（emoji/增补汉字）：`prpr-miniquad` `windows.rs` `WM_CHAR` 把代理对当码点丢弃。**未修**。

---

## 8. 待办 / 已知缺口

| 项 | 状态 | 备注 |
|---|---|---|
| 在线谱面一键下载并开局 | **已完成（10-15）** | `start_online_chart` 桥 + `PENDING_ONLINE_CHART` → `SongScene(local_path=None)`；下载仍走原生 `global_start_download` |
| 曲库分类「上架/特殊/未上架/热门」 | 已接真数据 | 但需登录；未登录只有空态提示 |
| `查看 B0`/`课题模式`/RKS 编辑器 | 已接原生浮层 | 从浮层返回后应回曲库页，**待实测** |
| 用户自己的设置页设计稿 | **等用户提供** | 拿到后照搬 + 把现有绑定（`data-config`/账号/字体/关于/日志）接过去，功能不丢 |
| 设置页多语言（14 语言） | **已按用户要求回退** | 以后要不要做，等用户发话 |
| 安卓 APK | `scripts\build_apk.ps1` | 从 agent 调须先 export `ANDROID_SDK_ROOT/ANDROID_HOME/ANDROID_NDK_HOME` |
| phirc BP 入口真机走查 | **待环境** | `webui.rs` 已改（`BP_NAV_JS` + 两分支挂点）→ 须先会话外 `cargo check -p phira`（零警告）+ 重编 exe；走查项：BP 钮显隐 / iframe 连通 / 房码回填 / ✕·Esc 双态返回 / `phirc:` 守卫不误伤 iframe |

## 9. 变更记录

| 日期 | 内容 | 产物 |
|---|---|---|
| 2026-10-14 | 曲库页 `library.html` 新建（照搬设计稿 + 接真数据）；`open_library` 路由；3 个桥（`list_online_charts`/`get_custom_rks`/`open_overlay`） | exe 29,026,816 B / sha `f70d97ab69290c02` |
| 2026-10-14 | 设置页 `Setting.html` 按用户要求**全部回退**到会话前（勿动） | 103,651 B |
| 2026-10-14 | 本文档建立（取代 `settings_webview_handoff.md`） | 236 行 |
| 2026-10-15 | 在线谱面「下载并开局」：`start_online_chart` 桥 + `PENDING_ONLINE_CHART` + `take_next_scene` 第二分支；曲库页在线卡接 `startOnlineChart(id)` | exe 29,230,080 B / sha `1e1fe4ff0bbb6d18`；`library.html` 44,728 B |
| 2026-10-15 | **设置页整页替换为用户设计稿**（斜切玻璃 + 8 标签 + 楼梯偏移 + 模糊背景），并接线全部真实功能（开关/滑块/下拉/Data 级/位标志/文件选择/账号页/语言/日志） | `Setting.html` 85,673 B（免重编） |
| 2026-10-15 | 修「曲库卡死」：① 模板选择器漏 `]`（致命，整页不渲染）② 本地分页 ③ `createIcons` 只调一次 ④ 桥超时兜底 | `library.html` 47,108 B（免重编） |
| 2026-10-15 | **修「点上架区卡死」**：`list_online_charts` 在 WebView 线程调 `check_read_tos_and_policy`（未接受条款时 `Dialog::show()` + 写盘）→ 消息泵死锁。改为线程安全只读判断 + `PENDING_TOS` 交主线程弹窗（先 `hide()` 让位，接受后回曲库）；`list_charts`/`get_custom_rks` 也改异步。另修 `listLanguages()` 字段（`list`/`code`） | exe 29,229,056 B / sha `ab8a63d78aea7076`；`Setting.html` 85,851 B |
| 2026-10-16 | 修「一直 must-accept-tos」：① 桥 promise 是**解包**的，library.html 误写 `res.data` → 本地列表永远空 ② TOS 三段式改**持久状态机**（条款拉到才弹窗、不提前隐藏 WebView） | exe 29,071,872 B / sha `9a99bb1d389b8a07`；`library.html` 47,883 B |
| 2026-10-16 | **新增 `.phc` 插件系统**：双形态容器（ZIP/JSON）+ 5 个桥 + 曲库左上角入口与管理面板 + 脚本运行时（register/api/hooks）+ 2 个示例 + 格式文档 | exe 29,371,904 B / sha `4b0e438832836ebf`；`library.html` 63,410 B |
| 2026-10-16 | 插件系统**支持 Python / Rust**：统一「旁挂进程 + JSON-lines」模型（无 FFI/ABI、崩溃隔离）+ 4 个桥 + 前端命令分发/事件推送 + 2 个新示例 + 协议文档 | exe 29,325,312 B / sha `7bf822502f7fcba3`；`library.html` 69,851 B |
| 2026-10-18 | **修「所有页面返回键都回开始界面」**：`library`/`Setting`/`tools`/`tripsUide` 的返回逻辑从 `phirc.hide()`/固定跳转改为 `history.back()`；Setting 双绑定用 `__phircBack` 去重（防连退两页） | `library.html` 71,765 B / `Setting.html` 86,698 B / `tools.html` 18,681 B / `tripsUide.html` 43,219 B（纯 HTML，**免重编**） |
| 2026-10-18 | **回放系统**（录制/库/播放 + 曲库风格管理页）、**曲绘/资源包贴图跨 origin 修复**（改相对路径）、**红黄键 10ms 保护窗**、**课题模式曲间黑场** | exe 29,821,440 B / sha `3f889d83f05c6dc6`；`Replays.html` 17,270 B；`library.html` 72,306 B |
| 2026-10-18 | 修 **`BRIDGE_JS` 漏逗号导致整个 WebView 失联**（开始页「点不动」）；新增 `scripts/check_injected_js.py` 注入脚本语法检查 | exe 29,8xx,xxx B（本次重建） |
| 2026-10-18 | **谱面管理面板 `ChartPanel.html`**（用户设计稿 4 tab：Mod/全局设置/编辑谱面/谱面信息）+ 5 个桥 + 原生入口 + PENDING_MODS 同步 | exe 29,751,808 B / sha `e0a1c2930f4b6b0a`；`ChartPanel.html` 41,971 B |
| 2026-10-25 | **phirc 内置 BP 连接入口**（user 点名「做一个新入口」）：挂点唯一合规解 = `webui.rs` 注入脚本——新增 `BP_NAV_JS`（左下 52px「BP」浮动圆钮 → `location.href="bpHost.html"`；幂等 + `phirc:` 协议守卫 + 排除自身）并在 `init_scripts_base` 两份 cfg 分支 format 挂入 + doc 注释 ×2；新建 `kaichang/bpHost.html` 10,926 B（地址簿 `phirc_bp_addrs` 留 5 展 3 / `normalize()` 只收 `host:port` / `no-cors` 探测失败不拦截 / **iframe 全屏嵌 bp-host 前端 —— 同源故 `/api/status` 房码回填 `/ws` 原生可用，bp-host 零改动** / ✕ `history.back()` 兜底 / Esc 双态分流）。主工程 `library.html`/`index.html`/`tripsUide.html` 与 `tools.html` 一字未动；HTML 改动**免重编** | `bpHost.html` 10,926 B（新）；`webui.rs` 214,421 B（改，待会话外 `cargo check`——`rustfmt --check --edition 2021` 整文件解析已过，见下） |
| 2026-10-25 | BP 入口审查一批（同日第二波）：① **修致命层叠 bug**——`HOTSPOT_JS` 开始页全屏触点（z 2147483647）原压在 BP 钮（…3646）之上 → **开始页上 BP 入口整页失效**（点击被触点截胡走 260ms 消歧去收藏夹）；修法 = BP 钮 z-index 同 2147483647（注入链 BP 在 HOTSPOT 之后执行、DOM 后挂居上）+ click `stopPropagation`，webui.rs doc 记坑。② bpHost.html（20,404 B）修三 bug：probe 无超时 → **3s AbortController + RTT 测量**；端口 `\d{2,5}` 未验数值 → **1–65535 数值校验**；进 iframe 无反馈盲区 → load 事件（一次性挂载 + loadTick 防旧监听累积）+ 8s 看门狗只提示不拦截。③ **创意：宿主信标实时徽章**——bp-host 前端新增 `beacon.ts`（仅 `window.parent!==window` 时 postMessage 上报，2s 心跳；bp 页走 store 合流点 + WS onStateChange 挂线、join 页上报大厅态、离线分享页一次性上报），bpHost.html 右上角常显（回合 X/Y · A Ban/Pick · 本地倒计时 · 大厅房码/在线数 · 重连/断线红黄蓝、>7s 无心跳灰显「信号中断」；origin 白名单 + 畸形消息吞掉）。dist 重建：`app-bp.js` 69,000 B / `app-index.js` 5,590 B（node --check 双绿） | `webui.rs`（再改 216,494 B） / `bpHost.html` 20,404 B / `beacon.ts`（新） / bp-host `app-core.ts`·`app-ui.ts`·`entry-index.ts`（信标接线） / dist 重产 |
| 2026-10-25 | **游玩跳转 + 连续游玩闭环**（同日第三波；user 令「选完图跳正常游玩、连续游玩」）：bpHost.html 新增反向通道 `phirc-bp-play`（与信标同一 msg channel、同一 origin 白名单）——接收方于既有 message 监听器扩支（beacon/play 双协议），play 横幅 `#play-hint`（居中提示 2.6s 自淡 / sticky 常显 / `playHintClear`）。链路：bp-host 前端 `beacon.ts` 新增 `playIntent`（同源 fetch `/library-snapshot.json` 按曲名补 `download/<id>` → onlineId）→ app-ui.ts `operationConfirmed` **显式 case**（pick 且 role=player → playIntent；观战只 toast）→ bpHost `playChart`：本机曲库 `phirc.listCharts` 曲名精确 + 难度就近 → `phirc.startGame(path)`（**原生既有桥**，SongScene auto-launch 直进谱面；webui.rs L2244/L684-706 零修改）；未命中 → `phirc.startOnlineChart(onlineId)`；再失败逐字报错（未下载/离线模式/未登录/TOS 原话），**不静默**；chartId 去重防重连重放重复跳。**连续游玩**靠原生 handoff 语义：桥开局置 `WEBVIEW_HANDOFF` → 打完回 MainScene 时 `scene/main.rs` L553-562 `take_webview_handoff()` 命中 → `show()` 只显示**不导航** → bpHost.html + iframe 大厅状态全保留 → 下一首 Pick 确认再来一局。观战/Tauri 控制台/浏览器直开分支短路。本波 `webui.rs` **零改动**（全用既有桥） | `bpHost.html` 25,262 B / bp-host `beacon.ts`（+playIntent）·`app-ui.ts`（+case） / dist 重产（app-bp 70,375 B · app-index 5,590 B，node --check 双绿）；`webui.rs` 216,494 B（未动，前批改动能一并重收） |

