/* ============================================================================
 *  Phirc Mod++ · WebView 界面逻辑
 *  页面：登录 / 开始 / 曲库
 * ========================================================================== */
'use strict';

const $ = (id) => document.getElementById(id);
const invoke = window.__PHIRA__.invoke;
const listen = window.__PHIRA__.listen;

/* 背景控制状态在 WebGL 初始化外定义：即使 WebGL 不可用，Rust 和按钮也能安全控制显隐。 */
let bgEnabled = true;
let bgHidden = false;
let applyBackgroundState = function () {};
window.__phiraSetBgEnabled = function (on) {
  bgEnabled = !!on;
  const canvas = $('trigrid');
  if (canvas) canvas.style.display = bgEnabled ? 'block' : 'none';
  applyBackgroundState();
};
window.__phiraSetHidden = function (hidden) {
  bgHidden = !!hidden;
  const canvas = $('trigrid');
  if (canvas) canvas.style.visibility = bgHidden ? 'hidden' : 'visible';
  applyBackgroundState();
};

/* ==========================================================================
 *  1. TriGrid 背景（WebGL2 / GLSL ES 3.00）
 *     30fps + 0.75 分辨率，只在 WebView 可见时运行；深黑灰底色上保留缓慢光波。
 * ========================================================================== */
(function initTriGrid() {
  const canvas = $('trigrid');
  const gl = canvas.getContext('webgl2');
  if (!gl) { console.warn('WebGL2 不可用，背景留黑'); return; }

  // 网格密度：值越大格子越大、越稀疏（1 = 原版密度）。
  const GRID_SCALE = 1.7;
  // 光波背景性能档：内部像素数约降至 56%，30fps；WebView 隐藏时会暂停。
  const RENDER_SCALE = 0.75;
  const MAX_FPS = 30;
  const FRAME_INTERVAL = 1000 / MAX_FPS;

  let ready = false;
  function resize() {
    canvas.width = Math.max(2, Math.round(window.innerWidth * RENDER_SCALE));
    canvas.height = Math.max(2, Math.round(window.innerHeight * RENDER_SCALE));
    gl.viewport(0, 0, canvas.width, canvas.height);
    if (ready && bgEnabled && !bgHidden && !document.hidden) draw(performance.now());
  }
  window.addEventListener('resize', resize);
  resize();

  const VS = `#version 300 es
in vec2 a_pos;
out vec2 uv;
void main() { uv = a_pos * 0.5 + 0.5; gl_Position = vec4(a_pos, 0.0, 1.0); }`;

  const FS = `#version 300 es
precision highp float;
in vec2 uv;
out vec4 out_color;
uniform sampler2D screenTexture;
uniform vec2 tex_size;
uniform float t;
void main() {
  vec2 uv2 = uv;
  vec2 w = uv2 - 0.5;
  uv2.x += pow(abs(w.y), 2.0) * (-w.x);
  uv2.x += tex_size.x / 2.0;
  vec2 new_uv = mod(uv2 / tex_size, 1.0);
  vec4 texColor = texture(screenTexture, new_uv);
  // 慢速上下浮动的光波，底色维持纯深黑灰。
  float k = 0.8, k2 = 1.6;
  float p = max(0.0, min(1.0, ((sin((uv2.y + t) * k2) - k) * (1.0 / (1.0 - k)))));
  p = 0.35 + 0.65 * p;
  vec3 base_color = vec3(0.035, 0.037, 0.040);   // 约 #09090A
  vec3 face_color = vec3(0.702, 0.945, 1.0);
  vec3 grid_color = vec3(1.0);
  vec3 glow_color = vec3(0.383, 0.884, 1.0);
  vec3 col = base_color
    + grid_color * 0.4 * texColor.r
    + face_color * 0.1 * texColor.b * p * 2.0
    + glow_color * (0.71 * texColor.g * p * 5.0);
  out_color = vec4(col, 1.0);
}`;

  function sh(type, src) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src); gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
      console.error('TriGrid 着色器编译失败:', gl.getShaderInfoLog(s));
      return null;
    }
    return s;
  }
  const prog = gl.createProgram();
  const vs = sh(gl.VERTEX_SHADER, VS), fs = sh(gl.FRAGMENT_SHADER, FS);
  if (!prog || !vs || !fs) return;
  gl.attachShader(prog, vs); gl.attachShader(prog, fs); gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
    console.error('TriGrid 链接失败:', gl.getProgramInfoLog(prog));
    return;
  }
  gl.useProgram(prog);

  const buf = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buf);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
  const loc = gl.getAttribLocation(prog, 'a_pos');
  gl.enableVertexAttribArray(loc);
  gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);

  // tex_size 越小重复越多、网格越密；乘 GRID_SCALE 即整体变稀疏
  gl.uniform2f(gl.getUniformLocation(prog, 'tex_size'), 0.22 * GRID_SCALE, 0.27 * GRID_SCALE);
  const u_t = gl.getUniformLocation(prog, 't');

  const tex = gl.createTexture();
  const img = new Image();
  img.onload = function () {
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, img);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.activeTexture(gl.TEXTURE0);
    gl.uniform1i(gl.getUniformLocation(prog, 'screenTexture'), 0);
    ready = true;
    applyBackgroundState(); // 图片就绪后再开始限帧的浮动背景
  };
  img.onerror = function () { console.warn('TriGridBase.png 未找到，背景留黑'); };
  img.src = './assets/TriGridBase.png';

  const t0 = performance.now();
  let rafId = 0;
  let lastDraw = 0;

  function draw(now) {
    gl.uniform1f(u_t, (now - t0) / 1000);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }

  function frame(now) {
    rafId = 0;
    if (!ready || !bgEnabled || bgHidden || document.hidden) return;
    if (now - lastDraw >= FRAME_INTERVAL) {
      lastDraw = now;
      draw(now);
    }
    rafId = requestAnimationFrame(frame);
  }

  function start() {
    if (ready && bgEnabled && !bgHidden && !document.hidden && !rafId) {
      lastDraw = 0;
      rafId = requestAnimationFrame(frame);
    }
  }

  function stop() {
    if (rafId) {
      cancelAnimationFrame(rafId);
      rafId = 0;
    }
  }

  applyBackgroundState = function () {
    if (bgEnabled && !bgHidden && !document.hidden) start();
    else stop();
  };
  document.addEventListener('visibilitychange', applyBackgroundState);
})();

