# 希夷语言编译器测试套件
# run_all_tests.ksh
# 此处的扩展名 .ksh 即是 ksh93，而非 pdksh / mksh 等。
#
# 依赖:
#   ksh93u+m >= 1.0.10 (低于此版本，或不是 ksh93u+m，直接退出)、cargo
#   jq  (仅当存在 Tests/list.json，或使用 --rerun-failed 时需要)
#   不需要 GNU timeout
#
# 平台:
#   只支持 64 位的 Solaris / illumos (uname -s 为 SunOS)。
#   Windows 请用 run_all_tests.ps1，
#   Linux 请用 run_all_tests.sh，
#   MacOS 请用 run_all_tests.zsh，
#   32 位一律拒绝。
#
#   仅在调试脚本时，可设 XIYI_TESTS_FORCE=1 跳过操作系统与位数自检 (ksh 版本检查不可跳过)。
#
# 命令:
#   ksh run_all_tests.ksh
#
# .exit golden 与其它版本一致，只接受 0-255。
# 平台细节差异: ksh93 里"被信号杀死"的子进程退出码是 256+信号号 (bash/zsh 是 128+信号号)。

MIN_KSH_MAJOR=1; MIN_KSH_MINOR=0; MIN_KSH_PATCH=10

case "${KSH_VERSION:-}" in
    *93u+m/*) ;;
    *)
        echo "❌ 请用 ksh93u+m 运行此脚本 (ksh run_all_tests.ksh)" >&2
        echo "   当前解释器为 ${KSH_VERSION:-不是 ksh}" >&2
        exit 1
        ;;
esac

# "Version AJM 93u+m/1.0.10+f0999ab7 2024-08-01" → 1.0.10
_ver=${KSH_VERSION#*93u+m/}
_ver=${_ver%%[!0-9.]*}
IFS=. read -r _maj _min _pat <<EOF
$_ver
EOF
if [ "${_maj:-0}" -lt "$MIN_KSH_MAJOR" ] 2>/dev/null \
   || { [ "${_maj:-0}" -eq "$MIN_KSH_MAJOR" ] && [ "${_min:-0}" -lt "$MIN_KSH_MINOR" ]; } \
   || { [ "${_maj:-0}" -eq "$MIN_KSH_MAJOR" ] && [ "${_min:-0}" -eq "$MIN_KSH_MINOR" ] && [ "${_pat:-0}" -lt "$MIN_KSH_PATCH" ]; }; then
    echo "❌ 需要 ksh93u+m >= $MIN_KSH_MAJOR.$MIN_KSH_MINOR.$MIN_KSH_PATCH，当前是 $KSH_VERSION" >&2
    echo "   请从 https://github.com/ksh93/ksh 获取并安装最新稳定版，再来运行。" >&2
    exit 1
fi
unset _ver _maj _min _pat

if [ "${XIYI_TESTS_FORCE:-0}" != "1" ] && [ "$(uname -s 2>/dev/null)" != "SunOS" ]; then
    echo "❌ 不支持的操作系统: $(uname -s 2>/dev/null || echo unknown)" >&2
    echo "   本脚本只支持 Solaris / illumos。" >&2
    exit 1
fi

# 从这里开始可以放心使用 ksh93 语法。
#   nounset  引用未定义的变量是错误 (抓拼写错误)
#   pipefail 管道中任何一段失败都算失败
# 故意不开 errexit：单个用例失败不能中断整个套件，错误由各处显式检查。
set -o nounset -o pipefail

# 关联数组下标里的路径常含 [ ] * ? 等字符；一律写成 h["$key"]，判存在用 ${h["$key"]+x}
# ([[ -v h[...] ]] 遇到含 ] 或 \ 的键会判错)。

# ============================================================
# 参数默认值
# ============================================================
EXPECT=all
typeset -a NAME_FILTERS NAME_REGEX_FILTERS EXCLUDE_FILTERS TAG_FILTERS
typeset -i SKIP_BUILD=0 RELEASE=0 FAIL_FAST=0 UPDATE_GOLDENS=0 STRICT_CRASH=0 RERUN_FAILED=0
JOBS=1
TIMEOUT_SECONDS=30
KEEP_LOGS=20
RERUN_SRC=                              # --rerun-failed 读取的 JSON (在日志轮转之前选定)
typeset -i SHARD_GIVEN=0 SHARD_INDEX=1 SHARD_TOTAL=1
SHARD_SPEC=

function usage {
    cat <<'EOF'
用法: run_all_tests.ksh [选项]

选项 (同时支持 --opt value 与 --opt=value):
  --expect {all|pass|fail}   只跑期望通过/期望失败的用例 (默认 all)
  --name PATTERN             匹配相对 Tests/ 的路径 (ksh 通配符)，可重复
  --name-regex PATTERN       匹配相对 Tests/ 的路径 (POSIX ERE，子串匹配，
                             需要整串匹配时自己加 ^ $)，可重复；与 --name 是"或"的关系
  --exclude PATTERN          排除匹配的用例 (ksh 通配符)，可重复；优先级最高
  --tag TAG                  匹配 tag，可重复
  --rerun-failed             只跑本机上一次完成的运行 (test_results/ 下最新的 .json)
                             里失败的用例；上次全部通过则直接成功退出
  --shard M/N                把过滤后的用例分成 N 片，只执行第 M 片 (1 <= M <= N)，
                             用于 CI 多机并行；某一片为空时成功退出
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

过滤规则:
  一个用例必须同时满足: --expect、(--name 或 --name-regex 任一命中)、
  (--tag 任一命中)、--rerun-failed；被任何一条 --exclude 命中的用例一律剔除；
  最后才按 --shard 切片。
  --name / --exclude 用的是 ksh 的通配符 (在 sh 版的基础上还认 @(a|b)、+(x) 这类写法)；
  --name-regex 是 POSIX ERE，不是 PCRE (没有 \d、惰性量词)；匹配用的路径统一以 / 分隔。
  --shard 按"过滤后用例的有序位置"轮转分配：各机器必须传入完全相同的过滤参数
  (且 Tests/ 内容一致)，各片才互不重叠、合起来恰好是完整集合。
EOF
}

function die_usage {
    print -u2 -r -- "$*"
    usage >&2
    exit 2
}

# 严格的非负十进制整数 (最多 9 位，避免溢出)
function is_uint { [[ -n $1 && $1 != *[!0-9]* ]] && (( ${#1} <= 9 )); }
function need_arg { (( $# >= 2 )) || die_usage "选项 $1 需要一个参数"; }

# 正则能否被编译。ksh 的 [[ == ~(E)... ]] 遇到非法正则不报错、只是不匹配，
# 所以只能借 grep -E 预检 (Solaris 上优先用 /usr/xpg4/bin/grep，它才是符合 POSIX 的版本)。
# 实际匹配仍由 ksh 自己的 ERE 引擎完成，两者方言细节不同时以 ksh 为准。
if [[ -x /usr/xpg4/bin/grep ]]; then GREP_E=/usr/xpg4/bin/grep; else GREP_E=grep; fi
function regex_is_valid {
    $GREP_E -E -e "$1" </dev/null >/dev/null 2>&1
    (( $? != 2 ))
}

# --opt=value → --opt value；其余按 case 分派
while (( $# > 0 )); do
    if [[ $1 == --*=* ]]; then
        set -- "${1%%=*}" "${1#*=}" "${@:2}"
    fi
    case $1 in
        --expect)         need_arg "$@"; typeset -l EXPECT=$2; shift 2;;
        --name)           need_arg "$@"; NAME_FILTERS+=("$2"); shift 2;;
        --name-regex)     need_arg "$@"; NAME_REGEX_FILTERS+=("$2"); shift 2;;
        --exclude)        need_arg "$@"; EXCLUDE_FILTERS+=("$2"); shift 2;;
        --tag)            need_arg "$@"; typeset -l _tag=$2; TAG_FILTERS+=("$_tag"); shift 2;;
        --rerun-failed)   RERUN_FAILED=1; shift;;
        --shard)          need_arg "$@"
                          (( SHARD_GIVEN == 0 )) || die_usage "❌ --shard 只能指定一次"
                          SHARD_GIVEN=1; SHARD_SPEC=$2; shift 2;;
        --skip-build)     SKIP_BUILD=1; shift;;
        --release)        RELEASE=1; shift;;
        --fail-fast)      FAIL_FAST=1; shift;;
        --update-goldens) UPDATE_GOLDENS=1; shift;;
        --strict-crash)   STRICT_CRASH=1; shift;;
        --jobs)           need_arg "$@"; JOBS=$2; shift 2;;
        --timeout)        need_arg "$@"; TIMEOUT_SECONDS=$2; shift 2;;
        --keep-logs)      need_arg "$@"; KEEP_LOGS=$2; shift 2;;
        -h|--help)        usage; exit 0;;
        *)                die_usage "未知参数: $1";;
    esac
done

# ---- 参数校验 ----
for _p in "${NAME_FILTERS[@]}";    do [[ -n $_p ]] || die_usage "❌ --name 的值不能为空"; done
for _p in "${EXCLUDE_FILTERS[@]}"; do [[ -n $_p ]] || die_usage "❌ --exclude 的值不能为空"; done
for _p in "${TAG_FILTERS[@]}"; do
    # 空 tag 会匹配所有"没有 tag"的用例；含空白/逗号的 tag 永远匹配不到
    [[ -n $_p && $_p != *[[:space:],]* ]] \
        || die_usage "❌ --tag 的值不能为空，且不能含空白或逗号: '$_p'"
done
for _p in "${NAME_REGEX_FILTERS[@]}"; do
    [[ -n $_p ]] || die_usage "❌ --name-regex 的值不能为空"
    regex_is_valid "$_p" || die_usage "❌ --name-regex 不是合法的 ERE: $_p"
done
unset _p

RE_SHARD='^([0-9]{1,9})/([0-9]{1,9})$'
if (( SHARD_GIVEN )); then
    [[ $SHARD_SPEC == ~(E)$RE_SHARD ]] \
        || die_usage "❌ --shard 的格式应为 M/N (例如 2/4)，收到: '$SHARD_SPEC'"
    (( SHARD_INDEX = 10#${.sh.match[1]} ))
    (( SHARD_TOTAL = 10#${.sh.match[2]} ))
    (( SHARD_INDEX >= 1 && SHARD_INDEX <= SHARD_TOTAL )) \
        || die_usage "❌ --shard M/N 要求 1 <= M <= N，收到: $SHARD_INDEX/$SHARD_TOTAL"
fi

[[ $EXPECT == @(all|pass|fail) ]] || die_usage "❌ --expect 只接受 all / pass / fail，收到: $EXPECT"
is_uint "$JOBS"            && (( 10#$JOBS >= 1 ))            || die_usage "❌ --jobs 必须是 >= 1 的整数，收到: $JOBS"
is_uint "$TIMEOUT_SECONDS" && (( 10#$TIMEOUT_SECONDS >= 1 )) || die_usage "❌ --timeout 必须是 >= 1 的整数，收到: $TIMEOUT_SECONDS"
is_uint "$KEEP_LOGS"       && (( 10#$KEEP_LOGS >= 1 ))       || die_usage "❌ --keep-logs 必须是 >= 1 的整数，收到: $KEEP_LOGS"
(( JOBS = 10#$JOBS, TIMEOUT_SECONDS = 10#$TIMEOUT_SECONDS, KEEP_LOGS = 10#$KEEP_LOGS ))

# ============================================================
# 颜色 (CI、NO_COLOR 或输出不是终端时关闭)
# ============================================================
C_RESET= C_RED= C_GREEN= C_YELLOW= C_CYAN= C_MAGENTA= C_GRAY=
if [[ -z ${CI:-} && -z ${NO_COLOR:-} && -t 1 ]]; then
    C_RESET=$'\E[0m' C_RED=$'\E[31m' C_GREEN=$'\E[32m' C_YELLOW=$'\E[33m'
    C_CYAN=$'\E[36m' C_MAGENTA=$'\E[35m' C_GRAY=$'\E[90m'
fi

function fatal { print -u2 -r -- "${C_RED}❌ $*${C_RESET}"; exit 1; }
function warn  { print -u2 -r -- "${C_YELLOW}⚠️  $*${C_RESET}"; }

# ============================================================
# 位数自检：只支持 64 位 (操作系统已在文件开头检查过)
#   Solaris / illumos 有专用工具 isainfo -b (内核/用户空间支持的位数)，优先用它；
#   没有时再看 getconf LONG_BIT。
# ============================================================
function platform_reject {
    print -u2 -r -- "${C_RED}❌ $1${C_RESET}"
    print -u2 "   本测试脚本只支持 64 位 Solaris / illumos。"
    print -u2 "   (仅调试脚本本身时可设 XIYI_TESTS_FORCE=1 跳过此检查)"
    exit 1
}

function check_platform {
    typeset bits=
    if command -v isainfo >/dev/null 2>&1; then
        bits=$(isainfo -b 2>/dev/null)
    fi
    [[ -n $bits ]] || bits=$(getconf LONG_BIT 2>/dev/null)
    case $bits in
        64) ;;
        32) platform_reject "当前是 32 位用户空间 (位数检测结果: 32)";;
        *)  platform_reject "无法确认位数 (isainfo / getconf 都不可用)";;
    esac
}

if [[ ${XIYI_TESTS_FORCE:-0} == 1 ]]; then
    warn "XIYI_TESTS_FORCE=1，已跳过平台自检（仅供调试脚本本身）"
else
    check_platform
fi

# ============================================================
# 临时目录 + 信号处理
#   - INT/TERM 必须真正退出，退出时无论如何都要清理
#   - 不依赖 job control / 进程组：靠"沿 ppid 找后代"来终止进程树 (见 descendants_of)
#   - 后台子 shell 不继承 EXIT trap，所以清理只会在主脚本里发生一次
# ============================================================
RESULTS_DIR= LOG_FILE= INTERRUPTED=
typeset -a FILTERED
typeset -i DISPATCHED=0

# 输出 $1 及其全部后代进程的 pid (自底向上：后代在前，$1 本身最后)。
# 一次 ps 快照建出 ppid→子进程 表，再做广度优先；pid <= 1 的绝不会出现在结果里，
# 避免误杀 init / 整个进程组。
function descendants_of {
    typeset root=$1 line pid ppid cur
    typeset -A kids
    typeset -a queue order words
    typeset -i head=0 i
    is_uint "$root" && (( root > 1 )) || return 0

    ps -A -o pid= -o ppid= 2>/dev/null | while read -r pid ppid; do
        is_uint "$pid" && is_uint "$ppid" && kids["$ppid"]+=" $pid"
    done

    queue=( $root )
    while (( head < ${#queue[@]} )); do
        cur=${queue[head]}
        head+=1
        order+=( $cur )
        # 先取出再拆分：直接写 words=( ${kids["$cur"]-} ) 会多出空元素，导致死循环
        line=${kids["$cur"]-}
        words=()
        [[ -n $line ]] && words=( $line )
        queue+=( "${words[@]}" )
    done
    for (( i = ${#order[@]} - 1; i >= 0; i-- )); do print -rn -- "${order[i]} "; done
    print
}

# 终止 $1 及其全部后代：先 TERM，最多等 $2 秒让它们自己退出，还活着的再 KILL。
# 只在开头拍一次快照：进程一退出，它的子孙就被 init 收养，之后再顺着 ppid 找就找不到了。
function kill_tree {
    typeset root=$1 p
    typeset -i grace=${2:-5} waited=0
    typeset -a pids alive
    pids=( $(descendants_of "$root") )
    (( ${#pids[@]} )) || return 0
    kill -TERM "${pids[@]}" 2>/dev/null
    while (( waited < grace * 20 )); do        # 每 50 毫秒查一次
        alive=()
        for p in "${pids[@]}"; do kill -0 "$p" 2>/dev/null && alive+=( "$p" ); done
        (( ${#alive[@]} )) || return 0
        sleep 0.05
        waited+=1
    done
    kill -KILL "${alive[@]}" 2>/dev/null
    return 0
}

function cleanup {
    typeset rc=$1 p
    typeset -a mine
    trap - EXIT INT TERM
    if [[ -n $INTERRUPTED && -n $LOG_FILE && -f $LOG_FILE ]]; then
        print -r -- "运行被 $INTERRUPTED 中断: 已派发 $DISPATCHED/${#FILTERED[@]} 个用例；未生成汇总与 JSON" >> "$LOG_FILE" 2>/dev/null
    fi
    # 本脚本启动的一切 (xiyi、watchdog、worker) 都是 $$ 的后代；自己不能杀自己
    # 先杀上层再杀下层：worker 先死，就不会在 xiyi 被杀之后接着去启动下一个用例
    for p in $(descendants_of $$); do [[ $p == $$ ]] || mine=( "$p" "${mine[@]}" ); done
    (( ${#mine[@]} )) && kill -KILL "${mine[@]}" 2>/dev/null
    [[ -n $RESULTS_DIR && -d $RESULTS_DIR ]] && rm -rf -- "$RESULTS_DIR"
    exit "$rc"
}
trap 'cleanup $?' EXIT
trap 'INTERRUPTED=INT;  exit 130' INT
trap 'INTERRUPTED=TERM; exit 143' TERM

# 临时目录：mkdir 是原子的，失败就换个名字重试 (不依赖各系统 mktemp 的差异)
typeset -i _try
for (( _try = 0; _try < 20; _try++ )); do
    RESULTS_DIR=${TMPDIR:-/tmp}/xiyi-test.$$.$RANDOM
    mkdir -m 700 "$RESULTS_DIR" 2>/dev/null && break
    RESULTS_DIR=
done
[[ -n $RESULTS_DIR ]] || fatal "无法创建临时目录"
unset _try

# ============================================================
# 路径发现
#   ${.sh.file} = 本脚本的路径；cd + pwd -P 得到解析过符号链接的绝对目录
# ============================================================
_self=${.sh.file}
_dir=${_self%/*}
[[ $_dir == "$_self" ]] && _dir=.
COMPILER_ROOT=$(cd -- "$_dir" && pwd -P) || fatal "无法确定脚本目录"
unset _self _dir

function find_workspace_root {
    typeset dir=$1
    while true; do
        if [[ -d $dir/Tests && -d $dir/Standard ]]; then
            print -r -- "$dir"
            return 0
        fi
        [[ $dir == / ]] && return 1
        dir=${dir%/*}
        [[ -n $dir ]] || dir=/
    done
}

WORKSPACE_ROOT=$(find_workspace_root "$COMPILER_ROOT") \
    || fatal "从 $COMPILER_ROOT 向上找不到同时包含 Tests/ 与 Standard/ 的工作区根目录"

STDLIB=$WORKSPACE_ROOT/Standard
TEST_DIR=$WORKSPACE_ROOT/Tests
TEST_LIST_JSON=$TEST_DIR/list.json

# 固定工作目录，避免用例里的相对路径随调用位置变化
cd -- "$COMPILER_ROOT" || fatal "无法进入 $COMPILER_ROOT"

# ============================================================
# 日志目录 + 轮转 (先创建本次日志，再保留最近 KEEP_LOGS 个，含本次)
#   日志文件名就是时间戳 (YYYYMMDD_HHMMSS)，按文件名排序即按时间排序，
#   而且不受"复制 / checkout 改了修改时间"的影响。glob 展开本身就是升序的。
# ============================================================
LOG_DIR=$COMPILER_ROOT/test_results
mkdir -p -- "$LOG_DIR" || fatal "无法创建日志目录 $LOG_DIR"

RUN_ID=$(printf '%(%Y%m%d_%H%M%S)T' now)
# 同一秒内重复启动 (CI 并发) 时避免日志互相覆盖
[[ -e $LOG_DIR/$RUN_ID.log || -e $LOG_DIR/$RUN_ID.json ]] && RUN_ID=${RUN_ID}_$$
LOG_FILE=$LOG_DIR/$RUN_ID.log
JSON_FILE=$LOG_DIR/$RUN_ID.json
: > "$LOG_FILE" || fatal "无法写入日志 $LOG_FILE"

# --rerun-failed 的数据来源 = 上一次"完成"的运行留下的 JSON (被中断的运行不会产生 JSON)。
# 必须赶在轮转之前选定并加以保护，否则 --keep-logs 1 时它会被当成旧文件清掉。
# 只认本脚本的命名规则 (YYYYMMDD_HHMMSS*.json)，不会误取别人放进来的其它 json。
if (( RERUN_FAILED )); then
    for _f in "$LOG_DIR"/[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]_[0-9][0-9][0-9][0-9][0-9][0-9]*.json; do
        [[ -f $_f && $_f != "$JSON_FILE" ]] && RERUN_SRC=$_f     # 升序展开：最后一个最新
    done
    unset _f
fi

# 保留 $2 份最新的 .$1 文件，其余删除；本次日志与重跑数据源受保护
function rotate_logs {
    typeset ext=$1 f
    typeset -i keep=$2 n i
    typeset -a files
    for f in "$LOG_DIR"/*."$ext"; do [[ -f $f ]] && files+=( "$f" ); done
    n=${#files[@]}
    for (( i = 0; i < n - keep; i++ )); do        # 前 n-keep 个是最旧的
        f=${files[i]}
        [[ $f == "$LOG_FILE" || $f == "$JSON_FILE" || $f == "$RERUN_SRC" ]] && continue
        rm -f -- "$f"
    done
}
rotate_logs log  "$KEEP_LOGS"            # 本次 .log 已创建，含在 KEEP_LOGS 之内
rotate_logs json "$(( KEEP_LOGS - 1 ))"  # 本次 .json 在最后才写，先给它留一个位置

# ============================================================
# cargo metadata：只调用一次，版本号与 target 目录共用
#   --no-deps 不需要依赖已下载，--offline 避免联网。
#   比自己解析 Cargo.toml 稳：version.workspace = true 这类继承写法 cargo 会替我们解析好。
# ============================================================
CARGO_META=
if command -v cargo >/dev/null 2>&1 && command -v jq >/dev/null 2>&1; then
    CARGO_META=$(cd -- "$COMPILER_ROOT" && cargo metadata --no-deps --format-version 1 --offline 2>/dev/null) \
        || CARGO_META=
fi

# 取 $COMPILER_ROOT/Cargo.toml 对应那个包的版本；
# 路径对不上 (如符号链接) 时，只有一个包才采用它，否则宁可显示 unknown 也不猜。
function get_xiyi_version {
    typeset v=
    if [[ -n $CARGO_META ]]; then
        v=$(jq -r --arg m "$COMPILER_ROOT/Cargo.toml" '
            .packages
            | (map(select(.manifest_path == $m)) | first)
              // (if length == 1 then .[0] else null end)
            | .version // empty
        ' <<< "$CARGO_META" 2>/dev/null) || v=
    fi
    print -r -- "${v:-unknown}"
}
XIYI_VERSION=$(get_xiyi_version)

# ============================================================
# 可执行文件：尊重 CARGO_TARGET_DIR / workspace 的 target 目录
# ============================================================
function resolve_target_dir {
    typeset d=
    [[ -n $CARGO_META ]] && d=$(jq -r '.target_directory // empty' <<< "$CARGO_META" 2>/dev/null)
    if [[ -z $d ]]; then
        if [[ -n ${CARGO_TARGET_DIR:-} ]]; then
            d=$CARGO_TARGET_DIR
            [[ $d == /* ]] || d=$COMPILER_ROOT/$d
        else
            d=$COMPILER_ROOT/target
        fi
    fi
    print -r -- "$d"
}

# ============================================================
# 头部
# ============================================================
SHARD_INFO=
(( SHARD_GIVEN )) && SHARD_INFO="   分片: $SHARD_INDEX/$SHARD_TOTAL"
print -r -- "${C_CYAN}========================================${C_RESET}"
print -r -- "${C_CYAN} 希夷编译器测试套件${C_RESET}"
print -r -- "${C_CYAN} xiyi-compiler 版本: $XIYI_VERSION${C_RESET}"
print -r -- "${C_CYAN} Expect 过滤: $EXPECT   并发: $JOBS   超时(默认): ${TIMEOUT_SECONDS}s${SHARD_INFO}${C_RESET}"
print -r -- "${C_GRAY} 编译器目录: $COMPILER_ROOT${C_RESET}"
print -r -- "${C_GRAY} 标准库目录: $STDLIB${C_RESET}"
print -r -- "${C_GRAY} 测试目录:   $TEST_DIR${C_RESET}"
print -r -- "${C_GRAY} 日志:       $LOG_FILE${C_RESET}"
print -r -- "${C_CYAN}========================================${C_RESET}"
print

# 把数组按 "; " 连接 (没有元素时输出空串)
function join_by { typeset sep=$1 out= x; shift; for x in "$@"; do out+="${out:+$sep}$x"; done; print -rn -- "$out"; }

{
    print -r -- "运行 ID: $RUN_ID"
    print -r -- "版本: $XIYI_VERSION"
    print -r -- "参数: Expect=$EXPECT Jobs=$JOBS SkipBuild=$SKIP_BUILD Release=$RELEASE UpdateGoldens=$UPDATE_GOLDENS FailFast=$FAIL_FAST StrictCrash=$STRICT_CRASH"
    print -r -- "过滤: name=[$(join_by '; ' "${NAME_FILTERS[@]}")] name-regex=[$(join_by '; ' "${NAME_REGEX_FILTERS[@]}")] exclude=[$(join_by '; ' "${EXCLUDE_FILTERS[@]}")] tag=[$(join_by '; ' "${TAG_FILTERS[@]}")] rerun-failed=$RERUN_FAILED shard=${SHARD_SPEC:-无}"
} >> "$LOG_FILE"

# ============================================================
# 步骤1: 编译
# ============================================================
if (( ! SKIP_BUILD )); then
    print -r -- "${C_YELLOW}[1/4] 编译 xiyi-compiler...${C_RESET}"
    command -v cargo >/dev/null 2>&1 || fatal "找不到 cargo (可用 --skip-build 跳过编译)"

    typeset -a build_args=( build -q )
    (( RELEASE )) && build_args+=( --release )

    build_output=$(cd -- "$COMPILER_ROOT" && cargo "${build_args[@]}" 2>&1)
    build_rc=$?
    print -r -- "$build_output" >> "$LOG_FILE"

    if (( build_rc != 0 )); then
        print -r -- "$build_output"
        fatal "编译失败，停止测试"
    fi
    print -r -- "${C_GREEN}✅ 编译成功${C_RESET}"
else
    print -r -- "${C_YELLOW}[1/4] 跳过编译（--skip-build）${C_RESET}"
fi
print

# ============================================================
# 步骤2: 定位可执行文件
# ============================================================
PROFILE_NAME=debug
(( RELEASE )) && PROFILE_NAME=release

# cargo metadata 在大 workspace 上要几百毫秒，只算一次
TARGET_DIR=$(resolve_target_dir)
XIYI_EXE=$TARGET_DIR/$PROFILE_NAME/xiyi
[[ -f $XIYI_EXE && -x $XIYI_EXE ]] || fatal "在 $TARGET_DIR/$PROFILE_NAME 下找不到可执行的 xiyi"
[[ -d $STDLIB ]] || fatal "找不到标准库目录 $STDLIB"

print -r -- "${C_GRAY}使用可执行文件: $XIYI_EXE${C_RESET}"
print

# ============================================================
# 步骤3: 扫描测试
#   globstar 打开后 ** 可以递归；glob 本身会跳过符号链接目录，
#   再用 -f && ! -L 排除符号链接文件 (与 find -type f 一致)。
#   FIGNORE 设为只忽略 . 和 .. 后，以 . 开头的目录也会被扫到。
#   set -s 对位置参数做字节序排序 (C 区域设置)：各机器顺序一致，分片才互不重叠。
# ============================================================
print -r -- "${C_YELLOW}[2/4] 扫描测试用例（$TEST_DIR）...${C_RESET}"

typeset -a ALL_TESTS
function scan_tests {
    typeset LC_ALL=C f
    typeset -a found
    set -o globstar
    FIGNORE='@(.|..)'
    cd -- "$TEST_DIR" || return 1
    for f in **/*.xiyi; do
        [[ -f $f && ! -L $f ]] && found+=( "$f" )
    done
    cd -- "$COMPILER_ROOT"
    (( ${#found[@]} )) || return 0
    set -s -- "${found[@]}"
    ALL_TESTS=( "$@" )
}
scan_tests || fatal "无法进入测试目录 $TEST_DIR"
(( ${#ALL_TESTS[@]} )) || fatal "$TEST_DIR 下找不到任何 .xiyi 文件"

# ---- list.json: 只作为"例外覆盖"，优先级: defaults < 源码内联 < list.json 里针对该文件的条目 ----
BASE_EXPECT=PASS
BASE_TIMEOUT=$TIMEOUT_SECONDS

# 每个覆盖项的各字段放在以相对路径为键的关联数组里
typeset -A OV_SEEN OV_EXPECT OV_TIMEOUT OV_STDERR OV_TAG OV_SKIP OV_REASON

# 把 "a, b  c" 规范化成 "a b c"
function norm_tags {
    typeset raw=${1//,/ }
    typeset -a words
    words=( $raw )
    print -rn -- "${words[*]-}"
}

# 把 expect 字符串规范成 PASS / FAIL (FAIL / EXPECT_FAIL 都算 FAIL，大小写不敏感)
function expect_word {
    typeset -u up=$1
    [[ $up == @(FAIL|EXPECT_FAIL) ]] && print FAIL || print PASS
}

function load_list_json {
    typeset clean=$RESULTS_DIR/list.json err v d_expect d_timeout rows row key
    typeset -a f
    typeset sep=$'\x1f'

    command -v jq >/dev/null 2>&1 || fatal "$TEST_LIST_JSON 存在但未找到 jq，无法解析；请安装 jq，或临时移除该文件"

    # 去掉 UTF-8 BOM (Windows 编辑器常带)：在 ksh 里直接处理，不依赖各系统 sed 对 \x 转义的支持
    v=$(<"$TEST_LIST_JSON") || fatal "无法读取 $TEST_LIST_JSON"
    print -r -- "${v#$'\xEF\xBB\xBF'}" > "$clean"

    # 解析失败必须报错退出；不能静默丢掉全部覆盖项
    err=$(jq empty "$clean" 2>&1) || fatal "解析 $TEST_LIST_JSON 失败: ${err%%$'\n'*}"
    jq -e 'type == "object"' "$clean" >/dev/null 2>&1 || fatal "$TEST_LIST_JSON 顶层必须是 JSON 对象"

    v=$(jq -r '.version? // empty' "$clean")
    [[ -z $v || $v == 1 ]] || warn "list.json 声明的 version=$v，本脚本只认识 version 1，按 1 处理"

    d_expect=$(jq -r '.defaults.expect? // empty' "$clean")
    [[ -n $d_expect ]] && BASE_EXPECT=$(expect_word "$d_expect")

    d_timeout=$(jq -r '.defaults.timeout? // empty' "$clean")
    if [[ -n $d_timeout ]]; then
        if is_uint "$d_timeout" && (( 10#$d_timeout >= 1 )); then
            (( BASE_TIMEOUT = 10#$d_timeout ))
        else
            warn "list.json defaults.timeout=\"$d_timeout\" 不是 >= 1 的整数，已忽略"
        fi
    fi

    # 字段之间用 0x1f 分隔 (不会出现在文本里：jq 已把控制字符替换成空格)
    rows=$(jq -r '
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
    ' "$clean") || fatal "读取 $TEST_LIST_JSON 的 tests 失败"

    [[ -n $rows ]] || return 0
    while IFS= read -r row; do
        [[ -n $row ]] || continue
        # 分隔符不是空白字符时，read 会保留空字段；末尾的空字段可能被吞掉，所以取字段一律带默认值
        IFS=$sep read -rA f <<< "$row"
        [[ -n ${f[0]-} ]] || continue
        key=${f[0]//\\//}      # list.json 与 ps1 共用：键里的 \ 视为目录分隔符
        key=${key#./}
        if [[ -n ${f[2]-} ]] && ! { is_uint "${f[2]}" && (( 10#${f[2]} >= 1 )); }; then
            warn "list.json: $key 的 timeout=\"${f[2]}\" 不是 >= 1 的整数，已忽略"
            f[2]=
        fi
        OV_SEEN["$key"]=1
        OV_EXPECT["$key"]=${f[1]-};  OV_TIMEOUT["$key"]=${f[2]-}; OV_STDERR["$key"]=${f[3]-}
        OV_TAG["$key"]=${f[4]-};     OV_SKIP["$key"]=${f[5]-};    OV_REASON["$key"]=${f[6]-}
    done <<< "$rows"
}
[[ -f $TEST_LIST_JSON ]] && load_list_json

# ---- 源码头内联元数据 ----
# 结果写入全局 META_*。只看前 40 行，例如:
#   // @expect fail   // @timeout 5   // @stderr-contains "类型不匹配"
#   // @tag parser, typecheck   // @skip 尚未实现 break
RE_EXPECT='^[[:space:]]*//[[:space:]]*@expect[[:space:]]+fail[[:space:]]*$'
RE_TIMEOUT='^[[:space:]]*//[[:space:]]*@timeout[[:space:]]+([0-9]{1,9})[[:space:]]*$'
RE_STDERR='^[[:space:]]*//[[:space:]]*@stderr-contains[[:space:]]+"([^"]*)"[[:space:]]*$'
RE_TAG='^[[:space:]]*//[[:space:]]*@tag[[:space:]]+(.+)$'
RE_SKIP='^[[:space:]]*//[[:space:]]*@skip([[:space:]]+(.*))?$'

function parse_inline_meta {
    typeset file=$1 line
    typeset -i n=0
    META_EXPECT=$2 META_TIMEOUT=$3 META_STDERR= META_TAGS= META_SKIP=0 META_REASON=

    # 文件读不了时静默跳过 (整个循环的 stderr 一起吞掉)
    {
        while (( n < 40 )) && { IFS= read -r line || [[ -n $line ]]; }; do
            n+=1
            line=${line%$'\r'}                              # CRLF 源文件
            (( n == 1 )) && line=${line#$'\xEF\xBB\xBF'}    # UTF-8 BOM
            if   [[ $line == ~(E)$RE_EXPECT  ]]; then META_EXPECT=FAIL
            elif [[ $line == ~(E)$RE_TIMEOUT ]]; then (( 10#${.sh.match[1]} >= 1 )) && (( META_TIMEOUT = 10#${.sh.match[1]} ))
            elif [[ $line == ~(E)$RE_STDERR  ]]; then META_STDERR=${.sh.match[1]}
            elif [[ $line == ~(E)$RE_TAG     ]]; then META_TAGS=$(norm_tags "${.sh.match[1]}")
            elif [[ $line == ~(E)$RE_SKIP    ]]; then META_SKIP=1; META_REASON=${.sh.match[2]-}
            fi
        done
    } < "$file" 2>/dev/null
    return 0
}

# ---- 组装测试用例 (并行数组，下标即用例编号，从 0 开始) ----
typeset -a T_REL T_FULL T_EXPECT T_TIMEOUT T_STDERR T_TAGS T_SKIP T_REASON
typeset -A REL_SET

function build_cases {
    typeset f rel
    typeset -i i=0
    for f in "${ALL_TESTS[@]}"; do
        rel=$f
        REL_SET["$rel"]=1

        # 优先级(低→高): list.json 的 defaults -> 源码内联 -> list.json 里针对该文件的例外条目
        parse_inline_meta "$TEST_DIR/$f" "$BASE_EXPECT" "$BASE_TIMEOUT"

        if [[ -n ${OV_SEEN["$rel"]+x} ]]; then
            [[ -n ${OV_EXPECT["$rel"]} ]]  && META_EXPECT=$(expect_word "${OV_EXPECT["$rel"]}")
            [[ -n ${OV_TIMEOUT["$rel"]} ]] && META_TIMEOUT=${OV_TIMEOUT["$rel"]}
            [[ -n ${OV_STDERR["$rel"]} ]]  && META_STDERR=${OV_STDERR["$rel"]}
            [[ -n ${OV_TAG["$rel"]} ]]     && META_TAGS=$(norm_tags "${OV_TAG["$rel"]}")
            if [[ ${OV_SKIP["$rel"]} == true ]]; then
                META_SKIP=1; META_REASON=${OV_REASON["$rel"]}
            fi
        fi

        T_REL[i]=$rel;             T_FULL[i]=$TEST_DIR/$f
        T_EXPECT[i]=$META_EXPECT;  T_TIMEOUT[i]=$META_TIMEOUT
        T_STDERR[i]=$META_STDERR;  T_TAGS[i]=$META_TAGS
        T_SKIP[i]=$META_SKIP;      T_REASON[i]=$META_REASON
        i+=1
    done

    # list.json 里写了、磁盘上却不存在的条目：几乎必然是拼写 / 移动文件后忘了改，
    # 否则覆盖项会被静默丢弃。
    for rel in "${!OV_SEEN[@]}"; do
        [[ -n ${REL_SET["$rel"]+x} ]] || warn "list.json 中的条目在 Tests/ 下找不到对应文件: $rel"
    done
}
build_cases

# ============================================================
# 过滤
#   一个用例要被选中，必须"同时"满足 (AND):
#     1. --expect                 期望结果相符
#     2. --name / --name-regex    两者合起来任一命中即可 (OR)；都没给就不限制
#     3. --tag                    任一命中即可 (OR)；没给就不限制
#     4. --rerun-failed           在上一次完成的运行里是 FAIL
#   满足以上之后，被任何一条 --exclude 命中就剔除 (优先级最高)。
#   最后才按 --shard 切片：切片永远作用在"已经过滤完"的有序列表上。
# ============================================================
typeset -A FAILED_SET

# 读取上一次完成的运行里失败的用例，填充 FAILED_SET
function load_rerun_set {
    typeset src_name=${RERUN_SRC##*/} check p
    typeset -i nfail=0
    command -v jq >/dev/null 2>&1 || fatal "--rerun-failed 需要 jq 来读取上一次的 JSON 结果"
    [[ -n $RERUN_SRC ]] || fatal "--rerun-failed: $LOG_DIR 下没有任何历史 JSON 结果，请先完整跑一次"

    # 先确认这确实是本脚本产出的结果文件；损坏或格式不对时要明说，
    # 不能让它悄悄表现成"没有失败的用例"。
    if ! check=$(jq -e '(.Results | type) == "array"' "$RERUN_SRC" 2>&1); then
        [[ $check == false ]] && check="缺少 Results 数组"
        fatal "--rerun-failed: 无法读取 $src_name: ${check%%$'\n'*}"
    fi

    while IFS= read -r p; do
        [[ -n $p ]] || continue
        FAILED_SET["$p"]=1
        nfail+=1
        # 用例被改名或删除后，旧记录对不上任何文件：提醒一声，而不是静默丢掉
        [[ -n ${REL_SET["$p"]+x} ]] || warn "上次失败的用例已不存在，将被忽略: $p"
    done < <(jq -r '.Results[] | select(.Status == "FAIL") | .RelPath // empty' "$RERUN_SRC")

    if (( nfail == 0 )); then
        # 上次全绿不是错误："先跑一遍、再重跑失败项"的用法里，这应当算成功
        print -r -- "${C_GREEN}✅ $src_name 里没有失败的用例，无需重跑${C_RESET}"
        print -r -- "--rerun-failed: $src_name 中没有失败用例，未执行任何测试" >> "$LOG_FILE"
        exit 0
    fi
    print -r -- "${C_GRAY}[rerun-failed] 从 $src_name 载入 $nfail 个失败用例${C_RESET}"
}
(( RERUN_FAILED )) && load_rerun_set

# 用例 $1 是否通过 --name / --name-regex 这一关
function passes_name_filter {
    typeset rel=${T_REL[$1]} pat
    (( ${#NAME_FILTERS[@]} + ${#NAME_REGEX_FILTERS[@]} )) || return 0
    # 变量放在 [[ == ]] 右侧、不加引号，就按通配符匹配；~(E) 前缀表示按 ERE 子串匹配
    for pat in "${NAME_FILTERS[@]}";       do [[ $rel == $pat ]] && return 0; done
    for pat in "${NAME_REGEX_FILTERS[@]}"; do [[ $rel == ~(E)$pat ]] && return 0; done
    return 1
}

function passes_tag_filter {
    typeset want tags
    (( ${#TAG_FILTERS[@]} )) || return 0
    typeset -l tags=${T_TAGS[$1]}
    # tag 是 "a b c" 形式；两侧补空格后做整词匹配。右侧加引号 = 按字面比较
    for want in "${TAG_FILTERS[@]}"; do [[ " $tags " == *" $want "* ]] && return 0; done
    return 1
}

function is_excluded {
    typeset rel=${T_REL[$1]} pat
    for pat in "${EXCLUDE_FILTERS[@]}"; do [[ $rel == $pat ]] && return 0; done
    return 1
}

function apply_filters {
    typeset want=PASS
    typeset -i i
    [[ $EXPECT == fail ]] && want=FAIL
    for (( i = 0; i < ${#T_REL[@]}; i++ )); do
        [[ $EXPECT == all || ${T_EXPECT[i]} == $want ]] || continue
        passes_name_filter $i || continue
        passes_tag_filter $i || continue
        (( ! RERUN_FAILED )) || [[ -n ${FAILED_SET["${T_REL[i]}"]+x} ]] || continue
        is_excluded $i && continue
        FILTERED+=( $i )
    done
}
apply_filters

(( ${#FILTERED[@]} )) || fatal "过滤后没有匹配的测试用例"

# ---- 分片前的"过滤结果指纹" ----
# --shard 的正确性依赖各机器的过滤结果完全一致。这里对分片前的有序相对路径列表
# (每条路径后跟一个 \n) 求 SHA-256，连同用例数写进 JSON 的 FilterCount / FilterDigest：
# CI 汇总各分片的报告时，这两项不全相同，就说明各机器看到的 Tests/ 不一致
# (浅克隆深度不同、中途有新提交等)，分片可能重叠或漏用例。
# 四个脚本对同一份用例列表算出的值相同，所以 Linux / macOS / Windows 分片混用也能互相比对。
# 只覆盖"选中了哪些用例"，不覆盖用例文件内容的改动。

# 对标准输入求 SHA-256 (小写十六进制)；没有可用工具时返回 1。
# Solaris / illumos 自带 digest(1)；GNU coreutils 的 sha256sum 作为备选。
function sha256_stdin {
    typeset out
    if   command -v digest >/dev/null 2>&1;    then out=$(digest -a sha256)
    elif command -v sha256sum >/dev/null 2>&1; then out=$(sha256sum)
    else return 1
    fi
    print -rn -- "${out%% *}"
}

typeset -i FILTER_COUNT=${#FILTERED[@]}
FILTER_DIGEST=$(for i in "${FILTERED[@]}"; do print -r -- "${T_REL[i]}"; done | sha256_stdin) || FILTER_DIGEST=
if (( SHARD_GIVEN )) && [[ -z $FILTER_DIGEST ]]; then
    warn "找不到 digest / sha256sum，无法生成分片指纹 (JSON 里的 FilterDigest 留空)"
fi

# ---- 分片 ----
# FILTERED 的顺序 = 扫描时按字节序排好的相对路径顺序，与机器、locale 无关，所以各分片划分一致。
# 按位置轮转 (而非按目录切块)，把相邻的、通常耗时相近的用例打散到不同分片，负载更均衡。
if (( SHARD_GIVEN )); then
    function slice_shard {
        typeset -i pos before=${#FILTERED[@]}
        typeset -a picked
        for (( pos = 0; pos < before; pos++ )); do
            (( pos % SHARD_TOTAL == SHARD_INDEX - 1 )) && picked+=( "${FILTERED[pos]}" )
        done
        FILTERED=( "${picked[@]}" )
        # N 大于用例数时，后面的分片会是空的——这不是错误 (见文末)
        print -r -- "${C_GRAY}[shard $SHARD_INDEX/$SHARD_TOTAL] 过滤后共 $before 个用例，本片 ${#FILTERED[@]} 个${C_RESET}"
    }
    slice_shard
fi

print -r -- "共 ${#FILTERED[@]} 个测试用例（目录下总计发现 ${#T_REL[@]} 个）"
(( SHARD_GIVEN )) && print -r -- "${C_GRAY}[shard] 过滤结果指纹: $FILTER_COUNT 个 / ${FILTER_DIGEST:-无} (各分片应完全相同)${C_RESET}"
print

# ============================================================
# 规范化 (用于 golden 比对)，结果放进 $NORM
#   - 去 NUL (ksh 的命令替换遇到 NUL 会截断，所以先用 tr 删掉)、CRLF / 孤立 CR → LF
#   - 去开头 BOM (ps1 5.1 写出的 golden 带 BOM)
#   - 去全部尾部空白，再补一个换行
# ============================================================
function normalize_file {
    typeset s
    s=$(LC_ALL=C tr -d '\000' < "$1" 2>/dev/null)
    s=${s#$'\xEF\xBB\xBF'}
    s=${s//$'\r\n'/$'\n'}
    s=${s//$'\r'/$'\n'}
    NORM=${s%%+([[:space:]])}$'\n'
}

# 两个文件规范化后是否相同 (== 右侧加引号：按字面比较，不当成通配符)
function same_normalized {
    typeset a
    normalize_file "$1"; a=$NORM
    normalize_file "$2"
    [[ $a == "$NORM" ]]
}

# ============================================================
# 单用例执行 (串行 / 并行共用)
#   每个用例一个目录 $RESULTS_DIR/<编号>/，里面有 stdout、stderr 和最终的 res。
#   res 的格式: 第 1 行状态，第 2 行耗时(秒)，第 3 行退出码 (没有则为 -)，之后每行一条原因。
#   先写临时文件再改名：别的进程只要看到 res，内容就一定是完整的。
# ============================================================
function write_result {      # 用法: write_result 编号 状态 耗时 退出码 [原因...]
    typeset rdir=$RESULTS_DIR/$1 st=$2 el=$3 ec=$4 r
    shift 4
    {
        print -r -- "$st"
        print -r -- "$el"
        print -r -- "$ec"
        for r in "$@"; do print -r -- "$r"; done
    } > "$rdir/res.tmp" && mv -f -- "$rdir/res.tmp" "$rdir/res"
}

# 读取 res 文件：REC[0]=状态 REC[1]=耗时 REC[2]=退出码 REC[3..]=原因；REASONS 是用 "; " 连好的原因
typeset -a REC
REASONS=
function read_result {
    typeset line
    REC=()
    while IFS= read -r line || [[ -n $line ]]; do REC+=( "$line" ); done < "$RESULTS_DIR/$1/res"
    REASONS=
    typeset -i k
    for (( k = 3; k < ${#REC[@]}; k++ )); do REASONS+="${REASONS:+; }${REC[k]}"; done
}

function run_case {
    typeset -i idx=$1 timeout_s=${T_TIMEOUT[$1]} rc pid wd ok=1 have_exit_golden=0
    typeset rel=${T_REL[idx]} full=${T_FULL[idx]} expect=${T_EXPECT[idx]}
    typeset stderr_contains=${T_STDERR[idx]}
    typeset rdir=$RESULTS_DIR/$idx
    typeset out_file=$rdir/stdout err_file=$rdir/stderr
    typeset -F3 t0 elapsed
    typeset el expected_exit= crash_what= err_text st=FAIL
    typeset -a reasons
    mkdir -p -- "$rdir"

    if [[ ${T_SKIP[idx]} == 1 ]]; then
        : > "$out_file"; : > "$err_file"
        write_result $idx SKIP 0.000 - "skip${T_REASON[idx]:+: ${T_REASON[idx]}}"
        return 0
    fi

    # ---- 运行：超时由"看门狗"实现 ----
    # 看门狗是个后台子 shell，睡满时限后留下 timedout 标记并终止用例的进程树
    # (sleep 是 ksh93 内建，支持小数，也不额外起进程)。
    # 主流程只管 wait 用例本身：用例一结束就立刻返回，没有轮询延迟；
    # 是否超时看标记，而不是靠退出码去猜 (被 OOM killer 杀掉的进程同样是 SIGKILL)。
    # </dev/null：避免用例读 stdin 时挂住终端 / CI 管道。
    t0=$SECONDS
    "$XIYI_EXE" --stdlib "$STDLIB" "$full" </dev/null >"$out_file" 2>"$err_file" &
    pid=$!
    (
        sleep $timeout_s
        : > "$rdir/timedout"
        kill_tree $pid 5
    ) &
    wd=$!
    wait $pid 2>/dev/null
    rc=$?
    (( elapsed = SECONDS - t0 ))
    # 超时：让看门狗把进程树清理干净再走；正常结束：直接撤掉看门狗
    [[ -e $rdir/timedout ]] || kill $wd 2>/dev/null
    wait $wd 2>/dev/null
    printf -v el '%.3f' $elapsed

    if [[ -e $rdir/timedout ]]; then
        write_result $idx FAIL $el - "timeout ${timeout_s}s"
        return 0
    fi

    if (( UPDATE_GOLDENS )); then
        if cp -- "$out_file" "$full.out" && cp -- "$err_file" "$full.err"; then
            write_result $idx UPDATED $el $rc "golden 已更新，请 git diff 审查"
        else
            write_result $idx FAIL $el $rc "无法写入 golden: $full.{out,err}"
        fi
        return 0
    fi

    # --- 退出码 ---
    if [[ -f $full.exit ]]; then
        have_exit_golden=1
        expected_exit=$(<"$full.exit")
        expected_exit=${expected_exit#$'\xEF\xBB\xBF'}
        expected_exit=${expected_exit//[[:space:]]/}
        # 只接受 0-255：共享的 golden 在所有平台上含义必须相同
        if is_uint "$expected_exit" && (( 10#$expected_exit <= 255 )); then
            (( expected_exit = 10#$expected_exit ))
        else
            ok=0
            reasons+=( "无效的 .exit golden: \"$expected_exit\" (只接受 0-255，这样所有平台才能共享)" )
            expected_exit=
        fi
    fi

    if (( have_exit_golden )); then
        # golden 无效时上面已记录原因并置 ok=0，这里只处理有效值，不会再退回"按 expect 推导"
        if [[ -n $expected_exit ]] && (( rc != expected_exit )); then
            ok=0
            reasons+=( "exit $rc, expected $expected_exit" )
        fi
    elif [[ $expect == FAIL ]]; then
        (( rc != 0 )) || { ok=0; reasons+=( "expected failure but exited 0" ); }
    else
        (( rc == 0 )) || { ok=0; reasons+=( "exit $rc, expected 0" ); }
    fi

    # --- 崩溃识别：被信号终止 (ksh93 里表现为 256+信号) / Rust panic / ICE ---
    err_text=$(LC_ALL=C tr -d '\000' < "$err_file")
    if (( rc > 256 && rc < 256 + 64 )); then
        crash_what="被信号 $(( rc - 256 )) 终止"
    elif [[ $err_text == *@(panicked at|internal compiler error)* ]]; then
        crash_what="stderr 含 panic/ICE 标志"
    fi

    # --- golden 比对 (有才比) ---
    if [[ -f $full.out ]] && ! same_normalized "$full.out" "$out_file"; then
        ok=0; reasons+=( "stdout mismatch" )
    fi
    if [[ -f $full.err ]] && ! same_normalized "$full.err" "$err_file"; then
        ok=0; reasons+=( "stderr mismatch" )
    fi

    # --- stderr 子串 (引号：按字面匹配) ---
    if [[ -n $stderr_contains && $err_text != *"$stderr_contains"* ]]; then
        ok=0
        reasons+=( "stderr missing: $stderr_contains" )
    fi

    # --- 崩溃处置 / 提示："期望失败"的用例很容易被编译器崩溃悄悄蒙混过关 ---
    if [[ -n $crash_what ]] && (( ! have_exit_golden )) && [[ $expect == FAIL ]]; then
        if (( STRICT_CRASH )); then
            ok=0
            reasons+=( "compiler crash: $crash_what" )
        elif (( ok )); then
            reasons+=( "(提示: $crash_what，通过可能源于编译器崩溃而非预期诊断；加 --strict-crash 视为失败)" )
        fi
    fi

    if [[ $expect == FAIL && -z $stderr_contains && ! -f $full.err ]]; then
        reasons+=( "(提示: EXPECT_FAIL 未绑定 stderr 校验，通过不代表失败原因正确)" )
    fi

    (( ok )) && st=PASS
    write_result $idx $st $el $rc "${reasons[@]}"
}

# ============================================================
# 结果打印
# ============================================================
# 最多输出文件开头的 4096 字节；超出则注明已截断
# (Solaris 的 head 没有 -c，用 dd 取固定字节数)
function print_head {
    typeset -i size
    LC_ALL=C dd if="$1" bs=4096 count=1 2>/dev/null
    size=$(LC_ALL=C wc -c < "$1" 2>/dev/null) || size=0
    (( size > 4096 )) && print -r -- $'\n...(截断)'
    print
}

function print_result {
    typeset -i idx=$1
    typeset rdir=$RESULTS_DIR/$idx rel=${T_REL[$1]} verdict elapsed color suffix

    # 子任务异常退出 (被 OOM 杀、被信号终止…) 而没留下 res：必须算失败，
    # 绝不能被汇总悄悄漏掉 (否则可能"全部通过"退出 0)。
    if [[ ! -e $rdir/res ]]; then
        mkdir -p -- "$rdir"
        : >> "$rdir/stdout"; : >> "$rdir/stderr"
        write_result $idx FAIL 0.000 - "no result recorded (worker died?)"
    fi

    read_result $idx
    verdict=${REC[0]} elapsed=${REC[1]}

    case $verdict in
        SKIP)    color=$C_GRAY;;
        UPDATED) color=$C_MAGENTA;;
        PASS)    color=$C_GREEN;;
        *)       color=$C_RED;;
    esac
    suffix=${REASONS:+ - $REASONS}
    print -r -- "${color}[$verdict] $rel (${elapsed}s)${suffix}${C_RESET}"

    if [[ $verdict == FAIL ]]; then
        {
            print -r -- $'\n'"---- $rel ----"
            print -r -- "reasons: $REASONS"
            print -r -- "stdout:"
            print_head "$rdir/stdout"
            print -r -- "stderr:"
            print_head "$rdir/stderr"
        } >> "$LOG_FILE"
    fi
}

# ============================================================
# 步骤4: 执行
# ============================================================
print -r -- "${C_YELLOW}[3/4] 执行测试...${C_RESET}"
print

# 并行：启动 JOBS 个常驻 worker，各自按顺序"认领"尚未被认领的用例。
# 认领靠 mkdir (原子操作：只有一个进程能成功)，所以不需要 wait -n 或令牌管道；
# 某个 worker 意外死亡时，最多丢它手里那一个用例 (记为失败)，不会拖死整个套件。
function worker {
    typeset -i i
    for i in "${FILTERED[@]}"; do
        mkdir "$RESULTS_DIR/claim/$i" 2>/dev/null || continue
        run_case $i
    done
}

typeset -a WORKER_PIDS
function workers_alive {
    typeset p
    for p in "${WORKER_PIDS[@]}"; do kill -0 "$p" 2>/dev/null && return 0; done
    return 1
}

typeset -i i n
if (( JOBS > 1 )); then
    (( FAIL_FAST )) && warn "--fail-fast 在并行模式下不生效"
    mkdir -p -- "$RESULTS_DIR/claim"
    (( n = JOBS < ${#FILTERED[@]} ? JOBS : ${#FILTERED[@]} ))
    while (( n-- > 0 )); do
        worker &
        WORKER_PIDS+=( $! )
    done
    DISPATCHED=${#FILTERED[@]}
    # 按用例顺序、边完成边打印 (某个用例慢，只会推迟它后面的打印，不影响并行度)
    for i in "${FILTERED[@]}"; do
        until [[ -e $RESULTS_DIR/$i/res ]]; do
            workers_alive || break
            sleep 0.05
        done
        print_result $i
    done
    wait
else
    for i in "${FILTERED[@]}"; do
        DISPATCHED+=1
        run_case $i
        print_result $i
        if (( FAIL_FAST )); then
            read_result $i
            if [[ ${REC[0]} == FAIL ]]; then
                print -r -- "${C_YELLOW}⏹  --fail-fast 触发，停止后续测试${C_RESET}"
                break
            fi
        fi
    done
fi
print

# ============================================================
# 步骤5: 汇总
#   只统计本次实际执行的用例 (串行 + --fail-fast 时 DISPATCHED < 过滤后总数)
# ============================================================
print -r -- "${C_YELLOW}[4/4] 汇总结果...${C_RESET}"

typeset -a EXECUTED=( "${FILTERED[@]:0:DISPATCHED}" )
typeset -i total=0 passed=0 failed=0 timeouts=0 skipped=0 updated=0
for i in "${EXECUTED[@]}"; do
    read_result $i
    total+=1
    case ${REC[0]} in
        PASS)    passed+=1;;
        SKIP)    skipped+=1;;
        UPDATED) updated+=1;;
        *)       failed+=1
                 [[ -e $RESULTS_DIR/$i/timedout ]] && timeouts+=1;;
    esac
done

print -r -- "${C_CYAN}========================================${C_RESET}"
print -r -- "${C_CYAN} 测试结果汇总${C_RESET}"
print -r -- "${C_CYAN}========================================${C_RESET}"
print -r -- " 总计:   $total"
print -r -- " ${C_GREEN}通过:   $passed${C_RESET}"
print -r -- " ${C_RED}失败:   $failed${C_RESET}"
print -r -- " ${C_YELLOW}超时:   $timeouts${C_RESET}"
print -r -- " ${C_GRAY}跳过:   $skipped${C_RESET}"
print -r -- " ${C_MAGENTA}已更新 golden: $updated${C_RESET}"
print -r -- "${C_CYAN}========================================${C_RESET}"
print -r -- " 日志:  $LOG_FILE"
print -r -- " 结果:  $JSON_FILE"
print -r -- "${C_CYAN}========================================${C_RESET}"

# ============================================================
# 写 JSON 结果 (先写临时文件再改名，避免中途被中断留下半截文件)
# ============================================================
function json_escape {
    typeset s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    s=${s//$'\n'/\\n}
    s=${s//$'\r'/\\r}
    s=${s//$'\t'/\\t}
    s=${s//$'\b'/\\b}
    s=${s//$'\f'/\\f}
    s=${s//[[:cntrl:]]/}      # 其余控制字符 (如 ANSI ESC) 在 JSON 里非法，直接丢弃
    print -rn -- "$s"
}

function write_json {
    typeset -i k first=1 first_r
    typeset r verdict ok sk up ec el
    print -r -- '{'
    print -r -- "  \"RunId\": \"$(json_escape "$RUN_ID")\","
    print -r -- "  \"Version\": \"$(json_escape "$XIYI_VERSION")\","
    print -r -- "  \"Total\": $total,"
    print -r -- "  \"Passed\": $passed,"
    print -r -- "  \"Failed\": $failed,"
    print -r -- "  \"Timeouts\": $timeouts,"
    print -r -- "  \"Skipped\": $skipped,"
    print -r -- "  \"Updated\": $updated,"
    print -r -- "  \"ShardIndex\": $SHARD_INDEX,"
    print -r -- "  \"ShardTotal\": $SHARD_TOTAL,"
    print -r -- "  \"FilterCount\": $FILTER_COUNT,"
    print -r -- "  \"FilterDigest\": \"$FILTER_DIGEST\","
    print -rn -- '  "Results": ['
    for i in "${EXECUTED[@]}"; do
        read_result $i
        verdict=${REC[0]} el=${REC[1]} ec=${REC[2]} first_r=1
        ok=false sk=false up=false
        [[ $verdict == @(PASS|SKIP|UPDATED) ]] && ok=true
        [[ $verdict == SKIP ]] && sk=true
        [[ $verdict == UPDATED ]] && up=true
        [[ $ec == +([0-9]) ]] || ec=null

        (( first )) || print -rn -- ','
        first=0
        print -rn -- $'\n'"    {\"RelPath\":\"$(json_escape "${T_REL[i]}")\",\"Status\":\"$verdict\","
        print -rn -- "\"Ok\":$ok,\"Skipped\":$sk,\"Updated\":$up,\"Elapsed\":$el,\"ExitCode\":$ec,\"Reasons\":["
        for (( k = 3; k < ${#REC[@]}; k++ )); do
            (( first_r )) || print -rn -- ','
            first_r=0
            print -rn -- "\"$(json_escape "${REC[k]}")\""
        done
        print -rn -- ']}'
    done
    print -r -- $'\n  ]'
    print -r -- '}'
}

JSON_TMP=$JSON_FILE.tmp.$$
if ! { write_json > "$JSON_TMP" && mv -f -- "$JSON_TMP" "$JSON_FILE"; }; then
    rm -f -- "$JSON_TMP"
    warn "写入 $JSON_FILE 失败"
fi

print -r -- "汇总: 总计 $total, 通过 $passed, 失败 $failed, 超时 $timeouts, 跳过 $skipped, 已更新 $updated" >> "$LOG_FILE"

# ============================================================
# 退出码
# ============================================================
if (( failed > 0 )); then
    print -r -- "${C_RED}❌ 有 $failed 个测试失败${C_RESET}"
    exit 1
fi

if (( total == 0 )); then
    # 走到这里只可能是分片为空 (过滤结果为空已在前面 fatal)。
    # CI 里 N 大于用例数时，多出来的分片应当成功，而不是无故变红。
    print -r -- "${C_YELLOW}ℹ️  本次没有分到任何用例 (分片 $SHARD_INDEX/$SHARD_TOTAL 为空)，视为成功${C_RESET}"
    exit 0
fi

if (( UPDATE_GOLDENS )); then
    print -r -- "${C_GREEN}✅ Golden 更新完成，请务必 git diff 审查改动后再提交${C_RESET}"
    exit 0
fi

print -r -- "${C_GREEN}✅ 所有测试通过！${C_RESET}"
exit 0
