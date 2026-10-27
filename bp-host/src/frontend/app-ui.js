/**
 * app-ui.js ——【已废弃｜deprecated】打包链切换（handoff §5 任务 7）后 dist 产物生效，
 * 源头改为 app-ui.ts（import ./net + ./app-core）；本 JS 仅存档参照，下一轮删除。
 * 原身份：BP 页渲染层（host / player / spectator 三角色同页）
 *
 * 分层：app-core.js 提供 store / transport / toast / sheet 助手；本文件只做
 * 「状态 → DOM」与「交互 → 意图」。契约与 net.ts / tokens.ts（app-core.js 镜像）
 * 逐字段一致；事件形状以 protocol.rs serde 输出为准（含拍平的新类型变体事件）。
 *
 * 硬约束（handoff 第 6 节）：
 * - 按钮只发意图；**渲染与状态只来自主机事件**（OperationConfirmed 先行标色
 *   只是「主机已确认信息」的展示，不是乐观推进）；
 * - 非法意图一律经过事件流（Error / OperationRejected / InterferenceDenied）
 *   显式回显，不静默；
 * - 移动端底线：Bottom Sheet（≤40dvh + 二次确认）承载确认流，BP 进行中不使用
 *   模态框，提示走 toast；
 * - 主机裁定约束忠实呈现：仲裁回退全历史可点（state_machine.rs 已支持
 *   **级联回退**——撤销目标及其后全部操作；sheet 内展示级联影响提示）。
 */
import {
  ACTION_NAME, KIND_META, STATUS_NAME, other, sideLabel, deadlineTone,
  createWsTransport, createInvokeTransport, loadJoin, saveJoin,
  createStore, el, qs, qsa, toast, openSheet,
} from "./app-core.js";

/* ------------------------------------------------------------------ 引导 */
const params = new URLSearchParams(location.search);
const isTauri = Boolean(window.__TAURI__?.core?.invoke);
let role = params.get("role") || loadJoin().role || null;

const store = createStore();
let transport = null;
let meSide = null;       // join 意图侧（快照后以 meRow 为准）
let connState = "connecting"; // 连接态供顶栏 chip 渲染（WS 状态流唯一来源）

/** 组装意图并上行（唯一出口；不等待结果，结果走事件流） */
const send = (intent) => transport?.send(intent);
const hostCmd = (t, extra = {}) =>
  send({ t: "hostCommand", hostCommand: { t, ...extra } });

/** bp.html module 入口 */
export async function boot() {
  // 分享链接打开：#bp=… → 离线只读结果页（零传输；文件直开也支持）
  const shared = new URLSearchParams(location.hash.slice(1)).get("bp");
  if (shared) { bootSharedResult(shared); return; }
  if (!isTauri && !location.protocol.startsWith("http")) {
    // 文件直开：无主机可连 → 引导去入口页
    location.replace("index.html");
    return;
  }
  const j = loadJoin();
  role = role || j.role;
  if (!role) { location.replace("index.html"); return; }
  meSide = j.side || null;

  buildSkeleton();
  window.addEventListener("pagehide", () => {
    if (cdTimer) clearInterval(cdTimer);
    transport?.close();
  });

  transport = isTauri
    ? createInvokeTransport(window.__TAURI__)
    : createWsTransport(wsUrl());
  transport.onEvent(handleFlowEvent);
  mountConnectionState();

  if (isTauri) await prepareHostConsole();
  await join();
}

