# run_all_tests.ps1 — 希夷编译器测试套件 (Windows)
# 对应 Linux/macOS 上的 run_all_tests.sh
#
# 要求:
#   - 64 位 Windows + 64 位 PowerShell，PowerShell >= 7.6.6 (低于此版本直接退出)
#   - cargo (除非 -SkipBuild)
#
# 平台:
#   只能在 Windows 上运行；Linux/macOS 请使用 run_all_tests.sh。
#   32 位操作系统或 32 位 PowerShell 进程会被脚本主动拒绝。
#   仅调试脚本本身时，可设环境变量 XIYI_TESTS_FORCE=1 跳过操作系统与位数自检
#   (PowerShell 版本检查不可跳过)。
#
# 与 run_all_tests.sh 的语义保持基本一致 (参数、过滤、golden、超时、JSON 字段)。
# .exit golden 两边都只接受 0-255 (POSIX 退出码范围)，保证仓库里共享的 golden 在两个平台含义一致。
# 平台细节差异: sh 以"信号 / 退出码 > 128"识别崩溃，这里以 NTSTATUS 错误码
# (0xC0000000-0xC000FFFF，如访问违例、栈溢出) 识别；超时由 WaitForExit 直接判定，
# 不依赖退出码。
#
# 注意: 为了让低版本 PowerShell 也能"解析通过并走到下面的版本检查、给出友好提示"，
# 本文件在版本检查之前不使用任何 PowerShell 7 才有的语法 (三元、??、&& 等)。

param(
    [ValidateSet('all', 'pass', 'fail')]
    [string]$Expect = 'all',

    # 支持通配，如 *loop*，匹配相对 Tests/ 的路径 (正反斜杠均可)
    [ValidateScript({ if ($_ -and $_.Trim()) { $true } else { throw '-Name 的值不能为空' } })]
    [string[]]$Name,

    # 匹配源码头 // @tag a, b 或 list.json 里的 tag
    [ValidateScript({ if ($_ -match '^[^\s,]+$') { $true } else { throw "-Tag 的值不能为空，且不能含空白或逗号: '$_'" } })]
    [string[]]$Tag,

    [switch]$SkipBuild,
    [switch]$Release,
    [switch]$FailFast,        # 仅串行模式有效
    [switch]$UpdateGoldens,   # 把本次实际输出写回 .out / .err，然后自己 git diff 审查
    [switch]$StrictCrash,     # 崩溃 (NTSTATUS 错误码 / panic / ICE) 时，即使用例"期望失败"也判为失败

    [ValidateRange(1, [int]::MaxValue)]
    [int]$Jobs = 1,

    [ValidateRange(1, [int]::MaxValue)]
    [int]$TimeoutSeconds = 30,

    [ValidateRange(1, [int]::MaxValue)]
    [int]$KeepLogs = 20
)

# ============================================================
# 平台 / 版本自检 (必须最先执行)
# ============================================================
$ForceDev = ($env:XIYI_TESTS_FORCE -eq '1')

if (-not $ForceDev) {
    # OSVersion.Platform 在 Windows PowerShell 与 PowerShell 7 上都可用
    if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
        Write-Host '❌ 不支持的操作系统：本脚本只支持 Windows。' -ForegroundColor Red
        Write-Host '   Linux/macOS 请使用 run_all_tests.sh。' -ForegroundColor Red
        exit 1
    }
    if (-not [System.Environment]::Is64BitOperatingSystem) {
        Write-Host '❌ 不支持 32 位操作系统：本测试脚本只支持 64 位 Windows。' -ForegroundColor Red
        exit 1
    }
    if (-not [System.Environment]::Is64BitProcess) {
        Write-Host '❌ 当前 PowerShell 是 32 位进程，拒绝运行。请改用 64 位 PowerShell 7.6.6 或更新版本。' -ForegroundColor Red
        exit 1
    }
}

$psv = $PSVersionTable.PSVersion
# -or 短路：低版本 (PSVersion 是 System.Version，没有 SemanticVersion 可比) 不会走到右边
if ($psv.Major -lt 7 -or $psv -lt [System.Management.Automation.SemanticVersion]'7.6.6') {
    Write-Host "❌ 需要 PowerShell >= 7.6.6，当前是 $psv。" -ForegroundColor Red
    Write-Host '   请安装最新的 PowerShell，例如: winget install --id Microsoft.PowerShell' -ForegroundColor Red
    Write-Host '   然后用 pwsh (而不是 powershell.exe) 运行本脚本。' -ForegroundColor Red
    exit 1
}

# ===== CI / NO_COLOR 下关闭彩色转义序列 =====
if (($env:CI -or $env:NO_COLOR) -and $PSStyle) {
    $PSStyle.OutputRendering = 'PlainText'
}

if ($Name) { $Name = @($Name | ForEach-Object { $_ -replace '\\', '/' }) }   # 兼容 Windows 风格反斜杠

function Exit-WithError {
    param([string]$Message)
    Write-Host "❌ $Message" -ForegroundColor Red
    exit 1
}

function Write-Warn {
    param([string]$Message)
    Write-Host "⚠️  $Message" -ForegroundColor Yellow
}

