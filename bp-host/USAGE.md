# bp-host 使用手册

> 2026-10-25 定稿（打包链切换 + 结果分享三件套之后）。写给主办人 / 接手维护者。
> 形态总定位：**主机单文件开服；选手在 phira 客户端（phirc）左下「BP」钮或浏览器打开主机地址加入**；普通浏览器开同一地址是等价兜底。

## 0. 当前可用状态

- 后端：`cargo check` / `cargo test`（26 用例）全绿零警告。
- 前端：esbuild 产物 `dist/app-bp.js` + `dist/app-index.js` 语法校验全绿、可重复构建。
- **未做**：NSIS 实机打包；真机端到端走查（办赛首跑前务必过第 7 节清单）。
- 发现方式：**手动告知 IP**（mDNS 是需求锚点中的待办，未实现）。

## 1. 三件套与配置（standalone 主线形态）

主机 = 三个文件放一起，双击 exe 即开服：

```
bp-host/
├── bp-host.exe            ← cargo build --no-default-features --release 的单文件服务器
├── bp-host.config.json     ← 从 config.example.json 拷贝修改
└── frontend/               ← 前端产物（src/frontend/dist 所在树，或按 config.frontend_dir 指定）
```

- 工作目录即数据落点：`bp-host.db`（SQLite 持久化 + 崩溃恢复）建在配置文件旁。
- 缺配置文件**直接退出并给指引**（standalone 不用隐式默认值——拷贝 example 是唯一正统起点）。

配置常用项（缺字段容忍，但端口非 0 等硬约束不降级）：

| 字段 | 默认 | 含义 |
|---|---|---|
| `httpBindAddr` / `httpPort` | `0.0.0.0` / `7878` | 服务器监听 |
| `roomCode` / `roomCodeDigits` | 空 / 6 | 固定房码 / 随机位数 |
| `sequence` | `"bppppt"` | 回合序列（b=双方各 Ban，p=双方各 Pick，t=TB） |
| `firstSide` | `a` | 先手方 |
| `turnTimeoutMs` | 30000 | 单回合倒计时 |
| `timerSyncIntervalMs` | 500 | 计时广播间隔 |
| `resumeTimerReset` | false | 崩溃恢复：false=剩余时间续算，true=满时长重置 |
| `timeoutPick` | RandomPick | Pick 超时随机替选（可配判对手方） |
| `autoRecover` | true | 同房码未完结会话重启后恢复 |
| `publicHintUrl` | 无 | 给选手的公网提示地址（纯局域网可不填） |
| `interference` | 见下表 | 干扰系统参数（主机持有，下发全端） |

干扰配置（控制台卡片展示真实值，一切以主机配置为准）：初始点数默认 3（clamp 0–9）、反制消耗 1 点、每回合每方策略干扰上限默认 1、TB 默认禁用。

### 1.1 Tauri 壳形态（可选，非必需）

`cargo build`（default 特性）= 桌面 App：本机 GUI 控制台 + `bp_server_start/stop`（幂等启动 / 5s 优雅停机）管理**同一** service 层与数据库。适合主办方想要本机窗口的场景；与 standalone 不能同时跑（同房码数据库要注意锁定语义）。

## 2. 主办方流程（开赛 SOP）

1. 双击 standalone exe。控制台日志输出：`[bp-host] 房间 <房码> 监听 0.0.0.0:7878`。
2. 本机浏览器开 `http://localhost:7878` → 确认显示「加入房间/主机控制台」页（服务器托管同一份前端）。
   - **进入控制台**：直接开 `bp.html?role=host` 或入口页选择；大厅等待选手就位。
3. 大厅卡片显示：房码、序列、双方席位（未占用=虚位以待）、干扰配置摘要。
4. **导入曲库**：点「导入曲库…」→ 选 `library-snapshot.json` → 预览前 8 首 → 「下发曲池」。
   - 快照生成：`npm run export:library`（或 `node tools/export-library.cjs`）——**只读**主工程 `data/charts/{download,custom}`；曲 id 从 1 起避开 0 哨兵；同目录内容 → 同 id（可重复生成）。曲库扩充后重跑一遍。
   - 替换曲池仅大厅可用；赛中下发会被主机显式拒绝。
5. 等 A/B 选手就绪（可选先「锁定房间」挡新加入）→ 「开始 BP」。
   - 可全局关闭干扰（配置层）；或对个别选手「禁干扰权」（大厅面板）。

## 3. 选手入口（phira 客户端内置 BP 钮）

客户端 WebView 所有页面左下角有一个 52px「BP」浮动圆钮（`phira/src/webui.rs` 注入 `BP_NAV_JS`，10-25 新增）→ 点开 `bpHost.html`：

1. 地址簿：localStorage `phirc_bp_addrs`（记最近 5 条、chip 展示前 3 条，**进入页面会自动测延迟**，chip 上标 `· 45ms` / `· 不可达`）；首次手输 `主机IP:端口`——**只收 `host:port`**（端口数值 1–65535，域名与 IPv6 方括号允许）。
2. 「测试连接」＝ `no-cors` 探测（**3s 超时**，附 RTT 显示）；**探测失败不拦截跳转**（误伤率高的检查不该挡路），直接「连接进入」。
3. 进入后 iframe 全屏加载 `http://<addr>/index.html`——与 bp-host 前端**同源**：`/api/status` 房码自动回填、`/ws` 原生可用；之后的对局操作见第 4 节「选手」。
4. **右上角实时徽章（宿主信标）**：嵌在 phirc 里时，对局页会把大厅在线数 / 当前回合 / 本地倒计时 / 断线重连经 postMessage 上报给本页常显——主办方关服或断线时选手**即使在嵌入页里也能一眼看到**（>7s 无心跳灰显「信号中断」）。
5. 退出：左上 ✕ 常驻返回（`history.back()` 兜底回 `index.html`）；Esc 双态分流——嵌入态退出嵌入、面板态放行宿主。