/* ==========================================================================
 *  2. 世界坐标单位（和游玩 HUD 同一套）
 * ========================================================================== */
function syncUnits() {
  const w = Math.max(1, window.innerWidth);
  const h = Math.max(1, window.innerHeight);
  const aspect = w / h;
  const s = document.documentElement.style;
  s.setProperty('--gu', (50 * aspect).toFixed(4) + 'vh');
  s.setProperty('--gux', '50vw');
  s.setProperty('--gs', 'calc(' + (50 * aspect).toFixed(4) + 'vh * 0.0764)');
}
window.addEventListener('resize', syncUnits);
syncUnits();

/* ==========================================================================
 *  3. 提示条
 * ========================================================================== */
let toastTimer = 0;
function toast(msg, isErr) {
  const el = $('toast');
  el.textContent = msg;
  el.classList.toggle('err', !!isErr);
  el.classList.add('on');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove('on'), 2600);
}

/* ==========================================================================
 *  3.5 动态背景开关（状态持久化在 data.json；关掉可明显省 GPU、界面更跟手）
 * ========================================================================== */
function applyBg(enabled) {
  bgEnabled = !!enabled;
  const btn = $('bg-toggle');
  btn.classList.toggle('off', !bgEnabled);
  $('bg-toggle-text').textContent = bgEnabled ? '背景 开' : '背景 关';
  if (window.__phiraSetBgEnabled) window.__phiraSetBgEnabled(bgEnabled);
}

$('bg-toggle').addEventListener('click', async function () {
  const next = !bgEnabled;
  applyBg(next);                       // 先本地生效，点下去立刻有反馈
  try {
    await invoke('set_bg', { enabled: next });
    toast(next ? '背景已开启（已保存）' : '背景已关闭（已保存）');
  } catch (e) {
    toast('已切换，但保存失败：' + (e.message || e), true);
  }
});

/* ==========================================================================
 *  4. 页面切换
 * ========================================================================== */
