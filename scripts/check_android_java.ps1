# 只编译 Android 的 Java 源码（不打包 APK、不编译 Rust）
#
# 用途：改 MainActivity / WebViewHost 之类的 Java 代码后，快速确认**语法与符号**没问题。
# 完整 APK（含 Rust .so）仍然要跑 scripts\build_apk.ps1。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts\check_android_java.ps1
#
# 需要：JDK + Gradle + ANDROID_HOME（或 ANDROID_SDK_ROOT）。
# 首次跑会下载/解析依赖，之后是增量，通常 10~30 秒。

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

function Find-Gradle {
    $g = Get-Command gradle -ErrorAction SilentlyContinue
    if ($g) { return $g.Source }
    foreach ($p in @(
        'D:\Environment\Gradle\gradle-8.10.2\bin\gradle.bat',
        'D:\Environment\Gradle\gradle-8.10.2\bin\gradle'
    )) {
        if (Test-Path $p) { return $p }
    }
    throw '找不到 gradle，请把 gradle 加进 PATH 或改本脚本里的候选路径'
}

if (-not $env:ANDROID_HOME -and -not $env:ANDROID_SDK_ROOT) {
    foreach ($p in @('D:\Environment\Android\Sdk', "$env:LOCALAPPDATA\Android\Sdk")) {
        if (Test-Path $p) { $env:ANDROID_HOME = $p; break }
    }
}
if (-not $env:ANDROID_HOME -and -not $env:ANDROID_SDK_ROOT) {
    throw '找不到 Android SDK，请先设置 ANDROID_HOME'
}

$gradle = Find-Gradle
Write-Host "gradle : $gradle"
Write-Host "sdk    : $(if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { $env:ANDROID_SDK_ROOT })"

Push-Location $root
try {
    # gradle 会往 stderr 打 SDK 警告；PS 5.1 在 ErrorActionPreference=Stop 下
    # 会把它当终止错误，所以这里临时放宽，并显式把两路输出都打到控制台。
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $gradle -p (Join-Path $root 'android') :app:compileDebugJavaWithJavac --no-daemon --console=plain 2>&1 |
            ForEach-Object { Write-Host $_ }
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $prev
    }
    if ($code -ne 0) { throw "Java 编译失败（exit $code）" }
    Write-Host ''
    Write-Host '✅ Java 编译通过（未打包 APK；完整构建请跑 scripts\build_apk.ps1）'
} finally {
    Pop-Location
}
