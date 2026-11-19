#!/bin/sh
set -eu

workspace=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$workspace"

cargo_bin=${CARGO:-cargo}
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
if [ -z "$version" ]; then
    echo "package-macos: workspace version unavailable" >&2
    exit 1
fi

output_root=${1:-"$workspace/dist"}
bundle="$output_root/Prism Studio.app"
contents="$bundle/Contents"
macos="$contents/MacOS"
resources="$contents/Resources"
archive="$output_root/Prism-Studio-$version-macos.zip"
icon_work=$(mktemp -d "${TMPDIR:-/tmp}/prism-icon.XXXXXX")
trap 'rm -rf "$icon_work"' EXIT HUP INT TERM

"$cargo_bin" build --release --locked -p prism-studio -p prism-cli

rm -rf "$bundle"
mkdir -p "$macos" "$resources/bin" "$resources/brand" "$resources/licenses"
cp target/release/prism-studio "$macos/Prism Studio"
cp target/release/prism-cli "$resources/bin/prism-cli"
cp assets/brand/prism-icon.svg "$resources/brand/prism-icon.svg"
iconset="$icon_work/PrismStudio.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    double=$((size * 2))
    sips -z "$size" "$size" assets/brand/prism-icon-1024.png \
        --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z "$double" "$double" assets/brand/prism-icon-1024.png \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
"$cargo_bin" run --release --locked -p prism-icns-packager -- \
    "$resources/PrismStudio.icns" \
    "icp4=$iconset/icon_16x16.png" \
    "icp5=$iconset/icon_32x32.png" \
    "ic07=$iconset/icon_128x128.png" \
    "ic08=$iconset/icon_256x256.png" \
    "ic09=$iconset/icon_512x512.png" \
    "ic10=$iconset/icon_512x512@2x.png"
cp LICENSE "$resources/licenses/Apache-2.0.txt"
"$cargo_bin" run --release --locked -p prism-license-inventory -- \
    "$resources/licenses/THIRD_PARTY_NOTICES.txt"
"$cargo_bin" metadata --locked --format-version 1 > "$resources/dependency-metadata.json"
source_revision=$(git rev-parse HEAD 2>/dev/null || echo unknown)
source_dirty=false
if [ -n "$(git status --porcelain --untracked-files=normal 2>/dev/null)" ]; then
    source_dirty=true
fi
rustc_bin=${RUSTC:-"$(dirname "$cargo_bin")/rustc"}
if [ ! -x "$rustc_bin" ]; then rustc_bin=rustc; fi
rustc_version=$($rustc_bin --version)
{
    echo "product=Prism Studio"
    echo "version=$version"
    echo "source_revision=$source_revision"
    echo "source_dirty=$source_dirty"
    echo "rustc=$rustc_version"
    echo "target=$(uname -s)-$(uname -m)"
} > "$resources/RELEASE_BUILD.txt"

if [ -n "${PRISM_CODEC_DIR:-}" ]; then
    for tool in ffmpeg ffprobe ffplay; do
        source_tool="$PRISM_CODEC_DIR/$tool"
        if [ ! -x "$source_tool" ]; then
            echo "package-macos: missing executable audited codec tool $source_tool" >&2
            exit 1
        fi
        cp "$source_tool" "$resources/bin/$tool"
    done
fi

sed \
    -e "s/@VERSION@/$version/g" \
    packaging/macos/Info.plist.in > "$contents/Info.plist"

if command -v codesign >/dev/null 2>&1; then
    identity=${PRISM_CODESIGN_IDENTITY:--}
    codesign --force --deep --options runtime --sign "$identity" "$bundle"
fi

rm -f "$archive"
ditto -c -k --sequesterRsrc --keepParent "$bundle" "$archive"
(cd "$output_root" && shasum -a 256 "$(basename "$archive")" > "$(basename "$archive").sha256")

echo "package-macos: created $bundle"
echo "package-macos: created $archive"
echo "package-macos: created $archive.sha256"
