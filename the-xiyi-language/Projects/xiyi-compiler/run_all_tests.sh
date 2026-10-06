#!/usr/bin/env bash
# run_all_tests.sh — 希夷编译器测试套件 (Linux/macOS)
# 对应 Windows 上的 run_all_tests.ps1
#
# 依赖:
#   bash >= 5.3 (低于此版本直接退出), timeout (GNU coreutils / busybox; macOS 为 gtimeout), cargo
#   jq  (仅当存在 Tests/list.json 时需要)
#
# 平台:
#   仅支持 64 位 Linux 与 macOS；Windows (含 Git Bash / MSYS / Cygwin) 请使用 run_all_tests.ps1。
#   位数以 getconf LONG_BIT 报告的用户空间位数为准，getconf 不可用时退回 uname -m 白名单:
#     x86_64 / amd64 / aarch64 / arm64 / riscv64 / ppc64 / ppc64le / s390x /
#     mips64 / mips64el / loongarch64
#   32 位 (i686、armv7l 等) 会被脚本主动拒绝。
#   仅调试脚本本身时，可设 XIYI_TESTS_FORCE=1 跳过操作系统与位数自检 (bash 版本检查不可跳过)。
#
# .exit golden 两边都只接受 0-255 (POSIX 退出码范围)，保证仓库里共享的 golden 在两个平台含义一致。
# 与 run_all_tests.ps1 的语义保持基本一致 (参数、过滤、golden、超时、JSON 字段)；
# 两者的平台细节差异 (信号 vs NTSTATUS 崩溃码等) 见各自的注释。
#
# macOS: brew install bash coreutils jq，然后用新 bash 运行。

# ---- 解释器自检(必须用 POSIX 语法写，因为此时可能还在 dash 里) ----
if [ -z "${BASH_VERSION:-}" ]; then
    echo "❌ 请用 bash 运行本脚本 (bash run_all_tests.sh)，不要用 sh" >&2
    exit 1
fi
# 先认操作系统，再比版本：Windows 上的 Git Bash 应该先看到"请用 ps1"，而不是"bash 版本太低"
if [ "${XIYI_TESTS_FORCE:-0}" != "1" ]; then
    case "$(uname -s 2>/dev/null || echo unknown)" in
        Linux|Darwin) ;;
        *)
            echo "❌ 不支持的操作系统: $(uname -s 2>/dev/null || echo unknown)" >&2
            echo "   本脚本只支持 Linux 与 macOS；Windows 请使用 run_all_tests.ps1。" >&2
            exit 1
            ;;
    esac
fi
if (( BASH_VERSINFO[0] < 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] < 3) )); then
    echo "❌ 需要 bash >= 5.3，当前 $BASH_VERSION" >&2
    echo "   macOS: brew install bash，并用新 bash 运行 (自带的是 3.2)。" >&2
    echo "   Linux: 请从发行版仓库 / 源码安装 bash 5.3 或更新版本。" >&2
    exit 1
fi

# 故意不加 -e：单个用例失败不能中断整个套件；错误由各处显式检查。
set -uo pipefail

# ============================================================
# 参数默认值
# ============================================================
EXPECT="all"
NAME_FILTERS=()
TAG_FILTERS=()
SKIP_BUILD=0
RELEASE=0
FAIL_FAST=0
UPDATE_GOLDENS=0
STRICT_CRASH=0
JOBS=1
TIMEOUT_SECONDS=30
KEEP_LOGS=20

usage() {
    cat <<'EOF'
用法: run_all_tests.sh [选项]

选项 (同时支持 --opt value 与 --opt=value):
  --expect {all|pass|fail}   只跑期望通过/期望失败的用例 (默认 all)
  --name PATTERN             匹配相对 Tests/ 的路径 (glob)，可重复
  --tag TAG                  匹配 tag，可重复
  --skip-build               跳过 cargo build
  --release                  使用 release profile
  --fail-fast                串行模式下遇到第一个失败即停止 (并行模式忽略)
  --update-goldens           把本次实际输出写回 .out / .err
  --strict-crash             进程被信号杀死 / 出现 Rust panic / ICE 时，
                             即使该用例"期望失败"也判为失败 (有 .exit golden 时不适用)
  --jobs N                   并行度 (默认 1)
  --timeout SECONDS          默认超时秒数，须 >= 1 (默认 30)
  --keep-logs N              只保留最近 N 次日志，须 >= 1 (默认 20)
  -h, --help                 显示本帮助
EOF
}

die_usage() {
    echo "$*" >&2
    usage >&2
    exit 2
}

is_uint() { [[ "$1" =~ ^[0-9]{1,9}$ ]]; }
need_arg() { (( $# >= 2 )) || die_usage "选项 $1 需要一个参数"; }

while (( $# > 0 )); do
    # --opt=value → --opt value
    if [[ "$1" == --*=* ]]; then
        set -- "${1%%=*}" "${1#*=}" "${@:2}"
    fi
    case "$1" in
        --expect)         need_arg "$@"; EXPECT="${2,,}"; shift 2;;
        --name)           need_arg "$@"; NAME_FILTERS+=("${2//\\//}"); shift 2;;   # 兼容 Windows 风格反斜杠
        --tag)            need_arg "$@"; TAG_FILTERS+=("${2,,}"); shift 2;;
        --skip-build)     SKIP_BUILD=1; shift;;
        --release)        RELEASE=1; shift;;
        --fail-fast)      FAIL_FAST=1; shift;;
        --update-goldens) UPDATE_GOLDENS=1; shift;;
        --strict-crash)   STRICT_CRASH=1; shift;;
        --jobs)           need_arg "$@"; JOBS="$2"; shift 2;;
        --timeout)        need_arg "$@"; TIMEOUT_SECONDS="$2"; shift 2;;
        --keep-logs)      need_arg "$@"; KEEP_LOGS="$2"; shift 2;;
        -h|--help)        usage; exit 0;;
        *)                die_usage "未知参数: $1";;
    esac
done

for _p in "${NAME_FILTERS[@]}"; do
    [[ -n "$_p" ]] || die_usage "❌ --name 的值不能为空"
done
for _p in "${TAG_FILTERS[@]}"; do
    # 空 tag 会变成 *"  "* 而匹配所有无 tag 的用例；含空白/逗号的 tag 永远匹配不到
    [[ -n "$_p" && "$_p" != *[[:space:],]* ]] \
        || die_usage "❌ --tag 的值不能为空，且不能含空白或逗号: '$_p'"
