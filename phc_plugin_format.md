# `.phc` 插件格式与 API 规范

> 项目：PhiRa-Mod-Meew-11 · 曲库页（`kaichang/library.html`）插件系统
> 版本：phc 1 · 2026-10-16（**支持 JS / Python / Rust 三种运行时**）
> 入口：曲库页**左上角**的 🧩 按钮 → 「导入 .phc」

---

## 1. 这是什么

`.phc` 是给曲库页用的**扩展包**。用户导入后，插件可以：

- 往曲库顶栏**加按钮**（例如「随机开局」）
- 在**每张卡片**上做装饰（加标签、改边框、加角标）
- 监听**列表刷新**，做统计 / 提示 / 二次处理
- 读写**自己命名空间的存储**，把偏好持久化

插件运行在曲库页面里（WebView 的 JS 环境），能调用宿主提供的受限 API。

---

## 2. 两种文件形态

| 形态 | 结构 | 适用 |
|---|---|---|
| **ZIP**（推荐） | 根目录（或单一顶层文件夹）下有 `manifest.json`，脚本放 `entry` 指定的文件（默认 `main.js`），可带任意资源 | 有脚本文件 / 有图片等资源 |
| **纯 JSON** | 一个 JSON 文件，就是 manifest，脚本写在 `"script"` 字段里 | 小插件，手写/分享最方便 |

导入时按文件头自动识别：以 `PK` 开头当 ZIP，否则当 JSON。两种都可以直接把扩展名写成 `.phc`。

---

## 3. `manifest.json` 字段

| 字段 | 必填 | 说明 |
|---|---|---|
| `phc` | 建议 | 格式版本，当前为 `1` |
| `id` | **必填** | 唯一标识，只允许 `A-Za-z0-9._-`，最长 64。会当目录名用，**也是目录穿越防护点** |
| `name` | 建议 | 显示名（缺省用 `id`） |
| `version` | 建议 | 版本号，纯展示 |
| `author` | 可选 | 作者 |
| `description` | 可选 | 一句话说明，显示在插件列表里 |
| `runtime` | 可选 | `js`（默认） / `python` / `rust`。见 §4 |
| `entry` | 可选 | ZIP 形态的入口文件，默认 `main.js`（python 常写 `main.py`，rust 写 `xxx.exe`） |
| `script` | 可选 | **纯 JSON 形态**的脚本正文（仅 `runtime = js`）；若同时存在，优先用它 |
| `hooks` | 可选 | 进程型插件想订阅的事件数组，默认 `["ready","charts"]`。要 `cardRender` 必须显式声明 |

示例（纯 JSON 形态，最小可用）：

```json
{
  "phc": 1,
  "id": "com.example.hello",
  "name": "你好插件",
  "version": "1.0.0",
  "author": "无名",
  "description": "点按钮弹一句话",
  "script": "PhiraPlugin.register({ id:'com.example.hello', name:'你好插件', onLoad:function(api){ api.addToolbarButton({ label:'打个招呼', onClick:function(){ api.toast('你好！'); } }); } });"
}
```

---

## 4. 三种运行时

| `runtime` | 怎么跑 | 适合 |
|---|---|---|
| `js`（默认） | 脚本读进曲库页面里用 `new Function` 执行，直接操作 DOM | 轻量、要精细改 DOM |
| `python` | 宿主起一个子进程 `python <entry>`，走 JSON-lines 协议 | 想用 Python 生态（numpy 等） |
| `rust` | 宿主直接跑 `<entry>`（编译好的 **.exe**），同一套 JSON-lines 协议 | 想要性能 / 单文件分发 |

**关键点：Python 和 Rust 走的是同一套「旁挂进程」模型** —— 插件就是一个普通子进程，
用 stdin/stdout 跟宿主对话。好处：

- **不需要 FFI / ABI**：Rust 插件就是普通可执行文件，不用管 dylib 加载与 ABI 稳定；
- **崩溃隔离**：插件崩了只死自己，游戏和别的插件不受影响（面板里会显示进程已退出）；
- **语言无关**：任何能读写 stdin/stdout 的语言都能写插件。

### 4.1 进程型插件的启动方式
- `python`：宿主按 `PHC_PYTHON` 环境变量 → `python` → `python3` → `py -3` 的顺序找解释器；
  找不到会在插件面板报「没有找到 Python 解释器」。工作目录 = 插件目录。
