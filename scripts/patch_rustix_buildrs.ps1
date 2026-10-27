# patch_rustix_buildrs.ps1 — 修掉本机「APK 构建卡在 rustix」的问题（幂等，可重复跑）
#
# 【症状】`cargo ndk ... build --release -p phira` 报：
#   error: failed to run custom build command for `rustix v1.x`
#   thread 'main' panicked at ...\rustix-1.x\build.rs:281:10:
#     called `Result::unwrap()` on an `Err` value:
#     Os { code: 231, kind: Uncategorized, message: "所有的管道范例都在使用中。" }
#
# 【根因】本机无法为子进程创建 stdin 管道：Rust 的 Command::spawn 配
# Stdio::piped() 会直接失败并返回 Windows 错误 231（ERROR_PIPE_BUSY）。
# 同样的事用 Python subprocess(stdin=PIPE) 测是正常的 —— 说明是 Rust 走命名
# 管道的那条路径在本机被拦（已确认与沙箱无关，沙箱外同样复现）。
# rustix 的 build.rs 用 `rustc -`（stdin 走管道）做能力探测，原代码
# `.spawn().unwrap()` 于是 panic，整个构建中断。
# 桌面 exe 不受影响：Windows 目标的依赖图里没有 rustix。
#
# 【本脚本做两件事】
#   1. 把探测改成「spawn 失败就 return false」——
#      这个探测只用来决定要不要开某些优化 cfg，保守返回 false 完全安全；
#   2. 删掉 target 里 rustix 的旧构建产物。cargo 把 registry 源码当**不可变**、
#      不比对 mtime，不删就会继续用旧 build script，照旧 panic。
#
# 【何时需要重跑】`cargo clean`、清过 registry 缓存、或 rustix 升版本之后。
#
# 用法：powershell -ExecutionPolicy Bypass -File scripts\patch_rustix_buildrs.ps1

$ErrorActionPreference = 'Stop'

$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$targetDir = $env:CARGO_TARGET_DIR

$srcRoot = Join-Path $cargoHome 'registry\src'
if (-not (Test-Path $srcRoot)) { throw "找不到 cargo registry 源码目录：$srcRoot" }

# 原始代码块（从 `let mut child = cmd` 到函数末尾的返回值）。
# 注意 `.arg/.stdin/.stderr` 那几行**行尾都带注释**，所以每行要用 [^\n]* 吃掉注释再换行。
$oldPattern = 'let mut child = cmd[^\n]*\r?\n\s*\.arg\("-"\)[^\n]*\r?\n\s*\.stdin\(Stdio::piped\(\)\)[^\n]*\r?\n\s*\.stderr\(Stdio::null\(\)\)[^\n]*\r?\n\s*\.spawn\(\)\r?\n\s*\.unwrap\(\);\r?\n\r?\n\s*writeln!\(child\.stdin\.take\(\)\.unwrap\(\), "\{\}", test\.as_ref\(\)\)\.unwrap\(\);\r?\n\r?\n\s*child\.wait\(\)\.unwrap\(\)\.success\(\)'

$newBlock = @'
let mut child = match cmd
        .arg("-") // Read from stdin.
        .stdin(Stdio::piped()) // Stdin is a pipe.
        .stderr(Stdio::null()) // Errors from feature detection aren't interesting and can be confusing.
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    if writeln!(child.stdin.take().unwrap(), "{}", test.as_ref()).is_err() {
        return false;
    }

    child.wait().map(|s| s.success()).unwrap_or(false) // meew11 patch
'@
$newBlock = $newBlock -replace "`r`n", "`n"

$patched = 0
$already = 0
$packages = Get-ChildItem $srcRoot -Directory | ForEach-Object {
    Get-ChildItem $_.FullName -Directory -Filter 'rustix-*' -ErrorAction SilentlyContinue
}

foreach ($pkg in $packages) {
    $br = Join-Path $pkg.FullName 'build.rs'
    if (-not (Test-Path $br)) { continue }
    $text = [System.IO.File]::ReadAllText($br)

    if ($text -like '*meew11*') {
        Write-Host "[=] 已打过补丁，跳过：$br"
        $already++
        continue
    }

    $new = $text -replace $oldPattern, $newBlock
    if ($new -eq $text) {
        Write-Warning "没匹配到待替换片段（rustix 版本变了？）：$br"
        continue
    }

    [System.IO.File]::WriteAllText($br, $new, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "[+] 已打补丁：$br"
    $patched++
}

if ($patched -eq 0 -and $already -eq 0) {
    Write-Warning '没有找到任何 rustix 源码 —— 先跑一次 cargo（下载依赖）再执行本脚本。'
}

# 删掉 rustix 的旧构建产物，逼 cargo 重新编译 build script
if (-not $targetDir) {
    Write-Warning '未设置 CARGO_TARGET_DIR，跳过清理（请手动清 target/<profile>/build|.fingerprint|deps 下的 rustix-*）。'
} elseif (-not (Test-Path $targetDir)) {
    Write-Host "[i] target 目录还不存在：$targetDir"
} else {
    foreach ($prof in @('release', 'debug')) {
        foreach ($sub in @('build', '.fingerprint', 'deps')) {
            $dir = Join-Path $targetDir (Join-Path $prof $sub)
            if (-not (Test-Path $dir)) { continue }
            Get-ChildItem $dir -Filter 'rustix-*' -ErrorAction SilentlyContinue | ForEach-Object {
                Remove-Item $_.FullName -Recurse -Force -ErrorAction SilentlyContinue
                Write-Host "[x] 已删除 $($_.FullName)"
            }
        }
    }
}

Write-Host "[done] 打补丁 $patched 个，已是最新 $already 个。"
