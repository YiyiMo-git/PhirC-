# bp-host 设计文档（DESIGN.md）

> 任务#8 交付：网络架构 / BP 逻辑 / 干扰机制 / 技术约束 / 交付对照表。
> 接手事项、文件实况与下一步以 `../bp-host_handoff.md` 为准；本文只讲「系统如何设计、为何这样设计」。
> 知识基线 2026-10-25。测试基线：`cargo test --no-default-features` = **26/26**（22 行为断言 + 3 serde 契约测试 + 1 优雅停机 smoke）；default-features `cargo check` 零警告全绿。

## 0. 一句话

以 Ban/Pick 选图为核心流程、**主机即服务器**（Rust 内嵌 axum HTTP + WebSocket）、内置「干扰」互动层的赛事主持工具；**状态权威 100% 在主机 Rust 侧**，客户端只发意图、只渲染广播结果。

## 1. 体系与部署形态

```
┌─ 主机（玩家自己的电脑，Tauri 应用） ──────────────────────┐
│  commands.rs   Tauri 命令契约（本地控制台 = 第 0 号连接）     │
│  service.rs    BpService：状态锁 + 会话表 + 广播 + 审计落盘   │
│  ├─ bp/state_machine.rs   回合状态机（唯一可变权威状态）       │
│  ├─ bp/interference.rs    干扰资源与效果（validate/apply 纯函数）│
│  ├─ net/protocol.rs       版本化协议契约（Tagged serde）        │
│  ├─ net/server.rs         axum：静态前端 + /ws + /api/status     │
│  ├─ net/session.rs        WS 连接生命周期（读写分离/踢出/解析）    │
│  └─ persistence/sqlite.rs 会话与事件审计 + 崩溃恢复             │
└──────────────────────────────────────────────────────┘
      ▲ 手机浏览器 http://主机IP:端口（主力客户端形态）
      ▲ 同款 Tauri 应用作本地控制台（invoke 通道）
```

- 一份前端代码（`src/frontend/`），以 `role` 参数切换 `host` / `player` / `spectator` 三种视图——不是三个前端实例，而是同一页面按角色渲染。
- 本工具与 `duel/`（phira-duel）**只共享 serde 约定风格**，不共享代码；duel v2 已明确不做 BP。

## 2. 网络架构（交付要求一）

### 2.1 传输与编码

- WebSocket **JSON 文本帧**（浏览器友好），帧内只有消息体；不走 duel 的 `[u32 len][JSON]` TCP 帧。
- serde 约定（沿用 duel 风格）：`#[serde(tag = "t", rename_all = "camelCase")]` internally-tagged，抓包/日志直接可读。
- **新类型变体拍平**：`HostEvent::Snapshot(BpSnapshot)`、`OperationConfirmed(BpOperation)` 等包结构体的变体，线上形如 `{t:"snapshot", roomCode:…}`——结构体字段直接在顶层，**没有** `snapshot:` 包裹字段（contract tests 锁定）。
- **嵌套同 tag 枚举必须命名承载**（本轮修复的教训）：`ClientIntent::HostCommand(HostCommand)` 新类型嵌套两层同 `"t"` tag 会撞字段名（serde: duplicate field `t`），任何主机裁定命令都到不了状态机。现改为结构体变体 `HostCommand { host_command }`，线上形如 `{t:"hostCommand", hostCommand:{t:"startBp"}}`。
- 无法解析的帧 → `Error{code:"BAD_JSON"}` 事件（不静默，session.rs）。
- 协议版本 `PROTOCOL_VERSION = 1`，随 `Welcome` 下发。

### 2.2 事件流拓扑（三条入口，同一 service 层）

| 入口 | 意图路径 | 结果到达路径 |
|---|---|---|
| WS 客户端 | session.rs 读循环 → `svc.process_intent(conn_id, intent)` | 该连接专属回执写回 socket；公共广播从 `svc.events` 订阅桥接 |
| 本地控制台 | Tauri 命令 `bp_send_intent`（commands.rs） | 返回值 = 连接专属直接回执 `Vec<HostEvent>`；公共广播走 Tauri `bp-event`（spawn_event_bridge，与 WS 同源同序） |
| 主机定时器 | `spawn_timer_ticker`（server.rs，间隔 `config.timer_sync_interval_ms`） | TimerSync 广播、超时裁决、SessionEnded 走 `svc.tick()` 后 broadcast |

前端 `net.ts` 两个传输实现产出同一 `BpTransport` 契约：