# ============================================================
# 路径发现：不假设固定目录层级深度
#   从脚本所在目录向上找，直到某一级目录"同时"含有 Tests/ 和 Standard/ 为止。
#   全部使用 -LiteralPath：路径里的 [ ] 否则会被当成通配符。
# ============================================================
function Find-WorkspaceRoot {
    param([string]$StartDir)
    $dir = Get-Item -LiteralPath $StartDir
    while ($dir) {
        $testsPath = Join-Path $dir.FullName 'Tests'
        $stdPath   = Join-Path $dir.FullName 'Standard'
        if ((Test-Path -LiteralPath $testsPath) -and (Test-Path -LiteralPath $stdPath)) {
            return $dir.FullName
        }
        # 不用 Split-Path：它的 -Parent 只在 -Path 参数集里，与 -LiteralPath 不能同时使用；
        # 且 -Path 会把 [ ] 当通配符。GetDirectoryName 按字面处理，到根目录时返回 $null。
        $parentPath = [System.IO.Path]::GetDirectoryName($dir.FullName)
        if (-not $parentPath -or $parentPath -eq $dir.FullName) { return $null }
        $dir = Get-Item -LiteralPath $parentPath
    }
    return $null
}

# 版本号从 Cargo.toml 的 [package] 段读，不在脚本里写死，也不会误取依赖项的 version。
function Get-XiyiVersion {
    param([string]$CompilerRoot)
    $cargoToml = Join-Path $CompilerRoot 'Cargo.toml'
    if (Test-Path -LiteralPath $cargoToml) {
        $inPackage = $false
        foreach ($line in (Get-Content -LiteralPath $cargoToml -ErrorAction SilentlyContinue)) {
            if ($line -match '^\s*\[') {
                $inPackage = ($line -match '^\s*\[package\]\s*(#.*)?$')
                continue
            }
            if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
                return $Matches[1]
            }
        }
    }
    return 'unknown'
}

