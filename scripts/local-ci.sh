#!/usr/bin/env bash
# 本地 CI：发版前在本机把 GitHub Actions 会做的检查全部跑一遍，全绿再打 tag。
#
# 用法（Git Bash / 任意 POSIX sh）:
#   bash scripts/local-ci.sh
#
# 与 AutoUploadReleaseBuild.yml 的对应关系:
#   1. fmt                     —— 代码格式
#   2. clippy                  —— 静态检查（比 CI 更严的一步，CI 没有，白送）
#   3. cargo test --release    —— 对应 Linux job 的 Test 步（在本机 Windows 上跑；
#                                 用例本体是跨平台的，平台相关的坑另有 zigbuild 兜着）
#   4. rustup target add ...   —— Linux 目标 std（第一次需要联网，之后秒过）
#   5. cargo zigbuild ...2.17  —— 对应 Linux job 的 Build 步：同一条命令、同一个
#                                 glibc 2.17 基线，产出交付用的 .so，同时兜住
#                                 「无默认 features 编不过」这类平台差分（v1.6.1 那次
#                                 框架的事故就是它防的）
#   6. cargo build --release   —— 对应 Windows job 的 Build 步，产出交付用的 .dll
#
# 本机替代不了 CI 的只有一件事：**在真 Linux 上跑测试的运行期**
# （字体、调度这些环境差异）。所以纪律是——本地全绿 → 再 tag → CI 仍会兜底复验。
set -euo pipefail
cd "$(dirname "$0")/.."

banner() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }

banner "1/7 fmt"
cargo fmt --all -- --check

banner "2/7 clippy"
cargo clippy --all-targets --release

banner "3/7 test --release（本机跑全部用例）"
cargo test --release

banner "4/7 rustup target add x86_64-unknown-linux-gnu"
rustup target add x86_64-unknown-linux-gnu

banner "5/7 cargo zigbuild（Linux .so，glibc 2.17 基线，与 CI 同一条命令）"
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17

banner "6/7 cargo build --release（Windows .dll）"
cargo build --release

banner "7/7 Linux 容器测试（真 Linux 运行期，Docker）"
# 前六步覆盖不了的最后一块：在真正的 Linux 用户态里跑测试运行期（字体、调度、
# 文件系统差异都在这一档）。容器挂宿主整个 rustprojects：插件对框架是
# path 依赖（../Arona-rs），只挂插件仓库会解不出来。CARGO_TARGET_DIR 指到容器
# 内部，Linux 产物不污染 Windows 的 target/；cargo 缓存放命名卷，二次跑很快。
if docker version >/dev/null 2>&1; then
    docker run --rm \
        -v "$(cd .. && pwd -W 2>/dev/null || cd .. && pwd)":/ws \
        -w /ws/bilibili-dynamic-arona-plugin \
        -e CARGO_TARGET_DIR=/tmp/target \
        -v arona-bili-cargo:/usr/local/cargo \
        rust:latest cargo test --release
else
    echo "!! Docker 未运行，跳过 Linux 容器测试 —— GitHub CI 仍会兜底复验"
fi

printf '\n\033[1;32m本地 CI 全绿 —— 可以打 tag 发版了。\033[0m\n'
