# Phira Pro 对比笔记（第三方项目，2026-09-30 记录）

> 内部笔记。记录了工作区里 `E:\Code\Rust\Phigros\Phira-Pro-pro.2` 这个目录是什么、
> 与我们（Phirc Mod++）的关系、以及敏感内容核验结果。

## 1. 它是什么

- 仓库：[baiji666/Phira-Pro](https://github.com/baiji666/Phira-Pro)（公开，非 fork）
- 自我定位：`Phira Pro · 基于 Phira v0.8.2 的改版`，**只做 Windows 桌面版**（无 `android/`、无 iOS、无联机）
- 许可证：GPL-3.0（仓库 API 报 `GPL-3.0`，LICENSE 是标准 35149 字节版）
- 时间线：仓库创建 2026-09-28T23:38Z → 首个 release `pro.1` 2026-09-28T23:40Z
  → `pro.2` 2026-09-29T08:43Z；最后 push 2026-09-29T08:42Z
- 热度：2 stars、0 fork、两个 release 各 0 下载（截至本次核验）
- 产物：`PhiraPro-v0.8.2-pro.2-win64.zip`（46.3 MB，release 附件）
- 本地目录：`E:\Code\Rust\Phigros\Phira-Pro-pro.2`（2026-09-30 20:15 出现，无 `.git`，
  文件时间统一 2026-09-29 01:33 CST ⇒ 是 **tag pro.2 的源码 zipball 解压**，729 文件 / 14.1 MB）

## 2. 与我们的关系：他们移植了我们的代码，并在注释里致谢

命中「Phirc Mod++」共 5 处，全部是**明确的来源标注**：

| 位置 | 注释 |
| --- | --- |
| `phira/src/history.rs:11` | 移植自上游改版 Phirc Mod++ 的 `phira/src/history.rs` |
| `phira/src/page/history.rs:4` | 移植自上游改版 Phirc Mod++ 的 `phira/src/page/history.rs`（未引入每日挑战） |
| `prpr/src/config.rs:35` | 血条颜色可选值（对齐上游改版 Phirc Mod++ 的 `HealthBarColor`） |
| `prpr/src/config.rs:94` | 注：上游改版 Phirc Mod++ 把这个标志放在 `0x0200`，但我们的 `0x0200` 已被占用 |
| `prpr/src/scene/game.rs:182` | 判定线调试 / 音符调试（横向判定范围）移植自上游改版 Phirc Mod++ |

结论：**是第三方 fork/借鉴关系，不是我们的下游镜像**；他们把我们的「成绩历史页」「判定线/音符调试」
「血条颜色枚举」等移植过去并保留了出处注释。GPL-3.0 下这是允许的。

## 3. 敏感内容核验（成绩上传逆向）—— 未泄漏

在本目录全树做了字节级扫描：

| 标记 | 命中 |
| --- | --- |
| `HMAC_KEY` | 0 |
| `diff_sum_sq` | 0 |
| `custom_enc` | 0 |
| `上传成绩逆向` | 0 |
| `phira-0.8.2/prpr/src/record.rs`（我们的编码器） | 文件不存在（`record.rs` 标记 0 命中） |
| `encode_record` | 1（`prpr/src/scene/game.rs:1251`） |

关于那唯一一处 `encode_record`：

- 它位于 `#[cfg(closed)]` 块内，是**上游原样的死代码**（注释还留着上游的 `// TODO strengthen the protection`），
  且该函数在他们的树里**没有定义**（无 `fn encode_record`）⇒ 只在 `cfg(closed)` 打开时才编译，公开构建永不触发
- 他们的 `prpr/Cargo.toml` 仍留着 `hmac = "0.12.1"` 与 `sha2`，但全树 `.rs` 里**没有任何 `hmac::` / `Hmac` 调用**
  （`sha2` 只用于 `phira/src/page.rs` 与 `scene/song.rs` 的谱面校验和）⇒ 应是继承自上游/我们仓库的依赖残留
- 其 README 明写「成绩只保存在本机，本改版不会向任何服务器上传成绩，也不收集设备信息」

⇒ **判定：我们的编码器与静态密钥没有进入他们的源码。**（未核验他们发布的 46 MB Windows 二进制；
如需彻底排除，可下载该 zip 对 `phira-main.exe` 做字符串扫描。）

## 4. 他们有、我们没有的功能（潜在借鉴清单）

- **自动偏移估计**：`prpr-auto-offset/`（superflux 15.8 KB、weighted gaussian 音符模型、
  systematic_offset 测试）+ `tools/auto-offset-cli` ⇒ 结算页「一键应用推荐偏移」
- **血条**：模式（扣到 0 失败）/扣血倍率/长度/厚度/整体倍率/颜色
- **自动重试 + 续练提前量**：失败后自动重开，可从失败前若干秒开始
- **变速练习**：每完成一圈自动提速（起始速度 + 每圈增幅）
- **结算判定分布图**：以 0 ms 居中、早←→晚、左蓝右橙
- **自定义外观**：图标 / 背景 / 立绘导入替换；**界面主题**：强调色 + 表面色可调
- **判定线调试 / 音符调试叠加层**：线号 / 线高 / z-index；音符线号 / 时间 / 高度 / 类型 + 横向判定范围
- **连击文字自定义**（≤16 字符）、左下角 FPS
- pro.2 新增：**玩家黑名单**、Hold 尾判与判定窗口数值化、HUD 自定义编辑器与界面槽位化、设置列表行高可调

## 5. 我们有、他们没有的

Android APK / iOS IPA 打包、**官方成绩上传**（他们公开版明确不做）、每日挑战、
音符拖影（≤255 残影）/判定线残影与发光、音乐可视化（自写 FFT）、触点标记、全屏判定、
漏键标记、Early-Late 提示、九宫格 HUD 定位、判定文字四种动画、
以及已下架的 `launcher/`（Windows 联机开服器）与 `duel/`。

## 6. 合规观察（供参考，不影响我们当前动作）

- 他们的公开仓库**跟踪了 73 个官方美术 / 音频**（`assets/*.png|jpg|ogg`，含 `player.jpg`、`rank/*`），
  但 README 声称「仓库不包含部分资源文件（`assets/res`、`assets/font.ttf`、`assets/background.jpg` 等）」——
  说法与实际跟踪内容不完全一致。
- 我们自己的公开仓库 `PhiCraft/Phira-Mod-New` 同样跟踪了 78 个这类资源（待办 #3 尚未决策），
  两者在这一项上处境相同；若将来要"干净"，双方都得清。
- 他们的注释主动标注了取自我们，属善意使用；GPL-3.0-only 下无需额外授权，也无义务通知我们。
