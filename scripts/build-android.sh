#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
if [[ -f "$root/.env.local" ]]; then
  while IFS='=' read -r k v; do
    [[ "$k" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
    v="${v%$'\r'}"
    v="${v%\"}"; v="${v#\"}"
    v="${v%\'}"; v="${v#\'}"
    if [[ -z "${!k+x}" ]]; then
      export "$k=$v"
    fi
  done < "$root/.env.local"
fi

cd "$root/apps/desk"
: "${JAVA_HOME:?需要 JDK 21（或 17）}"
: "${ANDROID_HOME:?需要 Android SDK}"
if [[ -z "${NDK_HOME:-}" && -d "$ANDROID_HOME/ndk" ]]; then
  latest_ndk="$(ls -1 "$ANDROID_HOME/ndk" | sort -V | tail -n 1)"
  if [[ -n "$latest_ndk" ]]; then
    export NDK_HOME="$ANDROID_HOME/ndk/$latest_ndk"
  fi
fi

is_debug=false
has_target=false
for arg in "$@"; do
  [[ "$arg" == "--debug" ]] && is_debug=true
  [[ "$arg" == "--target" || "$arg" == "--target="* ]] && has_target=true
done
build_args=("$@")
if ! $has_target; then
  build_args+=(--target aarch64)
fi

pnpm install --frozen-lockfile
if [[ ! -x src-tauri/gen/android/gradlew ]]; then
  pnpm exec tauri android init --ci
fi
pnpm exec tauri icon native/mark.svg --output src-tauri/icons
cp -R src-tauri/icons/android/. src-tauri/gen/android/app/src/main/res/
rm -f src-tauri/gen/android/app/src/main/res/mipmap-*/ic_launcher_foreground.png \
  src-tauri/gen/android/app/src/main/res/drawable-v24/ic_launcher_foreground.xml \
  src-tauri/gen/android/app/src/main/res/drawable/ic_launcher_background.xml
rm -rf src-tauri/gen/android/app/build/intermediates/merged_res \
  src-tauri/gen/android/app/build/intermediates/packaged_res
cp -R native/android-icon/. src-tauri/gen/android/app/src/main/res/
pnpm exec tauri android build --apk --ci "${build_args[@]}"

apk_root="src-tauri/gen/android/app/build/outputs/apk"
if ! $is_debug && [[ -n "${FESEN_ANDROID_KEYSTORE:-}" ]]; then
  : "${FESEN_ANDROID_KEYSTORE_PASS:?需要 FESEN_ANDROID_KEYSTORE_PASS}"
  : "${FESEN_ANDROID_KEY_ALIAS:?需要 FESEN_ANDROID_KEY_ALIAS}"
  export FESEN_ANDROID_KEY_PASS="${FESEN_ANDROID_KEY_PASS:-$FESEN_ANDROID_KEYSTORE_PASS}"
  [[ -f "$FESEN_ANDROID_KEYSTORE" ]] || { echo "找不到 Android keystore: $FESEN_ANDROID_KEYSTORE" >&2; exit 1; }
  bt_dir="$(ls -1d "$ANDROID_HOME"/build-tools/* 2>/dev/null | sort -V | tail -n 1)"
  [[ -n "$bt_dir" && -x "$bt_dir/zipalign" && -x "$bt_dir/apksigner" ]] || {
    echo "Android Build Tools 中缺少 zipalign 或 apksigner" >&2
    exit 1
  }
  signed_any=false
  while IFS= read -r unsigned_apk; do
    signed_apk="${unsigned_apk%-unsigned.apk}.apk"
    aligned_apk="${unsigned_apk%-unsigned.apk}-aligned.apk"
    "$bt_dir/zipalign" -f -p 4 "$unsigned_apk" "$aligned_apk"
    "$bt_dir/apksigner" sign \
      --ks "$FESEN_ANDROID_KEYSTORE" \
      --ks-pass env:FESEN_ANDROID_KEYSTORE_PASS \
      --ks-key-alias "$FESEN_ANDROID_KEY_ALIAS" \
      --key-pass env:FESEN_ANDROID_KEY_PASS \
      --out "$signed_apk" \
      "$aligned_apk"
    rm -f "$aligned_apk"
    "$bt_dir/apksigner" verify --verbose "$signed_apk" >/dev/null
    printf '%s\n' "$PWD/$signed_apk"
    signed_any=true
  done < <(find "$apk_root" -type f -name '*-release-unsigned.apk' | sort)
  $signed_any || { echo "未找到待签名的 release APK" >&2; exit 1; }
else
  find "$apk_root" -type f -name '*.apk' | sort | while IFS= read -r apk; do
    printf '%s\n' "$PWD/$apk"
  done
fi
