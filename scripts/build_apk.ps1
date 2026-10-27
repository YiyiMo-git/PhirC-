# build_apk.ps1 — 在 Windows 上用 PowerShell 打 Phira Mod 的 Android APK（arm64-v8a, release）
#
# 正确运行方式（二选一）：
#   A. 仓库根目录执行：  powershell -ExecutionPolicy Bypass -File scripts\build_apk.ps1
#   B. 先 cd 到仓库根，再把脚本内容贴进 PowerShell（本脚本会从当前目录向上找仓库根）
# ⚠️ 不要从 C:\Users\xxx> 直接贴——那样找不到仓库根。
#
# 前置工具链（脚本会自动探测，缺了会打印安装命令并退出）：
#   - Android SDK + NDK 26.1.10909125（设 ANDROID_SDK_ROOT / ANDROID_NDK_HOME，或放常见路径）
#   - Gradle 8.9+（AGP 8.5.2 要求）
#   - JDK 17
#   - rustup target add aarch64-linux-android（脚本会自己补）
#   - cargo-ndk（cargo install cargo-ndk --locked）
#
# 体积：release + cargo-ndk 默认 strip + 只编 arm64-v8a，已是最小 .so。
# 想再小：把 android/app/build.gradle 的 minifyEnabled/shrinkResources 改成 true（开 R8，
# 但需保留 org.rustls.platformverifier.**，有闪退风险，慎开）。

# 注意：这里**不**设 $ErrorActionPreference='Stop'。
# 因为 cargo / rustup / gradle 等外部命令会把正常的 info/警告写到 stderr，
# PowerShell 5.1 在 Stop 模式下会把任何原生命令的 stderr 当成致命 NativeCommandError，
# 导致编译刚打印第一条警告就被中断。所以改为：对关键外部命令显式检查 $LASTEXITCODE。

# 已知可用配置：把编译产物放到 D:\meew11-target 并关掉增量编译。
# 默认 target 目录（仓库内 target/）在编译 proc-macro2 等构建脚本时会偶发
# os error 5（拒绝访问）；桌面构建一直用 D:\meew11-target + CARGO_INCREMENTAL=0 没问题。
$env:CARGO_TARGET_DIR = 'D:\meew11-target'
$env:CARGO_INCREMENTAL  = '0'

# ---------- 1) 定位仓库根（向上找含 Cargo.toml + android\app\build.gradle 的目录） ----------
$start = if ($PSScriptRoot) { Resolve-Path (Join-Path $PSScriptRoot '..') } else { $PWD }
$root = $null
$dir = $start
while ($dir) {
    if ((Test-Path (Join-Path $dir 'Cargo.toml')) -and
        (Test-Path (Join-Path $dir 'android\app\build.gradle'))) {
        $root = $dir
        break
    }
    $parent = Split-Path $dir -Parent
    if ($parent -eq $dir) { break }
    $dir = $parent
}
if (-not $root) {
    throw "找不到仓库根（需含 Cargo.toml 和 android\app\build.gradle）。请先 cd 到仓库根目录，再用 powershell -File 运行本脚本。"
}
Write-Host "[ok] 仓库根: $root"

# ---------- 2) 探测 Android SDK / NDK（env 优先，否则扫常见路径） ----------
$sdk = $env:ANDROID_SDK_ROOT
if (-not $sdk -or -not (Test-Path $sdk)) {
    $sdk = @($env:ANDROID_HOME, "$env:LOCALAPPDATA\Android\Sdk",
             'D:\Android\Sdk', 'C:\Android\Sdk', 'E:\Android\Sdk', 'E:\tools\android-sdk') |
        Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
}
if (-not $sdk) {
    throw "找不到 Android SDK。请安装：sdkmanager 或 Android Studio；或设 `$env:ANDROID_SDK_ROOT / ANDROID_HOME。"
}
$ndk = $env:ANDROID_NDK_HOME
if (-not $ndk -or -not (Test-Path $ndk)) {
    $ndk = Get-ChildItem (Join-Path $sdk 'ndk') -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 1
    if ($ndk) { $ndk = $ndk.FullName }
}
if (-not $ndk) {
    throw "找不到 NDK（在 $sdk\ndk 下）。请执行：& '$sdk\cmdline-tools\latest\bin\sdkmanager.bat' --install 'ndk;26.1.10909125'"
}
# 把探测到的路径注入当前会话，cargo-ndk / gradle 才能读到（两者都要设）
$env:ANDROID_SDK_ROOT = $sdk
$env:ANDROID_HOME      = $sdk
$env:ANDROID_NDK_HOME  = $ndk
Write-Host "[ok] SDK: $sdk"
Write-Host "[ok] NDK: $ndk"