/** WS 端点：同源推导（scheme 匹配 http/https → ws/wss），无任何硬编码地址 */
function wsUrl() {
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${location.host}/ws`;
}

/* ------------------------------------------------------- 事件流分流 */
/** join 在途收到即判失败的关键错误（浏览器端退回入口页） */
const FATAL_JOIN_CODES = new Set([
  "ROOM_CODE_MISMATCH", "SIDE_OCCUPIED", "HOST_SEAT_TAKEN",
  "BP_ALREADY_RUNNING", "IDENTITY_LOST",
]);

function handleFlowEvent(ev) {
  if (!ev || typeof ev !== "object") return;
  const s = store.state;
  switch (ev.t) {
    case "welcome":
      store.apply(ev);
      s.me.side = meSide ?? s.me.side;
      if (s.joining) { store.endJoin(); toast("已加入房间", "info"); }
      break;
    case "snapshot":
      store.apply(ev);
      if (s.me.side == null && s.me.role === "player") {
        const row = (s.snap.players || []).find((p) => p.id === s.me.id);
        if (row?.side) { s.me.side = row.side; meSide = row.side; saveJoin({ side: row.side }); }
      }
      store.endJoin();
      break;
    case "error": {
      if (s.joining) {
        // join 在途失败：显式呈现；浏览器端致命错误退回入口，Tauri 站内可重试
        s.joining = false;
        s.joinError = ev.message;
        toast(ev.message, "error");
        if (!isTauri && FATAL_JOIN_CODES.has(ev.code)) {
          setTimeout(() => { location.replace("index.html"); }, 900);
          return;
        }
        break;
      }
      toast(`[${ev.code}] ${ev.message}`, "error");
      break;
    }
    case "operationRejected":
      toast(ev.reason, "error");
      break;
    case "interferenceDenied":
      toast(ev.reason, "error");
      break;
    case "interferenceApplied": {
      const targetIsMe = s.me.side && ev.targetSide === s.me.side;
      const fromIsMe = s.me.side && ev.fromSide === s.me.side;
      const kname = KIND_META[ev.kind]?.name ?? ev.kind;
      if (targetIsMe && ev.kind === "forceReveal") {
        // 被要求公开候选：直接展开候选 sheet（必须 Pick 前先公开）
        openRevealSheet(ev.id);
        toast("对方要求你先公开候选，请选择意向曲目", "warn");
      } else if (targetIsMe && ev.kind === "extraBanPressure") {
        toast(`追加 Ban 压力：你的禁用回合被指定了强制位`, "warn");
      } else if (targetIsMe && ev.kind === "obscurePool") {
        toast(`曲池被遮蔽（${ev.params?.chartIds?.length ?? "?"} 首不可见），可反制`, "warn");
      } else if (targetIsMe) {
        toast(`你被「${kname}」干扰（可反制）`, "warn");
      } else if (fromIsMe) {
        toast(`「${kname}」已生效（主机已执行）`, "info");
      } else {
        toast(`${sideLabel(ev.fromSide)} → ${sideLabel(ev.targetSide)}「${kname}」`, "info");
      }
      store.apply(ev);
      break;
    }
    case "counterApplied":
      toast(`${sideLabel(ev.bySide)} 反制成功`, "info");
      store.apply(ev);
      break;
    case "sessionEnded":
      toast("BP 已结束", "info");
      store.apply(ev);
      break;
    case "hostActionResult":
      toast(ev.message, ev.ok ? "info" : "error");
      break;
    case "notice":
      toast(ev.message, "info");
      break;
    case "emote":
      fxEmote(ev);
      break; // 轻互动不触发整体重渲染
    case "pong":
      return; // 心跳回包：不渲染
    case "playerConnected":
      store.apply(ev);
      if (!s.joining && s.me.id && ev.id !== s.me.id) toast(`${ev.name ?? "玩家"} 已连接`, "info");
      break;
    default:
      store.apply(ev);
  }
  renderAll();
}

/** Tauri 控制台准备：服务器确保已启动（幂等；计时 ticker 随服务器启动） */
async function prepareHostConsole() {
  try {
    const st = await window.__TAURI__.core.invoke("bp_server_start");
    if (!st.running) toast(`服务器未运行：${st.error ?? "未知状态"}`, "warn");
  } catch (e) {
    toast(`服务器启动失败:${String(e)}`, "error");
  }
}

/** 上行 join（重连自动带 token 恢复身份；失败经 error 事件显式回送） */
async function join() {
  const j = loadJoin();
  let roomCode = j.roomCode || "";
  if (isTauri) {
    try {
      const hint = await window.__TAURI__.core.invoke("bp_join_hint");
      roomCode = hint.roomCode || hint.configRoomCode || roomCode;
    } catch { /* hint 失败不阻塞 join；房码以暂存为准 */ }
  }
  if (!roomCode && !isTauri) {
    // 浏览器端：同源 /api/status 兜底（地址即当前 origin，无硬编码）
    try {
      const res = await fetch("/api/status");
      if (res.ok) roomCode = (await res.json()).roomCode || roomCode;
    } catch { /* 保持手动输入值 */ }
  }
  saveJoin({ role, roomCode });
  store.beginJoin();
  store.state.joinError = null;
  send({
    t: "join",
    roomCode,
    playerName: j.name || (role === "host" ? "主机控制台" : "访客"),
    role,
    side: role === "player" ? (meSide ?? null) : null,
    token: j.token ?? null,
  });
}

/* ------------------------------------------------------- 骨架与连接态 */
function buildSkeleton() {
  document.body.classList.add("app-plain");
  qs("#stage").innerHTML = `
    <header class="app-top" id="top"></header>
    <main class="page-body" id="pagebody"></main>
    <div class="ops-dock" id="dock"></div>`;
}

/** 订阅一次连接态（invoke 传输无断线概念：直接视为已连接） */
function mountConnectionState() {
  const NAMES = { connecting: "连接中…", open: "已连接", reconnecting: "重连中…", closed: "已断开" };
  connState = isTauri ? "open" : "connecting";
  if (!isTauri) transport?.onStateChange?.((st) => { connState = st; renderAll(); });
}

/* ------------------------------------------------------- 渲染主入口 */
export function renderAll() {
  const s = store.state;
  if (!s.snap) { renderWaiting(); return; }
  const meRow = (s.snap.players || []).find((p) => p.id === s.me.id) || null;
  renderTop(meRow);
  if (s.finished || s.snap.status === "finished") {
    renderResult(meRow);
    renderDock(meRow);
    return;
  }
  (s.snap.status === "inProgress" || s.snap.status === "paused")
    ? renderProgress(meRow)
    : renderLobby(meRow);
  renderDock(meRow);
}

function renderWaiting() {
  const s = store.state;
  qs("#top").innerHTML = `<strong style="font-size:var(--fz-lg)">正在加入…</strong>
    <span class="badge" id="conn-chip" data-tone="${connState === "open" ? "pick" : "timeout"}">${connName()}</span>`;
  qs("#pagebody").innerHTML = `
    <div class="card loading-card">
      <p class="card-title">${s.joinError ? "加入失败" : "正在加入房间…"}</p>
      <p class="card-subtitle">${s.joinError ? escapeHtml(s.joinError) : "等待主机事件（快照即唯一事实源）"}</p>
      ${s.joinError ? `
        <div class="loading-actions">
          <button class="btn" variant="primary" id="retry-btn">重试加入</button>
          <button class="btn" variant="ghost" id="back-index">返回入口</button>
        </div>` : ""}
    </div>`;
  if (s.joinError) {
    qs("#retry-btn").onclick = () => join();
    qs("#back-index").onclick = () => { send({ t: "leave" }); location.replace("index.html"); };
  }
  qs("#dock").innerHTML = "";
}

function connName() {
  return { connecting: "连接中…", open: "已连接", reconnecting: "重连中…", closed: "已断开" }[connState] ?? connState;
}

/* ---------------------------------------------------------------- 顶栏 */
function renderTop(meRow) {
  const snap = store.state.snap;
  const chips = [
    `<strong style="font-size:var(--fz-lg)">房间 ${escapeHtml(snap.roomCode)}</strong>`,
    `<span class="badge">${STATUS_NAME[snap.status] ?? snap.status}</span>`,
    `<span class="badge" id="conn-chip" data-tone="${connState === "open" ? "pick" : "timeout"}">${connName()}</span>`,
  ];
  if (snap.locked) chips.push(`<span class="badge" sem="timeout">已锁房</span>`);
  if (role === "host") chips.push(`<span class="badge">控制台 · 主办方</span>`);
  else if (meRow?.side) chips.push(`<span class="badge" ${sideAttr(meRow.side)}>我 = ${sideLabel(meRow.side)}</span>`);
  else chips.push(`<span class="badge" role="spectator">观众视角</span>`);
  qs("#top").innerHTML = chips.join("");
}

/** 空值属性约定（wireframe.css [A-side]/[B-side] 匹配器） */
function sideAttr(side) {
  return side === "a" ? "A-side" : "B-side";
}

/* ---------------------------------------------------------------- 大厅 */
function renderLobby(meRow) {
  const snap = store.state.snap;
  const a = snap.players.find((p) => p.side === "a" && p.role === "player");
  const b = snap.players.find((p) => p.side === "b" && p.role === "player");
  const seatCard = (row, side) => `
    <article class="card seat-card" ${sideAttr(side)}>
      <span class="badge" ${sideAttr(side)}>${sideLabel(side)} 方</span>
      <strong>${row ? escapeHtml(row.name) : "虚位以待"}</strong>
      ${row ? `<span class="badge" sem="${row.ready ? "pick" : "timeout"}">${row.ready ? "已就绪" : "未就绪"}</span>` : ""}
      <span class="card-subtitle">${row ? (row.connected ? "在线" : "离线") : "等待加入"}</span>
    </article>`;
  const mine = meRow && meRow.role === "player";
  const myReady = Boolean(meRow?.ready);
  qs("#pagebody").innerHTML = `
    <section class="lobby">
      <article class="turn-card">
        <h2 class="card-title">${mine && !myReady ? "请就位并准备" : "等待开始"}</h2>
        <p class="card-subtitle">序列 ${escapeHtml(snap.sequence)} · 先手 ${sideLabel(snap.firstSide)} 方 · 共 ${snap.totalSteps || counterOf(snap.sequence)} 步</p>
        ${mine ? `
          <div class="lobby-actions">
            <button class="btn" variant="${myReady ? "ghost" : "primary"}" id="ready-btn">
              ${myReady ? "取消准备" : "我已就绪"}
            </button>
          </div>` : ""}
      </article>
      <div class="lobby-seats">
        ${seatCard(a, "a")}
        ${seatCard(b, "b")}
      </div>
      ${role === "host" ? `
        <article class="card">
          <h3 class="card-title">主办方操作</h3>
          <p class="card-subtitle">开始 BP 需双方就位并就绪；缺席 / 未就绪会被主机拒绝并回显原因。</p>
          <div class="lobby-actions">
            <button class="btn" variant="primary" id="start-btn">开始 BP</button>
            <button class="btn" id="lock-btn">${snap.locked ? "解锁房间" : "锁定房间"}</button>
            <button class="btn" id="import-pool-btn">导入曲库…</button>
          </div>
        </article>` : ""}
      <article class="card">
        <h3 class="card-title">曲池预览（${snap.pool.length} 曲）</h3>
        <div class="pool-grid" style="margin-top:var(--sp-2)" id="pool">${poolHtml()}</div>
      </article>
    </section>`;
  bindAll(meRow);
}

/** 本地步数预估（展示兜底；权威值来自 PhaseChanged.totalSteps）。
 *  语义与 parse_sequence 一致：b/t 各 1 步；连续 p 每 2 个合并为 1 步。 */
function counterOf(sequence) {
  let steps = 0; let run = 0;
  for (const ch of sequence || "") {
    if (ch === "b" || ch === "t") { if (run) { steps += Math.ceil(run / 2); run = 0; } steps += 1; }
    else if (ch === "p") run += 1;
  }
  if (run) steps += Math.ceil(run / 2);
  return steps;
}

/* ---------------------------------------------------------------- 进行中 */
function renderProgress(meRow) {
  const snap = store.state.snap;
  const turn = snap.current;
  const isMine = Boolean(role === "player" && turn && meRow?.side === turn.actorSide);
  const paused = snap.status === "paused";
  const tone = paused ? "normal" : toneOfTimer();

  const turnCard = paused ? `
    <article class="turn-card" tone="warn">
      <h2 class="card-title">BP 已暂停（主办方操作）</h2>
      <p class="card-subtitle">停表冻结推进；恢复后按主办方配置续算或重置时间。</p>
    </article>` : `
    <article class="turn-card" ${turn ? `actor="${turn.actorSide.toUpperCase()}" sem="${turn.action}"` : ""}>
      ${isMine ? `<div class="fx-tag">轮到你了</div>` : ""}
      <div class="turn-head">
        <div class="ring" ${tone !== "normal" ? `tone="${tone}"` : ""} id="ring">
          <div class="ring-label">
            <div class="countdown" ${tone !== "normal" ? `tone="${tone}"` : ""} id="count">…</div>
            <div class="countdown-unit">${snap.stepIndex}/${snap.totalSteps} 步</div>
          </div>
        </div>
        <div class="turn-info">
          <h2 class="card-title">${turn
            ? `${ACTION_NAME[turn.action]} · ${sideLabel(turn.actorSide)} 方${isMine ? "（轮到你了）" : ""}`
            : "回合推进中"}</h2>
          <p class="card-subtitle">序列 ${escapeHtml(snap.sequence)} · ${turn?.isTb ? "TB 加赛 · " : ""}${isMine
            ? "底部点选曲目，确认后发送"
            : (role === "player" ? "等待对方行动（可用干扰与反制）" : role === "spectator" ? "观战视角" : "等待选手行动")}</p>
          <div class="chip-row">
            ${turn ? `<span class="badge" ${sideAttr(turn.actorSide)}>${sideLabel(turn.actorSide)} 方行动</span>` : ""}
            ${turn?.decider ? `<span class="badge" sem="timeout">决胜 / 先手判定位</span>` : ""}
            ${store.state.masks.size ? `<span class="badge" sem="interference">本回合 ${store.state.masks.size} 曲被遮蔽</span>` : ""}
            ${store.state.forceReveal ? `<span class="badge" sem="interference">需公开候选</span>` : ""}
            ${snap.interference.enabled ? "" : `<span class="badge" sem="timeout">干扰系统已关闭</span>`}
          </div>
        </div>
      </div>
    </article>`;

  qs("#pagebody").innerHTML = `
    ${turnCard}
    <article class="card">
      <div class="pool-head">
        <h3 class="card-title">曲池</h3>
        <span class="card-subtitle">${clickHint()}</span>
        <span style="flex:1"></span>
        <span class="badge">${snap.pool.filter((c) => c.status === "available").length} 可选 / ${snap.pool.length}</span>
      </div>
      <div class="pool-grid" style="margin-top:var(--sp-2)" id="pool">${poolHtml()}</div>
    </article>
    <div class="detail-cols">
      <article class="card">
        <h3 class="card-title">操作时间线</h3>
        <div class="timeline" style="margin-top:var(--sp-2)" id="timeline">${timelineHtml()}</div>
      </article>
      ${role === "host" ? hostPanelHtml() : ""}
    </div>`;
  bindAll(meRow);
  startCountdownLoop();
}

function clickHint() {
  if (role === "spectator") return "观战只读";
  return role === "host" ? "主机只读视图（裁定在下方）" : "轮到你时点选曲目走确认";
}

/** 曲池 HTML：遮蔽（本机派生）/ 强制位（权威 status 优先，派生兜底）/ picked-by */
function poolHtml() {
  const s = store.state;
  const snap = s.snap;
  const pickBy = new Map();
  for (const op of snap.operations || []) {
    if (op.action === "pick" && op.chartId) pickBy.set(op.chartId, op.actorSide);
  }
  const forcedId = snap.current ? forcedChartIdOf() : 0;
  return snap.pool.map((c) => {
    const title = escapeHtml(c.title);
    if (s.masks.has(c.id)) {
      // 遮蔽是纯渲染态（逻辑上仍可选；点它走正常确认流 = 「蒙眼选择」）
      return `<button class="chart-cell" masked data-id="${c.id}" data-title="${title}" aria-label="已遮蔽曲目">
        <span class="chart-mask-hint">?</span><span>已遮蔽</span></button>`;
    }
    let status = c.status;
    if (status === "available" && forcedId === c.id) status = "forcedBanCandidate";
    const attrs = [`data-id="${c.id}"`, `data-title="${title}"`];
    if (status !== "available") attrs.push(`status="${status}"`);
    if (c.status === "picked" && pickBy.get(c.id)) attrs.push(`picked-by="${pickBy.get(c.id).toUpperCase()}"`);
    const forced = status === "forcedBanCandidate"
      ? `<span class="badge" sem="interference">强制位</span>` : "";
    return `<button class="chart-cell" ${attrs.join(" ")}>
      <strong>${title}</strong><span>${escapeHtml(c.artist)}</span>${forced}</button>`;
  }).join("");
}

/** 强制位曲目 id：最新一条针对我的未反制 / 未回滚 ExtraBanPressure（本回合有效） */
function forcedChartIdOf() {
  const s = store.state;
  if (!s.snap) return 0;
  const log = s.snap.interferenceLog || [];
  for (let i = log.length - 1; i >= 0; i -= 1) {
    const e = log[i];
    if (e.kind !== "extraBanPressure" || e.countered || e.rolledBack) continue;
    if (s.me.side && e.targetSide !== s.me.side) continue;
    return e.params?.forcedChartId || 0;
  }
  return 0;
}

/** 时间线：Ban / Pick（含空 Ban、空 Pick 哨兵 chartId=0）+ 干扰，新 → 旧 */
function timelineHtml() {
  const snap = store.state.snap;
  const items = [];
  const titles = new Map(snap.pool.map((c) => [c.id, c.title]));
  const ops = [...(snap.operations || [])].sort((x, y) => y.orderNo - x.orderNo);
  for (const op of ops) {
    const label = op.chartId
      ? `《${escapeHtml(titles.get(op.chartId) ?? `#${op.chartId}`)}》`
      : `空 ${op.action === "ban" ? "Ban" : "Pick"}（超时）`;
    items.push(`
      <div class="tl-item" sem="${op.action}">
        <i class="tl-dot"></i>
        <span class="badge" ${sideAttr(op.actorSide)}>${sideLabel(op.actorSide)}</span>
        <span class="badge" sem="${op.action}">${op.action === "ban" ? "Ban" : "Pick"}</span>
        <span class="tl-text">${label}</span>
        <span style="flex:1"></span><span class="tl-foot">⥓${op.orderNo}</span>
      </div>`);
  }
  for (const e of [...(snap.interferenceLog || [])].reverse()) {
    if (e.kind === "emote") continue;
    const marks = [];
    if (e.countered) marks.push(`<span class="badge" sem="pick">已反制</span>`);
    if (e.rolledBack) marks.push(`<span class="badge">已回滚</span>`);
    if (e.byHost) marks.push(`<span class="badge">主办方裁定</span>`);
    items.push(`
      <div class="tl-item" sem="interference">
        <i class="tl-dot"></i>
        <span class="badge" ${sideAttr(e.fromSide)}>${sideLabel(e.fromSide)}→${sideLabel(e.targetSide)}</span>
        <span class="badge" sem="interference">${KIND_META[e.kind]?.name ?? e.kind}</span>
        ${marks.join("")}
        <span style="flex:1"></span><span class="tl-foot">#${e.id}</span>
      </div>`);
  }
  const more = ops.length + (snap.interferenceLog || []).filter((e) => e.kind !== "emote").length > items.length
    ? `<p class="card-subtitle">…更早的记录见结果页</p>` : "";
  return (items.slice(0, 14).join("") || `<p class="card-subtitle">暂无记录</p>`) + more;
}

/** 主机面板：玩家列表（干扰权 / 移出）+ 仲裁 / 暂停 / 锁房 + 干扰配置摘要 */
function hostPanelHtml() {
  const snap = store.state.snap;
  const cfg = snap.interference;
  const rows = snap.players.map((p) => {
    const badge = p.role === "host" ? `<span class="badge">主办方</span>`
      : p.role === "spectator" ? `<span class="badge" role="spectator">观众</span>`
        : `<span class="badge" ${sideAttr(p.side ?? "")}>${p.side ? sideLabel(p.side) : "—"}</span>`;
    return `
      <div class="conn-row">
        ${badge}<strong>${escapeHtml(p.name)}</strong>
        ${p.ready ? `<span class="badge" sem="pick">就绪</span>` : ""}
        <span class="badge">干扰点 ${p.interferencePoints}</span>
        ${p.interferenceDisabled ? `<span class="badge" sem="timeout">干扰权被禁</span>` : ""}
        <span style="flex:1"></span>
        ${p.role === "player" ? `
          <button class="btn" data-act="disable" data-id="${p.id}">${p.interferenceDisabled ? "恢复干扰权" : "禁干扰权"}</button>
          <button class="btn" variant="danger" data-act="kick" data-id="${p.id}">移出</button>` : ""}
      </div>`;
  }).join("");
  return `
    <article class="card">
      <h3 class="card-title">选手与裁定</h3>
      <p class="card-subtitle">干扰配置：初始 ${cfg.initialPoints} 点 / 反制消耗 ${cfg.counterCost} 点 / 每回合每方上限 ${cfg.maxStrategicPerRoundPerSide} 次</p>
      <div class="host-rows">${rows}</div>
      <div class="lobby-actions">
        <button class="btn" variant="danger" id="rollback-btn">裁定回退…</button>
        <button class="btn" id="lock-btn">${snap.locked ? "解锁房间" : "锁定房间"}</button>
        ${snap.status !== "lobby" ? `<button class="btn" id="pause-btn">${snap.status === "paused" ? "恢复" : "暂停"}</button>` : ""}
      </div>
    </article>`;
}

/* ---------------------------------------------------------------- 结果页 */
const resultTitles = () => new Map((store.state.snap?.pool || []).map((c) => [c.id, c.title]));

/** 结果数据：优先 SessionEnded 拍平事件；错过时从快照幂等派生（同一事实源） */
function resultData() {
  const s = store.state;
  if (s.finished) return s.finished;
  const snap = s.snap;
  return {
    sequence: snap.sequence,
    players: snap.players || [],
    bans: (snap.operations || []).filter((o) => o.action === "ban"),
    picks: (snap.operations || []).filter((o) => o.action === "pick"),
    availableCharts: (snap.pool || []).filter((c) => c.status === "available").map((c) => c.id),
    interferenceLog: snap.interferenceLog || [],
    finishedAtMs: 0,
  };
}

function renderResult(meRow) {
  const r = resultData();
  const titles = resultTitles();
  const nameOf = (id) => escapeHtml(titles.get(id) ?? `#${id}`);
  const sidePicks = (side) => r.picks
    .filter((o) => o.actorSide === side && o.chartId)
    .sort((x, y) => x.orderNo - y.orderNo)
    .map((o) => `<li>${nameOf(o.chartId)}</li>`).join("");
  const bansText = r.bans.length
    ? `<p class="card-subtitle">${r.bans.map((o) => `${sideLabel(o.actorSide)} Ban ${o.chartId ? `《${nameOf(o.chartId)}》` : "（空 Ban）"}`).join(" · ")}</p>`
    : "";
  const log = r.interferenceLog;
  const ifStats = {
    total: log.filter((e) => e.kind !== "emote").length,
    countered: log.filter((e) => e.countered).length,
    rolled: log.filter((e) => e.rolledBack).length,
  };
  qs("#pagebody").innerHTML = `
    <section class="result-view">
      <article class="turn-card">
        <h2 class="card-title">BP 结束</h2>
        <p class="card-subtitle">序列 ${escapeHtml(r.sequence)} · 共 ${r.picks.length} 选 / ${r.bans.length} 禁 · 干扰 ${ifStats.total} 次（反制 ${ifStats.countered} / 回滚 ${ifStats.rolled}）</p>
      </article>
      <div class="lobby-seats">
        <article class="card seat-card" A-side>
          <span class="badge" A-side>A 方选曲</span>
          <ul class="pick-list">${sidePicks("a") || `<li class="card-subtitle">无</li>`}</ul>
        </article>
        <article class="card seat-card" B-side>
          <span class="badge" B-side>B 方选曲</span>
          <ul class="pick-list">${sidePicks("b") || `<li class="card-subtitle">无</li>`}</ul>
        </article>
      </div>
      <div class="detail-cols">
        <article class="card">
          <h3 class="card-title">禁用记录</h3>
          ${bansText || `<p class="card-subtitle">无</p>`}
        </article>
        <article class="card">
          <h3 class="card-title">最终可用曲目（${r.availableCharts.length}）</h3>
          <div class="chip-row" style="margin-top:var(--sp-2)">
            ${r.availableCharts.map((id) => `<span class="badge">${nameOf(id)}</span>`).join("") || `<span class="card-subtitle">无</span>`}
          </div>
        </article>
      </div>
      <article class="card">
        <h3 class="card-title">分享与导出</h3>
        <div class="lobby-actions">
          <button class="btn" variant="primary" id="copy-result">复制文本结果</button>
          <button class="btn" id="copy-result-link">复制分享链接</button>
          <button class="btn" id="back-index">回到入口</button>
        </div>
        <p class="card-subtitle">分享链接离线可看（数据内置，主机下线不失效）；Tauri 控制台也可用 bp_get_result 拉取同一份结构化结果。</p>
      </article>
    </section>`;
  bindAll(meRow);
}