# target 目录：优先问 cargo (能反映 workspace 与 .cargo/config)，其次 CARGO_TARGET_DIR，最后默认值。
function Resolve-TargetDir {
    param([string]$CompilerRoot)
    if (Get-Command cargo -ErrorAction SilentlyContinue) {
        Push-Location -LiteralPath $CompilerRoot
        try {
            $metaText = & cargo metadata --no-deps --format-version 1 --offline 2>$null | Out-String
            $metaExit = $LASTEXITCODE
            if ($metaExit -eq 0 -and $metaText) {
                $meta = $metaText | ConvertFrom-Json
                if ($meta.target_directory) { return [string]$meta.target_directory }
            }
        } catch {
            # 忽略，走下面的回退
        } finally {
            Pop-Location
        }
    }
    if ($env:CARGO_TARGET_DIR) {
        if ([System.IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) { return $env:CARGO_TARGET_DIR }
        return (Join-Path $CompilerRoot $env:CARGO_TARGET_DIR)
    }
    return (Join-Path $CompilerRoot 'target')
}

$COMPILER_ROOT  = $PSScriptRoot
$WORKSPACE_ROOT = Find-WorkspaceRoot -StartDir $COMPILER_ROOT
if (-not $WORKSPACE_ROOT) {
    Exit-WithError "从 $COMPILER_ROOT 向上找不到同时包含 Tests/ 与 Standard/ 的工作区根目录"
}

$STDLIB         = Join-Path $WORKSPACE_ROOT 'Standard'
$TEST_DIR       = Join-Path $WORKSPACE_ROOT 'Tests'
$TEST_LIST_JSON = Join-Path $TEST_DIR 'list.json'

# ============================================================
# 日志目录 + 轮转：先创建本次日志，再保留最近 KeepLogs 份 (含本次)
# ============================================================
$LOG_DIR = Join-Path $COMPILER_ROOT 'test_results'
try {
    [void][System.IO.Directory]::CreateDirectory($LOG_DIR)
} catch {
    Exit-WithError "无法创建日志目录 ${LOG_DIR}: $($_.Exception.Message)"
}

$runId = Get-Date -Format 'yyyyMMdd_HHmmss'
# 同一秒内重复启动 (CI 并发) 时避免日志互相覆盖
if ((Test-Path -LiteralPath (Join-Path $LOG_DIR "$runId.log")) -or (Test-Path -LiteralPath (Join-Path $LOG_DIR "$runId.json"))) {
    $runId = "${runId}_$PID"
}
$LOG_FILE  = Join-Path $LOG_DIR "$runId.log"
$JSON_FILE = Join-Path $LOG_DIR "$runId.json"

function Add-LogLine {
    param([string]$Text)
    try {
        [System.IO.File]::AppendAllText($LOG_FILE, $Text + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
    } catch {
        # 日志写失败不应该影响测试本身
    }
}

try {
    [System.IO.File]::WriteAllText($LOG_FILE, '', [System.Text.UTF8Encoding]::new($false))
} catch {
    Exit-WithError "无法写入日志 ${LOG_FILE}: $($_.Exception.Message)"
}

function Remove-OldLogs {
    # $KeepOthers：除"本次文件"以外还要保留几份
    param([string]$Filter, [int]$KeepOthers, [string[]]$Protect)
    $others = @(Get-ChildItem -LiteralPath $LOG_DIR -Filter $Filter -File -ErrorAction SilentlyContinue |
        Where-Object { $Protect -notcontains $_.FullName } |
        Sort-Object -Property @{ Expression = 'LastWriteTime'; Descending = $true }, @{ Expression = 'Name'; Descending = $true })
    if ($others.Count -gt $KeepOthers) {
        $others | Select-Object -Skip $KeepOthers | Remove-Item -Force -ErrorAction SilentlyContinue
    }
}
# .log 本次已创建；.json 要到最后才写。两者都是"再留 KeepLogs-1 份旧的"，总数恰为 KeepLogs。
Remove-OldLogs -Filter '*.log'  -KeepOthers ($KeepLogs - 1) -Protect @($LOG_FILE, $JSON_FILE)
Remove-OldLogs -Filter '*.json' -KeepOthers ($KeepLogs - 1) -Protect @($LOG_FILE, $JSON_FILE)

$xiyiVersion = Get-XiyiVersion -CompilerRoot $COMPILER_ROOT

Write-Host '========================================' -ForegroundColor Cyan
Write-Host ' 希夷编译器测试套件' -ForegroundColor Cyan
Write-Host " xiyi-compiler 版本: $xiyiVersion" -ForegroundColor Cyan
Write-Host " Expect 过滤: $Expect   并发: $Jobs   超时(默认): ${TimeoutSeconds}s" -ForegroundColor Cyan
Write-Host " 编译器目录: $COMPILER_ROOT" -ForegroundColor DarkGray
Write-Host " 标准库目录: $STDLIB" -ForegroundColor DarkGray
Write-Host " 测试目录:   $TEST_DIR" -ForegroundColor DarkGray
Write-Host " 日志:       $LOG_FILE" -ForegroundColor DarkGray
Write-Host '========================================' -ForegroundColor Cyan
Write-Host ''

Add-LogLine "运行 ID: $runId"
Add-LogLine "版本: $xiyiVersion"
Add-LogLine "参数: Expect=$Expect Jobs=$Jobs SkipBuild=$SkipBuild Release=$Release UpdateGoldens=$UpdateGoldens FailFast=$FailFast StrictCrash=$StrictCrash"

# ============================================================
# 步骤1：编译（可跳过）
# ============================================================
if (-not $SkipBuild) {
    Write-Host '[1/4] 编译 xiyi-compiler...' -ForegroundColor Yellow
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Exit-WithError '找不到 cargo (可用 -SkipBuild 跳过编译)'
    }
    Push-Location -LiteralPath $COMPILER_ROOT
    try {
        $buildArgs = @('build', '-q')
        if ($Release) { $buildArgs += '--release' }
        # 先把输出收进变量再统一写日志；$LASTEXITCODE 必须在 cargo 结束后立刻读取。
        $buildOutput   = & cargo @buildArgs 2>&1 | Out-String
        $buildExitCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    Add-LogLine $buildOutput
    if ($buildExitCode -ne 0) {
        Write-Host $buildOutput
        Exit-WithError '编译失败，停止测试'
    }
    Write-Host '✅ 编译成功' -ForegroundColor Green
} else {
    Write-Host '[1/4] 跳过编译（-SkipBuild）' -ForegroundColor Yellow
}
Write-Host ''

# ============================================================
# 步骤2：定位可执行文件与标准库
# ============================================================
$profileName = if ($Release) { 'release' } else { 'debug' }
# cargo metadata 要几百毫秒，只算一次
$TARGET_DIR = Resolve-TargetDir -CompilerRoot $COMPILER_ROOT
$XIYI_EXE   = Join-Path (Join-Path $TARGET_DIR $profileName) 'xiyi.exe'
if (-not (Test-Path -LiteralPath $XIYI_EXE -PathType Leaf)) {
    Exit-WithError "在 $(Join-Path $TARGET_DIR $profileName) 下找不到 xiyi.exe"
}
if (-not (Test-Path -LiteralPath $STDLIB)) {
    Exit-WithError "找不到标准库目录 $STDLIB"
}
Write-Host "使用可执行文件: $XIYI_EXE" -ForegroundColor DarkGray
Write-Host ''

# ============================================================
# 步骤3：递归扫描 Tests/**/*.xiyi，list.json 只作为例外覆盖
# ============================================================
Write-Host "[2/4] 扫描测试用例（$TEST_DIR）..." -ForegroundColor Yellow

$foundFiles = @(Get-ChildItem -LiteralPath $TEST_DIR -Filter '*.xiyi' -File -Recurse -ErrorAction SilentlyContinue)
if ($foundFiles.Count -eq 0) {
    Exit-WithError "$TEST_DIR 下找不到任何 .xiyi 文件"
}

# list.json 的项目级默认值：优先级高于脚本的硬编码兜底值，
# 因为这是随代码库走的约定，而不是执行者当次敲命令时的临时参数。
$overrides   = @{}
$baseExpect  = 'PASS'
$baseTimeout = $TimeoutSeconds

function ConvertTo-ExpectValue {
    param([string]$Text)
    if ($Text -and (@('FAIL', 'EXPECT_FAIL') -contains $Text.ToUpperInvariant())) { return 'FAIL' }
    return 'PASS'
}

function Test-PositiveInt {
    param($Value, [ref]$Parsed)
    $n = 0
    if ([int]::TryParse("$Value", [System.Globalization.NumberStyles]::Integer, [System.Globalization.CultureInfo]::InvariantCulture, [ref]$n) -and $n -ge 1) {
        $Parsed.Value = $n
        return $true
    }
    return $false
}

if (Test-Path -LiteralPath $TEST_LIST_JSON) {
    try {
        $listJson = Get-Content -LiteralPath $TEST_LIST_JSON -Raw -Encoding UTF8 | ConvertFrom-Json -ErrorAction Stop
    } catch {
        # 解析失败必须报错退出，不能静默丢掉全部覆盖项
        Exit-WithError "解析 $TEST_LIST_JSON 失败: $($_.Exception.Message)"
    }

    if ($null -ne $listJson.version -and "$($listJson.version)" -ne '1') {
        Write-Warn "list.json 声明的 version=$($listJson.version)，本脚本只认识 version 1，按 1 处理"
    }

    if ($listJson.defaults) {
        if ($listJson.defaults.expect) {
            $baseExpect = ConvertTo-ExpectValue -Text "$($listJson.defaults.expect)"
        }
        if ($null -ne $listJson.defaults.timeout) {
            $parsed = 0
            if (Test-PositiveInt -Value $listJson.defaults.timeout -Parsed ([ref]$parsed)) {
                $baseTimeout = $parsed
            } else {
                Write-Warn "list.json defaults.timeout=`"$($listJson.defaults.timeout)`" 不是 >= 1 的整数，已忽略"
            }
        }
    }

    foreach ($t in @($listJson.tests)) {
        if (-not $t -or -not $t.file) { continue }
        $key = ("$($t.file)" -replace '\\', '/')
        if ($key.StartsWith('./')) { $key = $key.Substring(2) }
        $overrides[$key] = $t
    }
}

# 源码头内联元数据，例如：
#   // @expect fail
#   // @timeout 5
#   // @stderr-contains "类型不匹配"
#   // @tag parser, typecheck
#   // @skip 尚未实现 break
function Get-InlineMeta {
    param([string]$FilePath, [string]$DefaultExpect, [int]$DefaultTimeout)
    $meta = [ordered]@{
        Expect = $DefaultExpect; Timeout = $DefaultTimeout; StderrContains = $null
        Tags = @(); Skip = $false; Reason = ''
    }
    $lines = Get-Content -LiteralPath $FilePath -TotalCount 40 -ErrorAction SilentlyContinue
    foreach ($line in $lines) {
        if ($line -match '^\s*//\s*@expect\s+fail\s*$') {
            $meta.Expect = 'FAIL'
        } elseif ($line -match '^\s*//\s*@timeout\s+(\d{1,9})\s*$') {
            $n = [int]$Matches[1]
            if ($n -ge 1) { $meta.Timeout = $n }
        } elseif ($line -match '^\s*//\s*@stderr-contains\s+"([^"]*)"\s*$') {
            $meta.StderrContains = $Matches[1]
        } elseif ($line -match '^\s*//\s*@tag\s+(.+)$') {
            $meta.Tags = @($Matches[1] -split '[,\s]+' | Where-Object { $_ })
        } elseif ($line -match '^\s*//\s*@skip(\s+(.*))?$') {
            $meta.Skip = $true
            $meta.Reason = "$($Matches[2])"
        }
    }
    return $meta
}

# 先按相对路径 (正斜杠) 做"序数比较"排序，保证各机器上顺序一致，也与 sh 版一致
$testDirNorm = $TEST_DIR.TrimEnd('\', '/')
$rows = [System.Collections.Generic.List[object]]::new()
foreach ($f in $foundFiles) {
    $relPath = $f.FullName.Substring($testDirNorm.Length).TrimStart('\', '/') -replace '\\', '/'
    $rows.Add([pscustomobject]@{ Rel = $relPath; Full = $f.FullName })
}
$rows.Sort([System.Comparison[object]]{ param($a, $b) [string]::CompareOrdinal($a.Rel, $b.Rel) })

$relSet = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
$testCases = [System.Collections.Generic.List[object]]::new()

foreach ($row in $rows) {
    $rel  = $row.Rel
    $full = $row.Full
    [void]$relSet.Add($rel)

    # 优先级（低到高）：list.json 的 defaults 块 -> 源码内联注释 -> list.json 里针对该文件的例外条目。
    $meta = Get-InlineMeta -FilePath $full -DefaultExpect $baseExpect -DefaultTimeout $baseTimeout

    if ($overrides.ContainsKey($rel)) {
        $ov = $overrides[$rel]
        if ($ov.expect) { $meta.Expect = ConvertTo-ExpectValue -Text "$($ov.expect)" }
        if ($null -ne $ov.timeout) {
            $parsed = 0
            if (Test-PositiveInt -Value $ov.timeout -Parsed ([ref]$parsed)) {
                $meta.Timeout = $parsed
            } else {
                Write-Warn "list.json: $rel 的 timeout=`"$($ov.timeout)`" 不是 >= 1 的整数，已忽略"
            }
        }
        if ($ov.stderrContains) { $meta.StderrContains = "$($ov.stderrContains)" }
        if ($ov.tag) {
            $meta.Tags = @(@($ov.tag) | ForEach-Object { "$_" -split '[,\s]+' } | Where-Object { $_ })
        }
        if ($ov.skip -eq $true) { $meta.Skip = $true; $meta.Reason = "$($ov.reason)" }
    }

    $testCases.Add([pscustomobject]@{
        RelPath        = $rel
        FullPath       = $full
        Expect         = $meta.Expect
        Timeout        = [int]$meta.Timeout
        StderrContains = $meta.StderrContains
        Tags           = @($meta.Tags)
        Skip           = [bool]$meta.Skip
        Reason         = [string]$meta.Reason
        OutGolden      = "$full.out"
        ErrGolden      = "$full.err"
        ExitGolden     = "$full.exit"
    })
}

# list.json 里写了、磁盘上却不存在的条目：几乎必然是拼写/移动文件后忘了改，
# 否则覆盖项会被静默丢弃。
foreach ($key in $overrides.Keys) {
    if (-not $relSet.Contains([string]$key)) {
        Write-Warn "list.json 中的条目在 Tests/ 下找不到对应文件: $key"
    }
}

# ===== 过滤 =====
$filtered = [System.Collections.Generic.List[object]]::new()
foreach ($tc in $testCases) {
    if ($Expect -ne 'all') {
        $want = if ($Expect -eq 'pass') { 'PASS' } else { 'FAIL' }
        if ($tc.Expect -ne $want) { continue }
    }
    if ($Name) {
        $matched = $false
        foreach ($pat in $Name) {
            if ($tc.RelPath -like $pat) { $matched = $true; break }
        }
        if (-not $matched) { continue }
    }
    if ($Tag) {
        $matched = $false
        foreach ($want in $Tag) {
            if ($tc.Tags -contains $want) { $matched = $true; break }
        }
        if (-not $matched) { continue }
    }
    $filtered.Add($tc)
}

if ($filtered.Count -eq 0) {
    Exit-WithError '过滤后没有匹配的测试用例'
}

Write-Host "共 $($filtered.Count) 个测试用例（目录下总计发现 $($testCases.Count) 个）"
Write-Host ''

# ============================================================
# 单用例执行器
#   以"源码字符串"形式保存，串行/并行共用：并行时每个 runspace 用
#   [scriptblock]::Create 重建，避免通过 $using: 传递 ScriptBlock 对象。
#   因此这段代码必须自包含 (不能依赖外面定义的函数/变量)。
# ============================================================
$CaseRunnerText = @'
param($test, [string]$exe, [string]$stdlib, [string]$workDir, [bool]$updateGoldens, [bool]$strictCrash)

function New-CaseResult {
    param(
        [string]$Status, $Test, [string[]]$Reasons = @(), $ExitCode = $null,
        [double]$Elapsed = 0, [bool]$TimedOut = $false, [string]$Stdout = '', [string]$Stderr = ''
    )
    [pscustomobject]@{
        RelPath  = $Test.RelPath
        Status   = $Status
        Ok       = ($Status -ne 'FAIL')
        Skipped  = ($Status -eq 'SKIP')
        Updated  = ($Status -eq 'UPDATED')
        TimedOut = $TimedOut
        Reasons  = @($Reasons)
        ExitCode = $ExitCode
        Elapsed  = $Elapsed
        Stdout   = $Stdout
        Stderr   = $Stderr
    }
}

# 启动进程，异步读 stdout/stderr，超时则杀整个进程树。
# finally 保证：即使 Ctrl-C / 管道被停止，也不会留下孤儿进程。
function Invoke-XiyiProcess {
    param([string]$Exe, [string[]]$ArgList, [int]$TimeoutMs, [string]$WorkDir)

    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $Exe
    foreach ($a in $ArgList) { $psi.ArgumentList.Add($a) }
    $psi.WorkingDirectory       = $WorkDir
    $psi.UseShellExecute        = $false
    $psi.RedirectStandardInput  = $true    # 立刻关闭，避免用例读 stdin 时挂住 (sh 版重定向 /dev/null)
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError  = $true
    $psi.CreateNoWindow         = $true
    $psi.StandardOutputEncoding = [System.Text.UTF8Encoding]::new($false)
    $psi.StandardErrorEncoding  = [System.Text.UTF8Encoding]::new($false)

    $p = [System.Diagnostics.Process]::new()
    $p.StartInfo = $psi
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        [void]$p.Start()
        $null = $p.Handle   # 沿用之前踩过的坑：异步等待路径下先摸一次 Handle
        try { $p.StandardInput.Close() } catch { }

        $outTask = $p.StandardOutput.ReadToEndAsync()
        $errTask = $p.StandardError.ReadToEndAsync()

        $timedOut = $false
        if (-not $p.WaitForExit($TimeoutMs)) {
            $timedOut = $true
            try { $p.Kill($true) } catch {
                try { & taskkill /PID $p.Id /T /F 2>$null | Out-Null } catch { }
            }
            [void]$p.WaitForExit(10000)
        }

        # 读管道设上限：孙进程若还攥着句柄，ReadToEndAsync 可能永远不结束
        $stdout = ''
        $stderr = ''
        try { if ($outTask.Wait(5000)) { $stdout = [string]$outTask.Result } } catch { }
        try { if ($errTask.Wait(5000)) { $stderr = [string]$errTask.Result } } catch { }

        $exit = $null
        if (-not $timedOut) { $exit = $p.ExitCode }

        return [pscustomobject]@{
            TimedOut = $timedOut; ExitCode = $exit
            Stdout = $stdout; Stderr = $stderr
            Elapsed = $sw.Elapsed.TotalSeconds
        }
    } finally {
        try { if (-not $p.HasExited) { $p.Kill($true) } } catch { }
        $p.Dispose()
    }
}

function Format-Normalized {
    param([string]$Text)
    if ([string]::IsNullOrEmpty($Text)) { return "`n" }
    $t = $Text.Replace([string][char]0, '')                    # 去 NUL
    if ($t.Length -gt 0 -and $t[0] -eq [char]0xFEFF) { $t = $t.Substring(1) }   # 去开头 BOM
    $t = $t.Replace("`r`n", "`n").Replace("`r", "`n")          # CRLF / 孤立 CR -> LF
    return ($t.TrimEnd() + "`n")
}

# .exit golden 只接受 0-255 (POSIX 退出码范围)，与 sh 版完全一致。
# 原因：golden 文件是仓库里共享的，Linux 的退出码最大 255，更大的值 (如 NTSTATUS 风格的
# 3221225477) 在 Linux 上永远无法匹配。期望"编译器崩溃"不应写进 golden，请用 -StrictCrash。
# 无效 (非数字 / 超出范围) 返回 $null，由调用方判为失败。
function ConvertTo-ExpectedExitCode {
    param([string]$Text)
    $s = "$Text".TrimStart([char]0xFEFF).Trim()
    $n = 0
    if (-not [int]::TryParse($s, [System.Globalization.NumberStyles]::Integer, [System.Globalization.CultureInfo]::InvariantCulture, [ref]$n)) { return $null }
    if ($n -ge 0 -and $n -le 255) { return $n }
    return $null
}

# 识别崩溃：NTSTATUS 错误码 (0xC0000000-0xC000FFFF，如访问违例 / 栈溢出 / fail-fast) 或 panic / ICE 标志。
function Get-CrashInfo {
    param($ExitCode, [string]$Stderr)
    if ($null -ne $ExitCode) {
        $u = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int]$ExitCode), 0)
        if ($u -ge 3221225472 -and $u -lt 3221291008) {
            return ('进程异常终止 (NTSTATUS 0x{0:X8})' -f $u)
        }
    }
    if ($Stderr -and ($Stderr -match 'panicked at|internal compiler error')) {
        return 'stderr 含 panic/ICE 标志'
    }
    return $null
}

