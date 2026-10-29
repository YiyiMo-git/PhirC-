# 第九章地图「曲绘不显示」——问题总结与 ChatGPT 交接提示词

> 生成时间：2026-10-29。把下面「提示词」整段复制给 ChatGPT 即可继续排查。
> 如果 ChatGPT 需要源码，按「关键文件」清单把对应文件内容贴给它。

---

## 提示词（复制这段给 ChatGPT）

```text
我在开发一个 Phigros 音乐游戏的修改版（Rust 引擎 + Windows/Android 双端，Android 壳是
Java WebView，桌面是 wry WebView2）。我给游戏加了一个「第九章地图」选曲界面：一个纯前端
的地图应用（ES modules，24 个 js），由游戏内置的本地 HTTP 静态服务（http://127.0.0.1:端口）
托管；页面通过注入的 window.phirc 桥（postMessage IPC）与 Rust 通信。

适配层 adapter.js 做的事：拦截 window.fetch('config.json')，把地图节点整批替换成
「本机曲库的谱面列表」（通过 phirc.listCharts() 拿 {charts:[{name,composer,path绝对}]}），
并通过 phirc.chartCover(path, 320)（Rust 侧读 info.yml 的 illustration、缩放、JPEG base64）
给每个节点配封面 data URL；没有封面的节点退化为标题文字卡片。

现在的问题是：在**我的安卓真机**上，地图显示的仍然是**改造前的原始节点**（游戏自带的
7 个示例曲目 + 剧情/档案节点），而我桌面上同一份代码是正常的。请帮我分析为什么修复
没有在真机生效，并指导我下一步怎么定位。

已知事实（都已验证过）：
1. 桌面端完全正常：我用 headless chromium + 假桥完整跑过，14 个节点全部渲染成封面卡片
   （渲染器自己上报 textures=14、textCards=0）；Rust 侧 chart_cover 对我真实曲库 6/6 成功。
2. 安卓的 Web 资源解压机制：APK assets 里的 kaichang/rpe-next 和 kaichang/chapter9 会在
   启动时解压到 filesDir，然后由本地静态服务托管。解压条件是「构建脚本生成的
   kaichang/webver.txt（内容哈希）≠ filesDir 里存的 .asset-version 标记」。我曾靠手动
   递增版本号，**有一次忘了递增，导致修复一直没到真机**——这是已确认踩过的坑，现已改为
   内容哈希自动失效（最新 APK 已包含）。但我不确定我装的是不是最新 APK。
3. 适配层曾经有一个已修复的崩溃：函数改名后（loadCharts→fetchCharts）预取处仍调旧名，
   ReferenceError 导致 fetch 钩子没挂上 → 地图保持原始状态。这与我在真机看到的症状
   完全一致，所以我怀疑**真机 filesDir 里仍是崩溃版本的 adapter.js**。
4. 适配层有诊断角标（右上角，显示「曲谱 N 张 · 曲绘 M/K」或错误信息），
   但我还没确认真机上它显示什么。
5. 打开方式：从游戏曲库点「第九章」→ Rust 收到桥命令后 Navigate 到
   http://127.0.0.1:8123(实际端口)/chapter9/index.html?phirc=1。
   ⚠️ 注意：WebView 里 phirc 自定义协议页面的 location.protocol 报的是 'http:'，
   不能用 protocol 判断页面来源，要用 href 前缀 http://127.0.0.1: 判断。

请你给我：
1. 一个 adb/logcat 排查清单：怎么确认 filesDir 里 adapter.js 的实际内容（adb 命令），
   怎么看 WebView 的 console 错误（logcat 过滤条件），怎么确认本地静态服务端口和
   解压是否发生（logcat tag 是 [web-static]）。
2. 分析「桌面正常、真机显示原始地图」最可能的原因排序。
3. 如果确认是 filesDir 缓存问题，给出手动清除方案（adb shell pm clear 的副作用，
   或只删 filesDir/chapter9 的命令）。
4. 如果不是缓存问题，给我在 adapter.js 里加「可见错误上报」的具体建议（当前已有
   右上角状态角标：曲谱 N 张 · 曲绘 M/K，错误也会显示）。

关键文件（需要看时我可以贴）：
- adapter.js：kaichang/chapter9/adapter.js（适配层，IIFE，fetch 补丁 + 节点替换 + 封面）
- WebViewHost.java：安卓 WebView 壳（静态服务启动、资源解压、loadUrl）
- scripts/build_apk.ps1：APK 构建（资源同步 + webver.txt 指纹生成）
- 游戏引擎侧（Rust）：webui.rs 的静态服务与桥命令（chart_cover / open_chapter9 /
  back_to_library）、editor.rs 的 chart_cover / list_charts
```

---

## 问题总结（给你自己看）

### 这个功能的架构

```
曲库页「第九章」按钮
  → 桥命令 open_chapter9 → WebView 跳 http://127.0.0.1:8123/chapter9/index.html?phirc=1
  → adapter.js（适配层）拦截 config.json
     · phirc.listCharts() 拿本机曲库
     · phirc.chartCover(path) 给每张谱配封面（Rust：info.yml→缩略图→JPEG base64）
     · 节点全部铺在舞台 (56,56)-(1992,887) 安全区内（自适应列数）
  → 点节点 → 详情 → phirc.startGame(path) 直接开局
  → 左上角「← 返回曲库」→ 桥 back_to_library
```

### 已经修掉的 bug（都有测试或探针证据）

| Bug | 证据 |
|---|---|
| 适配器崩溃（loadCharts 旧名引用）导致整个改造不生效 | jsdom 崩溃复现 → 修复后 14/14 |
| Navigate 拼出 `phirc://web/http://...` 404 | crash log 实锤 → 单测覆盖 |
| 开始页触点盖住第九章整页（点哪都跳收藏夹） | 触点脚本误判开始页 → jsdom 6/6 |
| 节点铺在舞台外被裁（舞台 2048×943，网格从 y=980 起） | overflow:clip + 坐标计算 |
| 真机拿不到新适配器 | 解压版本号忘了递增 → 改为内容哈希自动失效 |

### 当前卡点

你在真机上看到的仍是「原始地图」。最可能的原因（按概率排序）：

1. **手机上装的不是最新 APK**，或装了但 filesDir 里的旧 adapter.js 没被覆盖（上面的缓存坑）→ 最新 APK 已改为内容哈希自动失效，装最新包必重解压
2. filesDir 里的 adapter.js 已更新，但**运行时仍崩**（新的 ReferenceError 之类）→ 看右上角角标 / logcat
3. 桥调用（listCharts / chartCover）在安卓上行为不同

### 下一步排查（在手机上，按顺序）

1. 确认装的是最新 APK：`dist/PhiRa-Mod-Meew-11-slim.apk`（今天 21:25 之后构建的）
2. 进第九章，看**右上角角标**：
   - 显示「曲谱 N 张 · 曲绘 M/K」→ 适配器已跑通，看 M 是否等于 N
   - 显示「适配器错误：…」→ 把错误原文发我
   - **完全没有角标** → 适配器根本没加载，见第 3 步
3. logcat 抓证据：
   ```
   adb logcat -s WebViewHost | grep web-static     # 看解压是否发生
   adb logcat | findstr /i "chromium console"       # 看 WebView console 错误
   ```
4. 强制清掉旧解压副本：
   ```
   adb shell run-as org.flos.phira.modded rm -rf files/chapter9
   ```
   然后重进第九章（会自动重解压）
