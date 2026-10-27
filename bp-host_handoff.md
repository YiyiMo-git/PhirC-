# BP 赛事主持工具（bp-host）接手文档

> 写给下一个会话 / 接手人。起草于 2026-10-25；同日第二轮会话更新（后端全绿 + 前端交付）；同日第三轮会话更新（真实页面接线完成）；同日第四轮会话更新（Tauri 壳编译验证 + 级联回退 + 优雅停机 + 回退 sheet 解锁 + 分享链接 + 曲库导入器）；同日第五轮会话更新（打包链切换落地：app-*.js → app-*.ts + esbuild bundle + 结果页导出图片，任务 3 完结——双维护镜像消灭）；同日第六轮会话更新（**phirc 客户端内置连接入口**：`BP_NAV_JS` 左下 BP 钮 → `kaichang/bpHost.html` iframe 嵌入本前端；主工程 `cargo check` 受 agent 会话环境阻断，rustfmt 语法证书已过——见 §4.2）。
> 当前状态：**default-features `cargo check` 零警告全绿；`cargo test --no-default-features` 26/26 全绿；前端打包链构建全绿（node --check 产物 OK）；handoff §5 功能任务 1-6、任务 3-4（含导出图片）、任务 7 全部落地**。
> 剩余缺口：真实页面浏览器/移动端交互走查（dist 产物一起验收）、NSIS 实机打包、严格 tsc（esbuild 只转译不查类型）、phirc 入口真机走查（BP 钮/iframe 连通/房码回填/✕·Esc 返回）。
> 需求全文是用户当次提供的角色设定长文（本文件第 7 节有浓缩锚点，细节以需求原文为准）。
> 设计原理看 `bp-host/DESIGN.md`（任务#8 交付）。

## 0. 一句话定位

以 **Ban/Pick 选图**为流程核心、**主机-客户端组网**、内置**「干扰」互动机制**的赛事主持工具：

- 主机 = **standalone 单文件开服**：`cargo build --no-default-features` 产物 exe + `bp-host.config.json` + `frontend/` 目录三件套，双击即起 axum（`/` 托管静态前端 + `/ws` WebSocket）；主办方控制台与选手页同是这服务托管的前端（`bp.html?role=host|player|spectator`）。Tauri 壳形态（default 特性 `cargo build`）是**可选包装**：本机 GUI + `bp_server_start/stop` 管理同一 service 层，非必需。
- 客户端 = **phira 客户端（phirc）内嵌 WebView 打开 `http://主机IP:端口` 即加入**；普通浏览器开同一地址是等价兜底。发现方式**手动告知 IP**（mDNS 自动发现是需求锚点中的待办，未实现；`/api/status` 可产出二维码页所需信息）。
- 同一套前端代码以角色参数区分：`host`（主机控制台）/ `player`（参赛者）/ `spectator`（观众）
- **状态权威在主机 Rust 侧**：BP 状态机、回合序列、倒计时、干扰资源、操作日志全在主机；客户端只发意图、只渲染主机广播结果，禁止乐观推进/自行扣资源

## 1. 与既有模块的关系（别搞混）