- 主机地址**手动告知**（微信群 / 口头均可，见第 0 节）；mDNS 自动发现是需求待办，未实现。
- `bpHost.html` 改动**免重编**（Web 根开发目录）；`BP_NAV_JS` 在 `webui.rs` 内，改动需重编主工程 exe。

## 4. BP 进行中

**选手（phira 客户端内嵌 WebView / 浏览器）**：
- 开 `http://<主机IP>:7878` → 房码自动填（来自同源 `/api/status`，也可手输）→ 昵称 ≤16 字 → 选 **A 选手 / B 选手 / 观战** → 「加入」→ 「我已就绪」。
- 轮到自己：点曲池曲目 → **Bottom Sheet 二次确认** Ban/Pick（120ms 反馈；非我方回合/遮蔽曲目点击有分流提示）。
- **选曲确认 → 自动进正常游玩（10-25 新增，连续游玩闭环）**：Pick 被主机确认后，嵌入态下 bpHost 会自动替你开局——本机曲库按「曲名精确 + 难度就近」命中 → `phirc.startGame(path)` 直进谱面（跳过信息页）；未命中且主办方放了 `library-snapshot.json`（拷进托管前端目录即可，零代码）→ 在线谱 `phirc.startOnlineChart` 现下现玩；再不行就**明说原因**（未下载 / 离线模式 / 未登录），绝不静默。一局打完自动回到 bpHost 页（含 iframe 大厅状态全保留——原生 `WEBVIEW_HANDOFF` 语义），主机确认下一首 Pick 就再开一局。观战只提示不拉进游玩。
- **干扰**（花 1 点）：压缩倒计时（−8s 下限 5s，同回合不叠加）/ 遮蔽曲池（仅本回合）/ 强制公开（对方 Pick 前须公开候选，可改选）/ 追加 Ban 压力 / 表情弹幕（轻量可静音）。
- **被干扰且点数 ≥1**：可反制（−1 点，取消该效果）。
- 断线重连：自动带 token 恢复身份；错过事件由全量快照补齐；**断线不自动暂停 BP**（主办方手动）。

**主办方（随时可做）**：
- **暂停 / 恢复**（冻结倒计时；恢复默认按剩余时间续算）。
- **「裁定回退…」**：全历史操作任意点选，**级联撤销**该条及其后全部（曲目复位、强制位失效、回合重新计时）；sheet 提示「将同时撤销其后 N 条」；`order_no` 不回收，公告写明。
- 踢人（二次确认）、逐人禁/恢复干扰权。
- TB 回合双 Ban 后进决胜 Pick（`sequence` 含 `t` 时）。

**观众**：同页只读视角，曲池状态实时可见，可发表情弹幕（可被静音）。

## 5. 结束与分享（结果页三件套，数据同源）

- **复制文本结果**：纯文本摘要。
- **复制分享链接**：`bp.html#bp=<base64>` 离线 dataURL——发群里，对方不开服务器、甚至没有主机在线也能打开**只读结果页**。
- **导出图片**：PNG 结果卡（双方 Pick 含玩家名、Ban 记录、干扰记录前 8 条、可用曲统计）。
- 任一入口页失去主机连接时以快照兜底渲染，不白屏。

## 6. 停机与恢复

- standalone：直接关进程（环境无优雅信号通道时退化为硬断；**在 Tauri 形态才有 5s 优雅停机**——本质是 `bp_server_stop` 命令收口）。
- 同房码未完结会话下次启动自动恢复；倒计时按 `resumeTimerReset` 策略；主办方可用裁定回退/暂停收尾烂局。
- 数据全部在 `bp-host.db`，换机迁移拷走即可。

## 7. 首次办赛 10 分钟自查清单

1. `node tools/build-frontend.cjs` 重跑（产物可重复构建，确认 dist 存在且为最新）。
2. 拷贝 `config.example.json` → `bp-host.config.json`，改端口/房码/序列，重开服确认日志行正确。
3. `npm run export:library` 出快照 → 开服 → 导入曲库 → 曲池显示 10 首。
4. phira 客户端（左下「BP」钮进入，见第 3 节）或备用浏览器分别以 A / B / 观战各走一遍：就绪 → Ban/Pick 二次确认 → 干扰 + 反制 → 暂停恢复 → 裁定回退（级联提示出现）→ 结束 → 三件套逐一验证（文本/离线链接在另一台设备打开/PNG 落盘）。
5. 验收通过后删除 `src/frontend/` 内已标 deprecated 的 `app-core.js`、`app-ui.js`（纯存档）。

## 附：构建命令速查

| 命令 | 产出 |
|---|---|
| `cargo build --no-default-features --release`（在 `src-tauri/`） | standalone 单文件服务器（本手册主线） |
| `cargo build`（default 特性） | Tauri 桌面壳 |
| `cargo test --no-default-features` | 26 用例（含级联回退 ×2 + 停机 smoke） |
| `npm run build:frontend` | 重产 `src/frontend/dist/{app-bp.js, app-index.js}` |
| `npm run export:library` | 重产 `library-snapshot.json`（只读主工程曲库） |

> 工程细节 / 架构 / 协议契约见 `DESIGN.md`；开发接手见 `../bp-host_handoff.md`。