- `createWsTransport`：指数退避重连 800ms × 2^n 封顶 15s；`open` 后由业务层发 `RequestSnapshot` 恢复全量；
- `createInvokeTransport`：命令 `bp_send_intent` 的直接回执 + `bp-event` 桥**合流**到同一 handler 集合（两端不重叠：直接回执是连接专属事件）。

### 2.3 身份与连接生命周期

- `Join { roomCode, playerName, role, side, token }`：token 未携带则主机签发并随 `Welcome` 下发；**重连必须带 token** 才能恢复同一 PlayerId（防冒名）。
- 断线：WS 真关闭 → `PlayerDisconnected` 进时间线，**不自动暂停 BP**；同一身份仍有其他存活连接时不误报（多设备补线）。
- `LockRoom { locked }`：拒绝新连接。
- 局域网外的可访问性不做默认实现：UI 提供「端口转发/中继说明」入口，其数据来自 `bp_join_hint`（含 `public_hint_url`）。**所有地址/端口/房间码/间隔全部来自 `config.rs`，代码内零硬编码。**

### 2.4 状态权威与合法性分层

- 所有状态变更在主机锁内原子完成：校验 → 状态翻转 → 审计日志 → 广播。
- 非法意图**必回错误事件**：`OpError → OperationRejected`、`InterferenceDeny → InterferenceDenied`、`IntentRejected → Error`；禁止静默忽略。
- 两级合法性：`legal_intent`（状态机粗判：谁的回合、什么状态允许什么意图）→ `apply_operation` / `try_interference`（细判：曲目状态、资源、阶段门控）。

### 2.5 HTTP 路由（server.rs）

- `/` 及静态文件：读 `config.frontend_dir`（默认 `frontend`，相对启动 cwd）；有目录穿越防护；未就绪时返回引导页而非裸 404。
- `/ws`：升级进入 `net::session::run_socket`。
- `/api/status`：房间码 / 状态 / 在线人数 / 协议版本 / locked（二维码页与排障用）。

## 3. BP 逻辑（状态机）

### 3.1 回合序列解析（**验收契约以测试断言为准**）

`parse_sequence(seq, first)` 规则：

- `b` = 一个 Ban step：先手方 Ban → 另一方 Ban（2 turn）；
- 连续 `p` 段：**每 2 个 `p` 合并为一个 Pick step**（双方各 Pick 一张），奇数收尾并入最后一个 pick step——即 `run.div_ceil(2)` 个 step；
  - `"bppp"` = 3 step / **6 turn**（全 `!is_tb`）
  - `"bpbpp"` = 8 turn；单独 `"p"` = 1 step / 2 turn
- `t` = TB：先手方单 Pick，`is_tb = true`（1 step / 1 turn）；
- 空串 / 非法字符 → `ParseError`。

### 3.2 主循环 `apply_operation`

1. 校验：BP 进行中 → 轮到该方 → 动作匹配回合 → 曲目可用（`ForcedBanCandidate` 在「强制位匹配且动作是 Ban」时放行——**强制位必须可以被 Ban**）；
2. 强制 Ban 位：回合带 `current_forced_ban` 时 `chart_id` 必须等于它，否则 `MustIncludeForcedBan`；
3. 状态翻转：`Available → Banned / Picked`；
4. push 操作（`order_no` 全局自增，进时间线与导出）→ `advance_after_op`：索引 +1 → **跨 step 重置**（`strategic_received` / `compress_used_this_round` / 强制位释放恢复、`pending_force_reveal`）→ InProgress 时按回合重新计时；
5. turns 走完 → `Finished`。

暂停/恢复：`Pause` 冻结 deadline 记录剩余；`Resume` 按 `resume_mode` 续算（Continue，默认）或满时长重置（Reset）——配置项 `resume_timer_reset`。崩溃恢复：`config.auto_recover` 时启动从 SQLite 拉回未完结会话（service.rs:147）。

裁定回退 `arbitrate_rollback(order_no)` 为**级联语义**：撤销指定操作及其后的全部操作（`split_off(pos)`），被撤曲目回到 Available、`current_index = min(index, pos)` 精确对位被撤回合、强制位 / 挂起公开 / 回合计数随回合重开失效、Finished 回升 InProgress 并重新计时；返回被撤操作时间序数组（目标在首），`order_no` 全局自增不回收（审计可溯）。空操作哨兵（chart_id = 0）在真实曲库（id 从 1 起）找不到对应曲目，find 不命中即天然无害。

