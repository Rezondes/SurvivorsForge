#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $# -gt 4 || ! $2 =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
    echo "Usage: $0 <arm64 SurvivorsForge binary> <version> <output directory> [Wine runtime archive]" >&2
    exit 2
fi

binary="$1"
version="$2"
output="$3"
archive="${4:-}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
/usr/bin/lipo "$binary" -verify_arch arm64
mkdir -p "$output"
destination="$output/SurvivorsForge.app"
if [[ -e "$destination" || -L "$destination" ]]; then
    echo "Move the existing app at $destination before building a replacement." >&2
    exit 1
fi

work="$(mktemp -d "$output/.survivorsforge-build.XXXXXXXX")"
trap 'rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM
app="$work/SurvivorsForge.app"
resources="$app/Contents/Resources"
mkdir -p "$app/Contents/MacOS" "$resources/Wine"
install -m 755 "$binary" "$app/Contents/MacOS/SurvivorsForge"

# The standalone Wine 11.13 runtime used by WoWSilicon 3.2.2.
if [[ -z "$archive" ]]; then
    archive="$work/WineRuntime.tar.xz"
    curl --fail --silent --show-error --location --retry 2 \
        "https://github.com/WoWSilicon/WoWSilicon/releases/download/wine-runtime-r17/WoWSilicon-WineRuntime-r17.tar.xz" \
        -o "$archive"
fi
expected=dc67cf0c2dd1e4c1cfaffe924f4737aaa594645b135a7973ac3505c83c70f882
actual="$(shasum -a 256 < "$archive")"
if [[ "${actual%% *}" != "$expected" ]]; then
    echo "The Wine runtime archive did not match the pinned SHA-256." >&2
    exit 1
fi
tar -xf "$archive" --strip-components 1 -C "$resources/Wine"
cp "$root/packaging/macos/THIRD-PARTY-NOTICES.md" "$resources/"
cp -R "$root/packaging/macos/licenses" "$resources/"

iconset="$work/SurvivorsForge.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$root/assets/icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    retina=$((size * 2))
    sips -z "$retina" "$retina" "$root/assets/icon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$resources/SurvivorsForge.icns"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>SurvivorsForge</string>
    <key>CFBundleIdentifier</key>
    <string>net.rezondes.survivorsforge</string>
    <key>CFBundleName</key>
    <string>SurvivorsForge</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleIconFile</key>
    <string>SurvivorsForge.icns</string>
    <key>CFBundleShortVersionString</key>
    <string>${version%%-*}</string>
    <key>CFBundleVersion</key>
    <string>${version%%-*}</string>
    <key>LSMinimumSystemVersion</key>
    <string>26.2</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.games</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF

codesign --force --sign - "$app"
mv -n "$app" "$output/"
if [[ -e "$app" ]]; then
    echo "Could not install the app at $destination." >&2
    exit 1
fi
printf '%s\n' "$destination"