/** 结果页文案（复制用；纯文本，与渲染同源派生） */
function resultPlainText() {
  const r = resultData();
  const titles = resultTitles();
  const nameOf = (id) => titles.get(id) ?? `#${id}`;
  const line = (s2) => `  ${s2}`;
  const sideLine = (side) => r.picks
    .filter((o) => o.actorSide === side && o.chartId)
    .map((o) => nameOf(o.chartId)).join(", ");
  const bans = r.bans.map((o) => `${sideLabel(o.actorSide)} ${o.chartId ? nameOf(o.chartId) : "空 Ban"}`).join(", ");
  const log = r.interferenceLog;
  return [
    `BP 结果 · 房间 ${store.state.snap?.roomCode ?? ""} · 序列 ${r.sequence}`,
    `A 方选曲: ${sideLine("a") || "无"}`,
    `B 方选曲: ${sideLine("b") || "无"}`,
    bans ? `禁用: ${bans}` : `禁用: 无`,
    `最终可用: ${r.availableCharts.map(nameOf).join(", ") || "无"}`,
    `干扰: ${log.filter((e) => e.kind !== "emote").length} 次 · 反制 ${log.filter((e) => e.countered).length} · 回滚 ${log.filter((e) => e.rolledBack).length}`,
  ].map(line).join("\n");
}