- `duel/`（phira-duel）是**游戏内** 1v1 多人模块。它的 v2 设计（`duel/多人模式设计.md`）**明确砍掉了 BP**（第 9 节「v1 明确不做：BP/BO3」）。
- 本工具 `bp-host/` 是**独立**的赛事主持工具（不打歌、不做游戏内对战，是赛前/赛间的选图主持 + 干扰互动层）。二者只共享 **serde 约定风格**，不共享代码。
- 位置 `D:\Phigros\PhiRa-Mod-Meew-11\bp-host\`，**独立 workspace 已落地**（`Cargo.toml` 内 `[workspace]` 空表，打 App/CI 不会牵连）。
- 不动主工程任何现有文件；`library.html` / `index.html` / `tripsUide.html` 全局禁改的规则照旧。

## 2. 已完成文件清单（磁盘实况，2026-10-25 第二轮）

```
bp-host/
├── DESIGN.md                  任务#8 交付：网络架构/BP/干扰/技术约束/对照表
├── USAGE.md                   使用手册（standalone 开服三件套 / 配置字段 / 办赛 SOP / 选手 BP 钮入口 / 10 分钟自查清单）
├── tools/export-library.cjs   曲库快照导出工具（只读主工程 data/charts；id 从 1 起）
├── library-snapshot.json       曲库快照产物（首份 10 首；扩充曲库后重跑工具再生成）
├── src-tauri/
│   ├── Cargo.toml             独立 workspace（[workspace] 空表）；default=["tauri"]
│   ├── tauri.conf.json        frontendDist=../src/frontend；withGlobalTauri=true；bundle.icon=icons/icon.ico（多尺寸）
│   ├── config.example.json / build.rs
│   ├── .cargo/config.toml     rsproxy 源 + target-dir 外置（见 §4 教训）
│   ├── icons/icon.ico          程序化生成（16/32/48/64/256，32bpp BMP-in-ICO）
│   └── src/
│       ├── main.rs        3 行   入口
│       ├── lib.rs        76 行   模块树
│       ├── commands.rs   246 行  Tauri 命令：bp_send_intent / bp_get_snapshot /
│       │                          bp_get|save_config / bp_server_start|stop /
│       │                          bp_get_result / bp_join_hint / bp_last_order_no
│       │                          + spawn_event_bridge（bp-event 桥，与 WS 同源同序）
│       ├── service.rs  1114 行   BpService：状态锁 / 会话表（含 EmoteBucket 频控）/
│       │                          广播 / 审计 / 崩溃恢复（config.auto_recover）
│       ├── config.rs   201 行    全配置化：bind/端口/房间码/backend/回合计时/
│       │                          干扰/恢复模式/frontend_dir（相对启动 cwd）
│       ├── model.rs     64 行    共享领域原语（叶子模块，打破循环依赖）
│       ├── bp/
│       │   ├── state_machine.rs 1362 行  回合状态机（含 22 行为测试）
│       │   └── interference.rs   274 行  干扰资源与效果（本轮 UTF-8 重建）
│       ├── net/
│       │   ├── protocol.rs   314 行  协议契约（+3 serde 形状契约测试）
│       │   ├── server.rs     188 行  axum：静态+/ws+/api/status + 计时 ticker + 停机 smoke
│       │   └── session.rs     97 行  WS 生命周期：读写分离/kick/BAD_JSON 回错误事件
│       └── persistence/
│           └── sqlite.rs    157 行  会话与事件审计 + 崩溃恢复数据源
└── src/frontend/（本轮全部交付）
    ├── design-tokens.css      唯一视觉值源（9 颜色角色+扩展角色/字号/间距/
    │                          圆角阴影/44px 热区/100dvh/safe-bottom/动效三档/浅色对照）
    ├── tokens.ts              TOKEN_VARS 镜像 + readToken/deadlineTone/sideToken
    │                          （deadlineTone 由 app-core.ts re-export，唯一实现）
    ├── net.ts                 BpTransport 契约：WS（800ms×2^n 退避封顶 15s）/
    │                          invoke（bp_send_intent 直接回执 + bp-event 合流）
    ├── wireframe.css          三线框共用结构件，全 var() 引令牌
    ├── wireframe-host-console.html / wireframe-player-bp.html / wireframe-result.html
    ├── app-core.ts            运行时核心：serde 语义映射(ACTION_NAME/STATUS_NAME/
    │                          KIND_META/other/sideLabel) / joinStore(token 暂存) /
    │                          createStore（事件幂等 reducer + masks/forceReveal
    │                          派生与换 step 清空）/ toast / openSheet；
    │                          deadlineTone 自 ./tokens re-export；传输在 ./net
    ├── app-ui.ts              真实页渲染层：三角色同页（renderWaiting/Top/Lobby/
    │                          Progress/Result/Dock）+ 统一绑定层 bindAll +
    │                          倒计时 loop + 确认流 sheet（二次确认）+ 表情浮层 +
    │                          离线分享链接 + 曲库导入 sheet；import ./net + ./app-core
    ├── entry-index.ts         index.html 入口（自执行；Tauri → 控制台；浏览器 → 加入表单）
    ├── app-core.js / app-ui.js 【已废弃】打包链切换前产物，头部有 deprecated 标注，下轮删除
    ├── dist/                  esbuild 产物（build-frontend.cjs 生成，可重复）：app-bp.js
    │                          （= app-ui.ts bundle，暴露 { boot, renderAll }）+ app-index.js
    ├── app.css                布局与状态补充件（零硬编码视觉值，全令牌）
    ├── index.html             入口：module 引 ./dist/app-index.js（内联逻辑已抽出）
    └── bp.html                对局页壳（module 引 ./dist/app-bp.js）
