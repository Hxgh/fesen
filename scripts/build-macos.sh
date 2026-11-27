#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
root="$PWD"

if [[ -f .env.local ]]; then
  while IFS='=' read -r k v; do
    [[ "$k" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
    v="${v%$'\r'}"
    v="${v%\"}"; v="${v#\"}"
    v="${v%\'}"; v="${v#\'}"
    if [[ -z "${!k+x}" ]]; then
      export "$k=$v"
    fi
  done < .env.local
fi

[[ "$(uname -m)" == "arm64" ]] || { echo "macOS 仅支持 Apple Silicon (arm64)" >&2; exit 2; }

desk="$root/apps/desk"
tunnel="$root/apps/core/native/macos"
t="aarch64-apple-darwin"
profile=release
cargo_profile=release
unsigned=false
install_app=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --debug) profile=debug; cargo_profile=dev ;;
    --unsigned) unsigned=true ;;
    --install) install_app=true ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
  shift
done

build=(pnpm -C "$desk" exec tauri build --target "$t" --bundles app --no-sign)
if [[ "$profile" == debug ]]; then build+=(--debug); fi

if ! $unsigned; then
  : "${FESEN_TEAM_ID:?需要 Apple Team ID}"
  : "${FESEN_SIGN_ID:?需要 Developer ID Application 签名身份}"
  : "${FESEN_HOST_PROFILE:?需要应用的 Developer ID provisioning profile}"
  : "${FESEN_TUNNEL_PROFILE:?需要系统扩展的 Developer ID provisioning profile}"
fi

export MACOSX_DEPLOYMENT_TARGET=15.0
pnpm -C "$desk" install --frozen-lockfile
"${build[@]}"
CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_STRIP=none cargo build --locked -p fesen-core --lib --target "$t" --profile "$cargo_profile"
app="$desk/src-tauri/target/$t/$profile/bundle/macos/Fesen.app"
extension="$app/Contents/Library/SystemExtensions/io.fesen.client.tunnel.systemextension/Contents"
mkdir -p "$extension/MacOS"
cp "$tunnel/Info.plist" "$extension/Info.plist"
plutil -replace NetworkExtension.NEMachServiceName -string "${FESEN_TEAM_ID:-UNSIGNED}.io.fesen.client.tunnel" "$extension/Info.plist"
swiftc -target arm64-apple-macos15.0 -swift-version 5 -O -module-name FesenTunnel \
  -import-objc-header "$tunnel/Core.h" "$tunnel/PhysicalNetwork.swift" "$tunnel/Tunnel.swift" "$tunnel/main.swift" \
  "$root/target/$t/$profile/libfesen_core.a" -framework NetworkExtension -framework Security \
  -framework SystemConfiguration -o "$extension/MacOS/FesenTunnel"
swiftc -target arm64-apple-macos15.0 -swift-version 5 -O \
  "$desk/native/macos/Accessory.swift" "$desk/native/macos/MenuBarPreference.swift" "$desk/native/macos/Menu.swift" \
  -o "$app/Contents/MacOS/fesen-menu"
mkdir -p "$app/Contents/Resources"
cp -R "$desk/native/macos/zh-Hans.lproj" "$desk/native/macos/en.lproj" "$app/Contents/Resources/"
plutil -replace LSHasLocalizedDisplayName -bool true "$app/Contents/Info.plist"

if ! $unsigned; then
  signing="$(mktemp -d)"
  trap 'rm -r "$signing"' EXIT
  cp "$FESEN_HOST_PROFILE" "$app/Contents/embedded.provisionprofile"
  cp "$FESEN_TUNNEL_PROFILE" "$extension/embedded.provisionprofile"
  cp "$desk/native/macos/Host.entitlements" "$signing/host.plist"
  cp "$tunnel/Tunnel.entitlements" "$signing/tunnel.plist"
  for item in host tunnel; do
    id=io.fesen.client
    if [[ $item == tunnel ]]; then id+=.tunnel; fi
    /usr/libexec/PlistBuddy -c "Add :com.apple.application-identifier string $FESEN_TEAM_ID.$id" "$signing/$item.plist"
    /usr/libexec/PlistBuddy -c "Add :com.apple.developer.team-identifier string $FESEN_TEAM_ID" "$signing/$item.plist"
  done
  codesign --force --timestamp --options runtime --sign "$FESEN_SIGN_ID" --entitlements "$signing/tunnel.plist" "${extension%/Contents}"
  codesign --force --timestamp --options runtime --sign "$FESEN_SIGN_ID" --identifier io.fesen.client \
    --entitlements "$signing/host.plist" "$app/Contents/MacOS/fesen-menu"
  codesign --force --timestamp --options runtime --sign "$FESEN_SIGN_ID" --entitlements "$signing/host.plist" "$app"
else
  # Seal local builds too. A linker-only signature omits Info.plist and identifies
  # each build by a different executable name instead of the responsible app.
  codesign --force --sign - --timestamp=none "${extension%/Contents}"
  codesign --force --sign - --timestamp=none --identifier io.fesen.client.menu "$app/Contents/MacOS/fesen-menu"
  codesign --force --sign - --timestamp=none "$app"
fi
codesign --verify --deep --strict "$app"
signature=$(codesign --display --verbose=2 "$app" 2>&1)
[[ "$signature" == *$'\nIdentifier=io.fesen.client\n'* ]] || { echo "签名标识与应用标识不一致" >&2; exit 1; }

out_bundle="$root/target/$profile/bundle/macos"
mkdir -p "$out_bundle"
rm -rf "$out_bundle/Fesen.app"
cp -R "$app" "$out_bundle/Fesen.app"

if $install_app; then
  killall fesen fesen-menu 2>/dev/null || true
  rm -rf /Applications/Fesen.app
  cp -R "$out_bundle/Fesen.app" /Applications/Fesen.app
fi

printf '%s\n' "$out_bundle/Fesen.app"
