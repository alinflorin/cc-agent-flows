#!/usr/bin/env bash
# Builds QEMU from source and lays it out as <out>/{bin,lib,share/qemu}, ready to ship as a Tauri
# resource. Runs natively on Linux, macOS and Windows (from an MSYS2 MINGW64 shell).
#
#   build.sh <version> <sha256> <out-dir>
set -euo pipefail

version=$1
sha256=$2
out=$3

case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  MINGW* | MSYS*) os=windows ;;
  *) echo "unsupported OS $(uname -s)" >&2; exit 1 ;;
esac

work=$(mktemp -d)
mkdir -p "$out"
out=$(cd "$out" && pwd)
stage="$work/stage"
jobs=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)

curl -fsSL -o "$work/qemu.tar.xz" "https://download.qemu.org/qemu-$version.tar.xz"
if command -v sha256sum >/dev/null; then
  echo "$sha256  $work/qemu.tar.xz" | sha256sum -c -
else
  echo "$sha256  $work/qemu.tar.xz" | shasum -a 256 -c -
fi
# roms/ (firmware sources) and the CI fixtures are not needed to build, and their symlinks cannot
# be created under MSYS2. Any symlinks that still fail there belong to disabled features, so
# tolerate tar's exit status and check the tree instead.
tar -xJf "$work/qemu.tar.xz" -C "$work" --exclude="qemu-$version/roms" --exclude="qemu-$version/tests/lcitool/libvirt-ci" || [ "$os" = windows ]
[ -f "$work/qemu-$version/configure" ]

# Headless on purpose: guests are driven through VNC/websocket or a serial console, so no
# GUI toolkit (SDL/GTK) has to be bundled. Everything not listed here stays off.
# Windows (MSYS2 dropped dtc) and macOS (Homebrew's libfdt is outside the default search path)
# build dtc from QEMU's own subproject wrap, which needs git.
accel=()
fdt=system
case "$os" in
  linux) accel=(--enable-kvm --enable-seccomp) ;;
  macos) accel=(--enable-hvf); fdt=internal ;;
  windows) accel=(--enable-whpx); fdt=internal ;;
esac

mkdir "$work/build"
cd "$work/build"
"$work/qemu-$version/configure" \
  --prefix="$stage" \
  --target-list=x86_64-softmmu,aarch64-softmmu \
  --without-default-features \
  --enable-system --enable-tools --enable-pixman --enable-fdt=$fdt --enable-slirp --enable-vnc \
  --disable-werror --disable-docs \
  "${accel[@]}"
# The mingw32-make that comes with the toolchain reports D:/... paths, which QEMU's Makefile
# rejects for containing a colon. MSYS's own make sees /tmp/... instead.
make=make
[ "$os" = windows ] && make=/usr/bin/make
"$make" -j"$jobs"
"$make" install

exe=""
[ "$os" = windows ] && exe=".exe"

mkdir -p "$out/bin" "$out/lib" "$out/share"
for tool in qemu-system-x86_64 qemu-system-aarch64 qemu-img; do
  f=$(find "$stage" -name "$tool$exe" -type f | head -n1)
  cp "$f" "$out/bin/"
done
# Windows installs firmware next to the executables, the others under share/qemu.
firmware=$(dirname "$(find "$stage" -name bios-256k.bin | head -n1)")
cp -R "$firmware" "$out/share/qemu"

case "$os" in
  linux)
    # Ship everything except the libc family, which has to match the host.
    skip='^(linux-vdso|ld-linux|libc|libm|libdl|libpthread|librt|libresolv|libutil|libgcc_s)[.-]'
    for bin in "$out"/bin/*; do
      ldd "$bin" | awk '/=> \//{print $3}' | while read -r lib; do
        name=$(basename "$lib")
        [[ $name =~ $skip ]] || cp -n "$lib" "$out/lib/"
      done
      patchelf --set-rpath '$ORIGIN/../lib' "$bin"
    done
    for lib in "$out"/lib/*; do patchelf --set-rpath '$ORIGIN' "$lib"; done
    strip "$out"/bin/* || true
    ;;
  macos)
    cat > "$work/hvf.entitlements" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>com.apple.security.hypervisor</key><true/>
</dict></plist>
PLIST
    for bin in "$out"/bin/*; do
      dylibbundler -of -b -x "$bin" -d "$out/lib" -p '@executable_path/../lib/' >/dev/null
    done
    # Ad-hoc signatures: rewriting load paths invalidates the originals, and arm64 refuses unsigned code.
    for lib in "$out"/lib/*; do codesign -f -s - "$lib"; done
    for bin in "$out"/bin/*; do codesign -f -s - --entitlements "$work/hvf.entitlements" "$bin"; done
    ;;
  windows)
    # QEMU installs the DLLs it builds itself (libslirp) beside the executables.
    cp "$stage"/*.dll "$out/bin/"
    for bin in "$out"/bin/*.exe; do
      ldd "$bin" | awk '/=> \/mingw64\//{print $3}' | while read -r dll; do
        cp -n "$dll" "$out/bin/"
      done
    done
    # Two passes: copied DLLs have dependencies of their own.
    for dll in "$out"/bin/*.dll; do
      ldd "$dll" | awk '/=> \/mingw64\//{print $3}' | while read -r dep; do
        cp -n "$dep" "$out/bin/"
      done
    done
    rmdir "$out/lib"
    ;;
esac

# Smoke test: the layout has to run from where it was assembled.
"$out/bin/qemu-system-x86_64$exe" --version
"$out/bin/qemu-system-aarch64$exe" --version
"$out/bin/qemu-img$exe" --version
# The seccomp sandbox (-sandbox) must be compiled in on Linux. A machine with no devices stays
# idle, so timing out (124) means QEMU started under the filter; anything else is a failure.
if [ "$os" = linux ]; then
  set +e
  timeout 3 "$out/bin/qemu-system-x86_64" -L "$out/share/qemu" -sandbox on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny -machine none -display none -monitor none -nodefaults -S
  rc=$?
  set -e
  [ $rc = 124 ] || { echo "-sandbox did not start (exit $rc)" >&2; exit 1; }
fi