```

行数为生成时实测（不含空行统计差异）。

### model.rs（64 行）
`Side(A/B)`、`BpAction(Ban/Pick)`、`BpStatus(Lobby/InProgress/Paused/Finished)`、`BpTurn{action,side,is_tb,step}`、`ParseError`。
**为什么单独一个文件**：`net/protocol` 需要 `BpAction/Side`，`bp/state_machine` 又需要 protocol 的消息体，直接互相引用会循环依赖；抽成无依赖叶子后形成无环链：`model → {interference, protocol} → state_machine`。

### net/protocol.rs（本轮 314 行，交付要求一：协议契约）
- `PROTOCOL_VERSION = 1`，`PlayerId = u32`。
- `ClientRole{Host,Player,Spectator}`；`ChartMeta{...status: ChartStatus}`；`ChartStatus{Available,Banned,Picked,ForcedBanCandidate}`（**Obscured 是纯渲染态，故意不进主机枚举**）。
- `PlayerState{id,name,role,side,ready,connected,interference_points,interference_disabled}`。
- `BpOperation{order_no,actor_side,action,chart_id,at_ms}`（order_no 全局自增，导出用）。
- `InterferenceEvent{id,kind,from_side,target_side,at_ms,params,countered,rolled_back,by_host}`。
- `TimerState{deadline_ts_ms,duration_ms}`（广播截止时间戳，客户端本地平滑渲染+校准）。
- `BpSnapshot`（重连/初进推全量：room_code/status/sequence/first_side/step_index/total_steps/current/players/operations/pool/interference_log/timer/interference 配置/locked）。
- `BpResult`（bans/picks/available_charts/interference_log → 分享卡导出）。
- **`ClientIntent`（C→S，`#[serde(tag="t", rename_all="camelCase")]`）**：`Join{room_code,player_name,role,side,token}` / `SetReady` / `SetRole` / `Ban{chart_id}` / `Pick{chart_id}` / `RevealCandidate{chart_id}`（被强制公开后目标方上报候选）/ `UseInterference{kind,target_side,obscure_chart_ids,forced_ban_chart_id}` / `CounterInterference{interference_id}` / `SendEmote{emote,text}` / `HostCommand{host_command}`（结构体变体承载，见 §4.1）/ `RequestSnapshot` / `Leave` / `Ping{at}`。
- **`HostCommand`（Host 专属，服务端二次校验）**：`StartBp` / `LockRoom{locked}` / `Pause` / `Resume` / `ArbitrateRollback{operation_order_no}` / `DisableInterference{side,disabled}` / `RollbackInterference{interference_id}` / `Kick{player_id}` / `SetPool{pool}` / `SetInterferenceConfig(cfg)`。
- **`HostEvent`（S→C）**：`Welcome` / `Snapshot(BpSnapshot)` / `PhaseChanged{step_index,total_steps,actor_side,action,decider,is_tb}` / `PoolUpdated` / `OperationConfirmed` / `OperationRejected{reason,code}` / `InterferenceApplied` / `InterferenceDenied` / `CounterApplied` / `TimerSync` / `PlayerConnected` / `PlayerDisconnected` / `PlayerReady` / `HostActionResult` / `Emote` / `SessionEnded(BpResult)` / `Notice` / `Error` / `Pong`。
  ⚠ 包结构体的新类型变体（Snapshot / OperationConfirmed / InterferenceApplied / SessionEnded / PlayerConnected）serde 会把结构体字段**拍平到顶层**；线上形状由 `protocol.rs::tests` 3 个契约测试锁定，前端 net.ts 已对齐（详见 DESIGN.md §2.1）。
- 传输约定：WebSocket **JSON 文本帧**（浏览器），不像 duel 用 `[u32 len][JSON]` TCP 帧；serde 风格（camelCase + tag="t"）与 duel 保持一致便于排错。

### bp/interference.rs（本轮 274 行，交付要求二：干扰系统）
- **`InterferenceKind`**：`CompressTimer`（压缩倒计时，同回合不可叠加）/ `ObscurePool`（遮蔽曲池，只影响渲染）/ `ForceReveal`（强制公开，候选待目标上报）/ `ExtraBanPressure`（追加 Ban 压力，指定强制 Ban 曲目）/ `Emote`（轻量社交层）。
- 每类的阶段门控默认值 `default_allowed_actions()`：CompressTimer/ObscurePool→Ban+Pick；ForceReveal→仅 Pick；ExtraBanPressure→仅 Ban；Emote→全程。
- **`InterferenceConfig`（可配置清单）**：`enabled` 总开关、`initial_points`（默认 3）、`max_strategic_per_round_per_side`（默认 1）、`counter_cost`（默认 1）、`allow_on_tb`（默认 false）、`enabled_kinds` 白名单、`CompressTimerConfig{reduce_secs:8, floor_secs:5}`、`ObscurePoolConfig{max_count:3}`、`SpectatorConfig{can_emote:true, emote_per_sec:2}`。全部主机持有下发，客户端禁止自行决定。
- `InterferenceParams`（tag="k"）：每类效果的具体参数，随事件广播给受影响客户端渲染。
- **`validate(facts,cfg)` 纯函数校验**（可单测）：全局开关→种类白名单→不能打自己→（Emote 到此放行）→发起方干扰权未禁用→TB 需 `allow_on_tb`→阶段匹配→点数够→同回合压缩未叠加→目标承受未达上限。拒绝枚举 `InterferenceDeny` 带 message。
- **`apply(facts,cfg)`**：算效果参数+扣点。压缩的 `reduce = min(reduce_secs, 剩余 - floor_secs)`；遮蔽 `count = min(请求, max_count)`。