try {
    if ($test.Skip) {
        $why = 'skip'
        if ($test.Reason) { $why = "skip: $($test.Reason)" }
        return (New-CaseResult -Status 'SKIP' -Test $test -Reasons @($why))
    }

    $timeoutMs = [int][Math]::Min([long]$test.Timeout * 1000, [long][int]::MaxValue)
    $r = Invoke-XiyiProcess -Exe $exe -ArgList @('--stdlib', $stdlib, $test.FullPath) -TimeoutMs $timeoutMs -WorkDir $workDir

    if ($r.TimedOut) {
        return (New-CaseResult -Status 'FAIL' -Test $test -Reasons @("timeout $($test.Timeout)s") `
            -Elapsed $r.Elapsed -TimedOut $true -Stdout $r.Stdout -Stderr $r.Stderr)
    }

    if ($updateGoldens) {
        try {
            Set-Content -LiteralPath $test.OutGolden -Value $r.Stdout -Encoding utf8 -NoNewline -ErrorAction Stop
            Set-Content -LiteralPath $test.ErrGolden -Value $r.Stderr -Encoding utf8 -NoNewline -ErrorAction Stop
        } catch {
            return (New-CaseResult -Status 'FAIL' -Test $test -Reasons @("无法写入 golden: $($_.Exception.Message)") `
                -ExitCode $r.ExitCode -Elapsed $r.Elapsed -Stdout $r.Stdout -Stderr $r.Stderr)
        }
        return (New-CaseResult -Status 'UPDATED' -Test $test -Reasons @('golden 已更新，请 git diff 审查') `
            -ExitCode $r.ExitCode -Elapsed $r.Elapsed -Stdout $r.Stdout -Stderr $r.Stderr)
    }

    $ok = $true
    $reasons = [System.Collections.Generic.List[string]]::new()

    # --- 退出码：.exit golden 优先，否则按 Expect 推导 ---
    $haveExitGolden = Test-Path -LiteralPath $test.ExitGolden -PathType Leaf
    $expectedExit = $null
    if ($haveExitGolden) {
        $rawExit = Get-Content -LiteralPath $test.ExitGolden -Raw -ErrorAction SilentlyContinue
        $expectedExit = ConvertTo-ExpectedExitCode -Text $rawExit
        if ($null -eq $expectedExit) {
            $ok = $false
            $rawExitText = "$rawExit".Trim()
            $reasons.Add("无效的 .exit golden: `"$rawExitText`" (只接受 0-255，这样两个平台才能共享)")
        }
    }

    if ($haveExitGolden -and $null -eq $expectedExit) {
        # .exit golden 存在但内容无效：上面已记录原因并置 ok=$false，
        # 这里显式不再回退到"按 Expect 推导"，避免跳过退出码检查。
        $ok = $false
    } elseif ($null -ne $expectedExit) {
        if ($r.ExitCode -ne $expectedExit) {
            $ok = $false
            $reasons.Add("exit $($r.ExitCode), expected $expectedExit")
        }
    } elseif ($test.Expect -eq 'FAIL') {
        if ($r.ExitCode -eq 0) {
            $ok = $false
            $reasons.Add('expected failure but exited 0')
        }
    } else {
        if ($r.ExitCode -ne 0) {
            $ok = $false
            $reasons.Add("exit $($r.ExitCode), expected 0")
        }
    }

    # --- golden 比对（有才比；没有就不强行要求空输出，避免历史测试集体炸红）---
    if (Test-Path -LiteralPath $test.OutGolden -PathType Leaf) {
        $golden = Get-Content -LiteralPath $test.OutGolden -Raw -Encoding utf8
        if ((Format-Normalized -Text $r.Stdout) -cne (Format-Normalized -Text $golden)) {
            $ok = $false
            $reasons.Add('stdout mismatch')
        }
    }
    if (Test-Path -LiteralPath $test.ErrGolden -PathType Leaf) {
        $golden = Get-Content -LiteralPath $test.ErrGolden -Raw -Encoding utf8
        if ((Format-Normalized -Text $r.Stderr) -cne (Format-Normalized -Text $golden)) {
            $ok = $false
            $reasons.Add('stderr mismatch')
        }
    }

    # --- stderr 子串（序数比较，等同 grep -F）---
    if ($test.StderrContains) {
        if ($r.Stderr.IndexOf([string]$test.StderrContains, [System.StringComparison]::Ordinal) -lt 0) {
            $ok = $false
            $reasons.Add("stderr missing: $($test.StderrContains)")
        }
    }

    # --- 崩溃处置 / 提示："期望失败"的用例很容易被编译器崩溃悄悄蒙混过关 ---
    $crash = Get-CrashInfo -ExitCode $r.ExitCode -Stderr $r.Stderr
    if ($crash -and -not $haveExitGolden -and $test.Expect -eq 'FAIL') {
        if ($strictCrash) {
            $ok = $false
            $reasons.Add("compiler crash: $crash")
        } elseif ($ok) {
            $reasons.Add("(提示: $crash，通过可能源于编译器崩溃而非预期诊断；加 -StrictCrash 视为失败)")
        }
    }

    if ($test.Expect -eq 'FAIL' -and -not $test.StderrContains -and -not (Test-Path -LiteralPath $test.ErrGolden -PathType Leaf)) {
        $reasons.Add('(提示: EXPECT_FAIL 未绑定 stderr 校验，通过不代表失败原因正确)')
    }

    $status = 'FAIL'
    if ($ok) { $status = 'PASS' }
    return (New-CaseResult -Status $status -Test $test -Reasons $reasons.ToArray() `
        -ExitCode $r.ExitCode -Elapsed $r.Elapsed -Stdout $r.Stdout -Stderr $r.Stderr)
} catch {
    return (New-CaseResult -Status 'FAIL' -Test $test -Reasons @("internal error: $($_.Exception.Message)"))
}
'@

# ============================================================
# 结果输出
# ============================================================
function Limit-Text {
    param([string]$Text, [int]$Max = 4096)
    if ($Text -and $Text.Length -gt $Max) { return $Text.Substring(0, $Max) + "`n...(截断)" }
    return $Text
}