# ---------- 3) 探测 Gradle（PATH 优先，否则扫常见安装目录） ----------
$gradleCmd = $null
$g = Get-Command gradle -ErrorAction SilentlyContinue
if ($g) { $gradleCmd = $g.Source }
if (-not $gradleCmd) {
    $cand = Get-ChildItem @('C:\Gradle\gradle-*\bin\gradle.bat',
                             "$env:LOCALAPPDATA\Gradle\gradle-*\bin\gradle.bat",
                             'C:\Program Files\Gradle\gradle-*\bin\gradle.bat',
                             (Join-Path $root 'gradle-*\bin\gradle.bat'),
                             'E:\Code\Rust\Phigros\gradle-*\bin\gradle.bat',
                             'E:\tools\gradle-*\bin\gradle.bat') -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($cand) { $gradleCmd = $cand.FullName }
}
if (-not $gradleCmd) {
    throw "找不到 Gradle（AGP 8.5.2 需要 8.9+）。请安装：winget install -e --id Gradle.Gradle"
}
Write-Host "[ok] Gradle: $gradleCmd"

# ---------- 4) 探测 JDK 17（gradle 与 keytool 都要） ----------
# ⚠️ 必须保证 JDK 的 bin 在 PATH 上（keytool 只在 JDK bin 里，不在 JRE / 系统 PATH）。
# 即使 `java` 已经在 PATH，也可能来自别处，所以这里**总是**定位一个 JDK 并把它挂上。
$jdkDir = $null
if ($env:JAVA_HOME -and (Test-Path $env:JAVA_HOME)) { $jdkDir = $env:JAVA_HOME }
if (-not $jdkDir) {
    $javaCmd = Get-Command java -ErrorAction SilentlyContinue
    if ($javaCmd) {
        $binDir = Split-Path $javaCmd.Source -Parent
        $parent = Split-Path $binDir -Parent
        if (Test-Path (Join-Path $binDir 'keytool.exe')) { $jdkDir = $parent }
    }
}
if (-not $jdkDir) {
    $cands = Get-ChildItem @('C:\Program Files\Microsoft\jdk-17*',
                             'C:\Program Files\Java\jdk-17*',
                             'C:\Program Files\Eclipse Adoptium\jdk-17*',
                             'E:\tools\jdk-17*',
                             'E:\tools\jdk*') -ErrorAction SilentlyContinue |
        Where-Object { Test-Path (Join-Path $_.FullName 'bin\keytool.exe') } |
        Select-Object -First 1
    if ($cands) { $jdkDir = $cands.FullName }
}
if (-not $jdkDir) { throw "找不到 JDK 17（需要 keytool）。请安装：winget install -e --id Microsoft.OpenJDK.17" }
$env:JAVA_HOME = $jdkDir
$env:Path = "$jdkDir\bin;$env:Path"
Write-Host "[ok] JDK: $jdkDir"

# ---------- 5) Rust android 目标（已加则自动跳过） ----------
# rustup 正常也会往 stderr 打 info 行，这里吞掉避免日志里出现吓人的 NativeCommandError
rustup target add aarch64-linux-android 2>$null