### bp/state_machine.rs（本轮 1362 行：核心域逻辑 + 22 行为测试）
- `parse_sequence(seq, first)`（**验收契约，本轮已修**）：`b` = 双方各 Ban（1 step / 2 turn，先手方优先）；连续 `p` 段**每 2 个合并为 1 个 pick step**（双方各 Pick 一张，奇数收尾并入末段，`run.div_ceil(2)`）——"bppp" = 3 step / 6 turn、"bpbpp" = 8 turn；`t` = TB（先手方单 Pick，is_tb=true）。空序列/非法字符报 `ParseError`。
- `BpState`：权威状态。含每回合（step）计数：`strategic_received`（各方本回合承受次数）、`compress_used_this_round`、`current_forced_ban`、`pending_force_reveal`，**进入新 step 时全部重置**。
- `apply_operation(side,action,chart_id,now_ms)`：校验回合/动作/曲目可用 + 强制 Ban 位必须包含 → 改曲目状态 → push 操作（order_no 自增）→ 推进索引 → 跨 step 重置 → **InProgress 时重置计时**（每回合重新计时）；走完 turns → `Finished`。
- `try_interference(...)`：组装 `InterferenceFacts` → `validate` → `apply` 扣点 → 生成事件 → 维护每回合计数/副作用（CompressTimer 真扣 deadline、ExtraBanPressure 设强制位、ForceReveal 挂 pending）→ 入审计日志 → 返回事件供广播。
- `reveal_candidate(side,chart_id)`：补全 ForceReveal 挂起事件的候选曲目。
- `counter_interference(by_side,id)`：校验目标是自己+未反制过 → 扣 `counter_cost` 点 → 标记 countered。
- `legal_intent(sender,intent)`：**按状态粗校验**（非法返回 `IntentRejected`，协议层转错误事件）；细校验在 validate/apply_operation。各状态允许的意图表写在 protocol.rs 的 `ClientIntent` doc 注释里。
- `timeout_action()`：Ban 回合→`EmptyBan`（空 Ban 记超时）；Pick 回合→`RandomPick`（默认随机，`DeferToOpponent` 备选）。
- `build_snapshot()` / `build_result(now_ms)`；`start()/pause()/resume()`（resume 目前是**续算 deadline 不重置**，需求允许配置决定，待接配置项）；`arbitrate_rollback(order_no)`（**级联语义**：撤销目标及其后全部、`current_index = min(index, pos)` 精确对位、曲目恢复 + 强制位/挂起公开失效、返回时间序被撤数组；测试 3 条：最近一条回退 / 级联回退首条 / 回退后按 turns 重放终态自洽）。

## 3. 关键设计决策（为什么这么写）

1. **model.rs 叶子模块**：打破 net↔bp 循环依赖（见上）。
2. **serde 约定沿用 duel**：`tag="t"` internally-tagged + camelCase，抓包/日志直接可读。
3. **WS JSON 文本帧**：客户端主力是浏览器；duel 的长度前缀 TCP 帧不适用于 ws upgrade 后的 message 语义。
4. **干扰校验纯函数化**（validate/apply 不碰 IO）：单测友好；原子化（状态变更+日志+广播在同一把锁内）由调用方保证。
5. **遮蔽曲池不进主机曲目状态**：`Obscured` 只是目标客户端渲染态，主机只广播事件里的 chart_ids，本回合结束自动失效（不落 `ChartStatus`）。
6. **强制 Ban 的语义**：干扰方指定曲目 → `current_forced_ban`；目标 Ban 时 `chart_id` 必须 == 强制位，否则 `OpError::MustIncludeForcedBan`；Ban 成功即清除。注意：需求原文是「必须被包含」，若一回合多个 Ban 位需改为「包含集合」，当前实现按单 Ban 位。
7. **倒计时**：`TimerState` 只广播截止时间戳；`TimerSync` 定期推送（server 层实现，建议 250ms~1s）；客户端收到事件后校准、本地 rAF 平滑。
8. **暂停续算已接配置**：Pause 冻结、Resume 按 `resume_mode` 应用；配置项 `resume_timer_reset`（默认 false=按剩余续算，true=满时长重置），崩溃恢复同样遵循。

## 4. 已知坑 / 教训（写代码前先看）

### 4.1 本轮已修（记录，防再踩）