function Write-CaseResult {
    param($r)
    $color = switch ($r.Status) {
        'SKIP'    { 'DarkGray' }
        'UPDATED' { 'Magenta' }
        'PASS'    { 'Green' }
        default   { 'Red' }
    }
    $suffix = ''
    if ($r.Reasons -and @($r.Reasons).Count -gt 0) { $suffix = ' - ' + (@($r.Reasons) -join '; ') }
    $elapsedText = ([double]$r.Elapsed).ToString('F3', [System.Globalization.CultureInfo]::InvariantCulture)
    Write-Host "[$($r.Status)] $($r.RelPath) (${elapsedText}s)$suffix" -ForegroundColor $color

    if ($r.Status -eq 'FAIL') {
        $reasonText = @($r.Reasons) -join '; '
        Add-LogLine ("`n---- $($r.RelPath) ----`nreasons: $reasonText`nstdout:`n$(Limit-Text $r.Stdout)`nstderr:`n$(Limit-Text $r.Stderr)")
    }
}

# 并行时某个 worker 没有产出结果 (异常/被杀)：必须记为失败，绝不能让汇总悄悄漏掉
function New-MissingResult {
    param($test)
    [pscustomobject]@{
        RelPath = $test.RelPath; Status = 'FAIL'; Ok = $false; Skipped = $false; Updated = $false
        TimedOut = $false; Reasons = @('no result recorded (worker error?)'); ExitCode = $null
        Elapsed = 0; Stdout = ''; Stderr = ''
    }
}