# ---------- 6) 生成签名 keystore（缺失才生成，之后可覆盖安装） ----------
$ks = Join-Path $root 'android\keystore.jks'
if (-not (Test-Path $ks)) {
    Write-Host "[*] 生成签名 keystore（首次构建）"
    keytool -genkeypair -v -keystore $ks -storetype PKCS12 `
        -storepass phiramod -keypass phiramod -alias phiramod `
        -keyalg RSA -keysize 2048 -validity 10000 `
        -dname "CN=Phira Mod, OU=Mod, O=PhiCraft, L=-, ST=-, C=CN"
}
if (-not (Test-Path $ks)) { throw "keystore 生成失败" }

# ---------- 7) 编 libphira.so（release，**带 video**；cargo-ndk 默认 strip） ----------
# video 功能要链 FFmpeg 静态库：prpr-avc/static-lib/aarch64-linux-android/
# 没有的话先跑 `bash prpr-avc/pull-static-lib.sh`（或只拉 aarch64-linux-android 那个包）。
$avcLib = Join-Path $root "prpr-avc/static-lib/aarch64-linux-android"
if (-not (Test-Path (Join-Path $avcLib "libavformat.a"))) {
    throw "缺少 FFmpeg 静态库：$avcLib\libavformat.a（video 功能需要）。先跑 prpr-avc/pull-static-lib.sh 拉取。"
}
Push-Location $root
cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs `
    build --release -p phira --no-default-features --features video
$cargoExit = $LASTEXITCODE
Pop-Location
if ($cargoExit -ne 0) { throw "cargo ndk 编译失败（exit $cargoExit）" }

$jni = Join-Path $root "android/app/src/main/jniLibs/arm64-v8a"

# ---------- 8) 一并打包 libc++_shared.so（oboe 音频后端需要，否则启动闪退） ----------
$libcpp = Join-Path $ndk "toolchains/llvm/prebuilt/windows-x86_64/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so"
if (Test-Path $libcpp) {
    Copy-Item $libcpp $jni -Force
    Write-Host "[ok] 已复制 libc++_shared.so"
} else {
    Write-Warning "NDK 里找不到 libc++_shared.so：$libcpp —— oboe 后端会启动闪退，请检查 NDK 路径"
}

# ---------- 9) 拷游戏资源，剔除大目录 dist/res ----------
$assetsDst = Join-Path $root 'android/app/src/main/assets'
Remove-Item $assetsDst -Recurse -Force -ErrorAction SilentlyContinue
New-Item  $assetsDst -ItemType Directory | Out-Null
Copy-Item (Join-Path $root 'assets\*') $assetsDst -Recurse -Force
Remove-Item (Join-Path $assetsDst 'dist') -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item (Join-Path $assetsDst 'res')  -Recurse -Force -ErrorAction SilentlyContinue

# WebView 界面层（kaichang/*.html）：安卓端 WebView 也要用同一套页面。
# 只拷真正要用的文件 + 资源，别把 plugins/ 里的示例包和临时文件塞进 APK。
$kaichangSrc = Join-Path $root '..\kaichang'
if (Test-Path $kaichangSrc) {
    $kaichangDst = Join-Path $assetsDst 'kaichang'
    New-Item $kaichangDst -ItemType Directory -Force | Out-Null
    $pages = @('index.html','tripsUide.html','tools.html','Setting.html','ResPack.html',
               'library.html','Replays.html','ChartPanel.html',
               # 制谱器 / 成长树 / MOD 管理：曲库顶部按钮会直接跳这些页面，漏拷就是 404
               'ChartEditor.html','Growth.html','Mod.html')
    foreach ($f in $pages) {
        $src = Join-Path $kaichangSrc $f
        if (Test-Path $src) { Copy-Item $src (Join-Path $kaichangDst $f) -Force }
        else { Write-Host "  (跳过缺失页面 $f)" }
    }
    # 页面用到的贴图/字体/图标（按需列白名单，避免把整个 kaichang 塞进去）
    foreach ($f in 'cmdysj.ttf','qrcode.js','背景.png','Rubbish Sorting.png','导航栏.png','phirc.png',
                   '左上角1.png','左上角2.png','左上角3.png','右下角1.png','右下角2.png','右下角3.png',
                   '收集品icon.png','JoinQQGuildBanner.png','setting.png','beijing.jpeg','TriGridBase.png',
                   '3.jpg','play.png','sort.png','Random.png','Introduction.png','lock.png') {
        $src = Join-Path $kaichangSrc $f
        if (Test-Path $src) { Copy-Item $src (Join-Path $kaichangDst $f) -Force }
    }
    # rpe-next（照搬的 Re:PhiEdit Next）：安卓启动时解压到 filesDir，由本地静态服务托管
    foreach ($d in 'vendor','plugins','assets','rpe-next') {
        $src = Join-Path $kaichangSrc $d
        if (Test-Path $src) { Copy-Item $src (Join-Path $kaichangDst $d) -Recurse -Force }
    }
    # assets/ 子目录与根级白名单有 3 个完全重复的文件（页面两种引用路径都有：
    # `url("cmdysj.ttf")` 根级 + `assets/cmdysj.ttf`）。资源服务端（webui.rs serve_asset /
    # WebViewHost.intercept）都有「去掉 assets/ 前缀再试」兜底，只留根级一份即可 ——
    # APK zip 里每份 8.08MB/压缩后 5.12MB，去重共省 ~5.7MB。
    foreach ($dup in 'cmdysj.ttf','phirc.png','TriGridBase.png') {
        Remove-Item (Join-Path $kaichangDst "assets\$dup") -ErrorAction SilentlyContinue
    }
    Write-Host "  已打包 WebView 页面到 assets/kaichang/（assets/ 去重 3 个文件）"
} else {
    Write-Host "  ⚠️ 找不到 kaichang 源目录，跳过 WebView 页面打包"
}

# ---------- 10) 校验关键资源 ----------
foreach ($f in 'font.ttf', 'background.jpg') {
    if (-not (Test-Path (Join-Path $assetsDst $f))) { throw "缺少资源 $f，启动会崩，先补齐 assets/" }
}

# ---------- 11) 打包 release APK ----------
# 用绝对路径，避免依赖当前工作目录（Pop-Location 之后可能不是仓库根）
& $gradleCmd -p (Join-Path $root 'android') assembleRelease --no-daemon
if ($LASTEXITCODE -ne 0) { throw "gradle assembleRelease 失败（exit $LASTEXITCODE）" }

$apk = Get-ChildItem (Join-Path $root 'android/app/build/outputs/apk/release/*.apk') | Select-Object -First 1
if (-not $apk) { throw '没找到产物 APK，构建可能失败' }

# ---------- 12) 修 aapt2 的 zip 条目名编码缺陷，然后重签 ----------
# aapt2 在 Windows 上打 zip 时**漏置 UTF-8 flag（0x800）**：entry name 的字节是对的
# UTF-8，但按规范读取方（Android AssetManager / WebView asset 加载）会按 CP437
# 兜底解码 → `背景.png` / `默认.png` / `收集品icon.png` 等中文资源在移动端全部 404。
# 后处理（scripts/fix_apk_names.py）：重写 zip 补 flag → zipalign -p 4 → apksigner 重签。
# 顺序不能反：zipalign 必须在签名前，否则对齐会破坏签名。
$btRoot = Join-Path $sdk 'build-tools'
$bt = Get-ChildItem $btRoot -Directory -ErrorAction SilentlyContinue |
    Sort-Object Name -Descending | Select-Object -First 1
if (-not $bt) { throw "找不到 build-tools（$btRoot）" }
Write-Host "[*] 修 zip 条目名编码（build-tools $($bt.Name)）+ 重签"
python (Join-Path $PSScriptRoot 'fix_apk_names.py') $apk.FullName $bt.FullName $ks 'phiramod' 'phiramod' 'phiramod'
if ($LASTEXITCODE -ne 0) { throw "APK zip 条目名修复失败（exit $LASTEXITCODE）" }

Write-Host "[done] APK: $($apk.FullName)  ($([math]::Round($apk.Length/1MB, 1)) MB)"
# 显式 0：脚本里调用过 cargo/gradle/python，$LASTEXITCODE 可能残留非 0，
# 会让外层调用方误判构建失败（构建成功却返回 1）。
exit 0