- `rust`：直接执行 `entry` 指向的文件（Windows 上是 `.exe`）。
- 两者都以 `CREATE_NO_WINDOW` 启动（不会弹黑框），并注入环境变量 `PHC_PLUGIN_ID` / `PHC_PLUGIN_DIR`。

### 4.2 协议（每行一个 JSON）
宿主 → 插件（stdin）：

| 事件 | 载荷 | 时机 |
|---|---|---|
| `init` | `{ id, dir, api:1, storage:{…} }` | 进程刚启动，**第一件事**。`storage` 是该插件上次存的值 |
| `ready` | `{ plugins:[{id,name,version,runtime}] }` | 所有插件都加载完 |
| `charts` | `{ cat, total, list:[…] }` | 每次列表渲染完 |
| `cardRender` | `{ index, chart }` | 每张卡（**需在 `hooks` 里声明**）。`index` 可回给 `badge` |
| `click` | `{ button }` | 用户点了该插件通过 `addToolbarButton` 加的按钮 |
| `shutdown` | — | 宿主准备收掉进程（收到后应尽快退出） |

插件 → 宿主（stdout）：

| 命令 | 载荷 | 效果 |
|---|---|---|
| `log` | `{ msg }` | 写进 `phira-crash.log` |
| `toast` | `{ msg }` | 右下角提示 |
| `addToolbarButton` | `{ id, label, title }` | 顶栏加按钮；点击回推 `click{button:id}` |
| `badge` | `{ index, text, color }` | 给第 `index` 张卡加角标（`cardRender` 里拿到 `index`） |
| `storageSet` | `{ key, value }` | 持久化（落 `plugins/storage.json`，按插件隔离） |
| `startGame` | `{ path }` | 开一局（path 用 `charts` 事件里的） |
| `openPage` / `openUrl` | `{ name }` / `{ url }` | 打开原生页 / 浏览器 |
| `reload` | — | 让宿主重拉曲库列表 |

> 插件的 **stderr** 会原样写进 `phira-crash.log`（前缀 `[plugin:<id>]`），调试时很有用。
> stdout 只放协议 JSON，别混别的输出。

### 4.3 Python 插件最小骨架

```python
import json, sys
def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n"); sys.stdout.flush()

for raw in sys.stdin:
    raw = raw.strip()
    if not raw: continue
    msg = json.loads(raw)
    ev = msg.get("event")
    if ev == "init":
        send({"cmd": "addToolbarButton", "id": "hi", "label": "打个招呼"})
    elif ev == "click":
        send({"cmd": "toast", "msg": "你好，我是 Python 插件"})
    elif ev == "shutdown":
        break
```

### 4.4 Rust 插件最小骨架

```rust
// Cargo.toml: [dependencies] serde_json = "1"
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn main() {
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else { continue };
        match msg["event"].as_str().unwrap_or("") {
            "init" => {
                println!("{}", json!({"cmd":"addToolbarButton","id":"hi","label":"打个招呼"}));
                io::stdout().flush().ok();
            }
            "click" => {
                println!("{}", json!({"cmd":"toast","msg":"你好，我是 Rust 插件"}));
                io::stdout().flush().ok();
            }
            "shutdown" => break,
            _ => {}
        }
    }
}
```
编译成 exe 后，`entry` 指向它即可。

---

## 4b. JS 运行时脚本结构

插件脚本执行时会拿到一个全局 `PhiraPlugin`，**约定调用一次 `register`**：

```js
PhiraPlugin.register({
  id: 'com.example.demo',   // 与 manifest 的 id 一致
  name: '演示',
  version: '1.0.0',
  onLoad: function (api) {
    // 在这里注册按钮 / 钩子
  }
});
```

脚本被包在函数里执行，**内部变量不会污染页面作用域**。脚本抛出的异常会被捕获并显示在插件面板的红色报错框里，**不会影响其它插件和曲库本身**。

---

## 5. `api` 参考