/* ----------------------------------------------- 分享链接（离线 dataURL，user 拍板方案） */
/**
 * BpResult + 曲目标题快照 → 紧凑 JSON → URL-safe base64 → `bp.html#bp=…`。
 * 永不过期、零服务依赖；主机下线后链接仍可直接打开只读结果页。
 * 剥离的冗余（artist/params/id/atMs）不参与结果页渲染，导出文本结果仍有完整信息。
 */
function packBpLink() {
  const r = resultData();
  const s = store.state;
  const payload = {
    v: 1,
    seq: r.sequence,
    room: s.snap?.roomCode ?? "",
    fin: r.finishedAtMs,
    pool: (s.snap?.pool || []).map((c) => [c.id, c.title]),
    bans: r.bans.map((o) => [o.orderNo, o.actorSide, o.chartId]),
    picks: r.picks.map((o) => [o.orderNo, o.actorSide, o.chartId]),
    avail: r.availableCharts,
    iflog: r.interferenceLog.map((e) => [e.kind, e.fromSide, e.targetSide, e.countered ? 1 : 0, e.rolledBack ? 1 : 0, e.byHost ? 1 : 0]),
  };
  const bytes = new TextEncoder().encode(JSON.stringify(payload));
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  const b64 = btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  return `${location.href.split("#")[0]}#bp=${b64}`;
}