- `parse_sequence` 验收契约（第 99 行「bppp→6 turn」为准）：连续 `p` 段**每 2 个合并为 1 个 pick step**（双方各 Pick 一张，奇数收尾并入末段，`run.div_ceil(2)`）——"bppp" = 3 step / 6 turn、"bpbpp" = 8 turn、单独 "p" = 1 step；`b` = 双方各 Ban 一个 step；`t` = TB 单 turn。
- `apply_operation`：`ForcedBanCandidate` 在「强制位匹配 + 动作 Ban」时放行（强制位必须能被 Ban）；Ban 成功即清强制位并恢复其它候选。
- `try_interference` 饱和判定为**回合全局**：`strategic_received.values().max()`——本回合一旦有方达上限即封闭（交付用例语义，不是按目标方计数）。
- `execute_timeout` Pick 分支：有效选择先标 `ChartStatus::Picked` 再 push 操作；无效选择落 `chart_id = 0` 哨兵。
- **协议形状**（前端 net.ts 已同步对齐）：`ClientIntent::HostCommand(HostCommand)` 新类型嵌套同 `tag="t"` 会 duplicate field `t`（任何 JS 对象都造不出合法形状，命令永远进不来）→ 改结构体变体 `HostCommand { host_command }`，线上 `{"t":"hostCommand","hostCommand":{"t":"startBp"}}`；新类型变体事件（Snapshot / OperationConfirmed / InterferenceApplied / SessionEnded / PlayerConnected）serde 把结构体字段**拍平到顶层**——由 `protocol.rs::tests` 3 个契约测试锁定，改两端须同步。
- **前端回退 sheet 已解锁级联可点**：sheet 内每条操作可单点（级联影响数按全量 operations 计算，与展示截断无关——desc 序第 i 条其后恰 i 条）；按钮文案「回退最近操作」→「裁定回退」，头注释同步。
- **前端曲库导入**：`openPoolImportSheet` 快照校验（v/id≥1/去重/曲名非空/难度数值）→ 白名单 `{ id, title, artist, difficulty?, status: "available" }` 下发 `setPool`——ChartMeta serde 的 `status` 无 default，是**必填**，下发必须显式带（省略会反序列化失败）。
- **打包链切换的四个坑（本轮实测）**：① esbuild `target` 复合基线（`chrome92`+`safari13.1`）下**顶层 await 与解构拒绝降级**直接报 "not supported yet"（entry-index.ts 用了 TLA）——根因是源码语法超基线，改 `["es2022"]` 一把过；选基线前先盘源码语法。② `npm init -y` 生成的 `"type": "commonjs"` 会让 `node --check` 把 ESM 的 .js 按 CJS 解析报错——删掉 type 字段走 detect-module（工具脚本一律 .cjs 后缀不受影响）。③ bundle 入口 import 必须**无扩展名**（`./app-core`）——写 `./app-core.js` 会解析到已废弃的旧 JS 文件打出旧镜像。④ esbuild 能力校验只在构建时发生：`node --check` 校验**产物**语法有效，但类型正确性它俩都不管——严格 tsc 仍是独立缺口。
- **default-features 首次编译暴露的三处（本轮已修）**：① `commands.rs::bp_get_config` 对 `RwLock<Arc<BpHostConfig>>` 用 `(*guard).clone()`——解引用得 `Arc` 再 `.clone()` 是**克隆 Arc 本体**，类型不收敛到 `BpHostConfig`（E0308）；正解 `guard.as_ref().clone()`，且该写法改动会**连带改变同文件其他 `(*guard).clone()` 变量链的类型**（`bp_server_start` 的 `cfg`/`cfg_arc` 曾因此再报一次 E0308，改成 `Arc::new(config.clone())` 才稳定）。② `tauri::Builder::setup` 闭包借用外层 `svc` 超出 `'static`（E0373）→ 加 `move`。③ `BpState`/`Manager` unused imports（`App::handle()` 是固有方法，不需要 Manager trait）。
- **级联回退的哨兵坑（本轮已修）**：`arbitrate_rollback` 恢复曲目时**不要加 `chart_id != 0` 守卫**——测试 helper 曲库 id 是 0-based，加守卫会把「存档测试里 0 号曲被 Ban 再回退」的恢复路径整条封死（测试三连挂）。正确做法是靠 `pool.find(id)` 天然不命中处理哨兵（线上曲库 id 从 1 起，0 命中不了任何曲目）。
- `service.rs` 11 处无效 `drop(state)`（对 `&mut BpState` drop 是噪声）已清除；残留 3 处是真正的 `MutexGuard` 释放，勿动。
- `interference.rs` / `protocol.rs` 曾因 GBK 往返损坏，已用 Latin-1 视图提取 + UTF-8 直写重建（提取临时 `.latin1.txt` 已删）。

### 4.2 环境教训

- **360 保护树**：360 套件 minifilter 把 `D:\Phigros` 树设为保护区（ACL `Everyone:(DENY)(DC)` + 专属 DENY SID），树内启动的进程跨区加载 DLL 被静默拦截 → cl.exe `D8050 无法执行 c1.dll`。位置矩阵证实仅该树失败。解法：`src-tauri/.cargo/config.toml` 加 `[build] target-dir = 'D:\Environment\build\bp-host-target'`（外置）；旧 target 已删；`cargo clean -p <crate>` 才能强刷 registry 内 build.rs 的 mtime-only 缓存（libsqlite3-sys build.rs 已从 `.orig` 还原，勿再动）。
- **GBK 编码往返陷阱**：对带中文注释的 .rs 做 GBK decode→encode 会把 UTF-8 变 invalid。**永不再对源文件做编码往返**；批量改动用 Latin-1(28591) 视图（`28591` 编码读字节）提取 ASCII 结构，再 UTF-8 直写重建，API 字段以引用方构造点逐一核对。
- 重定向日志文件多为 UTF-16LE（255,254 头），读取需 `[System.Text.Encoding]::Unicode`；终端显示中文乱码是 GBK 伪影，不代表文件编码。
- **opencode agent 会话内 cargo→cl/lib 子进程链阻断（2026-10-25 第六轮，结论性，勿再重查）**：agent 会话里 `cargo check`（含独立最小工程 ccrepro 复现）必挂——cl 在 cargo 后代树中**编译成功却 exit 2 且零诊断**；cc-rs 增量归档步 lib.exe 恒定 `LNK1104 ……\Temp\lnk{GUID}.tmp`（响应文件建不出）；cargo 后代的写盘会被黑洞（exit 0 也不落盘）。已逐项排除：CARGO_MAKEFLAGS/jobserver、TMP/TEMP 重定向、INCLUDE/LIB 注入、stdio 三态、cwd、cmd 包裹、stable cargo 1.99.0；凡**多一跳**代理（Rust spawnany 包装转发）即一切正常，`schtasks` 脱离进程树被沙箱拒——缺陷靶向「cargo 直接后代」，与用户终端无关。**结论：会话外（vcvars64 环境）预期直接通过**；会话内替代证书 = `rustfmt --check --edition 2021` 整文件解析 + `node --check`。

