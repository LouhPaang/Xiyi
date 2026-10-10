# 希夷编译器测试套件
# run_all_tests.ps1
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
#
# 多值参数 (-Name -NameRegex -Exclude -Tag) 的写法 —— 与 sh 版的"重复 --name"不同，这是 PowerShell 的限制:
#   在 PowerShell 会话里，或用 pwsh -Command:   -Name 'lexer/*','parser/*'
#   用 pwsh -File 时，逗号不会被拆分、同名参数也不能重复，每个选项只能给一个值。
#   这时想"匹配多个路径"请改用 -NameRegex '^(lexer|parser)/'；-Tag 例外 (见下)，两种方式下 -Tag a,b 都可用。
# .exit golden 两边都只接受 0-255 (POSIX 退出码范围)，保证仓库里共享的 golden 在两个平台含义一致。
# 退出码: 0 成功 (含"空分片"与"上次没有失败"); 1 有用例失败或运行期致命错误; 2 脚本自己检出的参数错误
#         (参数绑定阶段由 PowerShell 自身拦截的错误，如 -Jobs 0，退出码是 1，这是 PowerShell 的行为)。
# 平台细节差异: sh 以"信号 / 退出码 > 128"识别崩溃，这里以 NTSTATUS 错误码
# (0xC0000000-0xC000FFFF，如访问违例、栈溢出) 识别；超时由 WaitForExit 直接判定，
# 不依赖退出码。
#
# 注意: 为了让低版本 PowerShell 也能"解析通过并走到下面的版本检查、给出友好提示"，
# 本文件在版本检查之前不使用任何 PowerShell 7 才有的语法 (三元、??、&& 等)。