/** 分享 hash 数据 → { snap, finished }（还原 packBpLink；数据不可信，仅当渲染输入用） */
function unpackBpResult(b64) {
  const norm = b64.replace(/-/g, "+").replace(/_/g, "/");
  const bin = atob(norm + "=".repeat((4 - (norm.length % 4)) % 4));
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  const p = JSON.parse(new TextDecoder().decode(bytes));
  if (p?.v !== 1 || !Array.isArray(p.pool)) throw new Error("bad payload");
  const opOf = ([orderNo, actorSide, chartId], action) => ({ orderNo, actorSide, action, chartId });
  const iflog = p.iflog.map(([kind, fromSide, targetSide, countered, rolledBack, byHost]) => ({
    kind, fromSide, targetSide, countered: !!countered, rolledBack: !!rolledBack, byHost: !!byHost,
  }));
  const bans = p.bans.map((o) => opOf(o, "ban"));
  const picks = p.picks.map((o) => opOf(o, "pick"));
  const finished = {
    sequence: p.seq || "",
    players: [],
    bans, picks,
    availableCharts: p.avail || [],
    interferenceLog: iflog,
    finishedAtMs: p.fin || 0,
  };
  const snap = {
    roomCode: p.room || "", sequence: p.seq || "", status: "finished",
    players: [], current: null,
    operations: [...bans, ...picks].sort((x, y) => x.orderNo - y.orderNo),
    pool: p.pool.map(([id, title]) => ({ id, title, artist: "", status: "available" })),
    interferenceLog: iflog,
  };
  return { snap, finished };
}

/** 分享链接打开：离线只读结果视图（零传输、零更新；数据损坏给明确提示不静默） */
function bootSharedResult(b64) {
  buildSkeleton();
  let restored;
  try {
    restored = unpackBpResult(b64);
  } catch (err) {
    console.warn("[bp-host] 分享数据解析失败", err);
    qs("#pagebody").innerHTML = `
      <div class="card loading-card">
        <p class="card-title">分享链接无效</p>
        <p class="card-subtitle">数据损坏或未完整复制。请回到主机结果页重新「复制分享链接」。</p>
      </div>`;
    return;
  }
  store.state.snap = restored.snap;
  store.state.finished = restored.finished;
  renderResult(null);
}

/* ------------------------------------------------------- 计时层 */
function toneOfTimer() {
  const s = store.state.snap;
  if (!s?.timer || s.status !== "inProgress") return "normal";
  return deadlineTone(Math.max(0, s.timer.deadlineTsMs - Date.now()));
}

let cdTimer = null;
function startCountdownLoop() {
  if (cdTimer) clearInterval(cdTimer);
  cdTimer = setInterval(() => {
    const s = store.state.snap;
    if (!s?.timer || s.status !== "inProgress") {
      // 计时未激活（暂停 / 无 timer）：显示占位；停表由主机冻结，本地绝不自行推进
      const c = qs("#count"); if (c) c.textContent = "—";
      return;
    }
    const remain = Math.max(0, s.timer.deadlineTsMs - Date.now());
    const tone = deadlineTone(remain);
    const c = qs("#count"); const r = qs("#ring");
    if (c) {
      c.textContent = String(Math.ceil(remain / 1000));
      if (tone !== "normal") c.setAttribute("tone", tone); else c.removeAttribute("tone");
    }
    if (r) {
      // 动态数据例外（DESIGN.md §4）：--ring-pct 为内联 CSS 自定义属性注入
      r.style.setProperty("--ring-pct", String(Math.max(0, Math.min(100, Math.round((remain / Math.max(1, s.timer.durationMs)) * 100)))));
      if (tone !== "normal") r.setAttribute("tone", tone); else r.removeAttribute("tone");
    }
  }, 200);
}

/* ------------------------------------------------------- 底部 dock */
function renderDock(meRow) {
  const s = store.state.snap;
  const finished = Boolean(store.state.finished) || s.status === "finished";
  const paused = s.status === "paused";
  const emoteBar = `
    <div class="emote-bar">
      ${["👍", "😂", "👏", "GG", "😭", "🎉"].map((x) => `
        <button class="btn" variant="ghost" data-emote="${x}">${x}</button>`).join("")}
    </div>`;
  if (finished) { qs("#dock").innerHTML = ""; return; }
  if (role === "host") {
    qs("#dock").innerHTML = `
      <button class="btn" id="dock-rollback" variant="danger">裁定回退…</button>
      <button class="btn" id="dock-lock">${s.locked ? "解锁房间" : "锁定房间"}</button>`;
    return;
  }
  if (paused) {
    // Paused 冻结除 resume 外全部推进意图（legal_intent）——只读提示，不发无效意图
    qs("#dock").innerHTML = `<span class="badge" sem="timeout" style="justify-self:center">BP 已暂停 · 操作冻结中</span>`;
    return;
  }
  if (s.status === "lobby") {
    const ready = meRow?.role === "player" && meRow.ready;
    qs("#dock").innerHTML = `
      ${meRow?.role === "player" ? `<button class="btn" variant="${ready ? "ghost" : "primary"}" id="ready-btn">${ready ? "取消准备" : "我已就绪"}</button>` : ""}
      ${emoteBar}`;
    return;
  }
  // inProgress：选手 = 干扰入口 / 反制 / 公开候选 / 离开；其它 = 表情层
  if (meRow?.role === "player" && meRow.side) {
    const pts = meRow.interferencePoints ?? 0;
    const cfg = s.interference;
    const canInterf = cfg.enabled && pts >= 1 && !meRow.interferenceDisabled;
    const counterId = counterTargetId(meRow.side);
    const cost = cfg.counterCost ?? 1;
    qs("#dock").innerHTML = `
      ${store.state.forceReveal ? `<button class="btn" variant="primary" id="reveal-btn" style="grid-column:1/-1">公开候选曲目…</button>` : ""}
      ${counterId ? `<button class="btn" variant="danger" id="counter-btn" style="grid-column:1/-1">反制本次干扰（−${cost} 点）</button>` : ""}
      <button class="btn" id="interf-btn" ${canInterf ? "" : "disabled"} title="${configHint(cfg)}">干扰（${pts}）</button>
      <button class="btn" variant="ghost" id="back-index">离开</button>
      ${emoteBar}`;
    return;
  }
  qs("#dock").innerHTML = emoteBar;
}