let currentPage = '';
function goto(page) {
  if (page === currentPage) return;
  const next = $('page-' + page);
  if (!next) return;
  const previous = $('page-' + currentPage);
  // 只改动当前 / 目标两个节点，不遍历所有页面；避免频繁布局扫描。
  if (previous) previous.classList.remove('active');
  next.classList.add('active');
  currentPage = page;
  // 告诉 Rust 当前在哪个界面（用于决定是否渲染游戏画面 / 是否显示 WebView）
  invoke('ui_page_changed', { page: page }).catch(function () {});
}

/* ==========================================================================
 *  5. 登录
 * ========================================================================== */
let me = null;

async function refreshMe() {
  try {
    me = await invoke('get_me');
  } catch (e) {
    me = null;
  }
  const nameEl = $('lib-user-name');
  const rksEl = $('lib-user-rks');
  if (me) {
    nameEl.textContent = me.name || ('#' + me.id);
    rksEl.textContent = me.rks != null ? ('RKS ' + Number(me.rks).toFixed(2)) : '';
  } else {
    nameEl.textContent = '未登录';
    rksEl.textContent = '';
  }
  return me;
}

$('login-submit').addEventListener('click', async function () {
  const user = $('login-user').value.trim();
  const pass = $('login-pass').value;
  const err = $('login-err');
  if (!user || !pass) { err.textContent = '请填写用户名和密码'; return; }
  err.textContent = '';
  const btn = this;
  btn.disabled = true;
  btn.textContent = '登录中…';
  try {
    const r = await invoke('login', { username: user, password: pass });
    if (r && r.ok) {
      toast('登录成功');
      await refreshMe();
      goto('home');
    } else {
      err.textContent = (r && r.error) || '登录失败';
    }
  } catch (e) {
    err.textContent = e.message || '登录失败';
  } finally {
    btn.disabled = false;
    btn.textContent = '登 录';
  }
});

$('login-pass').addEventListener('keydown', function (e) {
  if (e.key === 'Enter') $('login-submit').click();
});

$('login-skip').addEventListener('click', function () {
  toast('已跳过登录，仅显示本地谱面');
  goto('home');
});

/* ==========================================================================
 *  6. 开始界面
 * ========================================================================== */
$('btn-start').addEventListener('click', function () { goto('library'); loadLibrary(); });
$('btn-last').addEventListener('click', function () {
  invoke('start_last_chart').then(function (r) {
    if (r && r.ok) toast('启动上次谱面…');
    else toast('没有上次游玩记录', true);
  }).catch(function (e) { toast(e.message, true); });
});
$('btn-settings').addEventListener('click', function () { openSettings(); });
$('btn-activity').addEventListener('click', function () { invoke('open_rust_page', { page: 'event' }); });
$('btn-res').addEventListener('click', function () { openRespacks(); });

/* ==========================================================================
 *  7. 曲库
 * ========================================================================== */
const CATS = { local: -1, ranked: 0, special: 1, unstable: 2, popular: -2 };
let libCat = 'local';
let libOrder = 'updated';
let libOrderRev = true;
let libSearch = '';
let libBusy = false;

document.querySelectorAll('#lib-strip .lib-tab').forEach(function (tab) {
  tab.addEventListener('click', function () {
    document.querySelectorAll('#lib-strip .lib-tab').forEach((t) => t.classList.remove('on'));
    tab.classList.add('on');
    libCat = tab.dataset.cat;
    loadLibrary();
  });
});

document.querySelectorAll('#lib-chips .lib-chip[data-order]').forEach(function (chip) {
  chip.addEventListener('click', function () {
    document.querySelectorAll('#lib-chips .lib-chip[data-order]').forEach((c) => c.classList.remove('on'));
    chip.classList.add('on');
    libOrder = chip.dataset.order;
    loadLibrary();
  });
});

$('lib-orderdir').addEventListener('click', function () {
  libOrderRev = !libOrderRev;
  this.textContent = libOrderRev ? '↓' : '↑';
  loadLibrary();
});

let searchTimer = 0;
$('lib-search').addEventListener('input', function (e) {
  clearTimeout(searchTimer);
  const v = e.target.value.trim();
  searchTimer = setTimeout(function () { libSearch = v; loadLibrary(); }, 220);
});

$('lib-back').addEventListener('click', function () { goto('home'); });

