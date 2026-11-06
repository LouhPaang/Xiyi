# 希夷语言编译器测试套件
# run_all_tests.zsh
#
# 依赖:
#   zsh >= 5.9.2 (低于此版本直接退出)、cargo
#   jq  (仅当存在 Tests/list.json，或使用 --rerun-failed 时需要)
#   不需要 coreutils 的 timeout
#
# 平台:
#   只支持 64 位的 MacOS。
#   Windows 请用 run_all_tests.ps1，
#   Linux 请用 run_all_tests.sh，
#   Solaris/illumos 请用 run_all_tests.ksh，
#   32 位一律拒绝。
#
#   仅在调试脚本时，可设 XIYI_TESTS_FORCE=1 跳过操作系统与位数自检 (zsh 版本检查不可跳过)。
#
# 命令:
#   zsh run_all_tests.zsh
#
# .exit golden 与其它版本一致，只接受 0-255。

if [ -z "${ZSH_VERSION:-}" ]; then
    echo "❌ 请用 Zsh 运行此脚本 (zsh run_all_tests.zsh)" >&2
    exit 1
fi

typeset -r MIN_ZSH=5.9.2

autoload -Uz is-at-least
if ! is-at-least $MIN_ZSH; then
    print -u2 "❌ 需要 zsh >= $MIN_ZSH，当前是 $ZSH_VERSION。"
    print -u2 "   macOS 自带的 Zsh 可能低于此版本。请先 brew install zsh 之后，再来运行。"
    exit 1
fi

# 从这里开始可以放心使用较新的 zsh 语法。先把选项恢复成 zsh 的默认值，
# 不受调用者 (别名、ZDOTDIR、兼容模式) 的影响；再打开几个防呆选项:
#   pipe_fail          管道中任何一段失败都算失败
#   no_unset           引用未定义的变量是错误 (抓拼写错误)
#   warn_create_global 在函数里不小心给全局变量赋值时发出警告 (故意的全局变量一律写 typeset -g)
# 故意不开 err_exit / err_return：单个用例失败不能中断整个套件，错误由各处显式检查。
emulate -R zsh
setopt pipe_fail no_unset warn_create_global

zmodload zsh/datetime || { print -u2 "❌ 缺少 zsh/datetime 模块"; exit 1; }
zmodload zsh/zselect  || { print -u2 "❌ 缺少 zsh/zselect 模块"; exit 1; }

if [[ ${XIYI_TESTS_FORCE:-0} != 1 && $OSTYPE != darwin* ]]; then
    print -u2 "❌ 不支持的操作系统: $OSTYPE"
    print -u2 "   本脚本只支持 macOS。"
    exit 1
fi

# ============================================================
# 参数默认值
# ============================================================
typeset EXPECT=all
typeset -a NAME_FILTERS=() NAME_REGEX_FILTERS=() EXCLUDE_FILTERS=() TAG_FILTERS=()
typeset -i SKIP_BUILD=0 RELEASE=0 FAIL_FAST=0 UPDATE_GOLDENS=0 STRICT_CRASH=0 RERUN_FAILED=0
typeset JOBS=1 TIMEOUT_SECONDS=30 KEEP_LOGS=20
typeset RERUN_SRC=                       # --rerun-failed 读取的 JSON (在日志轮转之前选定)
typeset -i SHARD_GIVEN=0 SHARD_INDEX=1 SHARD_TOTAL=1
typeset SHARD_SPEC=

usage() {
    cat <<'EOF'
用法: run_all_tests.zsh [选项]

选项 (同时支持 --opt value 与 --opt=value):
  --expect {all|pass|fail}   只跑期望通过/期望失败的用例 (默认 all)
  --name PATTERN             匹配相对 Tests/ 的路径 (zsh 通配符)，可重复
  --name-regex PATTERN       匹配相对 Tests/ 的路径 (POSIX ERE，子串匹配，
                             需要整串匹配时自己加 ^ $)，可重复；与 --name 是"或"的关系
  --exclude PATTERN          排除匹配的用例 (zsh 通配符)，可重复；优先级最高
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
  --name / --exclude 用的是 zsh 的通配符 (在 sh / ksh 版的基础上还认 (a|b) 与 <1-9> 这类写法)；
  --name-regex 是 POSIX ERE，不是 PCRE (没有 \d、惰性量词)；匹配用的路径统一以 / 分隔。
  --shard 按"过滤后用例的有序位置"轮转分配：各机器必须传入完全相同的过滤参数
  (且 Tests/ 内容一致)，各片才互不重叠、合起来恰好是完整集合。
EOF
}

die_usage() {
    print -u2 -r -- "$*"
    usage >&2
    exit 2
}