### 4.3 仍存在的坑

- `arbitrate_rollback` 已从简化回退升级为**级联语义**（撤销目标及其后全部、`current_index = min(index, pos)` 精确对位、曲目恢复强制位失效、`order_no` 不回收）——不再是坑；残留工作仅前端回退 sheet 解锁（见 §5 任务 5 备注）。
- `chart_id = 0` 既是空操作哨兵也可能是真实曲目 id——曲库导入建议 id 从 1 起；要改哨兵语义需同步改 service 超时选择与导出渲染（测试已固化 0 = 哨兵）。
- `legal_intent` 是粗校验：UseInterference 只查 side/自指；阶段门控 / TB / 点数靠 `try_interference` 兜底转 `InterferenceDenied`——分层是有意的；两条拒绝路径（session.rs BAD_JSON 与 service err）都已发事件、不静默。
- `HostEvent::Welcome` 没带 `is_host` 字段（role 里已含 Host，够用；前端别再猜）。
- `ParseError`/`OpError`/`IntentRejected` 还没接 `thiserror`/`anyhow`（duel 用 anyhow，本 crate 暂零依赖设计）。
- default-features（Tauri 壳）已编译验证：icons + `bundle.icon` 已补，全量 `cargo check`（含 tauri / `generate_context!` / 9 命令 + bp-event 桥）exit 0 零警告。**NSIS 实机打包仍待真实构建环境验证**。
- **单方法级私有类型警告模式**：新 struct（如 `ServerHandle`）挂在 `pub struct` 的字段上会报 `more private than the item`——把字段本身收私有即可（同模块可访问，本 crate 无跨模块依赖）。

## 5. 下一步（按此顺序）