### 3.3 超时处理（主机 ticker 到点调用 `execute_timeout`）

- Ban 回合 → **空 Ban**：合成 `chart_id = 0` 占位操作（导出/时间线显示「空 Ban」），同时释放本回合强制位；
- Pick 回合 → 使用调用方选择（服务层按 `TimeoutAction` 提供：随机 / 判给对手→None）；**有效选择先把曲目标 `Picked` 再 push 操作**（本轮修复——否则曲池状态与时间线不一致）；无效/无可选 → `chart_id = 0` 空 Pick；
- ⚠ 已知冲突：`0` 既是「空操作哨兵」也可能是真实曲目 id。当前曲库若存 id=0 的曲目，导出与时间线渲染会歧义。测试固化了 0 = 哨兵语义；曲库导入时建议 id 从 1 起。

### 3.4 快照与结果

- `BpSnapshot`：重连/初进/大状态变化时全量推送，唯一事实源；
- `BpResult`：`bans / picks / available_charts / interference_log / finished_at_ms` → 结果页导出图片 / 复制链接。

## 4. 干扰系统（交付要求二）

### 4.1 资源模型

- 点数制：初始 `interference.initial_points`（默认 3），用尽不回复；
- 每回合每方最多承受 `max_strategic_per_round_per_side` 次策略干扰（默认 1）；
  - **饱和判定为回合全局**（本轮修复）：`strategic_received.values().max()`——本回合一旦有方达到上限，回合即封闭，不再接受新的策略干扰（交付用例语义）；
- 反制：`counter_interference(side, id)`，校验目标是自方 + 该事件未反制过 → 扣 `counter_cost`（默认 1）→ 标记 `countered`；同一事件只能反制一次；
- 主办方裁定：禁用某侧干扰权 / 回滚最近干扰（`by_host` / `rolled_back` 进审计）。

### 4.2 校验流水线（`validate` 纯函数 + 状态机兜底）

```
全局开关 → 种类白名单 → 不能自指 → (Emote 至此放行)
→ 发起方未禁用 → TB 需 allow_on_tb → 阶段匹配 default_allowed_actions
→ 点数充足 → 压缩同回合唯一 → 承受未达上限
[状态机侧，依赖曲池权威状态:]
→ ExtraBanPressure 必须指定强制曲目且该曲目 Available
→ ObscurePool 过滤后必须非空
```

拒绝全部枚举化（`InterferenceDeny` + 中文 `message()`），经服务层转 `InterferenceDenied` 事件。

### 4.3 五类效果（`InterferenceParams`，`tag = "k"`）

| 类型 | 参数 | 行为 |
|---|---|---|
| CompressTimer | `{reduceSecs}` | 实际减量 = min(配置, 剩余−floor)；真扣 deadline；同回合不可叠加 |
| ObscurePool | `{chartIds,count}` | 纯渲染态（不进 Actors' `ChartStatus`），本回合结束自动失效 |
| ForceReveal | `{candidateChartId}` | 挂 `pending_force_reveal`；目标方 `RevealCandidate` 上报补全；候选不可撤回、可改选 |
| ExtraBanPressure | `{forcedChartId}` | 设置 `current_forced_ban`，目标本回合必须 Ban 它 |
| Emote | `{emote,text?}` | 轻量社交层，0 点、不受策略约束、不进审计日志；频控为**连接级令牌桶**（`EmoteBucket`，按 `spectator.emote_per_sec`），在 service 会话表实现 |

emoji 是表情内容的一部分；`text` 为可选弹幕文本。

### 4.4 审计

`InterferenceEvent { id, kind, from, target, at, params, countered, rolledBack, byHost }` 与 Ban/Pick 并列进全局时间线；**干扰摘要（结果页）只统计策略干扰**，Emote 不入 log。

## 5. 前端结构与设计令牌（任务#6 / #7 / #10）

### 5.1 设计令牌