function localCoverUrl(path) {
  // UTF-8 → URL-safe Base64；路径只进自定义协议参数，不暴露本地磁盘绝对路径。
  const bytes = new TextEncoder().encode(path);
  let binary = '';
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
  const token = btoa(binary).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/g, '');
  return './local-cover?p=' + token;
}

function renderCharts(list) {
  const grid = $('lib-grid');
  grid.textContent = '';
  if (!list || !list.length) {
    const empty = document.createElement('div');
    empty.className = 'lib-empty';
    empty.textContent = '没有曲目';
    grid.appendChild(empty);
    return;
  }
  const frag = document.createDocumentFragment();
  list.forEach(function (c, i) {
    const card = document.createElement('div');
    card.className = 'song-card';
    // 入场错峰：只给前 24 张排延迟，后面的统一 0，避免长列表拖出一串长延迟
    card.style.animationDelay = (i < 10 ? i * 10 : 0) + 'ms';

    // 直接用 createElement 建 DOM，不再拼 innerHTML 字符串让浏览器去解析 ——
    // 曲目多时（上百张）这一项在切页时是很明显的一笔开销。
    const initials = String(c.name || '?').replace(/[^A-Za-z0-9]/g, '').slice(0, 2).toUpperCase() || 'PH';
    const ph = document.createElement('div');
    ph.className = 'song-cover-ph';
    ph.textContent = initials;
    card.appendChild(ph);

    // 在线谱面用服务器曲绘；本地谱面走 webui://local-cover，Rust 从谱包提取真实曲绘。
    const cover = c.illustration || (c.path ? localCoverUrl(c.path) : '');
    if (cover) {
      const img = document.createElement('img');
      img.className = 'song-cover';
      img.loading = 'lazy';
      img.decoding = 'async';
      img.alt = (c.name || '谱面') + ' 曲绘';
      img.onload = function () {
        card.classList.add('has-cover');
        ph.style.display = 'none';
      };
      img.onerror = function () {
        img.remove();
        card.classList.remove('has-cover');
        ph.style.display = 'flex';
      };
      img.src = cover;
      card.appendChild(img);
    }

    const shade = document.createElement('div');
    shade.className = 'song-shade';
    card.appendChild(shade);

    const diff = c.level ? ((c.difficulty || '') + ' ' + c.level) : (c.difficulty || '');
    if (diff) {
      const d = document.createElement('div');
      d.className = 'song-diff';
      d.textContent = diff;
      card.appendChild(d);
    }
    const typeTag = document.createElement('div');
    typeTag.className = 'song-type';
    typeTag.textContent = c.path ? '本地谱面' : '在线谱面';
    card.appendChild(typeTag);

    const title = document.createElement('div');
    title.className = 'song-title';
    title.textContent = c.name || '未命名';
    card.appendChild(title);

    const sub = document.createElement('div');
    sub.className = 'song-sub';
    sub.textContent = c.composer || c.charter || '';
    card.appendChild(sub);

    // 点卡片先进 Web 版的谱面预选页，真正开玩由那一页的「开始游玩」触发
    card.addEventListener('click', function () { openSong(c); });
    frag.appendChild(card);
  });
  grid.appendChild(frag);
}

async function loadLibrary() {
  if (libBusy) return;
  libBusy = true;
  const grid = $('lib-grid');
  grid.innerHTML = '<div class="lib-loading">加载中…</div>';
  try {
    const cmd = libCat === 'local' ? 'get_local_charts' : 'get_online_charts';
    const args = libCat === 'local'
      ? { search: libSearch }
      : {
          type: CATS[libCat],
          search: libSearch,
          order: (libOrderRev ? '-' : '') + libOrder,
          page: 0,
        };
    const list = await invoke(cmd, args);
    renderCharts(list);
  } catch (e) {
    grid.innerHTML = '<div class="lib-empty">' + (e.message || '加载失败') + '</div>';
  } finally {
    libBusy = false;
  }
}

/* ==========================================================================
 *  7.5 谱面预选（原来这里会直接跳 Rust 的 SongScene，现在由 Web 界面承担）
 * ========================================================================== */
let songCur = null;

function songInitials(name) {
  return String(name || '?').replace(/[^A-Za-z0-9]/g, '').slice(0, 2).toUpperCase() || 'PH';
}