1. ✅ **前端真实页面接线**（本轮完成）：`index.html`（入口）→ `bp.html`（对局壳）→ `app-ui.js`（三角色渲染 + 意图流）+ `app-core.js`（net.ts/tokens.ts 无打包链运行时镜像）+ `app.css`。Tauri 侧已开 `withGlobalTauri`（纯静态页可调 `bp_send_intent`）。事件重复以**幂等 reducer** 吸收（invoke 直接回执与 `bp-event` 合流后按事件类型幂等合并，见 store.apply）；「(id, atMs) 去重」按需再加。真页语法已 `node --check` 全绿；浏览器交互走查（点选→确认→倒计时→干扰→结果）待真实环境验收。
2. ✅ **Tauri 壳可用化**（本轮完成）：`icons/icon.ico` 程序化生成（品牌蓝圆角方块 + 内菱形，16/32/48/64/256 五尺寸）+ `bundle.icon` 配置；default-features 全量 `cargo check` 三轮迭代清掉 E0308 / E0373 / unused imports 后 exit 0 零警告（教训见 §4.1）。
3. ✅ **结果页分享三件套**（任务 3 完结）：「复制文本结果」+「复制分享链接」（user 拍板**离线 dataURL**：`packBpLink()` 把 BpResult + 曲目标题压成紧凑 JSON v1 → URL-safe base64 → `bp.html#bp=…`，`boot()` hash 检测进**离线只读结果页**，损坏数据显式报错卡）+ **「导出图片」**（canvas 手绘版式 PNG：颜色/字号全 `readToken` 读 design-tokens.css 零硬编码视觉值、未加载显式报错；2x backing store 行构建-两遍绘制，长曲名 measure 截断；内容与 `resultPlainText` 同源 `resultData()`；干扰记录截断 8 条 + 共 N 条；`toBlob → a[download]`；离线分享页同源可导）。
4. ✅ **曲库导入器**（本轮完成）：`bp-host/tools/export-library.cjs`（Node，只读主工程 `data/charts/{download,custom}` 的 `info.yml`/`info.txt` 扫描，曲 id **从 1 起**避开 0 哨兵，排序规则使同目录内容 → 同 id）→ 产物 `bp-host/library-snapshot.json`（首份快照 10 首，覆盖下载谱与自定义谱）；Host 控制台大厅新增「导入曲库…」→ 快照校验（版本/id≥1/去重/曲名）→ 白名单字段构造 `setPool` 下发（前端大厅门控提示 + 主机 Lobby 拒绝兜底；`PoolUpdated` 广播为准）。**曲库扩充后重跑工具即可**；「未命名谱面」等本造谱器默认名不做过滤（快照保持完整由使用者取舍）。
5. ✅ **`arbitrate_rollback` 级联回退**（本轮落地）：撤销指定操作及其后全部（`split_off(pos)`）、曲目恢复 Available（空 Ban/Pick 哨兵靠 `pool.find` 不命中天然无害）、强制位 / 挂起公开 / 回合计数随回合重开失效、Finished 回升 InProgress 并重新计时、返回时间序被撤数组（目标在首）、`order_no` 全局自增不回收；service 公告升级为「主办方回退了操作 #N（含其后 M 条）」（M>0 时）。**前端 sheet 已同步解锁**（全历史可点 + 级联影响提示，见 §4.1）。
6. ✅ **`bp_server_stop` 优雅关闭**（本轮落地）：`serve_listener` 增停机信号参数 + `with_graceful_shutdown`；`AppCtx.server_handle` 持 `oneshot::Sender` + `JoinHandle`；stop 发信号后守 5s 超时（优雅退出 / 任务提前退出 / 超时分离三态收口）；smoke 测试 `graceful_shutdown_stops_accepting` 锁定「发信号后 listener 拒新连接且 serve 正常返回」。timer ticker 生命周期不变（停机仅停监听，空闲状态不产生事件）。
7. ✅ **打包链切换**（本轮完成）：**esbuild** bundle（bp-host npm 工作区，`npm run build:frontend`）；`app-core.js → app-core.ts`（删传输实现段，传输与令牌改自 `./net` / `./tokens` re-export）、`app-ui.js → app-ui.ts`（import 改源、功能全同——含分享链接/曲库导入/级联回退 sheet）、`index.html` 内联逻辑抽出为 `entry-index.ts`；`tools/build-frontend.cjs`（bundle、format esm、target es2022、minify/sourcemap 关）→ `dist/app-bp.js`（暴露 `{ boot, renderAll }`）+ `dist/app-index.js`（自执行），html 已切引产物、产物 `node --check` 绿且字节级可重复。**旧 app-core.js/app-ui.js 头部标 deprecated 暂存档，浏览器验收后下轮删除**；严格 tsc 属后续任务（esbuild 只转译，踩坑见 §4.1）。
8. ✅ **phirc 客户端内置连接入口**（第六轮，phira 侧交付；详录 `../webui_handoff.md` §1.1 / §2 / §9）：`phira/src/webui.rs` 新增 `BP_NAV_JS` 注入——所有 WebView 页左下 52px「BP」浮动圆钮 → `location.href="bpHost.html"`（**唯一合规挂点**：主工程三页禁改、`tools.html` 一行不改，只能走注入脚本；守卫 = 幂等 + `phirc:` 协议 + 排除自身，挂 `init_scripts_base` 两份 cfg 分支）。同日审查修**层叠致命 bug**：开始页 `HOTSPOT_JS` 全屏触点原压 BP 钮 → BP 钮 z-index 升至同级（DOM 后挂居上）+ click stopPropagation。bpHost.html（20.0KB）：地址簿留 5 展 3（**自动测延迟角标**）+ `no-cors` 探测 **3s 超时 + RTT** 不拦截 + **右上角宿主信标实时徽章**（对局回合/本地倒计时/断线灰显——数据来自本仓前端 `beacon.ts`，仅嵌入时上报）+ ✕/Esc 返回 + iframe load/8s 看门狗反馈。**本仓改动仅前端**：`beacon.ts`（新）+ `app-core.ts`·`app-ui.ts`·`entry-index.ts` 各几行接线，dist 已重建（app-bp 69,000 B / app-index 5,590 B，node --check 双绿）；服务器与协议零改动。改 HTML 免重编；`webui.rs` 待会话外 `cargo check -p phira`（rustfmt 整文件解析已过）。
9. ✅ **选曲确认 → 正常游玩 + 连续游玩闭环**（第七轮，同日；server/协议/phira-rs **零改动**）：主机广播 `operationConfirmed(pick)` 后，bp-host 前端（beacon.ts 新增 `playIntent`）经 postMessage 反向通道 `phirc-bp-play` 通知顶层 bpHost.html；顶层复用**原生既有桥**发起游玩——本机曲库 `listCharts` 按「曲名精确 + 难度就近」命中 → `phirc.startGame(path)`（SongScene auto-launch 直进谱面，webui.rs L2244/L684 既有）；未命中 → `phirc.startOnlineChart(id)`（onlineId 来自主办方把 `library-snapshot.json` 拷进托管前端目录，beacon 同源 fetch 按 title 补全 `download/<id>`；must-login/offline-mode/TOS 失败逐字交底）；都失败显式报错不静默。**连续游玩**靠原生 handoff 语义闭环：桥开局置 `WEBVIEW_HANDOFF`，一局打完回 MainScene 时 `scene/main.rs` L553-562 `take_webview_handoff()` → `show()` **只显示不导航** → bpHost.html 连同 iframe 大厅状态全保留 → 下一首 Pick 确认再来一局（`start_game` 注释原话即此场景）；顶层去重按 chartId。观战只 toast 不拉进。dist 重建 app-bp 70,375 B / app-index 5,590 B node --check 双绿；真机验收项见 USAGE §4 与 `../webui_handoff.md` §9。
10. 延后：`anyhow`/`thiserror` 接入（零依赖设计暂保留）。

