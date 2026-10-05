/* ============================================================================
 *  Phirc Mod++ · WebView 界面 ↔ Rust IPC 桥
 *  ---------------------------------------------------------------------------
 *  形状对齐 Tauri（`invoke` / `listen`），方便和 launcher/ui 的写法保持一致。
 *
 *  三个平台的后端自动探测：
 *    Windows (wry)  window.ipc.postMessage(json)
 *    Android        window.AndroidBridge.postMessage(json)
 *    iOS            window.webkit.messageHandlers.phira.postMessage(json)
 *
 *  Rust → JS：调用全局 `window.__phira_recv(json)`
 * ========================================================================== */
(function () {
  'use strict';

  const pending = new Map();
  const listeners = new Map();
  let seq = 0;

  /* ---------- 发送 ---------- */
  function sendRaw(json) {
    try {
      if (window.ipc && typeof window.ipc.postMessage === 'function') {
        window.ipc.postMessage(json);
        return true;
      }
      if (window.AndroidBridge && typeof window.AndroidBridge.postMessage === 'function') {
        window.AndroidBridge.postMessage(json);
        return true;
      }
      if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.phira) {
        window.webkit.messageHandlers.phira.postMessage(json);
        return true;
      }
    } catch (e) {
      console.error('[bridge] send failed', e);
    }
    return false;
  }

  /* ---------- 接收 ---------- */
  window.__phira_recv = function (json) {
    let msg;
    try {
      msg = typeof json === 'string' ? JSON.parse(json) : json;
    } catch (e) {
      console.error('[bridge] bad message', json);
      return;
    }
    if (msg.id != null && pending.has(msg.id)) {
      const p = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error != null) p.reject(new Error(String(msg.error)));
      else p.resolve(msg.result);
      return;
    }
    if (msg.event) {
      const arr = listeners.get(msg.event);
      if (arr) for (const cb of arr) { try { cb(msg.payload); } catch (e) { console.error(e); } }
    }
  };

  /* ---------- invoke ---------- */
  function invoke(cmd, args) {
    return new Promise(function (resolve, reject) {
      const id = ++seq;
      pending.set(id, { resolve: resolve, reject: reject });
      const ok = sendRaw(JSON.stringify({ id: id, cmd: cmd, args: args || {} }));
      if (!ok) {
        pending.delete(id);
        reject(new Error('IPC 桥不可用（不在 WebView 里？）'));
      }
      // 超时兜底，避免 Promise 永远挂着
      setTimeout(function () {
        if (pending.has(id)) {
          pending.delete(id);
          reject(new Error('IPC 超时：' + cmd));
        }
      }, 20000);
    });
  }

  /* ---------- listen ---------- */
  function listen(event, cb) {
    if (!listeners.has(event)) listeners.set(event, []);
    listeners.get(event).push(cb);
    return function off() {
      const arr = listeners.get(event);
      if (arr) {
        const i = arr.indexOf(cb);
        if (i >= 0) arr.splice(i, 1);
      }
    };
  }

  window.__PHIRA__ = {
    invoke: invoke,
    listen: listen,
    available: function () {
      return !!(window.ipc || window.AndroidBridge ||
                (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.phira));
    },
  };
})();
