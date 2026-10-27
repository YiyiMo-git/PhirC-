/**
 * app-core.js ——【已废弃｜deprecated】打包链切换（handoff §5 任务 7）后 dist 产物生效，
 * 源头改为 app-core.ts + ./net + ./tokens；本 JS 仅存档参照，下一轮删除。
 * 原身份：真实页面运行时核心（bp.html / index.html 共用）
 *
 * 契约说明：
 * - 本文件是 net.ts / tokens.ts 的**无打包链运行时镜像**（纯 JS ES module，
 *   浏览器直接可跑）；类型契约、传输语义与二者逐字段一致——修改任何一端
 *   必须同步（net.ts 头注释互链）。引入打包链后应改为直接 import 那两个 TS。
 * - 硬约束：客户端只发意图、只渲染主机广播结果；**零乐观推进 / 不自行扣资源**。
 * - 非法意图的拒绝必经事件流（Error / OperationRejected / InterferenceDenied），
 *   由 toast 显式呈现，不静默。
 */

/* ---------------------------------------------------------------- 契约镜像 */
/** serde 契约镜像（见 net.ts）：字段名与 Rust serde camelCase 输出一致 */
export const SIDE = { a: "a", b: "b" };
export const ACTION_NAME = { ban: "禁用", pick: "选曲" };
export const STATUS_NAME = {
  lobby: "等待就位",
  inProgress: "BP 进行中",
  paused: "已暂停",
  finished: "已结束",
};
export const KIND_META = {
  compressTimer: { name: "压缩倒计时", tag: "压缩对方时间（−8s，下限 5s）" },
  obscurePool: { name: "遮蔽曲池", tag: "对方本回合看不到所选曲目" },
  forceReveal: { name: "强制公开", tag: "要求对方 Pick 前先公开候选" },
  extraBanPressure: { name: "追加 Ban 压力", tag: "对方本回合必须禁用所选曲目" },
};

export function other(side) {
  return side === "a" ? "b" : "a";
}

export function sideLabel(side) {
  return side === "a" ? "A" : "B";
}

/** 倒计时紧急分级（tokens.ts deadlineTone 的运行时镜像） */
export function deadlineTone(remainMs) {
  if (remainMs <= 3000) return "hot";
  if (remainMs <= 10000) return "warn";
  return "normal";
}

/* ---------------------------------------------------------------- 传输实现 */
const WS_BASE_MS = 800; // 指数退避基数（net.ts 同参）
const WS_MAX_MS = 15000;

/**
 * WS 传输：JSON 文本帧；断线指数退避自动重连。
 * open 之后由上层发 Join（带 token 恢复身份）→ 收 Snapshot 全量恢复。
 */
export function createWsTransport(url) {
  let ws = null;
  let attempt = 0;
  let retryTimer = null;
  let closedByUser = false;
  const eventHandlers = new Set();
  const stateHandlers = new Set();
  const setState = (s) => stateHandlers.forEach((h) => h(s));
  const dispatch = (ev) => eventHandlers.forEach((h) => {
    try { h(ev); } catch (e) { console.error("[bp] handler error", e); }
  });

  const connect = () => {
    setState(attempt === 0 ? "connecting" : "reconnecting");
    let sock;
    try {
      sock = new WebSocket(url);
    } catch (e) {
      dispatch({ t: "error", code: "BAD_URL", message: `地址无效：${String(e)}` });
      return;
    }
    ws = sock;
    sock.onopen = () => { attempt = 0; setState("open"); };
    sock.onmessage = (m) => {
      try { dispatch(JSON.parse(String(m.data))); }
      catch (e) { dispatch({ t: "error", code: "BAD_JSON", message: `事件解析失败：${String(e)}` }); }
    };
    sock.onclose = () => {
      ws = null;
      if (closedByUser) { setState("closed"); return; }
      retryTimer = setTimeout(connect, Math.min(WS_MAX_MS, WS_BASE_MS * 2 ** attempt++));
    };
    sock.onerror = () => sock.close();
  };
  connect();

  return {
    send(intent) {
      if (!ws || ws.readyState !== WebSocket.OPEN) {
        // 显式反馈而非丢意图（net.ts 同约束）
        dispatch({ t: "error", code: "NOT_CONNECTED", message: "连接尚未就绪，意图未发送" });
        return;
      }
      ws.send(JSON.stringify(intent));
    },
    onEvent(h) { eventHandlers.add(h); return () => eventHandlers.delete(h); },
    onStateChange(h) { stateHandlers.add(h); return () => stateHandlers.delete(h); },
    close() {
      closedByUser = true;
      if (retryTimer) clearTimeout(retryTimer);
      ws?.close();
    },
  };
}