param(
    [ValidateSet('all', 'pass', 'fail')]
    [string]$Expect = 'all',

    # 匹配相对 Tests/ 的路径 (PowerShell -like 通配符)，可给多个值；正反斜杠均可。
    # 通配符里的转义符是反引号 `，不是反斜杠 (sh 版 glob 用反斜杠转义)。
    [ValidateScript({ if (-not [string]::IsNullOrEmpty($_)) { $true } else { throw '-Name 的值不能为空' } })]
    [string[]]$Name,

    # 匹配相对 Tests/ 的路径 (.NET 正则，子串匹配，需整串匹配请自己加 ^$)，
    # 可重复；与 -Name 是"或"的关系。区分大小写。
    [ValidateScript({ if (-not [string]::IsNullOrEmpty($_)) { $true } else { throw '-NameRegex 的值不能为空' } })]
    [string[]]$NameRegex,

    # 排除匹配的用例 (-like 通配符)，可给多个值；优先级最高。区分大小写。
    [ValidateScript({ if (-not [string]::IsNullOrEmpty($_)) { $true } else { throw '-Exclude 的值不能为空' } })]
    [string[]]$Exclude,

    # 匹配源码头 // @tag a, b 或 list.json 里的 tag (大小写不敏感)。
    # 唯一例外地支持逗号分隔 (-Tag a,b)：tag 本身不含逗号，拆分没有歧义，pwsh -File 也能传多个 tag。
    [ValidateScript({ if ($_ -match '^[^\s,]+(,[^\s,]+)*\z') { $true } else { throw "-Tag 的值不能为空、不能含空白，逗号只能用来分隔多个 tag: '$_'" } })]
    [string[]]$Tag,

    # 只跑本机上一次完成的运行 (test_results/ 下最新的 YYYYMMDD_HHMMSS*.json)
    # 里失败的用例；上次全部通过则直接成功退出
    [switch]$RerunFailed,

    # "M/N": 把过滤后的用例分成 N 片，只执行第 M 片 (1 <= M <= N)，用于 CI 多机并行；
    # 某一片为空时成功退出
    [string]$Shard,

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

function Exit-WithError {
    param([string]$Message)
    Write-Host "❌ $Message" -ForegroundColor Red
    exit 1
}

# 参数用法错误：退出码 2，与 sh 版一致，方便 CI 把"用法错误"和"测试失败"区分开
function Exit-WithUsage {
    param([string]$Message)
    Write-Host "❌ $Message" -ForegroundColor Red
    exit 2
}

function Write-Warn {
    param([string]$Message)
    Write-Host "⚠️  $Message" -ForegroundColor Yellow
}

# 路径过滤器 (-Name / -Exclude)：Windows 上 \ 就是目录分隔符，匹配前统一成 /。
# 这是与 sh 版唯一有意保留的差异：Linux 上 \ 是合法的文件名字符与 glob 转义，sh 版不做任何转换。
# 对 -like 来说这个转换是安全的：它的转义符是反引号，不是反斜杠，不会破坏转义语义。
# -NameRegex 不转换：正则里的 \ 是转义。
if ($Name)    { $Name    = @($Name    | ForEach-Object { $_ -replace '\\', '/' }) }
if ($Exclude) { $Exclude = @($Exclude | ForEach-Object { $_ -replace '\\', '/' }) }

if ($Tag)     { $Tag     = @($Tag     | ForEach-Object { $_ -split ',' } | Where-Object { $_ }) }

# 显式传了 -Name $null / -Name @() 之类：绑定得过去，但会让过滤器悄悄失效，所以拦下来。
foreach ($pn in @('Name', 'NameRegex', 'Exclude', 'Tag')) {
    if ($PSBoundParameters.ContainsKey($pn)) {
        $pv = $PSBoundParameters[$pn]
        if ($null -eq $pv -or @($pv).Count -eq 0) { Exit-WithUsage "-$pn 的值不能为空" }
    }
}

# 通配符是否合法：-like 遇到非法写法 (如未闭合的 [) 会在匹配时才抛 WildcardPatternException，
# 放到过滤循环里就成了半路崩溃。这里用和过滤时完全相同的运算先试一次。
function Get-GlobError {
    param([string]$Pattern)
    try {
        $null = ('' -clike $Pattern)
        return ''
    } catch {
        return $_.Exception.Message
    }
}
foreach ($pair in @(@('-Name', $Name), @('-Exclude', $Exclude))) {
    foreach ($pat in @($pair[1])) {
        if ($null -eq $pat) { continue }
        $globErr = Get-GlobError -Pattern ([string]$pat)
        if ($globErr) { Exit-WithUsage "$($pair[0]) 不是合法的通配符: '$pat' ($globErr)" }
    }
}

# ============================================================
# -NameRegex 正则合法性预检（.NET 正则；-cmatch 用的就是同一个引擎）
# ============================================================
foreach ($rx in @($NameRegex)) {
    if ($null -eq $rx) { continue }
    try {
        $null = [System.Text.RegularExpressions.Regex]::new([string]$rx)
    } catch {
        Exit-WithUsage "-NameRegex 不是合法的正则: '$rx' ($($_.Exception.Message))"
    }
}

# ============================================================
# -Shard 解析（M/N）
#   用 ContainsKey 判断"是否给了"：-Shard '' 也算给了 (然后因格式不对被拒绝)，
#   而不是被当成没给而静默忽略。\z 表示真正的字符串结尾 ($ 会在末尾换行前匹配)。
# ============================================================
$shardGiven = $PSBoundParameters.ContainsKey('Shard')
$shardIndex = 1
$shardTotal = 1
if ($shardGiven) {
    if ($Shard -cmatch '^([0-9]{1,9})/([0-9]{1,9})\z') {
        $shardIndex = [int]$Matches[1]
        $shardTotal = [int]$Matches[2]
    } else {
        Exit-WithUsage "-Shard 格式应为 M/N (例如 2/4)，收到: '$Shard'"
    }
    if ($shardIndex -lt 1 -or $shardIndex -gt $shardTotal) {
        Exit-WithUsage "-Shard M/N 要求 1 <= M <= N，收到: $shardIndex/$shardTotal"
    }
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
        if ((Test-Path -LiteralPath $testsPath -PathType Container) -and (Test-Path -LiteralPath $stdPath -PathType Container)) {
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
            if ($inPackage -and $line -match '^\s*version\s*=\s*["'']([^"'']+)["'']') {
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

# $PSScriptRoot 只有"以脚本文件方式运行"时才有值 (用 -Command 或粘贴执行时为空)
if (-not $PSScriptRoot) {
    Exit-WithError '无法确定脚本所在目录：请把本文件保存下来再运行 (pwsh -File .\run_all_tests.ps1)'
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

# -RerunFailed 的数据来源 = 上一次"完成"的运行留下的 JSON (被中断的运行不会产生 JSON)。
# 必须赶在轮转之前选定并加以保护，否则 -KeepLogs 1 时它会被当成旧文件清掉。
# 只认本脚本命名规则 (YYYYMMDD_HHMMSS*.json)，不会误取别人放进来的其它 json。
$rerunSrc = ''
if ($RerunFailed) {
    $candidates = @(Get-ChildItem -LiteralPath $LOG_DIR -Filter '*.json' -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^[0-9]{8}_[0-9]{6}.*\.json\z' -and $_.FullName -ne $JSON_FILE } |
        Sort-Object -Property @{ Expression = 'LastWriteTime'; Descending = $true }, @{ Expression = 'Name'; Descending = $true })
    if ($candidates.Count -gt 0) { $rerunSrc = $candidates[0].FullName }
}

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
    param([string]$Extension, [int]$KeepOthers, [string[]]$Protect)
    # Windows 的 -Filter 对"恰好 3 个字符的扩展名"有历史遗留行为 (如 *.log 也会匹配 x.logs)，
    # 所以拿到结果后再按扩展名精确核对一遍，避免误删别的文件。
    $others = @(Get-ChildItem -LiteralPath $LOG_DIR -Filter "*$Extension" -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Extension -eq $Extension -and $Protect -notcontains $_.FullName } |
        Sort-Object -Property @{ Expression = 'LastWriteTime'; Descending = $true }, @{ Expression = 'Name'; Descending = $true })
    if ($others.Count -gt $KeepOthers) {
        $others | Select-Object -Skip $KeepOthers | Remove-Item -Force -ErrorAction SilentlyContinue
    }
}
# .log 本次已创建；.json 要到最后才写。两者都是"再留 KeepLogs-1 份旧的"，总数恰为 KeepLogs。
# -RerunFailed 的数据源也要保护，否则 -KeepLogs 1 时它会先被清掉。
$protectSet = @($LOG_FILE, $JSON_FILE)
if ($rerunSrc) { $protectSet += $rerunSrc }
Remove-OldLogs -Extension '.log'  -KeepOthers ($KeepLogs - 1) -Protect $protectSet
Remove-OldLogs -Extension '.json' -KeepOthers ($KeepLogs - 1) -Protect $protectSet

$xiyiVersion = Get-XiyiVersion -CompilerRoot $COMPILER_ROOT

Write-Host '========================================' -ForegroundColor Cyan
Write-Host ' 希夷编译器测试套件' -ForegroundColor Cyan
Write-Host " xiyi-compiler 版本: $xiyiVersion" -ForegroundColor Cyan
$shardInfo = ''
if ($shardGiven) { $shardInfo = "   分片: $shardIndex/$shardTotal" }
Write-Host " Expect 过滤: $Expect   并发: $Jobs   超时(默认): ${TimeoutSeconds}s${shardInfo}" -ForegroundColor Cyan
Write-Host " 编译器目录: $COMPILER_ROOT" -ForegroundColor DarkGray
Write-Host " 标准库目录: $STDLIB" -ForegroundColor DarkGray
Write-Host " 测试目录:   $TEST_DIR" -ForegroundColor DarkGray
Write-Host " 日志:       $LOG_FILE" -ForegroundColor DarkGray
Write-Host '========================================' -ForegroundColor Cyan
Write-Host ''

Add-LogLine "运行 ID: $runId"
Add-LogLine "版本: $xiyiVersion"
Add-LogLine "参数: Expect=$Expect Jobs=$Jobs SkipBuild=$SkipBuild Release=$Release UpdateGoldens=$UpdateGoldens FailFast=$FailFast StrictCrash=$StrictCrash"
$shardLog = '无'
if ($shardGiven) { $shardLog = "$shardIndex/$shardTotal" }
Add-LogLine ("过滤: name=[$($Name -join '; ')] name-regex=[$($NameRegex -join '; ')] exclude=[$($Exclude -join '; ')] tag=[$($Tag -join '; ')] rerun-failed=$([int][bool]$RerunFailed) shard=$shardLog")

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

$foundFiles = @(Get-ChildItem -LiteralPath $TEST_DIR -Filter '*.xiyi' -File -Recurse -ErrorAction SilentlyContinue -ErrorVariable scanErrors)
# 权限等原因读不了的目录：不静默，否则"少了几个用例"没人知道
foreach ($e in @($scanErrors)) { Write-Warn "扫描 Tests/ 时出错，可能漏掉用例: $($e.Exception.Message)" }
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
$rows = [System.Collections.Generic.List[object]]::new()
foreach ($f in $foundFiles) {
    # GetRelativePath 不依赖"两个路径前缀字面相同" (大小写、结尾分隔符都不会让它错位)
    $relPath = [System.IO.Path]::GetRelativePath($TEST_DIR, $f.FullName) -replace '\\', '/'
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

# ============================================================
# 过滤
#   一个用例要被选中，必须"同时"满足 (AND)：
#     1. -Expect                期望结果相符
#     2. -Name / -NameRegex     两者合起来任一命中即可 (OR)；都没给就不限制
#     3. -Tag                   任一命中即可 (OR)；没给就不限制
#     4. -RerunFailed           在上一次完成的运行里是 FAIL
#   满足以上之后，被任何一条 -Exclude 命中就剔除 (优先级最高)。
#   最后才按 -Shard 切片：切片永远作用在"已经过滤完"的有序列表上。
#   路径匹配一律区分大小写 (-clike / -cmatch)，与 bash 的 [[ == ]] / [[ =~ ]] 一致；
#   tag 匹配大小写不敏感 (sh 版把两边都小写化)，因为 tag 词元惯例上全小写。
# ============================================================

# -RerunFailed: 读取上一次完成的运行里失败的用例
$failedSet = @{}
if ($RerunFailed) {
    if (-not $rerunSrc) {
        Exit-WithError "-RerunFailed: $LOG_DIR 下没有任何历史 JSON 结果，请先完整跑一次"
    }
    $srcName = [System.IO.Path]::GetFileName($rerunSrc)
    try {
        $prev = Get-Content -LiteralPath $rerunSrc -Raw -Encoding UTF8 | ConvertFrom-Json -ErrorAction Stop
    } catch {
        Exit-WithError "-RerunFailed: 无法读取 ${srcName}: $($_.Exception.Message)"
    }
    if ($null -eq $prev.Results) {
        Exit-WithError "-RerunFailed: ${srcName} 缺少 Results 数组"
    }
    foreach ($item in @($prev.Results)) {
        if ($item -and $item.Status -eq 'FAIL' -and $item.RelPath) {
            $failedSet["$($item.RelPath)"] = $true
            # 用例被改名或删除后，旧记录对不上任何文件：提醒一声，而不是静默丢掉
            if (-not $relSet.Contains("$($item.RelPath)")) {
                Write-Warn "上次失败的用例已不存在，将被忽略: $($item.RelPath)"
            }
        }
    }
    if ($failedSet.Count -eq 0) {
        # 上次全绿不是错误："先跑一遍、再重跑失败项"的用法里，这应当算成功
        Write-Host "✅ ${srcName} 里没有失败的用例，无需重跑" -ForegroundColor Green
        Add-LogLine "-RerunFailed: ${srcName} 中没有失败用例，未执行任何测试"
        exit 0
    }
    Write-Host "[rerun-failed] 从 ${srcName} 载入 $($failedSet.Count) 个失败用例" -ForegroundColor DarkGray
}

$filtered = [System.Collections.Generic.List[object]]::new()
foreach ($tc in $testCases) {
    if ($Expect -ne 'all') {
        $want = if ($Expect -eq 'pass') { 'PASS' } else { 'FAIL' }
        if ($tc.Expect -ne $want) { continue }
    }

    if (($Name -and $Name.Count -gt 0) -or ($NameRegex -and $NameRegex.Count -gt 0)) {
        $matched = $false
        if ($Name) {
            foreach ($pat in $Name) {
                if ($tc.RelPath -clike $pat) { $matched = $true; break }
            }
        }
        if (-not $matched -and $NameRegex) {
            foreach ($rx in $NameRegex) {
                if ($tc.RelPath -cmatch $rx) { $matched = $true; break }
            }
        }
        if (-not $matched) { continue }
    }

    if ($Tag -and $Tag.Count -gt 0) {
        $matched = $false
        foreach ($want in $Tag) {
            if ($tc.Tags -contains $want) { $matched = $true; break }
        }
        if (-not $matched) { continue }
    }

    if ($RerunFailed -and -not $failedSet.ContainsKey($tc.RelPath)) { continue }

    if ($Exclude -and $Exclude.Count -gt 0) {
        $excluded = $false
        foreach ($pat in $Exclude) {
            if ($tc.RelPath -clike $pat) { $excluded = $true; break }
        }
        if ($excluded) { continue }
    }

    $filtered.Add($tc)
}

if ($filtered.Count -eq 0) {
    Exit-WithError '过滤后没有匹配的测试用例'
}

# ============================================================
# 分片
#   $rows 已经按 CompareOrdinal 排好序，与 sh 版的 LC_ALL=C sort 一致；
#   按位置轮转 (而非按目录切块)，可以把相邻的、通常耗时相近的用例打散，
#   负载更均衡。N 大于用例数时后面的分片会空——不是错误。
# ============================================================
if ($shardGiven) {
    $shardBefore = $filtered.Count
    $shardPicked = [System.Collections.Generic.List[object]]::new()
    for ($i = 0; $i -lt $filtered.Count; $i++) {
        if (($i % $shardTotal) -eq ($shardIndex - 1)) {
            $shardPicked.Add($filtered[$i])
        }
    }
    $filtered = $shardPicked
    Write-Host "[shard $shardIndex/$shardTotal] 过滤后共 $shardBefore 个用例，本片 $($filtered.Count) 个" -ForegroundColor DarkGray
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
            # 直接用 .NET 写：UTF-8 无 BOM、不加换行，空内容也会得到空文件
            $utf8 = [System.Text.UTF8Encoding]::new($false)
            [System.IO.File]::WriteAllText($test.OutGolden, $r.Stdout, $utf8)
            [System.IO.File]::WriteAllText($test.ErrGolden, $r.Stderr, $utf8)
        } catch {
            return (New-CaseResult -Status 'FAIL' -Test $test -Reasons @("无法写入 golden: $($_.Exception.Message)") `
                -ExitCode $r.ExitCode -Elapsed $r.Elapsed -Stdout $r.Stdout -Stderr $r.Stderr)
        }
        return (New-CaseResult -Status 'UPDATED' -Test $test -Reasons @('golden 已更新，请 git diff 审查') `
            -ExitCode $r.ExitCode -Elapsed $r.Elapsed -Stdout $r.Stdout -Stderr $r.Stderr)
    }

    $ok = $true
    $reasons = [System.Collections.Generic.List[string]]::new()

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

    if ($haveExitGolden) {
        # golden 无效的情况上面已经记了原因并置为失败，这里只处理有效值
        if ($null -ne $expectedExit -and $r.ExitCode -ne $expectedExit) {
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

    if ($test.StderrContains) {
        if ($r.Stderr.IndexOf([string]$test.StderrContains, [System.StringComparison]::Ordinal) -lt 0) {
            $ok = $false
            $reasons.Add("stderr missing: $($test.StderrContains)")
        }
    }

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
$doneCount = 0      # 已经出结果的用例数，仅用于"被中断"时的日志

try {
    if ($Jobs -gt 1) {
        if ($FailFast) { Write-Warn '-FailFast 在并行模式下不生效' }

        $exeArg    = $XIYI_EXE
        $stdlibArg = $STDLIB
        $workArg   = $COMPILER_ROOT
        $updArg    = [bool]$UpdateGoldens
        $strictArg = [bool]$StrictCrash

        # 边完成边打印 (按完成顺序)，长时间运行时不至于"一片空白"；
        # 但 JSON / 汇总仍按排序后的顺序，保证与串行模式、与 sh 版一致。
        $byRel = @{}
        $filtered | ForEach-Object -ThrottleLimit $Jobs -Parallel {
            $sb = [scriptblock]::Create($using:CaseRunnerText)
            & $sb $_ $using:exeArg $using:stdlibArg $using:workArg $using:updArg $using:strictArg
        } | ForEach-Object {
            # 执行器只应返回一个带 RelPath 的结果对象；其它杂散输出直接忽略
            if ($_ -and $_.RelPath) {
                $byRel[[string]$_.RelPath] = $_
                $doneCount++
                Write-CaseResult $_
            }
        }

        foreach ($tc in $filtered) {
            $r = $byRel[[string]$tc.RelPath]
            if (-not $r) {
                $r = New-MissingResult -test $tc
                Write-CaseResult $r
            }
            $results.Add($r)
        }
    } else {
        $runner = [scriptblock]::Create($CaseRunnerText)
        foreach ($tc in $filtered) {
            $r = & $runner $tc $XIYI_EXE $STDLIB $COMPILER_ROOT ([bool]$UpdateGoldens) ([bool]$StrictCrash)
            if ($r -is [array]) { $r = $r[-1] }
            if (-not $r) { $r = New-MissingResult -test $tc }
            $results.Add($r)
            $doneCount++
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
        Add-LogLine "运行被中断: 已完成 $doneCount/$($filtered.Count) 个用例；未生成汇总与 JSON"
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

$jsonTmp = "$JSON_FILE.tmp.$PID"
try {
    [System.IO.File]::WriteAllText($jsonTmp, ($summary | ConvertTo-Json -Depth 4), [System.Text.UTF8Encoding]::new($false))
    [System.IO.File]::Move($jsonTmp, $JSON_FILE, $true)
} catch {
    Write-Warn "写入 $JSON_FILE 失败: $($_.Exception.Message)"
    Remove-Item -LiteralPath $jsonTmp -Force -ErrorAction SilentlyContinue
}

Add-LogLine "汇总: 总计 $total, 通过 $passedCount, 失败 $failedCount, 超时 $timeoutCount, 跳过 $skippedCount, 已更新 $updatedCount"

if ($failedCount -gt 0) {
    Write-Host "❌ 有 $failedCount 个测试失败" -ForegroundColor Red
    exit 1
}

if ($total -eq 0) {
    Write-Host "ℹ️  本次没有分到任何用例 (分片 $shardIndex/$shardTotal 为空)，视为成功" -ForegroundColor Yellow
    exit 0
}

if ($UpdateGoldens) {
    Write-Host '✅ Golden 更新完成，请务必 git diff 审查改动后再提交' -ForegroundColor Green
    exit 0
}

Write-Host '✅ 所有测试通过！' -ForegroundColor Green
exit 0