- `src/frontend/design-tokens.css`：**唯一视觉值来源**。含 handoff 第 5 节逐字的 9 个颜色角色（`surface`、`surface-elevated`、`brand-blue`（A/主操作）、`opponent`（B）、`ban-muted`、`pick-accent`、`danger-timeout`、`interference`、`spectator-muted`）+ 扩展角色（`surface-sunken` / `danger-hot` / `text-*` / `border-*` / `overlay-scrim`）；字号 10→44px 阶梯、4px 间距栅格、圆角/阴影/焦点环；`--touch-target: 44px`、`--dvh: 100dvh`、`--safe-bottom: env(safe-area-inset-bottom)`；动效三档（120ms Ban/Pick 反馈 / **180ms 回合切换边框迁移=唯一存在感动效** / 300ms fx）+ `prefers-reduced-motion` 降级 + `[data-theme="light"]` 对照版。
- `tokens.ts`：`TOKEN_VARS` 名称镜像 + `readToken()`（CSS 未加载时**显式报错**而非静默造一个视觉值）+ `sideToken` / `deadlineTone`（>10s 正常 / ≤10s 琥珀 / ≤3s 红）。
- 组件层**禁止硬编码视觉值**，一律 `var()` 读令牌。数据驱动的量（环形进度 `--ring-pct`、排行色条 `--leader-rest` / `--leader-tone`）以内联 CSS 自定义属性注入——这是**数据注入通道**而非硬编码；JS 侧一切颜色/时长走 `tokens.ts`。

### 5.2 线框（可预览、纯演示数据）

- `wireframe.css`：三页共用结构件（card / badge / btn / turn-card / ring conic 倒计时 / pool-grid + chart-cell（含 masked 毛玻璃问号）/ timeline / leader-bar / sheet≤40dvh + scrim / toast / cn-pulse + fx-tag / app-top / ops-dock / qr），全部 var() 引令牌。
- `wireframe-host-console.html`：桌面控制台（顶栏房间码+二维码+连接数；回合卡+ring；曲池+时间线+干扰裁定面板；底部仲裁 dock）。
- `wireframe-player-bp.html`：移动竖屏（对手+干扰点；回合卡+倒计时环；遮蔽格；底部确认+干扰+表情栏；Bottom Sheet 二次确认 + toast 演示 JS）。**BP 进行中无模态框**：确认走 Sheet、提示走 toast。
- `wireframe-result.html`：结果/分享卡（导出图片+复制链接入口；Ban/Pick 时间线按 `order_no`；干扰摘要；阶段进度；排行色条）。

### 5.3 net.ts 契约（与 protocol.rs 对齐）

- 类型全集逐字段镜像（含 `ChartStatus`、`InterferenceParams` tag="k"）；
- 意图/命令/事件形状见 §2.1（拍平事件、`hostCommand` 命名承载）；
- invoke 通道用 `bp_send_intent`；该返回值（连接专属回执）与 `bp-event` 广播合流分发；UI 合并器如见重复事件按 (id, atMs) 自行去重。

### 5.4 真实页面（本轮交付，任务#9）

- **无打包链方案**：真实页 = 纯静态 ES modules，`app-core.js`（net.ts/tokens.ts 的运行时镜像，头注释互链）+ `app-ui.js`（渲染层）+ `app.css`（布局补充件）；接打包链后改为直接 import 两个 TS（两文件头注释已互链）。
- **页面与角色**：`index.html` 入口双上下文（Tauri 窗口检测 `window.__TAURI__` → 控制台直达 `bp.html?role=host`；浏览器 → 加入表单：昵称 / A·B·观战 / 房码手动或同源 `/api/status` 自动填充）；`bp.html` 为对局壳，module onload 调 `boot()`。前端地址零硬编码：WS 同源推导（http/https → ws/wss）、`fetch` 与 `invoke` 各自走同源/全局 Tauri。
- **store 幂等 reducer**（app-core.js `createStore`）：snapshot/pool/phase/operation/interference/counter/timer/players/sessionEnded 全部幂等合并；**派生渲染态** `masks`（最新一条针对我的未反制 ObscurePool 的 chartIds）与 `forceReveal`（未填候选的 ForceReveal）在换 step（`phaseChanged.stepIndex` 变化）时本地清空——遮蔽/强制公开随回合过期。`OperationConfirmed` 仅作先行标色展示，权威仍是 `PoolUpdated`/`Snapshot` 覆盖。
- **渲染分流**（app-ui.js）：`renderWaiting → Lobby → Progress（inProgress/paused）→ Result`；顶栏含房间码 / 状态徽 / 连接 chip / 锁房 / 身份。曲池格：遮蔽态（`masked` + 问号占位，逻辑上仍可选）、强制位（权威 `status=forcedBanCandidate` 优先，`extraBanPressure` 派生兜底）、`picked-by` 内嵌描边。时间线区分空 Ban / 空 Pick（`chart_id=0` 哨兵）。
- **意图面**：`setReady` / `ban`/`pick`（Bottom Sheet 二次确认）/ `revealCandidate` / `useInterference`（遮蔽多选 ≤ `obscure.maxCount`、强制位单选、压缩与强制公开直接确认）/ `counterInterference`（− config.counterCost）/ `sendEmote` / `leave`；host 命令 `startBp`/`lockRoom`/`pause`/`resume`/`disableInterference`/`kick`（二次确认）/`arbitrateRollback`（**级联回退**：sheet 全历史可点，级联影响数按全量 operations 计算展示）/`setPool`（曲库快照导入：`tools/export-library.cjs` 产物 → 校验 → 白名单字段下发，曲 id 从 1 起避开 0 哨兵）。结果页分享：**离线 dataURL**（`packBpLink` → `bp.html#bp=<URL-safe base64>`，打开时 `boot()` hash 检测进只读结果页）+ **导出图片**（canvas 手绘版式 PNG：颜色/字号全 `readToken` 读令牌、2x backing store 两遍绘制、离线分享页同源可导）。
- **前置校验仅提示不越权**：非我回合 / 强制位双向约束（`MustIncludeForcedBan`）/ 遮蔽可视化提醒均为客户端提示，最终裁决以主机拒绝事件为准（`OperationRejected` / `InterferenceDenied` / `Error` 全部 toast 显式回显）。
- **join 失败语义**：join 在途错误 → toast 显式；浏览器端致命码退回 `index.html`；Tauri 站内提供「重试加入」。移动端底线全部落实（token 暂存 `bp-host:join`，重连带 token 恢复身份）。