const MOD_OPTIONS = [
  ['flipX', '镜像', '左右翻转谱面'],
  ['fadeIn', '淡入', '音符淡入出现'],
  ['fadeOut', '淡出', '音符淡出消失'],
  ['nightcore', '夜莺', '提高音调与播放速度'],
  ['rainbow', '彩虹', '彩色判定线效果'],
  ['noShader', '无特效', '关闭着色器效果'],
  ['instantDeathAp', 'AP即死', '出现非Perfect判定即结束'],
  ['instantDeathFc', 'FC即死', '出现Miss即结束'],
  ['noComboScore', '无连击分', '分数只按准确率计算'],
  ['fullScreenJudge', '全屏判定', '全屏范围内均可判定'],
  ['healthMode', '血条模式', '根据判定恢复或扣除血量'],
];
let selectedSongMods = new Set();
let gameSettings = {};
let settingsReturnPage = 'home';
const SETTINGS_KEYS = ['autoplay', 'showAcc', 'showAvgFps', 'judgeText', 'noteTrail', 'disableEffect', 'particle'];
const SETTING_LABELS = {
  autoplay: '自动游玩', showAcc: '显示 ACC', showAvgFps: '显示平均帧率',
  judgeText: '判定文字', noteTrail: '音符拖影', disableEffect: '简化打击特效', particle: '粒子特效',
};

function openSong(c) {
  songCur = c;
  selectedSongMods = new Set((gameSettings.mods || []).filter((id) => MOD_OPTIONS.some((m) => m[0] === id)));
  renderSongMods();
  $('song-name').textContent = c.name || '未命名';
  $('song-by').textContent = [c.composer, c.charter].filter(Boolean).join(' / ') || '未知';
  $('song-composer').textContent = c.composer || '-';
  $('song-charter').textContent = c.charter || '-';
  const lv = ((c.difficulty || '') + (c.level ? ' ' + c.level : '')).trim() || '-';
  $('song-level').textContent = lv;
  $('song-level2').textContent = lv;

  const img = $('song-cover'), ph = $('song-cover-ph');
  if (c.illustration) {
    img.src = c.illustration;
    img.style.display = 'block';
    ph.style.display = 'none';
  } else {
    img.removeAttribute('src');
    img.style.display = 'none';
    ph.style.display = 'flex';
    ph.textContent = songInitials(c.name);
  }
  goto('song');
}

function renderSongMods() {
  const grid = $('song-mod-grid');
  grid.textContent = '';
  MOD_OPTIONS.forEach(function ([id, title, desc]) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'mod-option' + (selectedSongMods.has(id) ? ' on' : '');
    button.setAttribute('aria-pressed', selectedSongMods.has(id) ? 'true' : 'false');
    const name = document.createElement('strong');
    name.textContent = title;
    const help = document.createElement('span');
    help.textContent = desc;
    button.append(name, help);
    button.addEventListener('click', function () {
      if (selectedSongMods.has(id)) selectedSongMods.delete(id);
      else {
        const conflict = { fadeIn: 'fadeOut', fadeOut: 'fadeIn', instantDeathAp: 'instantDeathFc', instantDeathFc: 'instantDeathAp' }[id];
        if (conflict) selectedSongMods.delete(conflict);
        selectedSongMods.add(id);
      }
      renderSongMods();
    });
    grid.appendChild(button);
  });
}

function syncSettingControls() {
  const pairs = [
    ['speed', 'set-speed', 'set-speed-value', (v) => Math.round(v * 100) + '%'],
    ['offset', 'set-offset', 'set-offset-value', (v) => Math.round(v) + ' ms'],
    ['noteScale', 'set-note-scale', 'set-note-scale-value', (v) => Math.round(v * 100) + '%'],
    ['volumeMusic', 'set-volume-music', 'set-volume-music-value', (v) => Math.round(v * 100) + '%'],
    ['volumeSfx', 'set-volume-sfx', 'set-volume-sfx-value', (v) => Math.round(v * 100) + '%'],
    ['volumeBgm', 'set-volume-bgm', 'set-volume-bgm-value', (v) => Math.round(v * 100) + '%'],
  ];
  pairs.forEach(function ([key, inputId, valueId, fmt]) {
    const input = $(inputId), value = $(valueId);
    if (!input || !value || gameSettings[key] == null) return;
    const multiplier = key.startsWith('volume') ? 100 : (key === 'speed' || key === 'noteScale' ? 100 : 1);
    input.value = String(Math.round(gameSettings[key] * multiplier));
    value.textContent = fmt(gameSettings[key]);
  });
  $('song-speed').value = String(Math.round((gameSettings.speed || 1) * 100));
  $('song-speed-value').textContent = Math.round((gameSettings.speed || 1) * 100) + '%';
  $('song-offset').value = String(Math.round(gameSettings.offset || 0));
  $('song-offset-value').textContent = Math.round(gameSettings.offset || 0) + ' ms';
  document.querySelectorAll('.setting-toggle[data-setting]').forEach(function (button) {
    const key = button.dataset.setting;
    const on = !!gameSettings[key];
    button.classList.toggle('on', on);
    button.setAttribute('aria-pressed', on ? 'true' : 'false');
    button.textContent = (on ? '已开启 · ' : '已关闭 · ') + SETTING_LABELS[key];
  });
}