/** 干扰按钮 disabled 时的解释（title 提示，不阻塞） */
function configHint(cfg) {
  if (!cfg.enabled) return "干扰系统已关闭";
  return "干扰点不足或干扰权被主办方禁用";
}

/** 最近一条针对我、未反制 / 未回滚的策略干扰 id（反制目标） */
function counterTargetId(mySide) {
  const log = store.state.snap.interferenceLog || [];
  for (let i = log.length - 1; i >= 0; i -= 1) {
    const e = log[i];
    if (e.kind === "emote") continue;
    if (e.targetSide !== mySide) continue;
    if (e.countered || e.rolledBack) continue;
    return e.id;
  }
  return null;
}

/* ------------------------------------------------------- 统一绑定层 */
/** 每个 render 分支末尾调用一次；所有 selector 各自独立、重复绑定安全 */
function bindAll(meRow) {
  const snap = store.state.snap;
  // —— 通用 ——
  const ready = qs("#ready-btn");
  if (ready) ready.onclick = () => send({ t: "setReady", ready: !meRow?.ready });
  const leave = qs("#back-index");
  if (leave) leave.onclick = () => { send({ t: "leave" }); location.replace("index.html"); };
  qsa("[data-emote]").forEach((b) => {
    b.onclick = () => send({ t: "sendEmote", emote: b.dataset.emote });
  });
  // 曲池点击（大厅预览 / 进行中 / 结果页均安全：按状态分流提示）
  qsa("#pool .chart-cell, #pool-preview .chart-cell").forEach((cell) => {
    cell.onclick = () => onChartClick(cell, meRow);
  });
  // —— 主机 ——
  const start = qs("#start-btn");
  if (start) start.onclick = () => hostCmd("startBp");
  qsa("#lock-btn, #dock-lock").forEach((b) => {
    b.onclick = () => hostCmd("lockRoom", { locked: !(snap?.locked) });
  });
  qsa("#rollback-btn, #dock-rollback").forEach((b) => {
    b.onclick = () => openRollbackSheet();
  });
  const pause = qs("#pause-btn");
  if (pause) pause.onclick = () => hostCmd(snap?.status === "paused" ? "resume" : "pause");
  qsa("[data-act]").forEach((b) => {
    b.onclick = () => {
      const pid = Number(b.dataset.id);
      const row = (snap.players || []).find((x) => x.id === pid);
      if (b.dataset.act === "disable") {
        hostCmd("disableInterference", { side: row?.side, disabled: !(row?.interferenceDisabled) });
      } else if (b.dataset.act === "kick") {
        openSheet({
          title: `移出 ${escapeHtml(row?.name ?? "该玩家")}？`,
          build: (body, close) => {
            body.innerHTML = `<p class="card-subtitle">确定移出后该连接会被踢出；操作进入审计日志。</p>`;
            const row2 = el(`<div class="sheet-actions">
              <button class="btn" variant="danger">确定移出</button>
              <button class="btn" variant="ghost">取消</button></div>`);
            const [ok, no] = qsa("button", row2);
            ok.onclick = () => { hostCmd("kick", { playerId: pid }); close(); };
            no.onclick = close;
            body.appendChild(row2);
          },
        });
      }
    };
  });
  // —— 选手 ——
  const interf = qs("#interf-btn");
  if (interf) interf.onclick = () => openInterferenceSheet(meRow);
  const reveal = qs("#reveal-btn");
  if (reveal) reveal.onclick = () => openRevealSheet();
  const counter = qs("#counter-btn");
  if (counter) {
    counter.onclick = () => {
      const id = counterTargetId(meRow.side);
      if (!id) { toast("暂无可反制的干扰", "warn"); return; }
      send({ t: "counterInterference", interferenceId: id });
    };
  }
  const copy = qs("#copy-result");
  if (copy) copy.onclick = copyResult;
  const copyLink = qs("#copy-result-link");
  if (copyLink) copyLink.onclick = () => copyToClipboard(packBpLink(), "分享链接已复制（离线可看，主机下线不失效）");
  const importPool = qs("#import-pool-btn");
  if (importPool) importPool.onclick = () => openPoolImportSheet();
}

/** 曲池点击分流：观战 / 非我方回合 / 遮蔽 / 强制位约束 / 正常确认流 */
function onChartClick(cell, meRow) {
  const id = Number(cell.dataset.id);
  const title = cell.getAttribute("data-title") || `#${id}`;
  const snap = store.state.snap;
  if (!snap || snap.status === "finished") return;
  const turn = snap.current;
  if (role !== "player" || !meRow?.side) {
    toast(`${title} · ${snap.status === "inProgress" ? "观战只读（点选仅查看）" : "等待开始"}`, "info");
    return;
  }
  if (!turn || turn.actorSide !== meRow.side) { toast("当前不是你的回合", "warn"); return; }
  const forcedId = forcedChartIdOf();
  if (store.state.masks.has(id)) { toast("该曲目被遮蔽（逻辑上仍可选）：确认后仍按选择执行", "info"); openActionConfirm(turn.action, id, "已遮蔽曲目"); return; }
  // 强制位双向约束（主机 MustIncludeForcedBan 语义的前置提示；裁决仍以主机为准）
  if (forcedId && turn.action === "ban" && id !== forcedId) {
    toast("本回合必须禁用强制位曲目（追加 Ban 压力）", "warn");
    return;
  }
  if (forcedId && turn.action !== "ban" && id === forcedId) {
    toast("强制位曲目需在禁用（Ban）回合处理", "warn");
    return;
  }
  openActionConfirm(turn.action, id, title);
}

/** Ban / Pick 确认流（Bottom Sheet + 二次确认；不使用模态弹窗） */
function openActionConfirm(action, chartId, title) {
  const verb = ACTION_NAME[action];
  openSheet({
    title: `${verb} ·《${title}》`,
    build: (body, close) => {
      body.innerHTML = `<p class="card-subtitle">操作经主机确认后生效并可进入时间线（回退仅主办方）——二次确认防误触。</p>`;
      const first = el(`<div class="sheet-actions">
        <button class="btn" variant="primary">确认${verb}</button>
        <button class="btn" variant="ghost">再想想</button></div>`);
      const [ok1, no1] = qsa("button", first);
      const second = el(`
        <div hidden>
          <h4 class="card-title" style="font-size:var(--fz-md)">二次确认</h4>
          <div class="sheet-actions">
            <button class="btn" variant="primary">发送到主机</button>
            <button class="btn" variant="ghost">放弃</button>
          </div></div>`);
      const [ok2, no2] = qsa("button", second);
      ok1.onclick = () => { first.hidden = true; second.hidden = false; };
      no1.onclick = close;
      ok2.onclick = () => {
        send({ t: action, chartId });
        close();
        toast("已发送，等待主机确认（结果以事件为准）", "info");
      };
      no2.onclick = close;
      body.append(first, second);
    },
  });
}