done
unset _p

case "$EXPECT" in
    all|pass|fail) ;;
    *) die_usage "❌ --expect 只接受 all / pass / fail，收到: $EXPECT";;
esac
is_uint "$JOBS"            && (( 10#$JOBS >= 1 )) || die_usage "❌ --jobs 必须是 >= 1 的整数，收到: $JOBS"
is_uint "$TIMEOUT_SECONDS" && (( 10#$TIMEOUT_SECONDS >= 1 )) || die_usage "❌ --timeout 必须是 >= 1 的整数，收到: $TIMEOUT_SECONDS"
is_uint "$KEEP_LOGS"       && (( 10#$KEEP_LOGS >= 1 )) || die_usage "❌ --keep-logs 必须是 >= 1 的整数，收到: $KEEP_LOGS"
JOBS=$((10#$JOBS)); TIMEOUT_SECONDS=$((10#$TIMEOUT_SECONDS)); KEEP_LOGS=$((10#$KEEP_LOGS))

# ============================================================
# 颜色 (CI 或非终端时关闭)
# ============================================================
if [[ -n "${CI:-}" ]] || [[ -n "${NO_COLOR:-}" ]] || [[ ! -t 1 ]]; then
    C_RESET=""; C_RED=""; C_GREEN=""; C_YELLOW=""; C_CYAN=""; C_MAGENTA=""; C_GRAY=""
else
    C_RESET=$'\033[0m'
    C_RED=$'\033[31m'
    C_GREEN=$'\033[32m'
    C_YELLOW=$'\033[33m'
    C_CYAN=$'\033[36m'
    C_MAGENTA=$'\033[35m'
    C_GRAY=$'\033[90m'
fi

fatal() { echo "${C_RED}❌ $*${C_RESET}" >&2; exit 1; }
warn()  { echo "${C_YELLOW}⚠️  $*${C_RESET}" >&2; }

# ============================================================
# 位数自检：只支持 64 位 (操作系统已在文件开头检查过)
#   以 getconf LONG_BIT (用户空间位数) 为准——容器/兼容层里 uname -m 可能撒谎；
#   getconf 缺失时才退回 uname -m 白名单。
# ============================================================
platform_reject() {
    echo "${C_RED}❌ $1${C_RESET}" >&2
    echo "   本测试脚本只支持 64 位 Linux 与 macOS；Windows 请使用 run_all_tests.ps1。" >&2
    echo "   (仅调试脚本本身时可设 XIYI_TESTS_FORCE=1 跳过此检查)" >&2
    exit 1
}

check_platform() {
    local arch bits=""
    arch="$(uname -m 2>/dev/null || echo unknown)"

    # 明确的 32 位架构名：无论 getconf 怎么说都拒绝
    case "$arch" in
        i?86|x86|arm|armv[0-9]*|mips|mipsel|ppc|ppcle|riscv32)
            platform_reject "不支持的 32 位 CPU 架构: $arch";;
    esac

    if command -v getconf >/dev/null 2>&1; then
        bits="$(getconf LONG_BIT 2>/dev/null || true)"
    fi
    case "$bits" in
        64) ;;
        32) platform_reject "getconf LONG_BIT=32：当前是 32 位用户空间 (uname -m 报告 $arch)";;
        *)
            case "$arch" in
                x86_64|amd64|aarch64|arm64|riscv64|ppc64|ppc64le|s390x|mips64|mips64el|loongarch64) ;;
                *) platform_reject "无法确认位数 (getconf 不可用)，且 CPU 架构 $arch 不在 64 位白名单内";;
            esac
            ;;
    esac
}

if [[ "${XIYI_TESTS_FORCE:-0}" == "1" ]]; then
    warn "XIYI_TESTS_FORCE=1，已跳过平台自检（仅供调试脚本本身）"
else
    check_platform
fi

# ============================================================
# 临时目录 + 信号处理
#   - INT/TERM 必须真正退出 (原版的 trap 只清理不退出，Ctrl-C 后脚本会继续跑)
#   - 退出时终止仍在运行的后台任务，避免遗留 xiyi / timeout 孤儿进程
# ============================================================
RESULTS_DIR=""
LOG_FILE=""
INTERRUPTED=""
CUR_TPID=0                       # 当前正在跑的 timeout 包装进程 pid (串行模式)
declare -a FILTERED=() EXECUTED=()   # 提前声明：信号可能在任何时刻到达，-u 下不能引用未定义变量

# 终止一个用例的进程树。timeout 自成进程组 (pgid == 其 pid)，所以按负 pid 杀整组。
# 守卫 pid > 1：kill 0 / kill -1 / kill -- -0 会波及调用者所在的整个进程组甚至全部进程。
kill_case_tree() {
    local pid="${1:-0}"
    [[ "$pid" =~ ^[0-9]+$ ]] && (( pid > 1 )) || return 0
    kill -TERM -- "-$pid" 2>/dev/null || true
    kill -TERM "$pid" 2>/dev/null || true
    return 0
}

cleanup() {
    local rc=$?
    trap - EXIT INT TERM
    if [[ -n "$INTERRUPTED" && -n "$LOG_FILE" && -f "$LOG_FILE" ]]; then
        printf '运行被 %s 中断: 已派发 %d/%d 个用例；未生成汇总与 JSON\n' \
            "$INTERRUPTED" "${#EXECUTED[@]}" "${#FILTERED[@]}" >> "$LOG_FILE" 2>/dev/null || true
    fi
    kill_case_tree "$CUR_TPID"
    local pids
    pids="$(jobs -p 2>/dev/null || true)"
    if [[ -n "$pids" ]]; then
        # shellcheck disable=SC2086
        kill -TERM $pids 2>/dev/null || true
        wait 2>/dev/null || true
    fi
    if [[ -n "$RESULTS_DIR" && -d "$RESULTS_DIR" ]]; then
        rm -rf -- "$RESULTS_DIR"
    fi
    exit "$rc"
}
trap cleanup EXIT
trap 'INTERRUPTED=INT;  exit 130' INT
trap 'INTERRUPTED=TERM; exit 143' TERM

RESULTS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/xiyi-test.XXXXXX")" || fatal "无法创建临时目录"

# ============================================================
# 外部命令探测
# ============================================================
if command -v timeout >/dev/null 2>&1; then
    TIMEOUT_BIN="timeout"
elif command -v gtimeout >/dev/null 2>&1; then
    TIMEOUT_BIN="gtimeout"
else
    fatal "找不到 timeout (macOS 请 brew install coreutils 以获得 gtimeout)"
fi

# 毫秒时间戳：bash >= 5.0 内置 EPOCHREALTIME (形如 1700000000.123456；个别 locale 用逗号)
[[ -n "${EPOCHREALTIME:-}" ]] || fatal "EPOCHREALTIME 不可用 (bash 内置变量，是否被 unset 了？)"
now_ms() { local t="${EPOCHREALTIME/[.,]/}"; printf '%d\n' $(( 10#$t / 1000 )); }

# ============================================================
# 路径发现
# ============================================================
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)" || fatal "无法确定脚本目录"

find_workspace_root() {
    local dir="$1" parent
    while [[ -n "$dir" ]]; do
        if [[ -d "$dir/Tests" && -d "$dir/Standard" ]]; then
            printf '%s\n' "$dir"
            return 0
        fi
        parent="$(dirname -- "$dir")"
        [[ "$parent" == "$dir" ]] && break
        dir="$parent"
    done
    return 1
}

COMPILER_ROOT="$SCRIPT_DIR"
WORKSPACE_ROOT="$(find_workspace_root "$COMPILER_ROOT")" \
    || fatal "从 $COMPILER_ROOT 向上找不到同时包含 Tests/ 与 Standard/ 的工作区根目录"

STDLIB="$WORKSPACE_ROOT/Standard"
TEST_DIR="$WORKSPACE_ROOT/Tests"
TEST_LIST_JSON="$TEST_DIR/list.json"

# 固定工作目录，避免用例里的相对路径随调用位置变化
cd -- "$COMPILER_ROOT" || fatal "无法进入 $COMPILER_ROOT"

# ============================================================
# 日志目录 + 轮转 (先创建本次日志，再保留最近 KEEP_LOGS 个，含本次)
# ============================================================
LOG_DIR="$COMPILER_ROOT/test_results"
mkdir -p -- "$LOG_DIR" || fatal "无法创建日志目录 $LOG_DIR"

RUN_ID="$(date +%Y%m%d_%H%M%S)"
# 同一秒内重复启动 (CI 并发) 时避免日志互相覆盖
if [[ -e "$LOG_DIR/$RUN_ID.log" || -e "$LOG_DIR/$RUN_ID.json" ]]; then
    RUN_ID="${RUN_ID}_$$"
fi
LOG_FILE="$LOG_DIR/$RUN_ID.log"
JSON_FILE="$LOG_DIR/$RUN_ID.json"
: > "$LOG_FILE" || fatal "无法写入日志 $LOG_FILE"

# a 是否比 b 旧 (mtime 相同则按文件名，保证排序稳定)。不依赖 GNU find -printf /
# sort -z / ls 解析，所以在 macOS 与含换行的文件名下都成立。
older_than() {
    [[ "$1" -ot "$2" ]] && return 0
    [[ "$1" -nt "$2" ]] && return 1
    [[ "$1" < "$2" ]]
}

rotate_logs() {
    local ext="$1" keep="$2" i j n tmp
    local -a files=()
    shopt -s nullglob
    files=("$LOG_DIR"/*."$ext")
    shopt -u nullglob
    n=${#files[@]}
    (( n > keep )) || return 0

    # 插入排序：新 → 旧
    for (( i = 1; i < n; i++ )); do
        tmp="${files[i]}"
        j=$(( i - 1 ))
        while (( j >= 0 )) && older_than "${files[j]}" "$tmp"; do
            files[j+1]="${files[j]}"
            j=$(( j - 1 ))
        done
        files[j+1]="$tmp"
    done

    for (( i = keep; i < n; i++ )); do
        [[ "${files[i]}" == "$LOG_FILE" || "${files[i]}" == "$JSON_FILE" ]] && continue
        rm -f -- "${files[i]}"
    done
}
rotate_logs log  "$KEEP_LOGS"            # 本次 .log 已创建，含在 KEEP_LOGS 之内
rotate_logs json "$(( KEEP_LOGS - 1 ))"  # 本次 .json 在最后才写，先给它留一个位置

# ============================================================
# 版本号：只读 [package] 段，避免误取依赖的 version = "..."
# ============================================================
get_xiyi_version() {
    local cargo_toml="$COMPILER_ROOT/Cargo.toml" v=""
    if [[ -f "$cargo_toml" ]]; then
        v="$(awk '
            /^[[:space:]]*\[/ { in_pkg = ($0 ~ /^[[:space:]]*\[package\][[:space:]]*(#.*)?\r?$/); next }
            in_pkg && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"/ {
                s = $0; sub(/^[^"]*"/, "", s); sub(/".*$/, "", s); print s; exit
            }
        ' "$cargo_toml" 2>/dev/null | tr -d '\r')"
    fi
    printf '%s\n' "${v:-unknown}"
}
XIYI_VERSION="$(get_xiyi_version)"

# ============================================================
# 可执行文件：尊重 CARGO_TARGET_DIR / workspace target 目录
# ============================================================
resolve_target_dir() {
    local d=""
    if command -v cargo >/dev/null 2>&1 && command -v jq >/dev/null 2>&1; then
        d="$(cd -- "$COMPILER_ROOT" && cargo metadata --no-deps --format-version 1 --offline 2>/dev/null \
             | jq -r '.target_directory // empty' 2>/dev/null)" || d=""
    fi
    if [[ -z "$d" ]]; then
        if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
            d="$CARGO_TARGET_DIR"
            [[ "$d" == /* ]] || d="$COMPILER_ROOT/$d"
        else
            d="$COMPILER_ROOT/target"
        fi
    fi
    printf '%s\n' "$d"
}

find_xiyi_exe() {
    local profile="$1" f
    for f in "$TARGET_DIR/$profile/xiyi" "$TARGET_DIR/$profile/xiyi.exe"; do
        if [[ -f "$f" && -x "$f" ]]; then
            printf '%s\n' "$f"
            return 0
        fi
    done
    return 1
}

# ============================================================
# 头部
# ============================================================
echo "${C_CYAN}========================================${C_RESET}"
echo "${C_CYAN} 希夷编译器测试套件${C_RESET}"
echo "${C_CYAN} xiyi-compiler 版本: $XIYI_VERSION${C_RESET}"
echo "${C_CYAN} Expect 过滤: $EXPECT   并发: $JOBS   超时(默认): ${TIMEOUT_SECONDS}s${C_RESET}"
echo "${C_GRAY} 编译器目录: $COMPILER_ROOT${C_RESET}"
echo "${C_GRAY} 标准库目录: $STDLIB${C_RESET}"
echo "${C_GRAY} 测试目录:   $TEST_DIR${C_RESET}"
echo "${C_GRAY} 日志:       $LOG_FILE${C_RESET}"
echo "${C_CYAN}========================================${C_RESET}"
echo

{
    echo "运行 ID: $RUN_ID"
    echo "版本: $XIYI_VERSION"
    echo "参数: Expect=$EXPECT Jobs=$JOBS SkipBuild=$SKIP_BUILD Release=$RELEASE UpdateGoldens=$UPDATE_GOLDENS FailFast=$FAIL_FAST StrictCrash=$STRICT_CRASH"
} >> "$LOG_FILE"

# ============================================================
# 步骤1: 编译
# ============================================================
if (( SKIP_BUILD == 0 )); then
    echo "${C_YELLOW}[1/4] 编译 xiyi-compiler...${C_RESET}"
    command -v cargo >/dev/null 2>&1 || fatal "找不到 cargo (可用 --skip-build 跳过编译)"

    build_args=(build -q)
    (( RELEASE == 1 )) && build_args+=(--release)

    build_output="$(cd -- "$COMPILER_ROOT" && cargo "${build_args[@]}" 2>&1)"
    build_rc=$?

    printf '%s\n' "$build_output" >> "$LOG_FILE"

    if (( build_rc != 0 )); then
        printf '%s\n' "$build_output"
        fatal "编译失败，停止测试"
    fi
    echo "${C_GREEN}✅ 编译成功${C_RESET}"
else
    echo "${C_YELLOW}[1/4] 跳过编译（--skip-build）${C_RESET}"
fi
echo

# ============================================================
# 步骤2: 定位可执行文件
# ============================================================
profile_name="debug"
(( RELEASE == 1 )) && profile_name="release"

# cargo metadata 在大 workspace 上要几百毫秒，只算一次 (失败路径也不再重复调用)
TARGET_DIR="$(resolve_target_dir)"
XIYI_EXE="$(find_xiyi_exe "$profile_name")" \
    || fatal "在 $TARGET_DIR/$profile_name 下找不到可执行的 xiyi 文件（xiyi 或 xiyi.exe）"
[[ -d "$STDLIB" ]] || fatal "找不到标准库目录 $STDLIB"

echo "${C_GRAY}使用可执行文件: $XIYI_EXE${C_RESET}"
echo

# ============================================================
# 步骤3: 扫描测试
# ============================================================
echo "${C_YELLOW}[2/4] 扫描测试用例（$TEST_DIR）...${C_RESET}"

declare -a ALL_TESTS=()
while IFS= read -r -d '' f; do
    ALL_TESTS+=("$f")
done < <(find "$TEST_DIR" -type f -name '*.xiyi' -print0 | LC_ALL=C sort -z)

if (( ${#ALL_TESTS[@]} == 0 )); then
    fatal "$TEST_DIR 下找不到任何 .xiyi 文件"
fi

# 把 "a, b  c" 规范化为 "a b c"
norm_tags() {
    local raw="${1//,/ }"
    local -a a=()
    read -r -a a <<< "$raw"
    printf '%s' "${a[*]:-}"
}

# ---- 解析 list.json ----
BASE_EXPECT="PASS"
BASE_TIMEOUT="$TIMEOUT_SECONDS"

declare -A OV_SEEN=()
declare -A OV_EXPECT=()
declare -A OV_TIMEOUT=()
declare -A OV_STDERR=()
declare -A OV_TAG=()
declare -A OV_SKIP=()
declare -A OV_REASON=()

load_list_json() {
    local clean="$RESULTS_DIR/list.json" err

    command -v jq >/dev/null 2>&1 \
        || fatal "$TEST_LIST_JSON 存在但未找到 jq，无法解析；请安装 jq，或临时移除该文件"

    # 去掉 UTF-8 BOM (Windows 编辑器常带)
    LC_ALL=C sed $'1s/^\xEF\xBB\xBF//' "$TEST_LIST_JSON" > "$clean" \
        || fatal "无法读取 $TEST_LIST_JSON"

    # 解析失败必须报错退出 (与 ps1 一致)；不能静默丢掉全部覆盖项
    if ! err="$(jq empty "$clean" 2>&1)"; then
        fatal "解析 $TEST_LIST_JSON 失败: ${err%%$'\n'*}"
    fi
    jq -e 'type == "object"' "$clean" >/dev/null 2>&1 \
        || fatal "$TEST_LIST_JSON 顶层必须是 JSON 对象"

    local v d_expect d_timeout
    v="$(jq -r '.version? // empty' "$clean")"
    if [[ -n "$v" && "$v" != "1" ]]; then
        warn "list.json 声明的 version=$v，本脚本只认识 version 1，按 1 处理"
    fi

    d_expect="$(jq -r '.defaults.expect? // empty' "$clean")"
    d_expect="${d_expect^^}"
    if [[ -n "$d_expect" ]]; then
        if [[ "$d_expect" == "FAIL" || "$d_expect" == "EXPECT_FAIL" ]]; then
            BASE_EXPECT="FAIL"
        else
            BASE_EXPECT="PASS"
        fi
    fi

    d_timeout="$(jq -r '.defaults.timeout? // empty' "$clean")"
    if [[ -n "$d_timeout" ]]; then
        if is_uint "$d_timeout" && (( 10#$d_timeout >= 1 )); then
            BASE_TIMEOUT=$((10#$d_timeout))
        else
            warn "list.json defaults.timeout=\"$d_timeout\" 不是 >= 1 的整数，已忽略"
        fi
    fi

    # 分隔符用 0x1f (非空白 IFS 字符)。
    # 不能用 tab：IFS 里的空白字符会被 read 折叠，空字段会导致后面的列整体错位。
    local rows
    rows="$(jq -r '
        .tests[]? | select(type == "object") | select((.file // "") != "") |
        [
            (.file | tostring),
            ((.expect // "") | tostring),
            ((.timeout // "") | tostring),
            ((.stderrContains // "") | tostring),
            ((.tag // []) | if type == "array" then join(",") else tostring end),
            ((.skip // false) | tostring),
            ((.reason // "") | tostring)
        ] | map(tostring | gsub("[\\x01-\\x1f]"; " ")) | join("\u001f")
    ' "$clean")" || fatal "读取 $TEST_LIST_JSON 的 tests 失败"

    local f_file f_expect f_timeout f_stderr f_tag f_skip f_reason key
    if [[ -n "$rows" ]]; then
        while IFS=$'\x1f' read -r f_file f_expect f_timeout f_stderr f_tag f_skip f_reason; do
            [[ -z "$f_file" ]] && continue
            key="${f_file//\\//}"
            key="${key#./}"
            if [[ -n "$f_timeout" ]] && ! { is_uint "$f_timeout" && (( 10#$f_timeout >= 1 )); }; then
                warn "list.json: $key 的 timeout=\"$f_timeout\" 不是 >= 1 的整数，已忽略"
                f_timeout=""
            fi
            OV_SEEN["$key"]=1
            OV_EXPECT["$key"]="$f_expect"
            OV_TIMEOUT["$key"]="$f_timeout"
            OV_STDERR["$key"]="$f_stderr"
            OV_TAG["$key"]="$f_tag"
            OV_SKIP["$key"]="$f_skip"
            OV_REASON["$key"]="$f_reason"
        done <<< "$rows"
    fi
}

if [[ -f "$TEST_LIST_JSON" ]]; then
    load_list_json
fi

# ---- 源码内联元数据 (结果写入全局 META_*) ----
parse_inline_meta() {
    local file="$1" default_expect="$2" default_timeout="$3"

    META_EXPECT="$default_expect"
    META_TIMEOUT="$default_timeout"
    META_STDERR=""
    META_TAGS=""
    META_SKIP="0"
    META_REASON=""

    local -a lines=()
    mapfile -t -n 40 lines < "$file" 2>/dev/null || true

    local i line
    for i in "${!lines[@]}"; do
        line="${lines[i]%$'\r'}"                       # CRLF 源文件
        (( i == 0 )) && line="${line#$'\xEF\xBB\xBF'}" # UTF-8 BOM

        if [[ "$line" =~ ^[[:space:]]*//[[:space:]]*@expect[[:space:]]+fail[[:space:]]*$ ]]; then
            META_EXPECT="FAIL"
        elif [[ "$line" =~ ^[[:space:]]*//[[:space:]]*@timeout[[:space:]]+([0-9]{1,9})[[:space:]]*$ ]]; then
            if (( 10#${BASH_REMATCH[1]} >= 1 )); then
                META_TIMEOUT=$((10#${BASH_REMATCH[1]}))
            fi
        elif [[ "$line" =~ ^[[:space:]]*//[[:space:]]*@stderr-contains[[:space:]]+\"([^\"]*)\"[[:space:]]*$ ]]; then
            META_STDERR="${BASH_REMATCH[1]}"
        elif [[ "$line" =~ ^[[:space:]]*//[[:space:]]*@tag[[:space:]]+(.+)$ ]]; then
            META_TAGS="$(norm_tags "${BASH_REMATCH[1]}")"
        elif [[ "$line" =~ ^[[:space:]]*//[[:space:]]*@skip([[:space:]]+(.*))?$ ]]; then
            META_SKIP="1"
            META_REASON="${BASH_REMATCH[2]:-}"
        fi
    done
}

# ---- 组装测试用例 ----
declare -a T_REL=() T_FULL=() T_EXPECT=() T_TIMEOUT=() T_STDERR=() T_TAGS=() T_SKIP=() T_REASON=()
declare -A REL_SET=()

test_dir_norm="${TEST_DIR%/}"

for f in "${ALL_TESTS[@]}"; do
    rel="${f#"$test_dir_norm"/}"     # 注意引号：路径里含 [ ] * ? 时不能被当成 glob
    REL_SET["$rel"]=1

    # 优先级(低→高): list.json defaults -> 源码内联 -> list.json 针对该文件的例外
    parse_inline_meta "$f" "$BASE_EXPECT" "$BASE_TIMEOUT"

    if [[ -n "${OV_SEEN["$rel"]+x}" ]]; then
        ov="${OV_EXPECT["$rel"]^^}"
        if [[ -n "$ov" ]]; then
            if [[ "$ov" == "FAIL" || "$ov" == "EXPECT_FAIL" ]]; then
                META_EXPECT="FAIL"
            else
                META_EXPECT="PASS"
            fi
        fi
        [[ -n "${OV_TIMEOUT["$rel"]}" ]] && META_TIMEOUT="${OV_TIMEOUT["$rel"]}"
        [[ -n "${OV_STDERR["$rel"]}"  ]] && META_STDERR="${OV_STDERR["$rel"]}"
        [[ -n "${OV_TAG["$rel"]}"     ]] && META_TAGS="$(norm_tags "${OV_TAG["$rel"]}")"
        if [[ "${OV_SKIP["$rel"]}" == "true" ]]; then
            META_SKIP="1"
            META_REASON="${OV_REASON["$rel"]}"
        fi
    fi

    T_REL+=("$rel")
    T_FULL+=("$f")
    T_EXPECT+=("$META_EXPECT")
    T_TIMEOUT+=("$META_TIMEOUT")
    T_STDERR+=("$META_STDERR")
    T_TAGS+=("$META_TAGS")
    T_SKIP+=("$META_SKIP")
    T_REASON+=("$META_REASON")
done

# list.json 里写了、磁盘上却不存在的条目：几乎必然是拼写/移动文件后忘了改，
# 否则覆盖项会被静默丢弃。
for key in "${!OV_SEEN[@]}"; do
    [[ -n "${REL_SET["$key"]+x}" ]] || warn "list.json 中的条目在 Tests/ 下找不到对应文件: $key"
done

# ---- 过滤 ----
for i in "${!T_REL[@]}"; do
    if [[ "$EXPECT" != "all" ]]; then
        want="PASS"
        [[ "$EXPECT" == "fail" ]] && want="FAIL"
        [[ "${T_EXPECT[$i]}" != "$want" ]] && continue
    fi

    if (( ${#NAME_FILTERS[@]} > 0 )); then
        matched=0
        for pat in "${NAME_FILTERS[@]}"; do
            # pat 故意不加引号，让 glob 生效
            # shellcheck disable=SC2053
            if [[ "${T_REL[$i]}" == $pat ]]; then
                matched=1
                break
            fi
        done
        (( matched == 0 )) && continue
    fi

    if (( ${#TAG_FILTERS[@]} > 0 )); then
        matched=0
        tags_sp=" ${T_TAGS[$i],,} "
        for want in "${TAG_FILTERS[@]}"; do
            if [[ "$tags_sp" == *" $want "* ]]; then
                matched=1
                break
            fi
        done
        (( matched == 0 )) && continue
    fi

    FILTERED+=("$i")
done

if (( ${#FILTERED[@]} == 0 )); then
    fatal "过滤后没有匹配的测试用例"
fi

echo "共 ${#FILTERED[@]} 个测试用例（目录下总计发现 ${#T_REL[@]} 个）"
echo

# ============================================================
# 规范化 (用于 golden 比对)
#   - 去 NUL (避免命令替换告警)、CRLF/孤立 CR → LF (与 ps1 一致)
#   - 去开头 BOM (ps1 5.1 的 -Encoding utf8 写出的 golden 带 BOM)
#   - 去全部尾部空白，再补一个换行
# ============================================================
normalize_file() {
    local s
    s="$(LC_ALL=C tr -d '\000' < "$1")"
    s="${s#$'\xEF\xBB\xBF'}"
    s="${s//$'\r\n'/$'\n'}"
    s="${s//$'\r'/$'\n'}"
    s="${s%"${s##*[![:space:]]}"}"
    printf '%s\n' "$s"
}

same_normalized() {
    [[ "$(normalize_file "$1")" == "$(normalize_file "$2")" ]]
}

# ============================================================
# 单用例执行（串行/并行共用；结果落到 $RESULTS_DIR/<idx>/）
# ============================================================
execute_test() {
    local idx="$1"
    local rel="${T_REL[$idx]}"
    local full="${T_FULL[$idx]}"
    local expect="${T_EXPECT[$idx]}"
    local timeout_s="${T_TIMEOUT[$idx]}"
    local stderr_contains="${T_STDERR[$idx]}"
    local skip="${T_SKIP[$idx]}"
    local reason="${T_REASON[$idx]}"

    local rdir="$RESULTS_DIR/$idx"
    mkdir -p -- "$rdir"

    if [[ "$skip" == "1" ]]; then
        : > "$rdir/stdout"
        : > "$rdir/stderr"
        printf '0.000\n' > "$rdir/elapsed"
        printf '\n'      > "$rdir/exit_code"
        if [[ -n "$reason" ]]; then
            printf 'skip: %s\n' "$reason" > "$rdir/reasons"
        else
            printf 'skip\n' > "$rdir/reasons"
        fi
        printf 'SKIP\n' > "$rdir/status"      # status 最后写：它是"结果已完整"的标志
        return 0
    fi

    local out_file="$rdir/stdout"
    local err_file="$rdir/stderr"

    local start_ms end_ms elapsed_ms rc=0
    start_ms="$(now_ms)"

    # 放到后台再 wait：这样 INT/TERM trap 能立即响应并转发信号。
    # </dev/null：避免用例读 stdin 时挂住终端/CI 管道。
    "$TIMEOUT_BIN" -s TERM -k 5 "$timeout_s" \
        "$XIYI_EXE" --stdlib "$STDLIB" "$full" \
        </dev/null >"$out_file" 2>"$err_file" &
    CUR_TPID=$!
    # 外层花括号 + 2>/dev/null：被测进程被信号杀死时，bash 会在 wait 处打印
    # "Segmentation fault" 之类的作业通知，属于噪音 (信息已体现在 rc 里)。
    { wait "$CUR_TPID"; } 2>/dev/null
    rc=$?
    CUR_TPID=0

    end_ms="$(now_ms)"
    elapsed_ms=$(( end_ms - start_ms ))
    (( elapsed_ms < 0 )) && elapsed_ms=0
    printf '%d.%03d\n' $(( elapsed_ms / 1000 )) $(( elapsed_ms % 1000 )) > "$rdir/elapsed"

    # timeout 的 124 / 137 只有在"确实跑满了时限"时才算超时。
    # 否则 137 可能是 OOM killer 的 SIGKILL，124 可能是被测程序自己的退出码。
    if { (( rc == 124 )) || (( rc == 137 )); } && (( elapsed_ms >= timeout_s * 1000 )); then
        printf '\n' > "$rdir/exit_code"
        printf 'timeout %ss\n' "$timeout_s" > "$rdir/reasons"
        printf 'FAIL\n' > "$rdir/status"
        return 0
    fi

    printf '%d\n' "$rc" > "$rdir/exit_code"

    if [[ "$UPDATE_GOLDENS" == "1" ]]; then
        if cp -- "$out_file" "$full.out" && cp -- "$err_file" "$full.err"; then
            printf 'golden 已更新，请 git diff 审查\n' > "$rdir/reasons"
            printf 'UPDATED\n' > "$rdir/status"
        else
            printf '无法写入 golden: %s.{out,err}\n' "$full" > "$rdir/reasons"
            printf 'FAIL\n' > "$rdir/status"
        fi
        return 0
    fi

    local ok=1
    local -a reasons=()

    # --- 退出码 ---
    local expected_exit="" have_exit_golden=0
    if [[ -f "$full.exit" ]]; then
        have_exit_golden=1
        expected_exit="$(tr -d '[:space:]' < "$full.exit")"
        expected_exit="${expected_exit#$'\xEF\xBB\xBF'}"
        # 只接受 0-255，与 ps1 版一致：共享的 golden 在两个平台含义必须相同。
        # (|| 短路：非数字时不会走到算术比较)
        if ! is_uint "$expected_exit" || (( 10#$expected_exit > 255 )); then
            ok=0
            reasons+=("无效的 .exit golden: \"$expected_exit\" (只接受 0-255，这样两个平台才能共享)")
            expected_exit=""
        fi
    fi

    if (( have_exit_golden == 1 )) && [[ -z "$expected_exit" ]]; then
        # .exit golden 存在但内容无效：上面已记录原因并置 ok=0。
        # 这里显式处理，不再回退到 "按 expect 推导"，避免结构上留下跳过退出码检查的漏洞。
        ok=0
    elif [[ -n "$expected_exit" ]]; then
        if (( rc != 10#$expected_exit )); then
            ok=0
            reasons+=("exit $rc, expected $((10#$expected_exit))")
        fi
    elif [[ "$expect" == "FAIL" ]]; then
        if (( rc == 0 )); then
            ok=0
            reasons+=("expected failure but exited 0")
        fi
    else
        if (( rc != 0 )); then
            ok=0
            reasons+=("exit $rc, expected 0")
        fi
    fi

    # --- 崩溃识别：信号终止 / Rust panic / ICE ---
    # "期望失败"的用例很容易被编译器崩溃 (退出码非 0) 悄悄蒙混过关。
    local crashed=0 crash_what=""
    if (( rc > 128 && rc < 192 )); then
        crashed=1; crash_what="被信号 $(( rc - 128 )) 终止"
    elif grep -qE "panicked at|internal compiler error" "$err_file" 2>/dev/null; then
        crashed=1; crash_what="stderr 含 panic/ICE 标志"
    fi

    # --- golden 比对（有才比）---
    if [[ -f "$full.out" ]] && ! same_normalized "$full.out" "$out_file"; then
        ok=0
        reasons+=("stdout mismatch")
    fi
    if [[ -f "$full.err" ]] && ! same_normalized "$full.err" "$err_file"; then
        ok=0
        reasons+=("stderr mismatch")
    fi

    # --- stderr 子串 ---
    if [[ -n "$stderr_contains" ]]; then
        if ! grep -qF -- "$stderr_contains" "$err_file"; then
            ok=0
            reasons+=("stderr missing: $stderr_contains")
        fi
    fi

    # --- 崩溃处置 / 提示 ---
    if (( crashed == 1 )) && (( have_exit_golden == 0 )) && [[ "$expect" == "FAIL" ]]; then
        if (( STRICT_CRASH == 1 )); then
            ok=0
            reasons+=("compiler crash: $crash_what")
        elif (( ok == 1 )); then
            reasons+=("(提示: $crash_what，通过可能源于编译器崩溃而非预期诊断；加 --strict-crash 视为失败)")
        fi
    fi

    if [[ "$expect" == "FAIL" && -z "$stderr_contains" && ! -f "$full.err" ]]; then
        reasons+=("(提示: EXPECT_FAIL 未绑定 stderr 校验，通过不代表失败原因正确)")
    fi

    if (( ${#reasons[@]} > 0 )); then
        printf '%s\n' "${reasons[@]}" > "$rdir/reasons"
    else
        : > "$rdir/reasons"
    fi

    if (( ok == 1 )); then
        printf 'PASS\n' > "$rdir/status"
    else
        printf 'FAIL\n' > "$rdir/status"
    fi
}

# ============================================================
# 结果打印
# ============================================================
print_result() {
    local idx="$1"
    local rdir="$RESULTS_DIR/$idx"
    local rel="${T_REL[$idx]}"

    # 子任务异常退出(被 OOM 杀、被信号终止…)而没留下 status：必须算失败，
    # 绝不能被汇总悄悄漏掉 (否则可能 "全部通过" 退出 0)。
    if [[ ! -f "$rdir/status" ]]; then
        mkdir -p -- "$rdir"
        printf 'no result recorded (worker died?)\n' > "$rdir/reasons"
        printf '0.000\n' > "$rdir/elapsed"
        printf '\n' > "$rdir/exit_code"
        printf 'FAIL\n' > "$rdir/status"
    fi

    local status elapsed
    status="$(<"$rdir/status")"
    elapsed="$(<"$rdir/elapsed")"; elapsed="${elapsed:-0.000}"

    local reasons="" r
    local -a rs=()
    if [[ -s "$rdir/reasons" ]]; then
        mapfile -t rs < "$rdir/reasons"
        for r in "${rs[@]}"; do
            reasons+="${reasons:+; }$r"
        done
    fi

    local color=""
    case "$status" in
        SKIP)    color="$C_GRAY";;
        UPDATED) color="$C_MAGENTA";;
        PASS)    color="$C_GREEN";;
        FAIL)    color="$C_RED";;
    esac

    local suffix=""
    [[ -n "$reasons" ]] && suffix=" - $reasons"

    printf '%s[%s] %s (%ss)%s%s\n' \
        "$color" "$status" "$rel" "$elapsed" "$suffix" "$C_RESET"

    if [[ "$status" == "FAIL" ]]; then
        {
            printf '\n---- %s ----\n' "$rel"
            printf 'reasons: %s\n' "$reasons"
            printf 'stdout:\n'
            LC_ALL=C head -c 4096 "$rdir/stdout" 2>/dev/null || true
            [[ "$(LC_ALL=C wc -c < "$rdir/stdout" 2>/dev/null || echo 0)" -gt 4096 ]] && printf '\n...(截断)'
            printf '\nstderr:\n'
            LC_ALL=C head -c 4096 "$rdir/stderr" 2>/dev/null || true
            [[ "$(LC_ALL=C wc -c < "$rdir/stderr" 2>/dev/null || echo 0)" -gt 4096 ]] && printf '\n...(截断)'
            printf '\n'
        } >> "$LOG_FILE"
    fi
}

# ============================================================
# 步骤4: 执行
# ============================================================
echo "${C_YELLOW}[3/4] 执行测试...${C_RESET}"
echo

if (( JOBS > 1 )); then
    (( FAIL_FAST == 1 )) && warn "--fail-fast 在并行模式下不生效"
    for i in "${FILTERED[@]}"; do
        while (( $(jobs -rp | wc -l) >= JOBS )); do
            wait -n 2>/dev/null || true
        done
        EXECUTED+=("$i")           # 派发时登记：中断时日志里能看到"派了几个"
        (
            # 子 shell 里 trap 会被重置：收到 TERM/INT 时把信号转给 timeout 的进程组再退出。
            # CUR_TPID 仍为 0 (信号恰好在 timeout 启动前到达) 时必须跳过——
            # kill -TERM 0 会给整个进程组 (含父脚本) 发信号。kill_case_tree 内部已做守卫。
            trap 'kill_case_tree "$CUR_TPID"; exit 143' TERM INT
            execute_test "$i"
        ) &
    done
    wait
    for i in "${EXECUTED[@]}"; do
        print_result "$i"
    done
else
    for i in "${FILTERED[@]}"; do
        EXECUTED+=("$i")
        execute_test "$i"
        print_result "$i"
        if (( FAIL_FAST == 1 )) && [[ "$(<"$RESULTS_DIR/$i/status")" == "FAIL" ]]; then
            echo "${C_YELLOW}⏹  --fail-fast 触发，停止后续测试${C_RESET}"
            break
        fi
    done
fi
echo

# ============================================================
# 步骤5: 汇总
# ============================================================
echo "${C_YELLOW}[4/4] 汇总结果...${C_RESET}"

total=0; passed=0; failed=0; timeouts=0; skipped=0; updated=0

for i in "${EXECUTED[@]}"; do
    status="$(<"$RESULTS_DIR/$i/status")"
    total=$(( total + 1 ))
    case "$status" in
        PASS)    passed=$(( passed + 1 ));;
        FAIL)
            failed=$(( failed + 1 ))
            if grep -q '^timeout ' "$RESULTS_DIR/$i/reasons" 2>/dev/null; then
                timeouts=$(( timeouts + 1 ))
            fi
            ;;
        SKIP)    skipped=$(( skipped + 1 ));;
        UPDATED) updated=$(( updated + 1 ));;
    esac
done

echo "${C_CYAN}========================================${C_RESET}"
echo "${C_CYAN} 测试结果汇总${C_RESET}"
echo "${C_CYAN}========================================${C_RESET}"
printf ' 总计:   %d\n' "$total"
printf ' %s通过:   %d%s\n' "$C_GREEN" "$passed" "$C_RESET"
printf ' %s失败:   %d%s\n' "$C_RED" "$failed" "$C_RESET"
printf ' %s超时:   %d%s\n' "$C_YELLOW" "$timeouts" "$C_RESET"
printf ' %s跳过:   %d%s\n' "$C_GRAY" "$skipped" "$C_RESET"
printf ' %s已更新 golden: %d%s\n' "$C_MAGENTA" "$updated" "$C_RESET"
echo "${C_CYAN}========================================${C_RESET}"
echo " 日志:  $LOG_FILE"
echo " 结果:  $JSON_FILE"
echo "${C_CYAN}========================================${C_RESET}"

# ============================================================
# 写 JSON 结果 (先写临时文件再 mv，避免中途被中断留下半截文件)
# ============================================================
json_escape() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    s="${s//$'\n'/\\n}"
    s="${s//$'\r'/\\r}"
    s="${s//$'\t'/\\t}"
    s="${s//$'\b'/\\b}"
    s="${s//$'\f'/\\f}"
    s="${s//[[:cntrl:]]/}"      # 其余控制字符 (如 ANSI ESC) 在 JSON 里非法，直接丢弃
    printf '%s' "$s"
}

json_tmp="$JSON_FILE.tmp.$$"
{
    printf '{\n'
    printf '  "RunId": "%s",\n'   "$(json_escape "$RUN_ID")"
    printf '  "Version": "%s",\n' "$(json_escape "$XIYI_VERSION")"
    printf '  "Total": %d,\n'     "$total"
    printf '  "Passed": %d,\n'    "$passed"
    printf '  "Failed": %d,\n'    "$failed"
    printf '  "Timeouts": %d,\n'  "$timeouts"
    printf '  "Skipped": %d,\n'   "$skipped"
    printf '  "Updated": %d,\n'   "$updated"
    printf '  "Results": ['

    first=1
    for i in "${EXECUTED[@]}"; do
        rdir="$RESULTS_DIR/$i"
        status="$(<"$rdir/status")"
        elapsed="$(<"$rdir/elapsed")"
        ec="$(tr -d '[:space:]' < "$rdir/exit_code" 2>/dev/null || true)"

        (( first == 0 )) && printf ','
        first=0

        ok="false"; sk="false"; up="false"
        [[ "$status" == "PASS" || "$status" == "SKIP" || "$status" == "UPDATED" ]] && ok="true"
        [[ "$status" == "SKIP" ]] && sk="true"
        [[ "$status" == "UPDATED" ]] && up="true"

        printf '\n    {'
        printf '"RelPath":"%s",' "$(json_escape "${T_REL[$i]}")"
        printf '"Status":"%s",'  "$status"
        printf '"Ok":%s,"Skipped":%s,"Updated":%s,' "$ok" "$sk" "$up"    # 与 ps1 的字段保持兼容
        printf '"Elapsed":%s,'   "$elapsed"
        if is_uint "$ec"; then
            printf '"ExitCode":%d,' "$((10#$ec))"
        else
            printf '"ExitCode":null,'
        fi
        printf '"Reasons":['
        fr=1
        if [[ -s "$rdir/reasons" ]]; then
            while IFS= read -r r || [[ -n "$r" ]]; do
                [[ -z "$r" ]] && continue
                (( fr == 0 )) && printf ','
                fr=0
                printf '"%s"' "$(json_escape "$r")"
            done < "$rdir/reasons"
        fi
        printf ']}'
    done
    printf '\n  ]\n'
    printf '}\n'
} > "$json_tmp" && mv -f -- "$json_tmp" "$JSON_FILE" || warn "写入 $JSON_FILE 失败"

printf '汇总: 总计 %d, 通过 %d, 失败 %d, 超时 %d, 跳过 %d, 已更新 %d\n' \
    "$total" "$passed" "$failed" "$timeouts" "$skipped" "$updated" >> "$LOG_FILE"

# ============================================================
# 退出码
# ============================================================
if (( failed > 0 )); then
    echo "${C_RED}❌ 有 $failed 个测试失败${C_RESET}"
    exit 1
fi

if (( UPDATE_GOLDENS == 1 )); then
    echo "${C_GREEN}✅ Golden 更新完成，请务必 git diff 审查改动后再提交${C_RESET}"
    exit 0
fi

echo "${C_GREEN}✅ 所有测试通过！${C_RESET}"
exit 0
