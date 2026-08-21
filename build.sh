#!/usr/bin/env bash
# WB 构建环境（Git Bash 下 `source build.sh` 使用）
# Rust GNU 工具链（rustup 用户目录）+ 仓库内 winlibs MinGW（C 链接器/dlltool/as）

USER_HOME="$(cygpath -u "${USERPROFILE:-$HOME}")"
export RUSTUP_HOME="$USER_HOME/.rustup"
export CARGO_HOME="$USER_HOME/.cargo"
WB_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PATH="$WB_ROOT/.toolchain/mingw64/bin:$CARGO_HOME/bin:$PATH"

# 网络异常时取消注释（走本地代理）：
# export http_proxy=http://127.0.0.1:7890 https_proxy=http://127.0.0.1:7890

echo "WB build env ready: rustc $(rustc --version 2>/dev/null | cut -d' ' -f2), gcc $(gcc --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"