# 严格的非负十进制整数 (最多 9 位，避免溢出)
is_uint() { [[ -n $1 && $1 != *[^0-9]* && ${#1} -le 9 ]] }
need_arg() { (( $# >= 2 )) || die_usage "选项 $1 需要一个参数" }

# 正则能否被编译：zsh 对非法正则只打印 "failed to compile regex" 且返回 1
# (与"不匹配"同一个返回值)，所以要看它的错误输出，而不是看返回值。
regex_is_valid() {
    local err
    err=$( { [[ '' =~ $1 ]] } 2>&1 )
    [[ $err != *'failed to compile regex'* ]]
}

# --opt=value → --opt value；其余按 case 分派
while (( $# > 0 )); do
    if [[ $1 == --*=* ]]; then
        set -- "${1%%=*}" "${1#*=}" "${@:2}"
    fi
    case $1 in
        (--expect)        need_arg "$@"; EXPECT=${(L)2}; shift 2;;
        (--name)          need_arg "$@"; NAME_FILTERS+=("$2"); shift 2;;
        (--name-regex)    need_arg "$@"; NAME_REGEX_FILTERS+=("$2"); shift 2;;
        (--exclude)       need_arg "$@"; EXCLUDE_FILTERS+=("$2"); shift 2;;
        (--tag)           need_arg "$@"; TAG_FILTERS+=("${(L)2}"); shift 2;;
        (--rerun-failed)  RERUN_FAILED=1; shift;;
        (--shard)         need_arg "$@"
                          (( SHARD_GIVEN == 0 )) || die_usage "❌ --shard 只能指定一次"
                          SHARD_GIVEN=1; SHARD_SPEC=$2; shift 2;;
        (--skip-build)    SKIP_BUILD=1; shift;;
        (--release)       RELEASE=1; shift;;
        (--fail-fast)     FAIL_FAST=1; shift;;
        (--update-goldens) UPDATE_GOLDENS=1; shift;;
        (--strict-crash)  STRICT_CRASH=1; shift;;
        (--jobs)          need_arg "$@"; JOBS=$2; shift 2;;
        (--timeout)       need_arg "$@"; TIMEOUT_SECONDS=$2; shift 2;;
        (--keep-logs)     need_arg "$@"; KEEP_LOGS=$2; shift 2;;
        (-h|--help)       usage; exit 0;;
        (*)               die_usage "未知参数: $1";;
    esac
done

# ---- 参数校验 ----
() {
    local p
    # 数组展开一律写成 "${arr[@]}"：不加引号时 zsh 会丢掉空字符串元素，空值就检查不到了
    for p in "${NAME_FILTERS[@]}";    do [[ -n $p ]] || die_usage "❌ --name 的值不能为空"; done
    for p in "${EXCLUDE_FILTERS[@]}"; do [[ -n $p ]] || die_usage "❌ --exclude 的值不能为空"; done
    for p in "${TAG_FILTERS[@]}"; do
        # 空 tag 会匹配所有"没有 tag"的用例；含空白/逗号的 tag 永远匹配不到
        [[ -n $p && $p != *[[:space:],]* ]] \
            || die_usage "❌ --tag 的值不能为空，且不能含空白或逗号: '$p'"
    done
    for p in "${NAME_REGEX_FILTERS[@]}"; do
        [[ -n $p ]] || die_usage "❌ --name-regex 的值不能为空"
        regex_is_valid $p || die_usage "❌ --name-regex 不是合法的 ERE: $p"
    done
}

if (( SHARD_GIVEN )); then
    [[ $SHARD_SPEC =~ '^([0-9]{1,9})/([0-9]{1,9})$' ]] \
        || die_usage "❌ --shard 的格式应为 M/N (例如 2/4)，收到: '$SHARD_SPEC'"
    SHARD_INDEX=$(( 10#$match[1] ))
    SHARD_TOTAL=$(( 10#$match[2] ))
    (( SHARD_INDEX >= 1 && SHARD_INDEX <= SHARD_TOTAL )) \
        || die_usage "❌ --shard M/N 要求 1 <= M <= N，收到: $SHARD_INDEX/$SHARD_TOTAL"
fi

[[ $EXPECT == (all|pass|fail) ]] || die_usage "❌ --expect 只接受 all / pass / fail，收到: $EXPECT"
is_uint $JOBS            && (( 10#$JOBS >= 1 ))            || die_usage "❌ --jobs 必须是 >= 1 的整数，收到: $JOBS"
is_uint $TIMEOUT_SECONDS && (( 10#$TIMEOUT_SECONDS >= 1 )) || die_usage "❌ --timeout 必须是 >= 1 的整数，收到: $TIMEOUT_SECONDS"
is_uint $KEEP_LOGS       && (( 10#$KEEP_LOGS >= 1 ))       || die_usage "❌ --keep-logs 必须是 >= 1 的整数，收到: $KEEP_LOGS"
JOBS=$(( 10#$JOBS )); TIMEOUT_SECONDS=$(( 10#$TIMEOUT_SECONDS )); KEEP_LOGS=$(( 10#$KEEP_LOGS ))

# ============================================================
# 颜色 (CI、NO_COLOR 或输出不是终端时关闭)
# ============================================================
typeset C_RESET= C_RED= C_GREEN= C_YELLOW= C_CYAN= C_MAGENTA= C_GRAY=
if [[ -z ${CI:-} && -z ${NO_COLOR:-} && -t 1 ]]; then
    C_RESET=$'\e[0m' C_RED=$'\e[31m' C_GREEN=$'\e[32m' C_YELLOW=$'\e[33m'
    C_CYAN=$'\e[36m' C_MAGENTA=$'\e[35m' C_GRAY=$'\e[90m'
fi

fatal() { print -u2 -r -- "${C_RED}❌ $*${C_RESET}"; exit 1 }
warn()  { print -u2 -r -- "${C_YELLOW}⚠️  $*${C_RESET}" }

# ============================================================
# 位数自检：只支持 64 位 (操作系统已在文件开头检查过)
#   以 getconf LONG_BIT (用户空间位数) 为准；拿不到再看 zsh 自己记录的 $CPUTYPE。
# ============================================================
platform_reject() {
    print -u2 -r -- "${C_RED}❌ $1${C_RESET}"
    print -u2 "   本测试脚本只支持 64 位 macOS。"
    print -u2 "   (仅调试脚本本身时可设 XIYI_TESTS_FORCE=1 跳过此检查)"
    exit 1
}

check_platform() {
    local bits
    bits=$(getconf LONG_BIT 2>/dev/null) || bits=
    case $bits in
        (64) ;;
        (32) platform_reject "getconf LONG_BIT=32：当前是 32 位用户空间 (CPU: $CPUTYPE)";;
        (*)  [[ $CPUTYPE == (x86_64|arm64|aarch64) ]] \
                 || platform_reject "无法确认位数 (getconf 不可用)，且 CPU 架构 $CPUTYPE 不在 64 位白名单内";;
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
#   - 没有 job control (脚本里没有终端就开不了 monitor)，没有进程组可杀，
#     所以靠"沿 ppid 找后代"来终止进程树 (见 descendants_of)
#   - 子 shell (后台 worker) 不继承这些 trap，所以清理只会在主脚本里发生一次
# ============================================================
typeset RESULTS_DIR= LOG_FILE= INTERRUPTED=
typeset -a FILTERED=()
typeset -i DISPATCHED=0

# 输出 $1 及其全部后代进程的 pid (自底向上：后代在前，$1 本身最后)。
# 一次 ps 快照建出 ppid→子进程 表，再做广度优先；pid <= 1 的绝不会出现在结果里，
# 避免误杀 init / 整个进程组。
descendants_of() {
    local root=$1 line pid ppid cur
    typeset -A kids=()
    local -a queue=() order=()
    is_uint $root && (( root > 1 )) || return 0

    for line in "${(@f)$(ps -A -o pid= -o ppid= 2>/dev/null)}"; do
        pid=${${(z)line}[1]} ppid=${${(z)line}[2]}
        is_uint $pid && is_uint $ppid && kids[$ppid]+=" $pid"
    done

    queue=( $root )
    while (( $#queue )); do
        cur=$queue[1]; queue[1]=()
        order+=( $cur )
        queue+=( ${=kids[$cur]-} )
    done
    print -r -- ${(Oa)order}
}

# 终止 $1 及其全部后代：先 TERM，最多等 $2 秒让它们自己退出，还活着的再 KILL。
# 只在开头拍一次快照：进程一退出，它的子孙就被 init 收养，之后再顺着 ppid 找就找不到了。
kill_tree() {
    local root=$1 grace=${2:-5} p
    local -a pids=( $(descendants_of $root) ) alive=()
    local -F deadline
    (( $#pids )) || return 0
    kill -TERM $pids 2>/dev/null
    deadline=$(( EPOCHREALTIME + grace ))
    while (( EPOCHREALTIME < deadline )); do
        alive=()
        for p in $pids; do kill -0 $p 2>/dev/null && alive+=( $p ); done
        (( $#alive )) || return 0
        zselect -t 5
    done
    kill -KILL $alive 2>/dev/null
    return 0
}

cleanup() {
    local rc=$?
    trap - EXIT INT TERM
    if [[ -n $INTERRUPTED && -n $LOG_FILE && -f $LOG_FILE ]]; then
        print -r -- "运行被 $INTERRUPTED 中断: 已派发 $DISPATCHED/${#FILTERED} 个用例；未生成汇总与 JSON" >> $LOG_FILE 2>/dev/null
    fi
    # 本脚本启动的一切 (xiyi、watchdog、worker) 都是 $$ 的后代；自己不能杀自己
    # 先杀上层再杀下层：worker 先死，就不会在 xiyi 被杀之后接着去启动下一个用例
    local -a mine=( ${=$(descendants_of $$)} )
    mine=( ${(Oa)mine:#$$} )
    (( $#mine )) && kill -KILL $mine 2>/dev/null
    [[ -n $RESULTS_DIR && -d $RESULTS_DIR ]] && rm -rf -- $RESULTS_DIR
    exit $rc
}
trap cleanup EXIT
trap 'INTERRUPTED=INT;  exit 130' INT
trap 'INTERRUPTED=TERM; exit 143' TERM

RESULTS_DIR=$(mktemp -d "${TMPDIR:-/tmp}/xiyi-test.XXXXXX") || fatal "无法创建临时目录"

# ============================================================
# 路径发现
#   ${0:A:h} = 本脚本所在目录 (绝对路径、已解析符号链接)
# ============================================================
typeset -r COMPILER_ROOT=${0:A:h}

find_workspace_root() {
    local dir=$1
    while true; do
        if [[ -d $dir/Tests && -d $dir/Standard ]]; then
            print -r -- $dir
            return 0
        fi
        [[ $dir == / ]] && return 1
        dir=${dir:h}
    done
}

typeset WORKSPACE_ROOT
WORKSPACE_ROOT=$(find_workspace_root $COMPILER_ROOT) \
    || fatal "从 $COMPILER_ROOT 向上找不到同时包含 Tests/ 与 Standard/ 的工作区根目录"

typeset -r STDLIB=$WORKSPACE_ROOT/Standard
typeset -r TEST_DIR=$WORKSPACE_ROOT/Tests
typeset -r TEST_LIST_JSON=$TEST_DIR/list.json

# 固定工作目录，避免用例里的相对路径随调用位置变化
cd -q -- $COMPILER_ROOT || fatal "无法进入 $COMPILER_ROOT"

# ============================================================
# 日志目录 + 轮转 (先创建本次日志，再保留最近 KEEP_LOGS 个，含本次)
# ============================================================
typeset -r LOG_DIR=$COMPILER_ROOT/test_results
mkdir -p -- $LOG_DIR || fatal "无法创建日志目录 $LOG_DIR"

typeset RUN_ID
strftime -s RUN_ID '%Y%m%d_%H%M%S' $EPOCHSECONDS
# 同一秒内重复启动 (CI 并发) 时避免日志互相覆盖
[[ -e $LOG_DIR/$RUN_ID.log || -e $LOG_DIR/$RUN_ID.json ]] && RUN_ID=${RUN_ID}_$$
LOG_FILE=$LOG_DIR/$RUN_ID.log
typeset -r JSON_FILE=$LOG_DIR/$RUN_ID.json
: > $LOG_FILE || fatal "无法写入日志 $LOG_FILE"

# --rerun-failed 的数据来源 = 上一次"完成"的运行留下的 JSON (被中断的运行不会产生 JSON)。
# 必须赶在轮转之前选定并加以保护，否则 --keep-logs 1 时它会被当成旧文件清掉。
# 只认本脚本的命名规则 (YYYYMMDD_HHMMSS*.json)，不会误取别人放进来的其它 json。
# 限定符 (.Nom) = 只要普通文件、无匹配时不报错、按修改时间从新到旧。
if (( RERUN_FAILED )); then
    setopt local_options extended_glob
    local -a cands=( $LOG_DIR/[0-9](#c8)_[0-9](#c6)*.json(.Nom) )
    cands=( ${cands:#$JSON_FILE} )
    (( $#cands )) && RERUN_SRC=$cands[1]
fi

# 保留 $2 份最新的 .$1 文件，其余删除；本次日志与重跑数据源受保护
rotate_logs() {
    local ext=$1 keep=$2 f
    local -a files=( $LOG_DIR/*.$ext(.Nom) )
    for f in ${files[keep+1,-1]}; do
        [[ $f == ($LOG_FILE|$JSON_FILE|$RERUN_SRC) ]] && continue
        rm -f -- $f
    done
}
rotate_logs log  $KEEP_LOGS            # 本次 .log 已创建，含在 KEEP_LOGS 之内
rotate_logs json $(( KEEP_LOGS - 1 ))  # 本次 .json 在最后才写，先给它留一个位置

# ============================================================
# cargo metadata：只调用一次，版本号与 target 目录共用
#   --no-deps 不需要依赖已下载，--offline 避免联网。
#   比自己解析 Cargo.toml 稳：version.workspace = true 这类继承写法 cargo 会替我们解析好。
# ============================================================
typeset CARGO_META=
if (( $+commands[cargo] && $+commands[jq] )); then
    CARGO_META=$(cd -q -- $COMPILER_ROOT && cargo metadata --no-deps --format-version 1 --offline 2>/dev/null) \
        || CARGO_META=
fi

# 取 $COMPILER_ROOT/Cargo.toml 对应那个包的版本；
# 路径对不上 (如符号链接) 时，只有一个包才采用它，否则宁可显示 unknown 也不猜。
get_xiyi_version() {
    local v=
    if [[ -n $CARGO_META ]]; then
        v=$(jq -r --arg m $COMPILER_ROOT/Cargo.toml '
            .packages
            | (map(select(.manifest_path == $m)) | first)
              // (if length == 1 then .[0] else null end)
            | .version // empty
        ' <<< $CARGO_META 2>/dev/null) || v=
    fi
    print -r -- ${v:-unknown}
}
typeset -r XIYI_VERSION=$(get_xiyi_version)

# ============================================================
# 可执行文件：尊重 CARGO_TARGET_DIR / workspace 的 target 目录
# ============================================================
resolve_target_dir() {
    local d=
    [[ -n $CARGO_META ]] && d=$(jq -r '.target_directory // empty' <<< $CARGO_META 2>/dev/null)
    if [[ -z $d ]]; then
        if [[ -n ${CARGO_TARGET_DIR:-} ]]; then
            d=$CARGO_TARGET_DIR
            [[ $d == /* ]] || d=$COMPILER_ROOT/$d
        else
            d=$COMPILER_ROOT/target
        fi
    fi
    print -r -- $d
}

# ============================================================
# 头部
# ============================================================
typeset SHARD_INFO=
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

{
    print -r -- "运行 ID: $RUN_ID"
    print -r -- "版本: $XIYI_VERSION"
    print -r -- "参数: Expect=$EXPECT Jobs=$JOBS SkipBuild=$SKIP_BUILD Release=$RELEASE UpdateGoldens=$UPDATE_GOLDENS FailFast=$FAIL_FAST StrictCrash=$STRICT_CRASH"
    print -r -- "过滤: name=[${(j:; :)NAME_FILTERS}] name-regex=[${(j:; :)NAME_REGEX_FILTERS}] exclude=[${(j:; :)EXCLUDE_FILTERS}] tag=[${(j:; :)TAG_FILTERS}] rerun-failed=$RERUN_FAILED shard=${SHARD_SPEC:-无}"
} >> $LOG_FILE

# ============================================================
# 步骤1: 编译
# ============================================================
if (( ! SKIP_BUILD )); then
    print -r -- "${C_YELLOW}[1/4] 编译 xiyi-compiler...${C_RESET}"
    (( $+commands[cargo] )) || fatal "找不到 cargo (可用 --skip-build 跳过编译)"

    typeset -a build_args=( build -q )
    (( RELEASE )) && build_args+=( --release )

    typeset build_output build_rc
    build_output=$(cd -q -- $COMPILER_ROOT && cargo $build_args 2>&1)
    build_rc=$?
    print -r -- $build_output >> $LOG_FILE

    if (( build_rc != 0 )); then
        print -r -- $build_output
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
typeset PROFILE_NAME=debug
(( RELEASE )) && PROFILE_NAME=release

# cargo metadata 在大 workspace 上要几百毫秒，只算一次
typeset -r TARGET_DIR=$(resolve_target_dir)
typeset -r XIYI_EXE=$TARGET_DIR/$PROFILE_NAME/xiyi
[[ -f $XIYI_EXE && -x $XIYI_EXE ]] || fatal "在 $TARGET_DIR/$PROFILE_NAME 下找不到可执行的 xiyi"
[[ -d $STDLIB ]] || fatal "找不到标准库目录 $STDLIB"

print -r -- "${C_GRAY}使用可执行文件: $XIYI_EXE${C_RESET}"
print

# ============================================================
# 步骤3: 扫描测试
#   **/ 递归、(.) 只要普通文件 (不含符号链接，与 find -type f 一致)、
#   D 包含以 . 开头的目录、N 无匹配不报错。
#   $TEST_DIR 是变量展开的结果，里面的 [ ] * 不会被当成通配符。
#   (o) 按字节序排序：C 区域设置，保证各机器顺序一致，分片才互不重叠。
# ============================================================
print -r -- "${C_YELLOW}[2/4] 扫描测试用例（$TEST_DIR）...${C_RESET}"

typeset -a ALL_TESTS=()
() {
    local LC_COLLATE=C
    ALL_TESTS=( $TEST_DIR/**/*.xiyi(.DN) )
    ALL_TESTS=( ${(o)ALL_TESTS} )
}
(( $#ALL_TESTS )) || fatal "$TEST_DIR 下找不到任何 .xiyi 文件"

# ---- list.json: 只作为"例外覆盖"，优先级见 load_list_json 下方的说明 ----
typeset BASE_EXPECT=PASS
typeset BASE_TIMEOUT=$TIMEOUT_SECONDS

# 每个覆盖项一行，各字段放在并行数组里；OV_INDEX 是 "相对路径 → 下标"。
# 关联数组一律用 += 追加、用 (e) 精确匹配读取：路径里常有 [ ] * ? 等字符，
# 直接写 h[$key] 会被当成下标表达式解析。
typeset -A OV_INDEX=()
typeset -a OV_EXPECT=() OV_TIMEOUT=() OV_STDERR=() OV_TAG=() OV_SKIP=() OV_REASON=()

# 把 "a, b  c" 规范化成 "a b c"
norm_tags() { print -r -- ${(j: :)${(s: :)${1//,/ }}} }

# 把 expect 字符串规范成 PASS / FAIL (FAIL / EXPECT_FAIL 都算 FAIL，大小写不敏感)
expect_word() { [[ ${(U)1} == (FAIL|EXPECT_FAIL) ]] && print FAIL || print PASS }

load_list_json() {
    local clean=$RESULTS_DIR/list.json err v d_expect d_timeout rows row key
    local sep=$'\x1f'
    local -a f

    (( $+commands[jq] )) || fatal "$TEST_LIST_JSON 存在但未找到 jq，无法解析；请安装 jq，或临时移除该文件"

    # 去掉 UTF-8 BOM (Windows 编辑器常带)
    LC_ALL=C sed $'1s/^\xEF\xBB\xBF//' $TEST_LIST_JSON > $clean || fatal "无法读取 $TEST_LIST_JSON"

    # 解析失败必须报错退出；不能静默丢掉全部覆盖项
    err=$(jq empty $clean 2>&1) || fatal "解析 $TEST_LIST_JSON 失败: ${err%%$'\n'*}"
    jq -e 'type == "object"' $clean >/dev/null 2>&1 || fatal "$TEST_LIST_JSON 顶层必须是 JSON 对象"

    v=$(jq -r '.version? // empty' $clean)
    [[ -z $v || $v == 1 ]] || warn "list.json 声明的 version=$v，本脚本只认识 version 1，按 1 处理"

    d_expect=$(jq -r '.defaults.expect? // empty' $clean)
    [[ -n $d_expect ]] && BASE_EXPECT=$(expect_word $d_expect)

    d_timeout=$(jq -r '.defaults.timeout? // empty' $clean)
    if [[ -n $d_timeout ]]; then
        if is_uint $d_timeout && (( 10#$d_timeout >= 1 )); then
            BASE_TIMEOUT=$(( 10#$d_timeout ))
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
    ' $clean) || fatal "读取 $TEST_LIST_JSON 的 tests 失败"

    for row in "${(@f)rows}"; do
        [[ -n $row ]] || continue
        # 用 read 拆分：分隔符不是空白字符时，它会保留空字段 (zsh 的 ${(s)} 在这里做不到)。
        # 末尾的空字段可能被吞掉，所以下面取字段一律带默认值 ${f[n]-}
        IFS=$sep read -rA f <<< $row
        [[ -n ${f[1]-} ]] || continue
        key=${f[1]//\\//}      # list.json 与 ps1 共用：键里的 \ 视为目录分隔符
        key=${key#./}
        if [[ -n ${f[3]-} ]] && ! { is_uint $f[3] && (( 10#$f[3] >= 1 )) }; then
            warn "list.json: $key 的 timeout=\"$f[3]\" 不是 >= 1 的整数，已忽略"
            f[3]=
        fi
        OV_INDEX+=( "$key" $(( $#OV_EXPECT + 1 )) )
        OV_EXPECT+=( "${f[2]-}" ); OV_TIMEOUT+=( "${f[3]-}" ); OV_STDERR+=( "${f[4]-}" )
        OV_TAG+=( "${f[5]-}" );    OV_SKIP+=( "${f[6]-}" );    OV_REASON+=( "${f[7]-}" )
    done
}
[[ -f $TEST_LIST_JSON ]] && load_list_json

# ---- 源码头内联元数据 ----
# 结果写入全局 META_*。只看前 40 行，例如:
#   // @expect fail   // @timeout 5   // @stderr-contains "类型不匹配"
#   // @tag parser, typecheck   // @skip 尚未实现 break
typeset -r RE_EXPECT='^[[:space:]]*//[[:space:]]*@expect[[:space:]]+fail[[:space:]]*$'
typeset -r RE_TIMEOUT='^[[:space:]]*//[[:space:]]*@timeout[[:space:]]+([0-9]{1,9})[[:space:]]*$'
typeset -r RE_STDERR='^[[:space:]]*//[[:space:]]*@stderr-contains[[:space:]]+"([^"]*)"[[:space:]]*$'
typeset -r RE_TAG='^[[:space:]]*//[[:space:]]*@tag[[:space:]]+(.+)$'
typeset -r RE_SKIP='^[[:space:]]*//[[:space:]]*@skip([[:space:]]+(.*))?$'

parse_inline_meta() {
    local -a match mbegin mend
    local file=$1 line
    local -i n=0
    typeset -g META_EXPECT=$2 META_TIMEOUT=$3 META_STDERR= META_TAGS= META_SKIP=0 META_REASON=

    # 文件读不了时静默跳过 (花括号 + 2>/dev/null 是为了吞掉重定向失败的报错)
    { while (( n++ < 40 )) && { IFS= read -r line || [[ -n $line ]] }; do
        line=${line%$'\r'}                        # CRLF 源文件
        (( n == 1 )) && line=${line#$'\xEF\xBB\xBF'}   # UTF-8 BOM
        if   [[ $line =~ $RE_EXPECT  ]]; then META_EXPECT=FAIL
        elif [[ $line =~ $RE_TIMEOUT ]]; then (( 10#$match[1] >= 1 )) && META_TIMEOUT=$(( 10#$match[1] ))
        elif [[ $line =~ $RE_STDERR  ]]; then META_STDERR=$match[1]
        elif [[ $line =~ $RE_TAG     ]]; then META_TAGS=$(norm_tags $match[1])
        elif [[ $line =~ $RE_SKIP    ]]; then META_SKIP=1; META_REASON=${match[2]-}
        fi
    done } < $file 2>/dev/null
    return 0
}

# ---- 组装测试用例 (并行数组，下标即用例编号) ----
typeset -a T_REL=() T_FULL=() T_EXPECT=() T_TIMEOUT=() T_STDERR=() T_TAGS=() T_SKIP=() T_REASON=()
typeset -A REL_SET=()

() {
    local f rel ov
    local -i k
    for f in $ALL_TESTS; do
        rel=${f#$TEST_DIR/}
        REL_SET+=( "$rel" 1 )

        # 优先级(低→高): list.json 的 defaults -> 源码内联 -> list.json 里针对该文件的例外条目
        parse_inline_meta $f $BASE_EXPECT $BASE_TIMEOUT

        k=${OV_INDEX[(e)$rel]-0}
        if (( k > 0 )); then
            [[ -n $OV_EXPECT[k]  ]] && META_EXPECT=$(expect_word $OV_EXPECT[k])
            [[ -n $OV_TIMEOUT[k] ]] && META_TIMEOUT=$OV_TIMEOUT[k]
            [[ -n $OV_STDERR[k]  ]] && META_STDERR=$OV_STDERR[k]
            [[ -n $OV_TAG[k]     ]] && META_TAGS=$(norm_tags $OV_TAG[k])
            if [[ $OV_SKIP[k] == true ]]; then
                META_SKIP=1; META_REASON=$OV_REASON[k]
            fi
        fi

        # 加引号：zsh 里不加引号的空串会在数组赋值时被丢掉，下标就错位了
        T_REL+=( "$rel" );            T_FULL+=( "$f" )
        T_EXPECT+=( "$META_EXPECT" ); T_TIMEOUT+=( "$META_TIMEOUT" )
        T_STDERR+=( "$META_STDERR" ); T_TAGS+=( "$META_TAGS" )
        T_SKIP+=( "$META_SKIP" );     T_REASON+=( "$META_REASON" )
    done

    # list.json 里写了、磁盘上却不存在的条目：几乎必然是拼写 / 移动文件后忘了改，
    # 否则覆盖项会被静默丢弃。
    for rel in ${(ko)OV_INDEX}; do
        (( ${+REL_SET[(e)$rel]} )) || warn "list.json 中的条目在 Tests/ 下找不到对应文件: $rel"
    done
}

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
typeset -A FAILED_SET=()

# 读取上一次完成的运行里失败的用例，填充 FAILED_SET
load_rerun_set() {
    local src_name=${RERUN_SRC:t} check p
    (( $+commands[jq] )) || fatal "--rerun-failed 需要 jq 来读取上一次的 JSON 结果"
    [[ -n $RERUN_SRC ]] || fatal "--rerun-failed: $LOG_DIR 下没有任何历史 JSON 结果，请先完整跑一次"

    # 先确认这确实是本脚本产出的结果文件；损坏或格式不对时要明说，
    # 不能让它悄悄表现成"没有失败的用例"。
    if ! check=$(jq -e '(.Results | type) == "array"' $RERUN_SRC 2>&1); then
        [[ $check == false ]] && check="缺少 Results 数组"
        fatal "--rerun-failed: 无法读取 $src_name: ${check%%$'\n'*}"
    fi

    for p in "${(@f)$(jq -r '.Results[] | select(.Status == "FAIL") | .RelPath // empty' $RERUN_SRC)}"; do
        [[ -n $p ]] || continue
        FAILED_SET+=( "$p" 1 )
        # 用例被改名或删除后，旧记录对不上任何文件：提醒一声，而不是静默丢掉
        (( ${+REL_SET[(e)$p]} )) || warn "上次失败的用例已不存在，将被忽略: $p"
    done

    if (( $#FAILED_SET == 0 )); then
        # 上次全绿不是错误："先跑一遍、再重跑失败项"的用法里，这应当算成功
        print -r -- "${C_GREEN}✅ $src_name 里没有失败的用例，无需重跑${C_RESET}"
        print -r -- "--rerun-failed: $src_name 中没有失败用例，未执行任何测试" >> $LOG_FILE
        exit 0
    fi
    print -r -- "${C_GRAY}[rerun-failed] 从 $src_name 载入 $#FAILED_SET 个失败用例${C_RESET}"
}
(( RERUN_FAILED )) && load_rerun_set

# 用例 $1 是否通过 --name / --name-regex 这一关
passes_name_filter() {
    local -a match mbegin mend
    local rel=$T_REL[$1] pat
    (( $#NAME_FILTERS + $#NAME_REGEX_FILTERS )) || return 0
    # 变量里的通配符默认按字面比较，要写成 ${~var} 才会当模式用 (正则则直接按正则)
    for pat in $NAME_FILTERS;       do [[ $rel == ${~pat} ]] && return 0; done
    for pat in $NAME_REGEX_FILTERS; do [[ $rel =~ $pat ]] && return 0; done
    return 1
}

passes_tag_filter() {
    local want
    (( $#TAG_FILTERS )) || return 0
    # tag 是 "a b c" 形式；两侧补空格后做整词匹配。右侧加引号 = 按字面比较
    for want in $TAG_FILTERS; do [[ " ${(L)T_TAGS[$1]} " == *" $want "* ]] && return 0; done
    return 1
}

is_excluded() {
    local rel=$T_REL[$1] pat
    for pat in $EXCLUDE_FILTERS; do [[ $rel == ${~pat} ]] && return 0; done
    return 1
}

() {
    local -i i
    local want=PASS
    [[ $EXPECT == fail ]] && want=FAIL
    for (( i = 1; i <= $#T_REL; i++ )); do
        [[ $EXPECT == all || $T_EXPECT[i] == $want ]] || continue
        passes_name_filter $i || continue
        passes_tag_filter $i || continue
        (( ! RERUN_FAILED || ${+FAILED_SET[(e)$T_REL[i]]} )) || continue
        is_excluded $i && continue
        FILTERED+=( $i )
    done
}

(( $#FILTERED )) || fatal "过滤后没有匹配的测试用例"

# ---- 分片前的"过滤结果指纹" ----
# --shard 的正确性依赖各机器的过滤结果完全一致。这里对分片前的有序相对路径列表
# (每条路径后跟一个 \n) 求 SHA-256，连同用例数写进 JSON 的 FilterCount / FilterDigest：
# CI 汇总各分片的报告时，这两项不全相同，就说明各机器看到的 Tests/ 不一致
# (浅克隆深度不同、中途有新提交等)，分片可能重叠或漏用例。
# 四个脚本对同一份用例列表算出的值相同，所以 Linux / macOS / Windows 分片混用也能互相比对。
# 只覆盖"选中了哪些用例"，不覆盖用例文件内容的改动。

# 对标准输入求 SHA-256 (小写十六进制)；没有可用工具时返回 1。
sha256_stdin() {
    local out
    if   (( $+commands[shasum] ));    then out=$(shasum -a 256)
    elif (( $+commands[sha256sum] )); then out=$(sha256sum)
    else return 1
    fi
    print -rn -- ${out%% *}
}

typeset -i FILTER_COUNT=$#FILTERED
typeset FILTER_DIGEST
FILTER_DIGEST=$(for i in $FILTERED; do print -r -- $T_REL[i]; done | sha256_stdin) || FILTER_DIGEST=
if (( SHARD_GIVEN )) && [[ -z $FILTER_DIGEST ]]; then
    warn "找不到 shasum / sha256sum，无法生成分片指纹 (JSON 里的 FilterDigest 留空)"
fi

# ---- 分片 ----
# FILTERED 的顺序 = 扫描时按字节序排好的相对路径顺序，与机器、locale 无关，所以各分片划分一致。
# 按位置轮转 (而非按目录切块)，把相邻的、通常耗时相近的用例打散到不同分片，负载更均衡。
if (( SHARD_GIVEN )); then
    () {
        local -i pos before=$#FILTERED
        local -a picked=()
        for (( pos = 1; pos <= before; pos++ )); do
            (( (pos - 1) % SHARD_TOTAL == SHARD_INDEX - 1 )) && picked+=( $FILTERED[pos] )
        done
        FILTERED=( $picked )
        # N 大于用例数时，后面的分片会是空的——这不是错误 (见文末)
        print -r -- "${C_GRAY}[shard $SHARD_INDEX/$SHARD_TOTAL] 过滤后共 $before 个用例，本片 $#FILTERED 个${C_RESET}"
    }
fi

print -r -- "共 $#FILTERED 个测试用例（目录下总计发现 $#T_REL 个）"
(( SHARD_GIVEN )) && print -r -- "${C_GRAY}[shard] 过滤结果指纹: $FILTER_COUNT 个 / ${FILTER_DIGEST:-无} (各分片应完全相同)${C_RESET}"
print

# ============================================================
# 规范化 (用于 golden 比对)，结果放进 $REPLY (不经过子 shell)
#   - 去 NUL、CRLF / 孤立 CR → LF；去开头 BOM (ps1 5.1 写出的 golden 带 BOM)
#   - 去全部尾部空白，再补一个换行
# ============================================================
normalize_file() {
    setopt local_options extended_glob
    local s
    s=$(<$1) 2>/dev/null
    s=${s//$'\0'/}
    s=${s#$'\xEF\xBB\xBF'}
    s=${s//$'\r\n'/$'\n'}
    s=${s//$'\r'/$'\n'}
    REPLY=${s%%[[:space:]]#}$'\n'
}

# 两个文件规范化后是否相同 (== 右侧加引号：按字面比较，不当成通配符)
same_normalized() {
    local a
    normalize_file $1; a=$REPLY
    normalize_file $2
    [[ $a == "$REPLY" ]]
}

# ============================================================
# 单用例执行 (串行 / 并行共用)
#   每个用例一个目录 $RESULTS_DIR/<编号>/，里面有 stdout、stderr 和最终的 res。
#   res 的格式: 第 1 行状态，第 2 行耗时(秒)，第 3 行退出码 (没有则为 -)，之后每行一条原因。
#   先写临时文件再改名：别的进程只要看到 res，内容就一定是完整的。
# ============================================================
write_result() {      # 用法: write_result 编号 状态 耗时 退出码 [原因...]
    local rdir=$RESULTS_DIR/$1 st=$2 el=$3 ec=$4
    shift 4
    print -rl -- $st $el $ec "$@" >| $rdir/res.tmp && mv -f -- $rdir/res.tmp $rdir/res
}

run_case() {
    local -i idx=$1
    local rel=$T_REL[idx] full=$T_FULL[idx] expect=$T_EXPECT[idx]
    local -i timeout_s=$T_TIMEOUT[idx]
    local stderr_contains=$T_STDERR[idx]
    local rdir=$RESULTS_DIR/$idx
    local out_file=$rdir/stdout err_file=$rdir/stderr
    mkdir -p -- $rdir

    if [[ $T_SKIP[idx] == 1 ]]; then
        : > $out_file; : > $err_file
        write_result $idx SKIP 0.000 - "skip${T_REASON[idx]:+: $T_REASON[idx]}"
        return 0
    fi

    # ---- 运行：超时由"看门狗"实现 ----
    # 看门狗是个后台子 shell，睡满时限后留下 timedout 标记并终止用例的进程树。
    # 主流程只管 wait 用例本身：用例一结束就立刻返回，没有轮询延迟；
    # 是否超时看标记，而不是靠退出码去猜 (137 也可能是 OOM killer 的 SIGKILL)。
    # </dev/null：避免用例读 stdin 时挂住终端 / CI 管道。
    local -F t0=$EPOCHREALTIME elapsed
    local -i rc pid wd
    $XIYI_EXE --stdlib $STDLIB $full </dev/null >$out_file 2>$err_file &
    pid=$!
    (
        zselect -t $(( timeout_s * 100 ))
        : > $rdir/timedout
        kill_tree $pid 5
    ) &
    wd=$!
    wait $pid 2>/dev/null
    rc=$?
    elapsed=$(( EPOCHREALTIME - t0 ))
    # 超时：让看门狗把进程树清理干净再走；正常结束：直接撤掉看门狗
    [[ -e $rdir/timedout ]] || kill $wd 2>/dev/null
    wait $wd 2>/dev/null

    local el
    printf -v el '%.3f' $elapsed

    if [[ -e $rdir/timedout ]]; then
        write_result $idx FAIL $el - "timeout ${timeout_s}s"
        return 0
    fi

    if (( UPDATE_GOLDENS )); then
        if cp -- $out_file $full.out && cp -- $err_file $full.err; then
            write_result $idx UPDATED $el $rc "golden 已更新，请 git diff 审查"
        else
            write_result $idx FAIL $el $rc "无法写入 golden: $full.{out,err}"
        fi
        return 0
    fi

    local -i ok=1 have_exit_golden=0
    local expected_exit=
    local -a reasons=()

    # --- 退出码 ---
    if [[ -f $full.exit ]]; then
        have_exit_golden=1
        expected_exit=$(<$full.exit)
        expected_exit=${${expected_exit#$'\xEF\xBB\xBF'}//[[:space:]]/}
        # 只接受 0-255：共享的 golden 在所有平台上含义必须相同
        if is_uint $expected_exit && (( 10#$expected_exit <= 255 )); then
            expected_exit=$(( 10#$expected_exit ))
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
        (( rc != 0 )) || { ok=0; reasons+=( "expected failure but exited 0" ) }
    else
        (( rc == 0 )) || { ok=0; reasons+=( "exit $rc, expected 0" ) }
    fi

    # --- 崩溃识别：被信号终止 (zsh 里表现为 128+信号) / Rust panic / ICE ---
    local crash_what=
    local err_text=$(<$err_file)
    if (( rc > 128 && rc < 192 )); then
        crash_what="被信号 $(( rc - 128 )) 终止"
    elif [[ $err_text == *(panicked at|internal compiler error)* ]]; then
        crash_what="stderr 含 panic/ICE 标志"
    fi

    # --- golden 比对 (有才比) ---
    [[ -f $full.out ]] && ! same_normalized $full.out $out_file && { ok=0; reasons+=( "stdout mismatch" ) }
    [[ -f $full.err ]] && ! same_normalized $full.err $err_file && { ok=0; reasons+=( "stderr mismatch" ) }

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

    local st=FAIL
    (( ok )) && st=PASS
    write_result $idx $st $el $rc "${reasons[@]}"
}

# ============================================================
# 结果打印
# ============================================================
print_result() {
    local -i idx=$1
    local rdir=$RESULTS_DIR/$idx rel=$T_REL[$1]
    local -a rec
    local verdict elapsed reasons color suffix

    # 子任务异常退出 (被 OOM 杀、被信号终止…) 而没留下 res：必须算失败，
    # 绝不能被汇总悄悄漏掉 (否则可能"全部通过"退出 0)。
    if [[ ! -e $rdir/res ]]; then
        mkdir -p -- $rdir
        : >> $rdir/stdout; : >> $rdir/stderr
        write_result $idx FAIL 0.000 - "no result recorded (worker died?)"
    fi

    # "${(@f)...}" 按行拆成数组并保留空行
    rec=( "${(@f)$(<$rdir/res)}" )
    verdict=$rec[1] elapsed=$rec[2]
    reasons=${(j:; :)rec[4,-1]}

    case $verdict in
        (SKIP)    color=$C_GRAY;;
        (UPDATED) color=$C_MAGENTA;;
        (PASS)    color=$C_GREEN;;
        (*)       color=$C_RED;;
    esac
    suffix=${reasons:+ - $reasons}
    print -r -- "${color}[$verdict] $rel (${elapsed}s)${suffix}${C_RESET}"

    if [[ $verdict == FAIL ]]; then
        {
            print -r -- $'\n'"---- $rel ----"
            print -r -- "reasons: $reasons"
            print -r -- "stdout:"
            print_head $rdir/stdout
            print -r -- "stderr:"
            print_head $rdir/stderr
        } >> $LOG_FILE
    fi
}

# 最多输出文件开头的 4096 字节；超出则注明已截断
print_head() {
    local -i size
    LC_ALL=C head -c 4096 $1 2>/dev/null
    size=$(LC_ALL=C wc -c < $1 2>/dev/null) || size=0
    (( size > 4096 )) && print -r -- $'\n...(截断)'
    print
}

# ============================================================
# 步骤4: 执行
# ============================================================
print -r -- "${C_YELLOW}[3/4] 执行测试...${C_RESET}"
print

# 并行：启动 JOBS 个常驻 worker，各自按顺序"认领"尚未被认领的用例。
# 认领靠 mkdir (原子操作：只有一个进程能成功)，所以不需要 wait -n 或令牌管道；
# 某个 worker 意外死亡时，最多丢它手里那一个用例 (记为失败)，不会拖死整个套件。
worker() {
    local -i i
    for i in $FILTERED; do
        mkdir $RESULTS_DIR/claim/$i 2>/dev/null || continue
        run_case $i
    done
}

typeset -a WORKER_PIDS=()
workers_alive() {
    local p
    for p in $WORKER_PIDS; do kill -0 $p 2>/dev/null && return 0; done
    return 1
}

if (( JOBS > 1 )); then
    (( FAIL_FAST )) && warn "--fail-fast 在并行模式下不生效"
    mkdir -p -- $RESULTS_DIR/claim
    repeat $(( JOBS < $#FILTERED ? JOBS : $#FILTERED )); do
        worker &
        WORKER_PIDS+=( $! )
    done
    DISPATCHED=$#FILTERED
    # 按用例顺序、边完成边打印 (某个用例慢，只会推迟它后面的打印，不影响并行度)
    for i in $FILTERED; do
        until [[ -e $RESULTS_DIR/$i/res ]]; do
            workers_alive || break
            zselect -t 2
        done
        print_result $i
    done
    wait
else
    for i in $FILTERED; do
        DISPATCHED+=1
        run_case $i
        print_result $i
        if (( FAIL_FAST )) && [[ $(head -n 1 $RESULTS_DIR/$i/res) == FAIL ]]; then
            print -r -- "${C_YELLOW}⏹  --fail-fast 触发，停止后续测试${C_RESET}"
            break
        fi
    done
fi
print

# ============================================================
# 步骤5: 汇总
#   只统计本次实际执行的用例 (串行 + --fail-fast 时 DISPATCHED < 过滤后总数)
# ============================================================
print -r -- "${C_YELLOW}[4/4] 汇总结果...${C_RESET}"

typeset -a EXECUTED=( ${FILTERED[1,DISPATCHED]} )
typeset -i total=0 passed=0 failed=0 timeouts=0 skipped=0 updated=0
() {
    local -i i
    local st
    for i in $EXECUTED; do
        st=$(head -n 1 $RESULTS_DIR/$i/res)
        total+=1
        case $st in
            (PASS)    passed+=1;;
            (SKIP)    skipped+=1;;
            (UPDATED) updated+=1;;
            (*)       failed+=1
                      [[ -e $RESULTS_DIR/$i/timedout ]] && timeouts+=1;;
        esac
    done
}

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
json_escape() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    s=${s//$'\n'/\\n}
    s=${s//$'\r'/\\r}
    s=${s//$'\t'/\\t}
    s=${s//$'\b'/\\b}
    s=${s//$'\f'/\\f}
    s=${s//[[:cntrl:]]/}      # 其余控制字符 (如 ANSI ESC) 在 JSON 里非法，直接丢弃
    print -rn -- $s
}

write_json() {
    local -i i first=1 first_r
    local -a rec reasons
    local r verdict ok sk up ec el
    print -r -- '{'
    print -r -- "  \"RunId\": \"$(json_escape $RUN_ID)\","
    print -r -- "  \"Version\": \"$(json_escape $XIYI_VERSION)\","
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
    for i in $EXECUTED; do
        rec=( "${(@f)$(<$RESULTS_DIR/$i/res)}" )
        verdict=$rec[1] el=$rec[2] ec=$rec[3] reasons=( ${rec[4,-1]} ) first_r=1
        ok=false sk=false up=false
        [[ $verdict == (PASS|SKIP|UPDATED) ]] && ok=true
        [[ $verdict == SKIP ]] && sk=true
        [[ $verdict == UPDATED ]] && up=true
        [[ $ec == <-> ]] || ec=null

        (( first )) || print -rn -- ','
        first=0
        print -rn -- $'\n'"    {\"RelPath\":\"$(json_escape $T_REL[i])\",\"Status\":\"$verdict\","
        print -rn -- "\"Ok\":$ok,\"Skipped\":$sk,\"Updated\":$up,\"Elapsed\":$el,\"ExitCode\":$ec,\"Reasons\":["
        for r in $reasons; do
            (( first_r )) || print -rn -- ','
            first_r=0
            print -rn -- "\"$(json_escape $r)\""
        done
        print -rn -- ']}'
    done
    print -r -- $'\n  ]'
    print -r -- '}'
}

json_tmp=$JSON_FILE.tmp.$$
if ! { write_json > $json_tmp && mv -f -- $json_tmp $JSON_FILE }; then
    rm -f -- $json_tmp
    warn "写入 $JSON_FILE 失败"
fi

print -r -- "汇总: 总计 $total, 通过 $passed, 失败 $failed, 超时 $timeouts, 跳过 $skipped, 已更新 $updated" >> $LOG_FILE

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