async function loadSettings() {
  try {
    gameSettings = await invoke('get_settings');
    syncSettingControls();
  } catch (e) {
    toast(e.message || '读取设置失败', true);
  }
}

async function saveSettingsPatch(patch) {
  Object.assign(gameSettings, patch);
  syncSettingControls();
  const state = $('settings-save-state');
  if (state) state.textContent = '保存中';
  try {
    await invoke('set_settings', { patch: patch });
    if (state) state.textContent = '已同步';
  } catch (e) {
    if (state) state.textContent = '保存失败';
    toast(e.message || '设置保存失败', true);
  }
}

function openSettings() {
  settingsReturnPage = currentPage || 'home';
  loadSettings().finally(function () { goto('settings'); });
}
$('settings-back').addEventListener('click', function () { goto(settingsReturnPage); });
$('song-settings-open').addEventListener('click', openSettings);

const settingInputs = [
  ['set-speed', 'speed', 100], ['set-offset', 'offset', 1], ['set-note-scale', 'noteScale', 100],
  ['set-volume-music', 'volumeMusic', 100], ['set-volume-sfx', 'volumeSfx', 100], ['set-volume-bgm', 'volumeBgm', 100],
  ['song-speed', 'speed', 100], ['song-offset', 'offset', 1],
];
settingInputs.forEach(function ([inputId, key, multiplier]) {
  const input = $(inputId);
  input.addEventListener('input', function () {
    const value = Number(input.value) / multiplier;
    gameSettings[key] = value;
    syncSettingControls();
  });
  input.addEventListener('change', function () {
    saveSettingsPatch({ [key]: Number(input.value) / multiplier });
  });
});

document.querySelectorAll('.setting-toggle[data-setting]').forEach(function (button) {
  button.addEventListener('click', function () {
    const key = button.dataset.setting;
    saveSettingsPatch({ [key]: !gameSettings[key] });
  });
});

$('song-back').addEventListener('click', function () { goto('library'); });

$('song-play').addEventListener('click', function () {
  if (!songCur) return;
  // autoPlay=true：本地谱面交给 SongScene 直接开玩，不再弹它自己那屏预选界面；
  // 在线谱面此时还没有本地路径，SongScene 会走原本的「下载 → 游玩」流程。
  invoke('start_chart', {
    id: songCur.id || null,
    path: songCur.path || null,
    autoPlay: true,
    mods: Array.from(selectedSongMods),
  })
    .then(function (r) {
      if (!r || !r.ok) toast((r && r.error) || '无法启动该谱面', true);
    })
    .catch(function (e) { toast(e.message, true); });
});

/* ==========================================================================
 *  7.6 资源包管理（只在浏览/选择资源包时隐藏当前页状态，不跳旧 Rust 页面）
 * ========================================================================== */
function openRespacks() {
  settingsReturnPage = currentPage || 'home';
  loadRespacks().finally(function () { goto('respacks'); });
}
$('respacks-back').addEventListener('click', function () { goto(settingsReturnPage); });
$('respack-import').addEventListener('click', async function () {
  try {
    await invoke('import_respack');
    toast('请选择资源包 ZIP 文件');
  } catch (e) {
    toast(e.message || '无法打开资源包选择器', true);
  }
});

