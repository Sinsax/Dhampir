#!/usr/bin/env bash
#
# scripts/release-local.sh —— Dhampir 单机双端发版
# （与下游宿主 YekiTrim 的 scripts/release-local.sh 同一套思路：那边是"宿主程序"，这边是"底座"。）
#
#   Linux  产物：native 二进制在 docker 容器（docker/Dockerfile.linux-x64，glibc 2.39 基线）里编；
#                wasm 那半与 glibc 无关，宿主机上由 package.mjs 出（wasm-pack）。
#                ⚠️ 容器里用**独立的 target 目录**（target-docker）：宿主与本容器的 rustc 同为
#                1.97.0 时指纹可复用，共享 target 会让容器"编译零动作"、把**宿主链接**的
#                二进制原样留下 —— glibc 门禁会拒收它，但正确路径不该靠门禁兜底。
#                滚动发行版（Arch，glibc 2.44）直编的 ELF 要求 GLIBC_2.43，在目标发行版
#                全都起不来 —— package.mjs 的 MAX_GLIBC 门禁是第二道保险。
#   Windows 产物：cargo-xwin 交叉编译（lld-link + 微软官方 SDK/CRT，由 xwin 拉取）。
#                依赖全是纯 Rust（png / gif / image-webp / wgpu），没有 C 工具链那一摊，
#                所以不需要 YekiTrim 那套 clang-cl / TARGET_CPU=haswell 的配套；
#                crt-static 与下游宿主同口径（产物自带 CRT，不挑用户机器）。
#
# 用法：
#   scripts/release-local.sh              # 两端都出
#   scripts/release-local.sh linux        # 只出 Linux
#   scripts/release-local.sh windows      # 只出 Windows
#   DHAMPIR_XWIN_CACHE=<已有缓存> scripts/release-local.sh windows   # 复用宿主上已有的 xwin SDK 缓存
#
# 产物落在 dist/（gitignored）：dhampir-<版本>+<sha>/ 与 dhampir-<版本>-<平台>.zip。
# 下游宿主（YekiTrim）把它挂进自己的打包：cargo xtask package --dhampir-artifact <dist 产物目录>。

set -euo pipefail

# wasm-pack / cargo-xwin 常装在 ~/.cargo/bin —— 不在 PATH 的最小环境里
# （cron / 别的 shell 里跑）就是 spawnSync ENOENT，报错还很误导（指向 wasm 段）。
export PATH="$HOME/.cargo/bin:$PATH"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$HERE")"
cd "$ROOT"

TARGETS="${*:-linux windows}"
echo "==> 发版目标: $TARGETS"

want() { case " $TARGETS " in *" $1 "*) return 0;; *) return 1;; esac; }

# ---------- Linux（容器基线；native 半） ----------
if want linux; then
  echo "==> [linux] 构建镜像（已存在则跳过）"
  docker image inspect dhampir-linux-x64-builder >/dev/null 2>&1 \
    || docker build -f docker/Dockerfile.linux-x64 -t dhampir-linux-x64-builder .

  echo "==> [linux] 容器内编译 native 二进制（独立 target 目录，见文件头说明）"
  docker run --rm --network host \
    -v "$ROOT:/src" \
    -e CARGO_TARGET_DIR=/src/target-docker \
    -w /src dhampir-linux-x64-builder \
    cargo build --release --bin dhampir --locked

  echo "==> [linux] 打包（wasm 半在宿主机出；glibc 门禁会实测产物）"
  node scripts/package.mjs --bin target-docker/release/dhampir --platform linux-x64
fi

# ---------- Windows（cargo-xwin 交叉编译，宿主机直跑） ----------
if want windows; then
  command -v lld-link >/dev/null || { echo "缺 lld-link（Arch: pacman -S lld）"; exit 1; }
  rustup target list --installed | grep -q x86_64-pc-windows-msvc || rustup target add x86_64-pc-windows-msvc

  export XWIN_CACHE_DIR="${DHAMPIR_XWIN_CACHE:-$ROOT/.workbuddy/xwin-cache}"
  export RUSTFLAGS="-C target-feature=+crt-static"

  echo "==> [windows] 交叉编译（crt-static；依赖全纯 Rust，无需 clang-cl / CPU 级选项）"
  # xwin 解包出的 SDK 库有大小写差异（pathcch.lib vs PathCch.lib），链接器按请求名找
  # ⇒ 首次链接前建一批规范大小写软链（幂等；windows-rs 的 raw-dylib 大多用不到，
  #   但留着不亏 —— 与 YekiTrim 同一条防御）。
  UM="$XWIN_CACHE_DIR/xwin/sdk/lib/um/x86_64"
  if [ -d "$UM" ]; then
    for n in PathCch Shlwapi Shell32 Ole32 OleAut32 Crypt32 Dwmapi Gdi32 User32 UxTheme Winmm Ws2_32 Shcore Comctl32 Comdlg32 Secur32 Webservices WindowsCodecs Dbghelp Setupapi Wintrust Iphlpapi Dnsapi Powrprof Userenv WtsApi32 Netapi32 Version Wininet Urlmon Msi RuntimeObject Bcrypt Ntdll Kernel32 Advapi32 Rpcrt4; do
      [ -e "$UM/$n.lib" ] || { src=$(ls "$UM" 2>/dev/null | grep -iE "^$n\.lib$" | head -1); [ -n "$src" ] && ln -sf "$UM/$src" "$UM/$n.lib"; }
    done
  fi
  cargo xwin build --release --target x86_64-pc-windows-msvc --bin dhampir --locked

  echo "==> [windows] 打包"
  node scripts/package.mjs --bin target/x86_64-pc-windows-msvc/release/dhampir.exe --platform windows-x64
fi

echo "==> 完成。产物："
ls -lh dist/dhampir-*.zip 2>/dev/null | tail -4 || true