/** ForceReveal：目标方公开候选意向（不可撤回，可改选后续） */
function openRevealSheet(_interferenceId) {
  const avail = (store.state.snap.pool || []).filter((c) => c.status === "available");
  openSheet({
    title: "公开候选（对方要求优先公开）",
    build: (body, close) => {
      body.innerHTML = `<p class="card-subtitle">公开意图曲目后仍按正式 Ban/Pick 确认流执行；先公开有助于减少争议。</p>
        <div class="sheet-grid"></div>`;
      const wrap = qs(".sheet-grid", body);
      if (!avail.length) {
        wrap.innerHTML = `<p class="card-subtitle">曲池已无可选曲目；可直接等待超时处理。</p>`;
        return;
      }
      avail.forEach((c) => {
        const b = el(`<button class="chart-cell" data-id="${c.id}" data-title="${escapeHtml(c.title)}">
          <strong>${escapeHtml(c.title)}</strong><span>${escapeHtml(c.artist)}</span></button>`);
        b.onclick = () => {
          send({ t: "revealCandidate", chartId: c.id });
          close();
          toast("候选已公开（以主机 Notice 广播为准）", "info");
        };
        wrap.appendChild(b);
      });
    },
  });
}

/** 主机仲裁回退：全历史可点；主机级联撤销目标及其后全部操作 */
function openRollbackSheet() {
  const snap = store.state.snap;
  const all = [...(snap.operations || [])].sort((x, y) => y.orderNo - x.orderNo);
  const ops = all.slice(0, 6);
  const titles = new Map(snap.pool.map((c) => [c.id, c.title]));
  openSheet({
    title: "仲裁回退",
    build: (body, close) => {
      body.innerHTML = `<p class="card-subtitle">选择要撤销的操作；主机将**级联撤销**该操作及其后全部（曲目复位、强制位失效、对位回合重新计时）。操作经审计日志支撑、order_no 不回收。</p>
        <div class="sheet-grid"></div>`;
      const wrap = qs(".sheet-grid", body);
      if (!ops.length) { wrap.innerHTML = `<p class="card-subtitle">暂无可回退的操作。</p>`; return; }
      ops.forEach((op, i) => {
        // desc 顺序下该 op 之后（orderNo 更大）的全量条数 = i——与展示截断无关
        const cascade = i;
        const b = el(`<button class="chart-cell">
          <strong style="font-size:var(--fz-sm)">⥓${op.orderNo} · ${op.action === "ban" ? "Ban" : "Pick"} · ${sideLabel(op.actorSide)}</strong>
          <span>${op.chartId ? `《${escapeHtml(titles.get(op.chartId) ?? "#")}》` : "空 Ban（超时）"}</span>
          <span class="card-subtitle">${cascade ? `↺ 将同时撤销其后 ${cascade} 条` : "撤销最近一条"}</span></button>`);
        b.onclick = () => {
          hostCmd("arbitrateRollback", { operationOrderNo: op.orderNo });
          close();
        };
        wrap.appendChild(b);
      });
      if (all.length > ops.length) {
        wrap.appendChild(el(`<p class="card-subtitle">仅显示最近 ${ops.length} 条（另有 ${all.length - ops.length} 条更早操作，同样可回退——由主机级联处理）。</p>`));
      }
    },
  });
}

/** 主办方曲库导入：快照 JSON → 校验 → 白名单字段构造 SetPool（仅 Lobby；曲 id 从 1 起避开哨兵） */
function openPoolImportSheet() {
  openSheet({
    title: "导入曲库",
    build: (body, close) => {
      body.innerHTML = `
        <p class="card-subtitle">选择由 bp-host/tools/export-library.cjs 生成的曲库快照 JSON。下发即**替换**当前曲池（全部曲目回 Available），且仅大厅阶段可用；曲 id 从 1 起，避开 chart_id=0 空操作哨兵。</p>
        <div class="sheet-actions">
          <button class="btn" variant="primary" id="pool-choose">选择快照文件…</button>
        </div>
        <div id="pool-preview"></div>
        <input type="file" accept="application/json,.json" hidden id="pool-file"/>`;
      const file = qs("#pool-file", body);
      const preview = qs("#pool-preview", body);
      qs("#pool-choose", body).onclick = () => file.click();
      file.onchange = async () => {
        const f = file.files?.[0];
        if (!f) return;
        let parsed;
        try {
          parsed = JSON.parse(await f.text());
        } catch {
          preview.innerHTML = `<p class="card-subtitle">JSON 解析失败：不是合法快照文件。</p>`;
          return;
        }
        const validated = validateLibrarySnapshot(parsed);
        if (typeof validated === "string") {
          preview.innerHTML = `<p class="card-subtitle">快照校验失败：${escapeHtml(validated)}</p>`;
          return;
        }
        const charts = validated;
        const sample = charts.slice(0, 8).map((c) =>
          `<span class="badge">#${c.id} ${escapeHtml(c.title)}${c.artist ? ` · ${escapeHtml(c.artist)}` : ""}</span>`).join("");
        preview.innerHTML = `
          <p class="card-subtitle">共 ${charts.length} 首（${escapeHtml(String(parsed.builtAt || ""))}）</p>
          <div class="chip-row" style="margin:var(--sp-2) 0">${sample}${charts.length > 8 ? `<span class="badge">…等 ${charts.length - 8} 首</span>` : ""}</div>
          ${store.state.snap?.status !== "lobby"
            ? `<p class="card-subtitle">⚠ 当前不在大厅阶段，主机将拒绝下发（回到 Lobby 后重新导入）。</p>`
            : ""}
          <div class="sheet-actions">
            <button class="btn" variant="primary" id="pool-send">下发曲池（${charts.length} 首）</button>
          </div>`;
        qs("#pool-send", preview).onclick = () => {
          // 白名单字段：id/title/artist/difficulty + status（ChartMeta serde 必填）
          hostCmd("setPool", {
            pool: charts.map((c) => ({
              id: c.id,
              title: c.title,
              artist: c.artist || "",
              ...(Number.isFinite(c.difficulty) ? { difficulty: c.difficulty } : {}),
              status: "available",
            })),
          });
          close();
          toast("曲池下发请求已发送，结果以主机 PoolUpdated 广播为准", "info");
        };
      };
    },
  });
}

/** 曲库快照校验（给 hostCmd 前最后一道闸）：结构 / 版本 / id≥1 去重 / 曲名非空 */
function validateLibrarySnapshot(p) {
  if (!p || typeof p !== "object") return "不是合法快照文件";
  if (p.v !== 1) return `快照版本不识别（v=${String(p.v)})`;
  if (!Array.isArray(p.charts) || !p.charts.length) return "快照没有曲目";
  const seen = new Set();
  for (const c of p.charts) {
    if (!c || typeof c !== "object" || typeof c.title !== "string" || !c.title.trim()) {
      return "存在无名称曲目，快照不完整";
    }
    if (!Number.isInteger(c.id)) return `曲目 id 非整数：${JSON.stringify(c.id)}`;
    if (c.id === 0) return "曲 id=0 与空操作哨兵冲突（快照生成有误）";
    if (seen.has(c.id)) return `重复曲 id：${c.id}`;
    if (c.difficulty != null && !Number.isFinite(c.difficulty)) return `曲 ${c.id} 难度非法`;
    seen.add(c.id);
  }
  return p.charts;
}

