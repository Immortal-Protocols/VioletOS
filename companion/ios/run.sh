#!/usr/bin/env bash
# Build, install and launch Violet Relay on the attached iPhone, hands-free.
#
#   ./run.sh                         build + install + grant prompts (UI test) + launch, print status
#   ./run.sh status                  copy Documents/status.json off the phone and print it
#   ./run.sh launch                  (re)launch the installed app with devicectl only
#   JOIN_SSID=Violet JOIN_PASSPHRASE=secret ./run.sh join   join a Wi-Fi network (taps "Join"), then launch
#   FORGET_SSID=Violet ./run.sh forget                      remove a stored hotspot configuration
#   ./run.sh rejoin                  rejoin the saved network in build/relay-pairing.json without UI
#                                    automation; iOS asks nothing for a network it already saved
#
# Env: RELAY_SECRET (default violet-dev), DEVICE (devicectl id), XCODE_DEST (xcodebuild udid),
#      SKIP_BUILD=1 to reuse the last build.
set -euo pipefail
cd "$(dirname "$0")"

DEVICE="${DEVICE:-3D9ACA61-50AF-5EAA-AE1C-2B5142827EDE}"
XCODE_DEST="${XCODE_DEST:-00008160-000E58CC18914036}"
BUNDLE=com.immortal.violet.relay
RELAY_SECRET="${RELAY_SECRET:-violet-dev}"
DD=build/dd
APP="$DD/Build/Products/Debug-iphoneos/VioletRelay.app"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"

build() {
  [[ "${SKIP_BUILD:-0}" == 1 && -d "$APP" ]] && return
  xcodegen generate -q
  xcodebuild -project VioletRelay.xcodeproj -scheme VioletRelay -destination "id=$XCODE_DEST" \
    -derivedDataPath "$DD" -allowProvisioningUpdates -quiet build-for-testing
}

# The UI test installs the app, launches it with the given arguments and taps system alerts
# (Local Network "Allow", Wi-Fi "Join"). TEST_RUNNER_X reaches the test process as X.
ui_test() {
  env TEST_RUNNER_RELAY_SECRET="$RELAY_SECRET" \
      TEST_RUNNER_JOIN_SSID="${JOIN_SSID:-}" TEST_RUNNER_JOIN_PASSPHRASE="${JOIN_PASSPHRASE:-}" \
      TEST_RUNNER_FORGET_SSID="${FORGET_SSID:-}" \
    xcodebuild -project VioletRelay.xcodeproj -scheme VioletRelay -destination "id=$XCODE_DEST" \
      -derivedDataPath "$DD" -allowProvisioningUpdates test-without-building \
      -only-testing:VioletRelayUITests/RelayUITests/testLaunchAndGrant 2>&1 \
    | grep -E "RELAY |error:|Test Case .*(passed|failed)|\*\* TEST" || true
}

launch() {
  xcrun devicectl device process launch --device "$DEVICE" --terminate-existing "$BUNDLE" -- \
    -pairingSecret "$RELAY_SECRET" >/dev/null
  echo "launched $BUNDLE"
}

status() {
  local out; out="$(mktemp -d)/status.json"
  xcrun devicectl device copy from --device "$DEVICE" --domain-type appDataContainer \
    --domain-identifier "$BUNDLE" --source Documents/status.json --destination "$out" >/dev/null
  cat "$out"
}

case "${1:-all}" in
  all)    build; ui_test; launch; sleep 3; status ;;
  build)  build ;;
  grant)  ui_test ;;
  launch) launch ;;
  status) status ;;
  join)   : "${JOIN_SSID:?set JOIN_SSID}"; SKIP_BUILD=${SKIP_BUILD:-1} build; ui_test; launch; sleep 3; status ;;
  rejoin) pairing=../../build/relay-pairing.json
          ssid=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["ssid"])' "$pairing")
          secret=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["socks_password"])' "$pairing")
          xcrun devicectl device process launch --device "$DEVICE" --terminate-existing "$BUNDLE" -- \
            -pairingSecret "$secret" -joinSSID "$ssid" >/dev/null
          sleep 10; status ;;
  forget) : "${FORGET_SSID:?set FORGET_SSID}"; ui_test; launch ;;
  *) echo "usage: $0 [all|build|grant|launch|status|join|forget]" >&2; exit 2 ;;
esac