# ============================================================
# 步骤4：执行
# ============================================================
Write-Host '[3/4] 执行测试...' -ForegroundColor Yellow
Write-Host ''

$results   = [System.Collections.Generic.List[object]]::new()
$completed = $false

try {
    if ($Jobs -gt 1) {
        if ($FailFast) { Write-Warn '-FailFast 在并行模式下不生效' }

        $exeArg    = $XIYI_EXE
        $stdlibArg = $STDLIB
        $workArg   = $COMPILER_ROOT
        $updArg    = [bool]$UpdateGoldens
        $strictArg = [bool]$StrictCrash

        $parallelOut = @($filtered | ForEach-Object -ThrottleLimit $Jobs -Parallel {
            $sb = [scriptblock]::Create($using:CaseRunnerText)
            & $sb $_ $using:exeArg $using:stdlibArg $using:workArg $using:updArg $using:strictArg
        })

        # 并行输出是完成顺序；按用例顺序归位，并补齐缺失结果
        $byRel = @{}
        foreach ($r in $parallelOut) {
            if ($r -and $r.RelPath) { $byRel[[string]$r.RelPath] = $r }
        }
        foreach ($tc in $filtered) {
            $r = $byRel[[string]$tc.RelPath]
            if (-not $r) { $r = New-MissingResult -test $tc }
            $results.Add($r)
            Write-CaseResult $r
        }
    } else {
        $runner = [scriptblock]::Create($CaseRunnerText)
        foreach ($tc in $filtered) {
            $r = & $runner $tc $XIYI_EXE $STDLIB $COMPILER_ROOT ([bool]$UpdateGoldens) ([bool]$StrictCrash)
            if ($r -is [array]) { $r = $r[-1] }
            if (-not $r) { $r = New-MissingResult -test $tc }
            $results.Add($r)
            Write-CaseResult $r
            if ($FailFast -and $r.Status -eq 'FAIL') {
                Write-Host '⏹  -FailFast 触发，停止后续测试' -ForegroundColor Yellow
                break
            }
        }
    }
    $completed = $true
} finally {
    if (-not $completed) {
        Add-LogLine "运行被中断: 已完成 $($results.Count)/$($filtered.Count) 个用例；未生成汇总与 JSON"
    }
}
Write-Host ''

