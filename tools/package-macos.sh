#!/usr/bin/env bash
#
# Wrap the oxwin binary in a macOS application bundle.
#
#   tools/package-macos.sh --binary dist/oxwin --out dist
#
# Why a bundle at all: a bare Unix executable double-clicked in Finder opens
# Terminal and runs it there, which is not a desktop app. A bundle is what gives it
# a Dock icon, a window title that is not the binary name, and "Open With" for an
# ISO. The binary inside is the same one the archive ships loose for CLI use, so
# both front ends stay one file's worth of behaviour.
#
# Signing is deliberately optional and off by default: there is no Developer ID
# certificate yet. Set MACOS_SIGN_IDENTITY to sign, and NOTARY_KEYCHAIN_PROFILE to
# notarize and staple as well. Until then the release notes tell people to clear
# the quarantine attribute by hand.

set -euo pipefail

APP_NAME="Windows Image Builder"
BUNDLE_ID="com.oxide.windows-image-builder"
MIN_MACOS="11.0"

binary=""
out="dist"
version=""

while [ $# -gt 0 ]; do
  case "$1" in
    --binary) binary="$2"; shift 2 ;;
    --binary=*) binary="${1#*=}"; shift ;;
    --out) out="$2"; shift 2 ;;
    --out=*) out="${1#*=}"; shift ;;
    --version) version="$2"; shift 2 ;;
    --version=*) version="${1#*=}"; shift ;;
    -h|--help)
      sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done

if [ "$(uname -s)" != "Darwin" ]; then
  echo "package-macos.sh needs macOS: iconutil and codesign live there" >&2
  exit 1
fi
if [ -z "$binary" ]; then
  echo "usage: $0 --binary <path to oxwin> [--out dist] [--version x.y.z]" >&2
  exit 2
fi
if [ ! -x "$binary" ]; then
  echo "$binary is not an executable" >&2
  exit 1
fi

repo="$(cd "$(dirname "$0")/.." && pwd)"
if [ -z "$version" ]; then
  # The workspace version, which every crate inherits.
  version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml" | head -1)"
fi
if [ -z "$version" ]; then
  echo "could not read the version out of Cargo.toml; pass --version" >&2
  exit 1
fi

app="$out/$APP_NAME.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cp "$binary" "$app/Contents/MacOS/oxwin"
chmod +x "$app/Contents/MacOS/oxwin"

# The icon, rendered from the same code that draws the window icon rather than a
# committed blob. Built here rather than in the release workflow so that running
# this script by hand produces the real bundle and not one missing its icon.
iconset="$out/oxwin.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
# iconutil matches on these names exactly; @2x is the same pixel count as the next
# size up, and both have to be present or the bundle falls back to a generic icon
# at whatever size is missing.
for spec in 16:icon_16x16 32:icon_16x16@2x 32:icon_32x32 64:icon_32x32@2x \
            128:icon_128x128 256:icon_128x128@2x 256:icon_256x256 \
            512:icon_256x256@2x 512:icon_512x512 1024:icon_512x512@2x; do
  edge="${spec%%:*}"
  name="${spec#*:}"
  cargo run --quiet --manifest-path "$repo/Cargo.toml" -p oxwin-gui \
    --example icon-png -- "$iconset/$name.png" "$edge" >/dev/null
done
iconutil --convert icns --output "$app/Contents/Resources/AppIcon.icns" "$iconset"
rm -rf "$iconset"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>            <string>$APP_NAME</string>
  <key>CFBundleDisplayName</key>     <string>$APP_NAME</string>
  <!-- The binary, not the app: CLI users run Contents/MacOS/oxwin directly. -->
  <key>CFBundleExecutable</key>      <string>oxwin</string>
  <key>CFBundleIdentifier</key>      <string>$BUNDLE_ID</string>
  <key>CFBundleShortVersionString</key> <string>$version</string>
  <key>CFBundleVersion</key>         <string>$version</string>
  <key>CFBundlePackageType</key>     <string>APPL</string>
  <key>CFBundleIconFile</key>        <string>AppIcon</string>
  <key>LSMinimumSystemVersion</key>  <string>$MIN_MACOS</string>
  <!-- Without this the window renders at 1x and is soft on every Mac sold since
       2012. -->
  <key>NSHighResolutionCapable</key> <true/>
  <!-- So "Open With" and a drop on the Dock icon reach the app, which the
       dispatcher turns into the app opened on that ISO. -->
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key>    <string>Windows installation media</string>
      <key>CFBundleTypeRole</key>    <string>Viewer</string>
      <key>LSHandlerRank</key>       <string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array>
        <string>public.iso-image</string>
      </array>
    </dict>
  </array>
</dict>
</plist>
PLIST

# Signing, when there is something to sign with. Hard-fails rather than warning:
# an unsigned bundle where one was asked for is the kind of thing that is only
# noticed by the person it fails for.
if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
  echo "signing as $MACOS_SIGN_IDENTITY"
  codesign --force --timestamp --options runtime \
    --sign "$MACOS_SIGN_IDENTITY" "$app/Contents/MacOS/oxwin"
  codesign --force --timestamp --options runtime \
    --sign "$MACOS_SIGN_IDENTITY" "$app"
  codesign --verify --deep --strict --verbose=2 "$app"

  if [ -n "${NOTARY_KEYCHAIN_PROFILE:-}" ]; then
    echo "notarizing"
    ditto -c -k --keepParent "$app" "$out/notarize.zip"
    xcrun notarytool submit "$out/notarize.zip" \
      --keychain-profile "$NOTARY_KEYCHAIN_PROFILE" --wait
    xcrun stapler staple "$app"
    rm -f "$out/notarize.zip"
  fi
else
  echo "MACOS_SIGN_IDENTITY unset: the bundle is unsigned, and Gatekeeper will"
  echo "refuse it until its quarantine attribute is cleared."
fi

# Check the bundle rather than assuming the layout came out right. A malformed
# Info.plist does not fail here, it fails as a Finder icon that does nothing.
plutil -lint "$app/Contents/Info.plist"
test -x "$app/Contents/MacOS/oxwin"
test -s "$app/Contents/Resources/AppIcon.icns"

echo "built $app ($version)"