/** 干扰发射台：类型 → （曲目选择 / 直接确认） */
function openInterferenceSheet(meRow) {
  const snap = store.state.snap;
  const cfg = snap.interference;
  if (!cfg.enabled) { toast("干扰系统已关闭", "warn"); return; }
  const target = other(meRow.side);
  const kinds = (cfg.enabledKinds || []).filter((k) => k in KIND_META);
  openSheet({
    title: `对 ${sideLabel(target)} 方使用干扰`,
    build: (body, close) => {
      body.innerHTML = `<p class="card-subtitle">主机原子校验并广播；每次策略干扰消耗 1 点，可被反制，不回复。</p>
        <div class="sheet-grid" id="kinds"></div>
        <div id="kind-detail"></div>`;
      const wrap = qs("#kinds", body);
      if (!kinds.length) {
        wrap.innerHTML = `<p class="card-subtitle">当前配置未启用任何干扰类型。</p>`;
        return;
      }
      kinds.forEach((k) => {
        const meta = KIND_META[k];
        const b = el(`<button class="btn kind-row" style="justify-content:flex-start">
          <span class="badge" sem="interference">${meta.name}</span>
          <span class="card-subtitle" style="margin:0">${meta.tag}</span>
          <span style="flex:1"></span><span class="badge">1 点</span></button>`);
        b.onclick = () => {
          wrap.hidden = true;
          buildKindFlow(k, target, body, close, wrap);
        };
        wrap.appendChild(b);
      });
    },
  });
}

/** 某类干扰的二级流：
 *  - obscurePool：多选 ≤ maxCount（遮蔽对象是对方视角，去除我方遮蔽无意义）；
 *  - extraBanPressure：单选曲目（forcedBanChartId）；
 *  - compressTimer / forceReveal：无曲目参数 → 直接确认。 */
function buildKindFlow(kind, target, body, close, backWrap) {
  const snap = store.state.snap;
  const detail = qs("#kind-detail", body);
  const avail = snap.pool.filter((c) => c.status === "available");
  const multi = kind === "obscurePool";
  const single = kind === "extraBanPressure";
  const maxN = snap.interference?.obscure?.maxCount || 3;

  const head = `
    <h4 class="card-title" style="font-size:var(--fz-md)">${KIND_META[kind].name}${multi ? ` · 多选 ≤ ${maxN}` : single ? " · 选择强制位曲目" : ""}</h4>
    <div class="sheet-grid"></div>
    <div class="sheet-actions">
      <button class="btn" variant="primary" id="k-go">发送</button>
      <button class="btn" variant="ghost" id="k-back">返回</button>
    </div>`;
  if (!avail.length) {
    detail.innerHTML = `<p class="card-subtitle">曲池已无可选曲目。</p>` + head.replace(/<div class="sheet-grid"><\/div>/, "");
    const go2 = qs("#k-go", detail);
    if (go2) go2.onclick = () => sendKind(kind, target, []);
    qs("#k-back", detail).onclick = () => { detail.innerHTML = ""; backWrap.hidden = false; };
    return;
  }
  detail.innerHTML = head;
  const grid = qs(".sheet-grid", detail);
  const ids = new Set();
  let selId = 0;
  const goBtn = qs("#k-go", detail);
  const refresh = () => {
    if (multi) {
      goBtn.disabled = ids.size === 0;
      goBtn.textContent = ids.size ? `发送（已选 ${ids.size}${ids.size >= maxN ? "，已达上限" : ""}）` : "发送";
    } else if (single) {
      goBtn.disabled = !selId;
      goBtn.textContent = selId ? "发送" : "发送（先选曲目）";
    }
  };
  if (multi || single) {
    avail.forEach((c) => {
      const cell = el(`<button class="chart-cell" data-id="${c.id}">
        <strong>${escapeHtml(c.title)}</strong><span>${escapeHtml(c.artist)}</span></button>`);
      cell.onclick = () => {
        if (multi) {
          if (ids.has(c.id)) { ids.delete(c.id); cell.removeAttribute("status"); }
          else if (ids.size < maxN) { ids.add(c.id); cell.setAttribute("status", "picked"); }
          else { toast(`最多 ${maxN} 首`, "warn"); return; }
          refresh();
        } else {
          grid.querySelectorAll(".chart-cell").forEach((x) => x.removeAttribute("status"));
          cell.setAttribute("status", "picked");
          selId = c.id;
          refresh();
        }
      };
      grid.appendChild(cell);
    });
  } else {
    grid.innerHTML = `<p class="card-subtitle">${kind === "compressTimer"
      ? `压缩当前行动方本回合倒计时（−${snap.interference.compress.reduceSecs}s，下限 ${snap.interference.compress.floorSecs}s；同回合不可叠加）。`
      : `要求对方在本回合 Pick 前先公开候选意向（对方随后会收到公开流程）。`}</p>`;
  }
  refresh();
  goBtn.onclick = () => {
    if (multi) sendKind(kind, target, [...ids]);
    else if (single) sendKind(kind, target, [selId]);
    else sendKind(kind, target, []);
    close();
  };
  qs("#k-back", detail).onclick = () => {
    detail.innerHTML = ""; backWrap.hidden = false;
  };
}

function sendKind(kind, targetSide, ids) {
  const payload = { t: "useInterference", kind, targetSide };
  if (kind === "obscurePool") payload.obscureChartIds = ids;
  if (kind === "extraBanPressure") payload.forcedBanChartId = ids[0] || null;
  send(payload);
  toast("干扰已提交（主机原子校验并广播）", "info");
}

/* ------------------------------------------------------- 表情浮层 */
/**
 * Emote 事件（拍平：{t:"emote", fromSide?, fromPlayer, fromName, emote, text}）。
 * 表现 = 屏幕右下淡出上浮气泡；不覆盖 dock 与 toast；屏上超过 6 个则丢弃
 * （观众的局部节流兜底，权威频控在主机 EmoteBucket）。
 */
function fxEmote(ev) {
  if (qsa(".emote-float").length >= 6) return;
  const who = ev.fromName && ev.fromName !== (loadJoin().name || "")
    ? `${escapeHtml(ev.fromName)} · ` : "";
  const box = el(`<div class="emote-float" aria-hidden="true">
    <span class="emote-face">${escapeHtml(ev.emote)}</span>
    <span class="emote-meta">${who}${escapeHtml(ev.text ?? "")}</span>
  </div>`);
  box.style.setProperty("--emote-dx", String((ev.fromPlayer % 7) - 3));
  document.body.appendChild(box);
  setTimeout(() => box.remove(), 2600);
}

function copyResult() {
  copyToClipboard(resultPlainText(), "结果已复制到剪贴板");
}

/** 剪贴板写入（非安全上下文走临时 textarea 兜底） */
function copyToClipboard(text, doneMsg) {
  const done = () => toast(doneMsg, "info");
  const fail = () => toast("复制失败（环境限制），请手动选取", "error");
  if (navigator.clipboard?.writeText) {
    navigator.clipboard.writeText(text).then(done, fail);
  } else {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    let ok = false;
    try { ok = document.execCommand("copy"); } catch { ok = false; }
    ta.remove();
    ok ? done() : fail();
  }
}

/* ------------------------------------------------------- 助手 */
function escapeHtml(s2) {
  return String(s2 ?? "")
    .replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;").replaceAll("'", "&#39;");
}