# ============================================================
# 步骤5：汇总（人读 log + 机器读 json）
# ============================================================
Write-Host '[4/4] 汇总结果...' -ForegroundColor Yellow

$total        = $results.Count
$passedCount  = 0
$failedCount  = 0
$timeoutCount = 0
$skippedCount = 0
$updatedCount = 0
foreach ($r in $results) {
    switch ($r.Status) {
        'PASS'    { $passedCount++ }
        'SKIP'    { $skippedCount++ }
        'UPDATED' { $updatedCount++ }
        default   {
            $failedCount++
            if ($r.TimedOut) { $timeoutCount++ }
        }
    }
}

Write-Host '========================================' -ForegroundColor Cyan
Write-Host ' 测试结果汇总' -ForegroundColor Cyan
Write-Host '========================================' -ForegroundColor Cyan
Write-Host " 总计:   $total" -ForegroundColor White
Write-Host " 通过:   $passedCount" -ForegroundColor Green
Write-Host " 失败:   $failedCount" -ForegroundColor Red
Write-Host " 超时:   $timeoutCount" -ForegroundColor Yellow
Write-Host " 跳过:   $skippedCount" -ForegroundColor DarkGray
Write-Host " 已更新 golden: $updatedCount" -ForegroundColor Magenta
Write-Host '========================================' -ForegroundColor Cyan
Write-Host " 日志:  $LOG_FILE"
Write-Host " 结果:  $JSON_FILE"
Write-Host '========================================' -ForegroundColor Cyan

