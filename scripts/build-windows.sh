#!/usr/bin/env bash
# Cross-compiles Ghostcaret for Windows x64 from Linux and packages it.
#
# Output (in $GHOSTCARET_WIN_OUT):
#   Ghostcaret_<version>_x64-setup.exe        NSIS installer (per-user, no admin)
#   ghostcaret-<version>-windows-x64.zip      portable: exe + DLLs, unzip and run
#   SHA256SUMS
#
# llama.cpp is built with GGML_BACKEND_DL + GGML_CPU_ALL_VARIANTS: llama.dll,
# ggml.dll and ggml-base.dll plus nine ggml-cpu-*.dll variants (SSE4.2 up to
# AVX-512/AMX/AVX-VNNI) ship beside the exe; the best variant for the processor
# is picked at run time, so one build runs at full speed on any x64 CPU. The C
# runtime is linked statically (.cargo/config.toml, LLAMA_STATIC_CRT), so no Visual C++
# Redistributable is needed. WebView2 is bootstrapped by the installer when
# missing (preinstalled on Windows 11).
#
# Host requirements (user-local installs are fine):
#   rustup target add x86_64-pc-windows-msvc
#   cargo install --locked cargo-xwin       (downloads the MSVC CRT + SDK once)
#   clang-cl, lld, llvm (llvm-rc, llvm-objdump), cmake, ninja, bun, zip
#   makensis 3.11+ on PATH for the installer (without it only the zip is made).
#     Needs NSIS's data directory (Stubs, Include, Contrib, Plugins). The Tauri
#     bundler does not pass the environment on, so makensis must find the data
#     itself: distro `nsis`, or a user-local build, e.g.
#       scons SKIPSTUBS=all SKIPPLUGINS=all SKIPUTILS=all SKIPMISC=all \
#             NSIS_CONFIG_CONST_DATA_PATH=no makensis         (in nsis-3.11-src)
#       cp build/urelease/makensis/makensis ~/.local/opt/nsis/bin/
#       # Stubs/Include/Contrib/Plugins from tauri-apps/binary-releases nsis-3.11.zip
#       # into ~/.local/opt/nsis/, and a ~/.local/bin/makensis wrapper script:
#       #   NSISDIR=$HOME/.local/opt/nsis exec $HOME/.local/opt/nsis/bin/makensis "$@"
#
# build.windows.staticVCRuntime is false in app/tauri.conf.json: Tauri's default
# (static vcruntime + dynamic UCRT) conflicts with +crt-static and fails to link.
#
# Environment (all optional):
#   CARGO_TARGET_DIR      cargo output directory      (default: <repo>/target/windows-app)
#   GHOSTCARET_WIN_OUT    artifact directory          (default: $CARGO_TARGET_DIR/dist)
#   XWIN_CACHE_DIR        where cargo-xwin caches the MSVC CRT/SDK
#   SKIP_INSTALLER=1      build only the portable zip
set -euo pipefail

TARGET=x86_64-pc-windows-msvc
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$ROOT/target/windows-app}
OUT=${GHOSTCARET_WIN_OUT:-$CARGO_TARGET_DIR/dist}
# Fixes ggml's per-CPU-variant DLLs under clang-cl; see the file's header.
export CMAKE_PROJECT_INCLUDE=$ROOT/build/windows/ggml-clang-cl.cmake
# The llama.cpp and ggml DLLs carry their own static C and C++ runtime, like the
# exe, so nothing needs the Visual C++ Redistributable.
export LLAMA_STATIC_CRT=1 CMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded
REL=$CARGO_TARGET_DIR/$TARGET/release
STAGE=$CARGO_TARGET_DIR/stage

need() { command -v "$1" >/dev/null || { echo "missing tool: $1" >&2; exit 1; }; }
for tool in cargo cargo-xwin bun clang-cl cmake ninja llvm-objdump zip; do need "$tool"; done
rustup target list --installed | grep -qx "$TARGET" \
    || { echo "run: rustup target add $TARGET" >&2; exit 1; }
installer=yes
if [ "${SKIP_INSTALLER:-}" = 1 ] || ! command -v makensis >/dev/null; then
    installer=no
    echo "note: no installer (set up makensis and unset SKIP_INSTALLER for one)" >&2
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)

