#!/usr/bin/env bash
# Builds PhotoCraft for Android: android/build/photocraft-<version>-android-arm64.apk
#
# Needs (all standard for an Android developer machine or a GitHub Actions ubuntu runner):
#   - Rust with the aarch64-linux-android target, and cargo-ndk  (cargo install cargo-ndk)
#   - the Android SDK with build-tools and platform 34 (ANDROID_HOME), the NDK (ANDROID_NDK_HOME)
#   - a JDK (javac, keytool) and zip
# Optional: KEYSTORE / KEYSTORE_PASS / KEY_ALIAS to sign with your own key. Without them a debug
# key is created in android/build/ (fine for installing on your own device; not for sharing).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
here="$root/android"
out="$here/build"
: "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK folder}"
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to the NDK folder}"
platform="${ANDROID_PLATFORM:-34}"
min_sdk=26
bt="$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)"
android_jar="$ANDROID_HOME/platforms/android-$platform/android.jar"
[ -f "$android_jar" ] || { echo "missing $android_jar (install platform android-$platform)" >&2; exit 1; }

# The Android build's own version (android/VERSION): the app's version plus a bump for each port
# release, so Android and the launcher see a new build as an update.
version="$(tr -d '[:space:]' <"$here/VERSION")"
IFS=. read -r major minor patch <<<"${version%%-*}"
version_code=$((major * 10000 + minor * 100 + patch))

rm -rf "$out"; mkdir -p "$out"/{classes,dex,res,apk}

echo "== native library"
# 16 KB pages: required by Android 15+ devices that use them, harmless elsewhere.
export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-z,max-page-size=16384"
(cd "$root" && cargo ndk -t arm64-v8a --platform "$min_sdk" -o "$out/lib" build -p photocraft-android --release)

echo "== java helper"
javac -source 8 -target 8 -Xlint:-options -cp "$android_jar" -d "$out/classes" "$here"/java/ai/storyteller/photocraft/*.java
"$bt/d8" --release --min-api "$min_sdk" --lib "$android_jar" --output "$out/dex" "$out"/classes/ai/storyteller/photocraft/*.class

echo "== resources and manifest"
sed -e "s/@VERSION_CODE@/$version_code/" -e "s/@VERSION_NAME@/$version/" "$here/AndroidManifest.xml" >"$out/AndroidManifest.xml"
"$bt/aapt2" compile --dir "$here/res" -o "$out/res.zip"
"$bt/aapt2" link -o "$out/base.apk" -I "$android_jar" --manifest "$out/AndroidManifest.xml" \
  --min-sdk-version "$min_sdk" --target-sdk-version 34 --version-code "$version_code" --version-name "$version" "$out/res.zip"

echo "== package"
cp "$out/dex/classes.dex" "$out/apk/"
mkdir -p "$out/apk/lib/arm64-v8a"
cp "$out/lib/arm64-v8a/libphotocraft_android.so" "$out/apk/lib/arm64-v8a/"
unaligned="$out/unaligned.apk"
cp "$out/base.apk" "$unaligned"
(cd "$out/apk" && zip -q -r -D "$unaligned" classes.dex && zip -q -r -D -0 "$unaligned" lib)
final="$out/photocraft-$version-android-arm64.apk"
"$bt/zipalign" -f -p 4 "$unaligned" "$out/aligned.apk"

echo "== sign"
if [ -n "${KEYSTORE:-}" ]; then
  "$bt/apksigner" sign --ks "$KEYSTORE" --ks-type "${KEYSTORE_TYPE:-PKCS12}" --ks-pass "pass:${KEYSTORE_PASS:?}" --key-pass "pass:$KEYSTORE_PASS" --ks-key-alias "${KEY_ALIAS:?}" --out "$final" "$out/aligned.apk"
else
  keytool -genkeypair -keystore "$out/debug.keystore" -storepass android -keypass android -alias debug \
    -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=PhotoCraft debug" >/dev/null 2>&1
  "$bt/apksigner" sign --ks "$out/debug.keystore" --ks-pass pass:android --key-pass pass:android --out "$final" "$out/aligned.apk"
fi
"$bt/apksigner" verify "$final"
echo "built $final"
