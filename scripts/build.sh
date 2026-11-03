#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

usage() {
  cat <<'EOF'
Usage: scripts/build.sh <platform> [--release]

Platforms:
  pc        Desktop launcher (crab_feast_pc)
  wasm      Web launcher (crab_feast_wasm + wasm-bindgen)
  android   Android APK (cargo-ndk + gradle)
  ios       iOS simulator app (xcodebuild)
  all       pc + wasm

Options:
  --release   Build with the release profile (debug by default)
  -h, --help  Show this help

Examples:
  scripts/build.sh pc
  scripts/build.sh wasm --release
  scripts/build.sh android --release

Serve the web build from the repository root, then open
http://localhost:8080/launcher/wasm/:
  python3 -m http.server 8080
EOF
}

require() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: '$1' not found in PATH" >&2
    exit 1
  }
}

build_pc() {
  cargo build -p crab_feast_pc ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}
}

build_wasm() {
  require wasm-bindgen
  rustup target list --installed | grep -q wasm32-unknown-unknown || {
    echo "error: missing rust target wasm32-unknown-unknown" >&2
    echo "hint: rustup target add wasm32-unknown-unknown" >&2
    exit 1
  }

  cargo build --target wasm32-unknown-unknown -p crab_feast_wasm ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}
  wasm-bindgen \
    --out-name crab_feast_wasm \
    --out-dir launcher/wasm/target \
    --target web \
    "target/wasm32-unknown-unknown/${PROFILE}/crab_feast_wasm.wasm"
}

build_android() {
  require cargo-ndk

  local abis="${ANDROID_ABIS:-aarch64-linux-android}"
  local ndk_targets=()
  for abi in $abis; do
    ndk_targets+=(-t "$abi")
  done

  cargo ndk "${ndk_targets[@]}" \
    -o ./launcher/mobile/android/app/src/main/jniLibs \
    build ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"} -p crab_feast_mobile

  local gradle_task="assembleDebug"
  if [[ "$RELEASE" == true ]]; then
    gradle_task="assembleRelease"
  fi
  ( cd launcher/mobile/android && ./gradlew "$gradle_task" )
}

build_ios() {
  ( cd launcher/mobile/ios && make xcodebuild-simulator )
}

if [[ $# -lt 1 ]]; then
  usage
  exit 1
fi

PLATFORM="$1"
shift

RELEASE=false
for arg in "$@"; do
  case "$arg" in
    --release) RELEASE=true ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "error: unknown option '$arg'" >&2
      usage
      exit 1
      ;;
  esac
done

PROFILE="debug"
CARGO_PROFILE_ARGS=()
if [[ "$RELEASE" == true ]]; then
  PROFILE="release"
  CARGO_PROFILE_ARGS=(--release)
fi

case "$PLATFORM" in
  pc) build_pc ;;
  wasm) build_wasm ;;
  android) build_android ;;
  ios) build_ios ;;
  all)
    build_pc
    build_wasm
    ;;
  -h|--help|help) usage ;;
  *)
    echo "error: unknown platform '$PLATFORM'" >&2
    usage
    exit 1
    ;;
esac