/**
 * Tauri invoke 传输：意图走 `bp_send_intent`（commands.rs）。
 * 返回值 = 连接专属直接回执；公共广播走 `bp-event` 桥 —— 合流分发。
 * 依赖 tauri.conf.json 的 app.withGlobalTauri（window.__TAURI__）。
 */
export function createInvokeTransport(tauri) {
  const bridges = new Set();
  const dispatch = (ev) => bridges.forEach((h) => {
    try { h(ev); } catch (e) { console.error("[bp] handler error", e); }
  });
  let unlisten = null;
  void tauri.event
    .listen("bp-event", (e) => dispatch(e.payload))
    .then((u) => { unlisten = u; });

  return {
    send(intent) {
      void (async () => {
        try {
          const evs = await tauri.core.invoke("bp_send_intent", { intent });
          (evs || []).forEach(dispatch);
        } catch (e) {
          dispatch({ t: "error", code: "INVOKE_FAILED", message: `意图派发失败：${String(e)}` });
        }
      })();
    },
    onEvent(h) { bridges.add(h); return () => { bridges.delete(h); unlisten?.(); }; },
    onStateChange() { return () => null; },
    close() {
      void tauri.core.invoke("bp_send_intent", { intent: { t: "leave" } }).catch(() => null);
    },
  };
}

/* ------------------------------------------------------------ 身份暂存区 */
const JOIN_KEY = "bp-host:join";

/** 本地身份暂存（token / 姓名等持久化；重连 / 刷新时带 token 恢复身份） */
export function loadJoin() {
  try { return JSON.parse(localStorage.getItem(JOIN_KEY)) || {}; }
  catch { return {}; }
}
export function saveJoin(patch) {
  const cur = loadJoin();
  localStorage.setItem(JOIN_KEY, JSON.stringify({ ...cur, ...patch }));
}
export function clearJoin() {
  localStorage.removeItem(JOIN_KEY);
}

/* ---------------------------------------------------------------- 会话壳 */
/**
 * 会话状态（缩减版 store）：快照为唯一事实源 + 本地派生渲染态。
 * 规则：事件驱动，无任何本地定时推进；mask 在换 step 时清（遮蔽本回合失效）。
 */