## 6. 技术约束（实施守则）

- 硬约束：不 commit/push；不启动程序；不改 `rev\phira-pc\data\`；地址/端口/房间码/回合计时全部走配置；客户端**零乐观推进、不自行扣资源**；非法意图必回错误事件；主工程三页（`library.html` / `index.html` / `tripsUide.html`）禁改。
- 移动端底线：热区 ≥44px、`touch-action: manipulation`、`100dvh`（禁 100vh）、底部 `env(safe-area-inset-bottom)`、确认走 Bottom Sheet（≤40dvh + 二次确认）、进行中禁模态。
- WebView 基线：Chrome Android / Safari iOS 最近两个大版本；不用仅桌面 Chromium API。
- 构建与工具链：
  - 校验/测试在 `bp-host/src-tauri` 目录跑 `cargo check/test --no-default-features`；
  - D 盘构建必须 `CARGO_INCREMENTAL=0`；`CARGO_HOME=D:\Environment\Rust\cargo`、`RUSTUP_HOME=D:\Environment\Rust\rustup`；
  - MSVC 先 vcvars64（helper `ccdbg\setup-env.ps1`）；
  - **target-dir 外置**：`D:\Phigros` 树被 360 套件 minifilter 设为保护区（ACL `Everyone:(DENY)(DC)`），树内进程跨区加载 DLL 被静默拦截 → cl.exe `D8050`。解法：`.cargo/config.toml` `[build] target-dir = 'D:\Environment\build\bp-host-target'`；
  - **源文件只 UTF-8 直写**：对带中文注释的 .rs 做 GBK decode→encode 往返会损坏文件（本轮 `interference.rs` / `protocol.rs` 曾坏并重建）。批量改动一律 Latin-1(28591) 视图提取结构 + UTF-8 直写重建。
- 测试即验收契约：实现向既有断言对齐（不改测试凑实现）；协议形状变更必须带 serde roundtrip 测试（`protocol.rs::tests`）。

## 7. 交付对照表（对应 handoff 第 5 节列表）

| 列表序 | 条目 | 状态 | 位置 |
|---|---|---|---|
| — | 领域原语 model.rs | ✅（先期完成） | src/model.rs |
| — | net/protocol.rs 协议契约 | ✅ + 本轮形状修复与 3 契约测试 | src/net/protocol.rs |
| — | bp/interference.rs 干扰系统 | ✅（本轮 UTF-8 重建） | src/bp/interference.rs |
| — | bp/state_machine.rs 状态机 | ✅ + 本轮 4 项语义修复 + 级联回退 | src/bp/state_machine.rs |
| 1 | 工程骨架（任务#1，独立 workspace + Tauri） | ✅（default features `cargo check` 全绿；NSIS 实机打包待真实环境） | src-tauri/* |
| 2 | net/server.rs + net/session.rs（任务#5） | ✅（含优雅停机） | src/net/* |
| 3 | persistence/sqlite.rs（含崩溃恢复接线） | ✅ | src/persistence/* |
| 4 | commands.rs（invoke 与 WS 共 service） | ✅ | src/commands.rs |
| 5 | config.rs（全配置化） | ✅ | src/config.rs |
| 6 | 单测 | ✅ 26/26（含级联回退 ×2 + 停机 smoke） | state_machine.rs / protocol.rs / server.rs tests |
| 7 | 设计令牌（任务#6） | ✅ | src/frontend/design-tokens.css + tokens.ts |
| 8 | 三个线框 HTML（任务#7） | ✅ | src/frontend/wireframe-*.html + wireframe.css |
| 9 | DESIGN.md 整合（任务#8） | ✅ | 本文件 |
| 10 | net.ts 网络抽象 | ✅（契约已对齐命令与事件形状） | src/frontend/net.ts |
| 11 | 真实页面接线（index/bp + app-core/app-ui/app.css） | ✅（浏览器/移动端交互走查待真实环境） | src/frontend/index.html、bp.html、app-core.{js→ts}、app-ui.{js→ts}、app.css |
| 12 | 曲库导入工具（tools/export-library.cjs + 快照产物） | ✅（只读主工程；id 从 1 起） | bp-host/tools/export-library.cjs、bp-host/library-snapshot.json |
| 13 | 打包链切换（esbuild bundle，handoff 任务 7） | ✅（dist/app-bp.js 63.7KB + dist/app-index.js 5.3KB 可重复构建；旧 JS 标 deprecated 下轮删） | bp-host/package.json、bp-host/tools/build-frontend.cjs、src/frontend/{app-core.ts、app-ui.ts、entry-index.ts、dist/} |

## 8. 交付缺口 / 备忘

- **default-features（Tauri 壳）已编译验证**：icons（`icons/icon.ico` 多尺寸程序化生成）+ `bundle.icon` 已补，全量 `cargo check`（含 tauri / `generate_context!` / 9 命令 + bp-event 桥）全绿。**NSIS 实机打包仍待真实构建环境**（勿在本机跑）。
- **真实页面接线已完成（§5.4）但未做交互走查**：`index/bp` + `app-core/app-ui/app.css` 语法级 `node --check` 全绿；浏览器端依赖同源 `/ws` 与 `/api/status`，Tauri 端依赖 `withGlobalTauri`（已开）与 `bp_send_intent`/`bp_join_hint`/`bp_server_start`；真实两终端 + 移动端 viewport 走查（点选→确认→倒计时→干扰→反制→结果）留待可启动环境验收。
- `chart_id = 0` 哨兵 vs 真实曲 id 0 的歧义（§3.3）；建议曲库导入 id 从 1 起（级联回退的恢复路径靠 find 不命中天然处理哨兵）。
- 回退 sheet + 曲库导入均已闭环（§5.4）；曲库数据扩充后重跑 `tools/export-library.cjs` 再导入即可（id 同目录内容保持稳定，新谱按排序规则续编）。
- **打包链已切换（handoff 任务 7，§7 表行 13）**：`app-core.ts` + `app-ui.ts` + `entry-index.ts` 为唯一源（传输 import `./net`、令牌 re-export `./tokens`），esbuild bundle（`node tools/build-frontend.cjs`）→ `dist/app-bp.js` / `dist/app-index.js`，html 已切引产物；双维护镜像消灭，旧 `app-core.js`/`app-ui.js` 头部标 deprecated 待删。**esbuild 只转译不查类型**——严格 tsc 是独立后续任务；当前源以轻量标注过渡（`apply(ev: any)` 等）。
- **结果页导出图片已实现（任务 3 完结）**：canvas 手绘版式（零第三方库）；颜色/字号 `readToken` 从 design-tokens.css 读（design-tokens.css 未加载时显式报错不静默）；宽 720 × 2x backing store；行构建-两遍绘制（长曲名按 measure 截断）；`toBlob → a[download]`（文件名 `bp-<房码>-<ts>.png`）；离线 dataURL 分享页同源可导。
- emote 频控是**连接级**桶（重连重置），非按身份持久——依赖 `emote_per_sec` 配置。
- 优雅停机已实现：`serve_listener` 停机信号 + `with_graceful_shutdown`，`bp_server_stop` 发信号并守 5s 超时（smoke 测试 `graceful_shutdown_stops_accepting` 锁定行为）。
- 结果页已提供「复制文本结果」（与渲染同源派生）；「复制链接」的分享链接格式（服务器端点或离线 dataURL）与「导出图片」canvas 序列化尚未实现，属下一阶段决策。