任务对照（第 5 节原列表 → 现状）：工程骨架(任务#1)✅、net/server+session(任务#5)✅（含优雅停机）、sqlite✅、commands✅、config✅、单测✅(26/26)、设计令牌(任务#6)✅、三线框(任务#7)✅、DESIGN.md(任务#8)✅、net.ts✅、**真实页面(任务#9)✅（交互走查待真实环境）**、**结果页分享三件套(任务#3)✅（文本/链接/导出图片均落地）**、**曲库导入器(任务#4)✅（工具 + Host UI）**、**打包链切换(任务#7)✅（esbuild bundle；严格 tsc 后续）**；剩余：NSIS 实机打包、严格 tsc（esbuild 只转译）、phirc 入口真机走查、anyhow/thiserror 延后。

## 6. 硬约束（必须遵守，沿袭项目全局规则 + 需求明示）

- **不 commit/push；不启动程序、不截图；不改 `rev\phira-pc\data\` 用户存档。**
- 所有网络地址/端口从配置读取，**禁止硬编码**；远程公网非默认能力，UI 给「端口转发/中继说明」入口。
- 客户端**不做乐观状态变更**、不本地推进回合、不自行扣减资源；非法意图**必须返回错误事件，不静默忽略**。
- 计时以主机为准；后台/切页不影响主机计时精度。
- 断线标记时间线但**不自动暂停 BP**（除非主办方手动）。
- 移动端触控底线：热区 ≥44×44px、`touch-action: manipulation`、`100dvh`（非 100vh）、底部 `env(safe-area-inset-bottom)`、Bottom Sheet 确认（二次确认防误触）、**BP 进行中禁止模态框**（确认用 Bottom Sheet，提示用 toast）。
- WebView 兼容基线：Chrome Android / Safari iOS 最近两个大版本；别用仅桌面 Chromium 的 API。
- 主工程禁改页面规则照旧：`library.html` / `index.html` / `tripsUide.html` 一律不动。

## 7. 需求锚点（浓缩，细节以原文为准）

- **网络**：主机即服务器（axum+WS+mDNS，房间码 4–6 位 PIN+二维码）；会话 ID 绑身份；重连推 `BpSnapshot`；锁定房间；断线不自动暂停。
- **BP**：可配置回合序列字符串（`bppp`/`bpbpp`/`t`…，b=双方各 Ban、p=双方各 Pick、t=TB）；A/B+先手固定；操作不可逆（裁定回退除外，留日志）；每回合可配置倒计时（默认 30s），超时：Ban→空 Ban、Pick→随机或判对手；结束生成结构化结果（Ban/Pick 列表+顺序号+最终可用集+干扰记录），可导出图片/复制链接。
- **干扰**：点数制（默认 2–3，不回复）；类型=压缩倒计时(−8s，下限 5s，同回合不叠加)/遮蔽曲池(渲染层，本回合结束失效)/强制公开(候选不可撤回可改选)/追加 Ban 压力(强制 Ban 位)/表情弹幕(轻量，可静音)；被干扰方可 1 点反制；每回合每方最多承受 1 次策略干扰；TB 默认禁用；全部主机原子化执行+广播+进审计；主办方有「禁用干扰权」「回滚最近干扰」裁定能力。
- **UI**：Phira 蓝、二次元简约；当前回合卡（谁/Ban-Pick/倒计时数字+环形进度）第一视觉权重，最后 10s 数字与环转琥珀/红（只变色不整屏闪）；干扰反馈克制（脉冲+−8s 标签、毛玻璃占位、边缘弹幕 2s 淡出不盖操作区）；唯一存在感动效=回合切换边框高亮迁移 150–200ms；Ban/Pick 120ms 反馈。
- **技术**：模块划分 `net/{server,protocol,session}` + `bp/{state_machine,interference}` + `persistence/sqlite.rs` + `commands.rs`（invoke 与 WS 共 service 层）；前端同构（host/player/spectator 角色参数）；SQLite 持久化+崩溃恢复。

## 8. 快速上手命令

```bash
# 源码总览（后端 src-tauri/src + 前端 src/frontend + DESIGN.md）
code D:\Phigros\PhiRa-Mod-Meew-11\bp-host

# 校验 / 测试（本 crate 独立 workspace，别在主工程根目录跑）
cd D:\Phigros\PhiRa-Mod-Meew-11\bp-host\src-tauri
CARGO_INCREMENTAL=0 cargo check --no-default-features
CARGO_INCREMENTAL=0 cargo test  --no-default-features
# 含 tauri 壳的全量检查（桌面应用形态，generate_context! 需要 tauri.conf.json + icons）
CARGO_INCREMENTAL=0 cargo check
```

- MSVC 环境先加载 `vcvars64.bat`（本轮 helper 脚本与日志在 `C:\Users\Administrator\AppData\Local\Temp\opencode\ccdbg\`，属临时产物，环境刷新后自建即可）。
- 工具链约定沿用主项目：`CARGO_HOME=D:\Environment\Rust\cargo` / `RUSTUP_HOME=D:\Environment\Rust\rustup`；D 盘构建必须带 `CARGO_INCREMENTAL=0`。
- **构建产物目录已外置**：`D:\Environment\build\bp-host-target`（`.cargo/config.toml` 配置，规避 360 保护树，见 §4.2）。
- 线框可视化：浏览器直接打开 `src/frontend/wireframe-*.html`（纯静态，无服务依赖；勿在本仓库跑服务进程验收）。