# JSON 字段与 sh 版保持一致 (不含 Stdout/Stderr，完整输出看 .log)；先写临时文件再移动，避免中断留下半截文件
$jsonResults = @(foreach ($r in $results) {
    [pscustomobject][ordered]@{
        RelPath  = $r.RelPath
        Status   = $r.Status
        Ok       = [bool]$r.Ok
        Skipped  = [bool]$r.Skipped
        Updated  = [bool]$r.Updated
        Elapsed  = [Math]::Round([double]$r.Elapsed, 3)
        ExitCode = $r.ExitCode
        Reasons  = @($r.Reasons)
    }
})

$summary = [pscustomobject][ordered]@{
    RunId    = $runId
    Version  = $xiyiVersion
    Total    = $total
    Passed   = $passedCount
    Failed   = $failedCount
    Timeouts = $timeoutCount
    Skipped  = $skippedCount
    Updated  = $updatedCount
    Results  = $jsonResults
}

try {
    $jsonTmp = "$JSON_FILE.tmp.$PID"
    [System.IO.File]::WriteAllText($jsonTmp, ($summary | ConvertTo-Json -Depth 4), [System.Text.UTF8Encoding]::new($false))
    [System.IO.File]::Move($jsonTmp, $JSON_FILE, $true)
} catch {
    Write-Warn "写入 $JSON_FILE 失败: $($_.Exception.Message)"
}

Add-LogLine "汇总: 总计 $total, 通过 $passedCount, 失败 $failedCount, 超时 $timeoutCount, 跳过 $skippedCount, 已更新 $updatedCount"

# ===== 退出码 =====
if ($failedCount -gt 0) {
    Write-Host "❌ 有 $failedCount 个测试失败" -ForegroundColor Red
    exit 1
}

if ($UpdateGoldens) {
    Write-Host '✅ Golden 更新完成，请务必 git diff 审查改动后再提交' -ForegroundColor Green
    exit 0
}

Write-Host '✅ 所有测试通过！' -ForegroundColor Green
exit 0