function renderRespackPreview(pack) {
  $('respack-preview-author').textContent = pack.author ? '作者 · ' + pack.author : '';
  $('respack-description').textContent = pack.description || '暂无介绍';
  const holder = $('respack-preview-notes');
  holder.textContent = '';
  if (!pack.preview) {
    const note = document.createElement('div');
    note.className = 'preview-empty';
    note.textContent = '内置资源包';
    holder.appendChild(note);
    return;
  }
  ['click.png', 'drag.png', 'flick.png', 'hold.png'].forEach(function (file) {
    const item = document.createElement('div');
    item.className = 'preview-note-item';
    const img = document.createElement('img');
    img.alt = file.replace('.png', '');
    img.src = './respack-preview?index=' + encodeURIComponent(pack.id) + '&file=' + encodeURIComponent(file);
    const label = document.createElement('span');
    label.textContent = file.replace('.png', '').toUpperCase();
    item.append(img, label);
    holder.appendChild(item);
  });
}

async function loadRespacks() {
  try {
    const result = await invoke('get_respacks');
    const packs = result.packs || [];
    const list = $('respack-list');
    list.textContent = '';
    packs.forEach(function (pack) {
      const card = document.createElement('article');
      card.className = 'respack-card' + (pack.selected ? ' selected' : '');
      const preview = document.createElement('div');
      preview.className = 'respack-card-art';
      if (pack.preview) {
        const img = document.createElement('img');
        img.loading = 'lazy';
        img.alt = '';
        img.src = pack.preview;
        preview.appendChild(img);
      } else {
        preview.innerHTML = '<span class="art-line"></span><span class="art-note"></span><span class="art-glow"></span>';
      }
      const body = document.createElement('div');
      body.className = 'respack-card-body';
      const title = document.createElement('strong');
      title.textContent = pack.name || '未命名资源包';
      const author = document.createElement('span');
      author.textContent = pack.author || 'Phira';
      const desc = document.createElement('p');
      desc.textContent = pack.description || '音符贴图、打击特效与按键音';
      const action = document.createElement('button');
      action.className = 'respack-select';
      action.textContent = pack.selected ? '当前使用中' : '应用此资源包';
      action.disabled = !!pack.selected;
      action.addEventListener('click', async function () {
        try {
          await invoke('set_respack', { id: pack.id });
          await loadRespacks();
          toast('资源包已应用');
        } catch (e) { toast(e.message || '资源包应用失败', true); }
      });
      body.append(title, author, desc, action);
      card.append(preview, body);
      card.addEventListener('mouseenter', function () { renderRespackPreview(pack); });
      card.addEventListener('focusin', function () { renderRespackPreview(pack); });
      list.appendChild(card);
      if (pack.selected) renderRespackPreview(pack);
    });
  } catch (e) {
    $('respack-list').textContent = e.message || '读取资源包失败';
  }
}

/* ==========================================================================
 *  8. Rust → JS 事件
 * ========================================================================== */
listen('toast', function (p) { if (p && p.msg) toast(p.msg, !!p.err); });
listen('goto', function (p) { if (p && p.page) goto(p.page); });
listen('me', function () { refreshMe(); });
listen('respacks_changed', function (p) {
  if (currentPage === 'respacks') loadRespacks();
  if (p && p.ok === true) toast('资源包已导入');
  else if (p && p.ok === false) toast('资源包导入失败', true);
});

/* ==========================================================================
 *  9. 启动
 * ========================================================================== */
(async function boot() {
  // 按 Esc 返回上一页（游玩由 Rust 侧处理）
  document.addEventListener('keydown', function (e) {
    if (e.key !== 'Escape') return;
    if (currentPage === 'settings' || currentPage === 'respacks') goto(settingsReturnPage);
    else if (currentPage === 'song') goto('library');
    else if (currentPage === 'library') goto('home');
  });

  if (!window.__PHIRA__.available()) {
    toast('未检测到 IPC 桥（界面在浏览器里独立打开？）', true);
  }

  try {
    await refreshMe();
  } catch (e) { /* 忽略 */ }

  // 读回上次的背景与打歌设置（持久化在 data.json）。
  try {
    const r = await invoke('get_bg');
    applyBg(r && r.enabled !== false);
  } catch (e) { /* 读不到就保持默认开 */ }
  await loadSettings();

  // 已登录 → 直接进开始界面；未登录 → 停在登录页
  if (me) goto('home');
  else goto('login');
})();