| 成员 | 说明 |
|---|---|
| `api.id` / `api.name` / `api.version` | 当前插件的元信息 |
| `api.toast(msg)` | 右下角提示（宿主统一样式） |
| `api.log(...args)` | 写进 `phira-crash.log`，前缀 `[plugin:<id>]` |
| `api.addToolbarButton({ label, title, onClick })` | 在曲库**顶栏右侧**加一个按钮；`label` 是按钮文字，`title` 是悬停提示 |
| `api.on(event, fn)` | 注册钩子，见 §6 |
| `api.cards()` | 当前页的谱面数组（`{ name, composer, charter, level, difficulty, path, cover, local, id? }`） |
| `api.reload()` | 重新拉取曲库列表并重渲染 |
| `api.bridge` | 原始 IPC 桥（`window.phirc`）。可用 `listCharts` / `startGame` / `listOnlineCharts` / `startOnlineChart` / `openPage` / `openTool` / `openUrl` / `getCustomRks` / `listPlugins` … |
| `api.storage.get(key, def)` / `.set(key, val)` / `.remove(key)` | **按插件 id 命名空间隔离**的 localStorage（`val` 走 JSON 序列化） |

> ⚠️ 桥的 promise 是「解包」的：成功直接给 `data`，失败 **reject**。所以用 `api.bridge.startGame(p).catch(e => …)`，不要读 `res.ok`。

---

## 6. 事件（`api.on`）

| 事件 | 回调参数 | 触发时机 |
|---|---|---|
| `ready` | `(plugins)` | 所有启用插件加载完毕后触发一次 |
| `charts` | `(list, info)` | 每次曲库列表渲染完。`info = { cat, total }`（`cat` 为 `local`/`ranked`/`special`/`unstable`/`popular`） |
| `cardRender` | `(el, chart)` | 每张卡片元素插入 DOM 后。`el` 是卡片根元素，可往里加装饰；`chart` 是该谱面数据 |

---

## 7. 安装位置与启停

- 导入后落盘到 **`<数据目录>/plugins/<id>/`**（数据目录 = `rev/phira-pc/data`，即 `data.json` 所在处）。
- 启用状态存在同目录 `state.json`（`{"<id>": true}`），**新导入的插件默认启用**。
- 插件面板里可以**停用 / 启用 / 删除**；停用只影响加载，不删文件。
- 重复导入同一个 `id` 会**覆盖**旧版本。

---

## 8. 示例插件

仓库里放了两个可直接导入的样例（`kaichang/plugins/`）：

| 文件 | 运行时 | 做什么 | 演示了什么 |
|---|---|---|---|
| `random-pick.phc` | JS（纯 JSON） | 顶栏加「随机开局」按钮，随机挑一张本地谱面开打 | `addToolbarButton` / `storage` / `charts` / `bridge.startGame` |
| `hard-mark.phc` | JS（ZIP） | 给难度 ≥ 15 的卡片加红色 `HARD` 标签 | `cardRender` / ZIP 形态 |
| `py-stats.phc` | **Python** | 顶栏加「统计」按钮，点一下报当前页难度分布 | `runtime=python` / 协议 / `storageSet` / `toast` |
| `rs-stats.phc` | **Rust**（内含 exe） | 顶栏加「谱面数」按钮显示张数与平均难度；给 ≥16 的卡打 `RS16+` 紫标 | `runtime=rust` / `badge` / `storage` |

自测流程：曲库页 → 左上角 🧩 → 「导入 .phc」→ 选一个样例 → 顶栏应出现「随机开局」/ 卡片上应出现 `HARD` 标签。

---

## 9. 边界与注意

1. **单张卡片请用内联样式或自己注入 `<style>`**，不要依赖宿主页面的 Tailwind 类名（宿主用的是运行时 JIT，动态新增的类名不保证立刻可用）。
2. 插件运行在页面里，**理论上能做页面能做的任何事**（包括调用 `api.bridge`）。只导入你信任的 `.phc`。
3. 单插件脚本异常不影响其它插件；报错会在面板里列出。
4. `id` 是目录名，**不要包含 `/`、`..`、`\`、`:`** —— 宿主会拒绝非法 id。
5. 插件脚本在**每次进入曲库页**时重新执行，不要在 `onLoad` 里做重活。
6. 目前只支持桌面端导入（文件框是同步 rfd）；移动端需要另做异步通道。
7. **进程型插件需要外部依赖**：`python` 要机器上有 Python 3（或用 `PHC_PYTHON` 指定）；`rust` 插件得自带编译好的 exe（体积会大一些）。
8. 进程型插件的 **stdout 只能输出协议 JSON**；调试信息请走 stderr（会被记进日志）。
9. 离开曲库页时宿主会向所有进程型插件发 `shutdown` 并结束进程，别在插件里起无法回收的后台任务。