# 1. The app. `tauri build` builds the UI (beforeBuildCommand) and compiles with
#    the custom-protocol feature, which makes the exe serve the bundled UI.
cd "$ROOT/app"
bun install --frozen-lockfile
bun run tauri build --no-bundle --runner cargo-xwin --target "$TARGET" \
    --features dynamic-backends

# 2. llama.cpp DLLs. The exe embeds the path of the backend directory of the
#    llama.cpp build it was linked against (a development fallback); that picks
#    the right one when stale builds from other feature sets sit in the target dir.
backends=$(grep -a -o -m1 "$REL/build/llama-cpp-sys-2-[0-9a-f]*/out/backends" \
    "$REL/open-flowtype.exe" | head -n1 || true)
if [ ! -d "${backends:-}" ]; then
    backends=$(find "$REL/build" -type d -path '*/llama-cpp-sys-2-*/out/backends' \
        -printf '%T@ %p\n' | sort -rn | head -n1 | cut -d' ' -f2-)
fi
llama_out=$(dirname "${backends:?llama.cpp build output not found}")
rm -rf "$STAGE" && mkdir -p "$STAGE"
cp "$llama_out"/bin/*.dll "$llama_out"/backends/ggml-cpu-*.dll "$STAGE"/
echo "staged $(ls "$STAGE" | wc -l) DLLs from $llama_out"

# The bundler stamps the exe it packages, so take the portable copy first.
rm -rf "$OUT" && mkdir -p "$OUT/portable"
cp "$REL/open-flowtype.exe" "$OUT/portable/Ghostcaret.exe"
cp "$STAGE"/*.dll "$OUT/portable/"

# 3. The installer ships the DLLs beside the exe.
if [ "$installer" = yes ]; then
    bun run tauri bundle --target "$TARGET" --features dynamic-backends --bundles nsis \
        --config "{\"bundle\":{\"resources\":{\"$STAGE/*.dll\":\"./\"}}}"
fi

# 4. Collect.
(cd "$OUT/portable" && zip -9 -q -r "../ghostcaret-$VERSION-windows-x64.zip" .)
if [ "$installer" = yes ]; then
    cp "$REL"/bundle/nsis/*-setup.exe "$OUT/"
fi

# 5. Import check: every DLL any shipped binary imports must be shipped or a
#    Windows system DLL; the C++/C runtimes must not be imported at all.
imports() { llvm-objdump -p "$1" | sed -n 's/^[[:space:]]*DLL Name: //p' | tr 'A-Z' 'a-z' | sort -u; }
shipped=$(cd "$OUT/portable" && ls | tr 'A-Z' 'a-z')
system='advapi32.dll bcrypt.dll bcryptprimitives.dll combase.dll comctl32.dll comdlg32.dll crypt32.dll dbghelp.dll dwmapi.dll
 gdi32.dll imm32.dll kernel32.dll msvcrt.dll ncrypt.dll netapi32.dll ntdll.dll ole32.dll
 oleaut32.dll powrprof.dll propsys.dll psapi.dll secur32.dll setupapi.dll shcore.dll shell32.dll
 shlwapi.dll uiautomationcore.dll user32.dll userenv.dll uxtheme.dll version.dll winmm.dll
 winspool.drv ws2_32.dll wtsapi32.dll'
bad=0
for f in "$OUT"/portable/*.exe "$OUT"/portable/*.dll; do
    for dll in $(imports "$f"); do
        case $dll in
            vcruntime*|msvcp*|concrt*|ucrtbase.dll|api-ms-win-crt-*)
                echo "FORBIDDEN  $(basename "$f") -> $dll"; bad=1 ;;
            api-ms-win-*|ext-ms-win-*) ;;
            *) if ! grep -qx "$dll" <<<"$shipped" && ! grep -qw -- "$dll" <<<"$system"; then
                   echo "UNKNOWN    $(basename "$f") -> $dll"; bad=1
               fi ;;
        esac
    done
done
[ "$bad" = 0 ] || { echo "import check failed" >&2; exit 1; }
echo "import check: ok"

( cd "$OUT" && sha256sum ./*.zip ./*.exe 2>/dev/null | tee SHA256SUMS )
ls -l "$OUT"