export function createStore() {
  const s = {
    snap: null,            // BpSnapshot（serde 契约形状）
    me: { id: 0, role: null, side: null, token: null },
    masks: new Set(),      // 本回合被我看到的遮蔽曲目（下行 ObscurePool 派生）
    forceReveal: null,     // { interferenceId } 待公开候选（ForceReveal 目标视角）
    joining: false,        // join 在途标记（错误事件→回入口页）
    finished: null,        // 拍平的 BpResult（sessionEnded 赋值；渲染层结果页数据源）
    joinError: null,       // join 失败信息（ui 层消费：Tauri 下重试入口）
    onState: null,         // 渲染回调（由 ui 层挂）
  };
  const touch = () => s.onState && s.onState();

  const upsertLog = (ev2) => {
    const i = s.snap.interferenceLog.findIndex((x) => x.id === ev2.id);
    if (i >= 0) s.snap.interferenceLog[i] = ev2; else s.snap.interferenceLog.push(ev2);
  };
  const upsertOp = (op) => {
    const i = s.snap.operations.findIndex((x) => x.orderNo === op.orderNo);
    if (i < 0) s.snap.operations.push(op);
  };
  /** 从干扰日志派生本回合遮蔽（最新一条针对我的未反制/未回滚事件） */
  const derivingMasks = () => {
    s.masks.clear();
    s.forceReveal = null;
    if (!s.me.side) return;
    const log = s.snap.interferenceLog;
    for (let i = log.length - 1; i >= 0; i -= 1) {
      const e = log[i];
      if (e.targetSide !== s.me.side) continue;
      if (e.countered || e.rolledBack) continue;
      if (e.kind === "obscurePool" && Array.isArray(e.params?.chartIds)) {
        e.params.chartIds.forEach((id) => s.masks.add(id));
        return; // 只取最新一条
      }
      if (e.kind === "forceReveal" && !e.params?.candidateChartId) {
        s.forceReveal = { interferenceId: e.id };
        return;
      }
    }
  };

  return {
    get state() { return s; },
    /** 事件入口：把一条 HostEvent 合入状态（全部幂等、可重复安全） */
    apply(ev) {
      if (!ev || typeof ev !== "object") return;
      switch (ev.t) {
        case "welcome": {
          s.me.id = ev.you;
          s.me.role = ev.role;
          s.me.token = ev.token ?? s.me.token;
          if (ev.token) saveJoin({ token: ev.token });
          break;
        }
        case "snapshot": {
          // 新类型变体已拍平：ev 本身就是 BpSnapshot（+t 字段）
          const { t: _t, ...snap } = ev;
          s.snap = snap;
          if (s.me.side == null) {
            const meRow = (snap.players || []).find((p) => p.id === s.me.id);
            if (meRow && meRow.side) s.me.side = meRow.side;
          }
          derivingMasks();
          break;
        }
        case "poolUpdated": {
          if (s.snap) {
            s.snap.pool = ev.pool;
            for (const ch of s.snap.pool) {
              if (ch.status !== "available") s.masks.delete(ch.id);
            }
          }
          break;
        }
        case "phaseChanged": {
          if (s.snap) {
            const prevStep = s.snap.stepIndex;
            s.snap.stepIndex = ev.stepIndex;
            s.snap.totalSteps = ev.totalSteps;
            s.snap.current = {
              actorSide: ev.actorSide,
              action: ev.action,
              decider: ev.decider,
              isTb: ev.isTb,
            };
            if (ev.stepIndex !== prevStep) {
              // 换 step：本回合干扰渲染态失效（遮蔽/强制公开随回合过期）
              s.masks.clear();
              s.forceReveal = null;
            }
          }
          break;
        }
        case "operationConfirmed": {
          // 拍平事件 = BpOperation；本地位移渲染辅助（PoolUpdated 仍为权威覆盖）
          if (s.snap) {
            upsertOp(ev);
            const ch = s.snap.pool.find((c) => c.id === ev.chartId);
            if (ch) ch.status = ev.action === "ban" ? "banned" : "picked";
          }
          break;
        }
        case "interferenceApplied": {
          if (s.snap) {
            upsertLog(ev);
            derivingMasks();
          }
          break;
        }
        case "counterApplied": {
          if (s.snap) {
            const e = s.snap.interferenceLog.find((x) => x.id === ev.interferenceId);
            if (e) e.countered = true;
            derivingMasks();
          }
          break;
        }
        case "interferenceDenied": {
          if (s.snap && ev.kind === "forceReveal") derivingMasks();
          break;
        }
        case "timerSync": {
          if (s.snap) s.snap.timer = { deadlineTsMs: ev.deadlineTsMs, durationMs: ev.durationMs };
          break;
        }
        case "playerConnected": {
          if (s.snap) {
            const i = s.snap.players.findIndex((p) => p.id === ev.id);
            if (i >= 0) s.snap.players[i] = ev; else s.snap.players.push(ev);
          }
          break;
        }
        case "playerDisconnected": {
          if (s.snap) {
            const p = s.snap.players.find((x) => x.id === ev.playerId);
            if (p) p.connected = false;
          }
          break;
        }
        case "playerReady": {
          if (s.snap) {
            const p = s.snap.players.find((x) => x.id === ev.playerId);
            if (p) p.ready = ev.ready;
          }
          break;
        }
        case "sessionEnded": {
          // 拍平事件 = BpResult；快照不再刷新（结束态由它兜底渲染）
          if (s.snap) {
            s.snap.status = "finished";
            s.snap.timer = null;
            s.snap.current = null;
          }
          s.finished = ev;
          break;
        }
        default:
          break; // notice/hostActionResult/emote/error/operationRejected → 纯 toast 层
      }
      touch();
    },
    /** join 在途：期间收到的关键错误 → 交给 ui 决定是否退回入口 */
    beginJoin() { s.joining = true; },
    endJoin() { s.joining = false; },
  };
}

/* ---------------------------------------------------------------- DOM 助手 */
export function el(html) {
  const t = document.createElement("template");
  t.innerHTML = html.trim();
  return t.content.firstElementChild;
}
export function qs(sel, root = document) { return root.querySelector(sel); }
export function qsa(sel, root = document) { return Array.from(root.querySelectorAll(sel)); }

/** toast（提示层：不打断 BP，不模态） */
let toastTimer = null;
export function toast(message, tone = "info") {
  const old = qs("#toast");
  if (old) old.remove();
  const box = el(`<div class="toast" role="status" data-tone="${tone}">${message}</div>`);
  document.body.appendChild(box);
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => box.remove(), 2400);
}

/**
 * Bottom Sheet 确认流（BP 进行中的唯一「弹窗」形态：≤40dvh + 二次确认）。
 * 返回 onClose；build(bodyEl) 负责填内容与绑定动作。
 */
export function openSheet({ title, build }) {
  const scrim = el('<div class="sheet-scrim"></div>');
  const sheet = el(`
    <aside class="sheet" aria-label="${title}">
      <div class="sheet-handle"></div>
      <h3 class="card-title">${title}</h3>
      <div class="sheet-body"></div>
    </aside>`);
  const body = qs(".sheet-body", sheet);
  const close = () => { scrim.remove(); sheet.remove(); };
  scrim.addEventListener("click", close);
  build(body, close);
  document.body.append(scrim, sheet);
  requestAnimationFrame(() => {
    scrim.setAttribute("open", "");
    sheet.setAttribute("open", "");
  });
  return close;
}
