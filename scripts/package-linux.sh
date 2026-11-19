#!/bin/sh
set -eu

workspace=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$workspace"

if [ "$(uname -s)" != "Linux" ]; then
    echo "package-linux: this package must be built on Linux" >&2
    exit 1
fi

cargo_bin=${CARGO:-cargo}
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
architecture=$(uname -m)
output_root=${1:-"$workspace/dist"}
package_name="Prism-Studio-$version-linux-$architecture"
package_root="$output_root/$package_name"
archive="$output_root/$package_name.tar.gz"

if [ -z "$version" ]; then
    echo "package-linux: workspace version unavailable" >&2
    exit 1
fi

"$cargo_bin" build --release --locked -p prism-studio -p prism-cli

rm -rf "$package_root"
mkdir -p \
    "$package_root/bin" \
    "$package_root/share/applications" \
    "$package_root/share/icons/hicolor/1024x1024/apps" \
    "$package_root/share/mime/packages" \
    "$package_root/share/licenses/prism-studio"
cp target/release/prism-studio "$package_root/bin/prism-studio"
cp target/release/prism-cli "$package_root/bin/prism-cli"
cp packaging/linux/com.prism-editor.studio.desktop \
    "$package_root/share/applications/com.prism-editor.studio.desktop"
cp packaging/linux/application-x-prism-project.xml \
    "$package_root/share/mime/packages/application-x-prism-project.xml"
cp assets/brand/prism-icon-1024.png \
    "$package_root/share/icons/hicolor/1024x1024/apps/com.prism-editor.studio.png"
cp LICENSE "$package_root/share/licenses/prism-studio/Apache-2.0.txt"
"$cargo_bin" run --release --locked -p prism-license-inventory -- \
    "$package_root/share/licenses/prism-studio/THIRD_PARTY_NOTICES.txt"
"$cargo_bin" metadata --locked --format-version 1 \
    > "$package_root/share/licenses/prism-studio/dependency-metadata.json"
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
    echo "target=$(uname -s)-$architecture"
} > "$package_root/RELEASE_BUILD.txt"

if [ -n "${PRISM_CODEC_DIR:-}" ]; then
    for tool in ffmpeg ffprobe ffplay; do
        source_tool="$PRISM_CODEC_DIR/$tool"
        if [ ! -x "$source_tool" ]; then
            echo "package-linux: missing executable audited codec tool $source_tool" >&2
            exit 1
        fi
        cp "$source_tool" "$package_root/bin/$tool"
    done
fi

rm -f "$archive"
tar -C "$output_root" -czf "$archive" "$package_name"
(cd "$output_root" && sha256sum "$(basename "$archive")" > "$(basename "$archive").sha256")
echo "package-linux: created $archive"
echo "package-linux: created $archive.sha256"
