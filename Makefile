.DEFAULT_GOAL := moto
.PHONY: build moto emulator qemu-image test-emulator sim setup
PYTHON ?= python3
VENV := build/sel4/venv
build: moto
moto: setup
	$(PYTHON) tools/build_sel4.py moto
qemu-image: setup
	$(PYTHON) tools/build_sel4.py qemu
setup: $(VENV)/.deps
$(VENV)/.deps: tools/requirements-sel4.txt
	$(PYTHON) -m venv $(VENV)
	$(VENV)/bin/python -m pip install -r $<
	touch $@
build/sel4/viewer: tools/emulator/viewer.c
	mkdir -p $(@D)
	$(CC) $(VIEWER_FLAGS) -std=c11 -O2 -Wall -Wextra $(shell sdl2-config --cflags) $< -o $@ $(shell sdl2-config --libs)
VIEWER := build/sel4/viewer
AARCH64_VIEWER := $(VIEWER)
ifeq ($(shell uname -s),Darwin)
VIEWER_FLAGS := -x objective-c
VIEWER := build/sel4/VioletOS.app/Contents/MacOS/viewer
AARCH64_APP := build/sel4/VioletOS-AArch64.app
AARCH64_VIEWER := $(AARCH64_APP)/Contents/MacOS/viewer
$(VIEWER): APP := build/sel4/VioletOS.app
$(AARCH64_VIEWER): APP := $(AARCH64_APP)
$(AARCH64_VIEWER): APP_ID := org.immortal-protocols.violetos.emulator.aarch64
$(VIEWER) $(AARCH64_VIEWER): build/sel4/viewer tools/emulator/Info.plist tools/emulator/VioletOS.icns
	mkdir -p $(@D) $(APP)/Contents/Resources
	cp tools/emulator/Info.plist $(APP)/Contents/
	$(if $(APP_ID),plutil -replace CFBundleIdentifier -string $(APP_ID) $(APP)/Contents/Info.plist)
	cp tools/emulator/VioletOS.icns $(APP)/Contents/Resources/
	cp $< $@.new
	mv -f $@.new $@
	codesign --force --sign - $(APP)
endif
emulator: qemu-image $(VIEWER)
	$(PYTHON) tools/run.py $(VIEWER)
# The optional 64-bit seL4 desktop keeps its own guest image and RAM.
AARCH64_DESKTOP := build/sel4/qemu-aarch64-desktop
.PHONY: qemu-image-aarch64 emulator-aarch64 test-ladybird-aarch64-media
qemu-image-aarch64: setup
	$(PYTHON) targets/qemu/aarch64/desktop.py --out $(AARCH64_DESKTOP)
emulator-aarch64: qemu-image-aarch64 $(AARCH64_VIEWER)
	$(PYTHON) targets/qemu/aarch64/viewer.py --out $(AARCH64_DESKTOP) --viewer $(AARCH64_VIEWER)
test-ladybird-aarch64-media: qemu-image-aarch64
	$(PYTHON) targets/qemu/aarch64/viewer.py --out $(AARCH64_DESKTOP) --test media \
	    --screenshot $(AARCH64_DESKTOP)/media.png

.PHONY: test-ladybird-aarch64-audio-mixer
test-ladybird-aarch64-audio-mixer: setup
	$(PYTHON) tools/ladybird_native/build_audio.py --test

test-emulator: qemu-image
	@set -e; for mode in normal delayed drop-before drop-after stuck setup reset1 reset2; do \
	  $(PYTHON) targets/qemu/test_guest.py --spi $$mode --watchdog-enabled; \
	done
build/simulator: src/screen.c src/screen.h src/glyphs.h targets/mac-simulator/simulator.c
	mkdir -p $(@D)
	$(CC) -std=c11 -O2 -Wall -Wextra $(shell sdl2-config --cflags) -Isrc src/screen.c targets/mac-simulator/simulator.c -o $@ $(shell sdl2-config --libs)
sim: build/simulator
	./build/simulator

.PHONY: test-isolation component
test-isolation: setup
	VIOLET_TEST_ISOLATION=1 $(PYTHON) tools/build_sel4.py qemu
	@set -e; for region in touch storage dma scanout peer radio radio-dma; do \
	  $(PYTHON) targets/qemu/test_guest.py --probe $$region; \
	done

component: setup
	@test -n "$(NAME)" || (echo "Set NAME to the component name"; exit 1)
	VIOLET_COMPONENT_ONLY="$(NAME)" $(PYTHON) tools/build_sel4.py qemu

.PHONY: test-audio
test-audio: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath targets/qemu/open-test-apps) $(PYTHON) tools/build_sel4.py qemu
	$(PYTHON) targets/qemu/test_audio.py

# The test programs (Rust, C, Go, Python) live in targets/qemu/open-test-apps
# and go into this image only; they show up on Home as extra icons.
.PHONY: test-open
test-open: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath targets/qemu/open-test-apps) $(PYTHON) tools/build_sel4.py qemu
	$(PYTHON) targets/qemu/test_open.py

.PHONY: test-memory
test-memory: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath targets/qemu/memory-test-apps) $(PYTHON) tools/build_sel4.py qemu
	$(PYTHON) targets/qemu/test_memory.py

.PHONY: test-ladybird
test-ladybird: qemu-image
	$(PYTHON) targets/qemu/test_ladybird.py

# In an offline guest, test decoded video and sound after a native touch.
.PHONY: test-ladybird-upstream-media
test-ladybird-upstream-media: setup
	$(PYTHON) targets/qemu/aarch64/desktop.py --upstream-media-test --release-services "$${LADYBIRD_RELEASE_SERVICES:-build/ladybird-native/release-audio/root}" --out build/sel4/qemu-aarch64-upstream-media
	$(PYTHON) targets/qemu/aarch64/viewer.py --out build/sel4/qemu-aarch64-upstream-media --test upstream-media --screenshot build/sel4/qemu-aarch64-upstream-media/media.png --timeout 360

YOUTUBE_IMAGE := build/sel4/qemu-aarch64-youtube-iframe
YOUTUBE_CHECK := --out $(YOUTUBE_IMAGE) --test youtube --player-rect 30 168 604 294 --screenshot $(YOUTUBE_IMAGE)/playing.png --timeout 660
.PHONY: qemu-image-ladybird-youtube test-ladybird-youtube-iframe emulator-ladybird-youtube
qemu-image-ladybird-youtube: setup
	$(PYTHON) targets/qemu/aarch64/desktop.py --youtube-iframe-test --out $(YOUTUBE_IMAGE) \
	    --browser-root "$${LADYBIRD_BROWSER_ROOT:-build/ladybird-native/headless-root}" \
	    --release-services "$${LADYBIRD_RELEASE_SERVICES:-build/ladybird-native/release-audio/root}"
test-ladybird-youtube-iframe: qemu-image-ladybird-youtube
	$(PYTHON) targets/qemu/aarch64/viewer.py $(YOUTUBE_CHECK)
emulator-ladybird-youtube: qemu-image-ladybird-youtube $(AARCH64_VIEWER)
	VIOLET_QEMU_TITLE="VioletOS · YouTube playback" $(PYTHON) targets/qemu/aarch64/viewer.py $(YOUTUBE_CHECK) --show-after-playback $(AARCH64_VIEWER)

.PHONY: test-ladybird-upstream-view
test-ladybird-upstream-view: setup
	$(PYTHON) targets/qemu/aarch64/desktop.py --presentation-test --out build/sel4/qemu-aarch64-presentation
	$(PYTHON) targets/qemu/aarch64/viewer.py --out build/sel4/qemu-aarch64-presentation --test presentation --timeout 240 --screenshot build/sel4/qemu-aarch64-presentation/presentation.png

.PHONY: test-ladybird-media
test-ladybird-media: qemu-image
	VIOLET_TEST_WEBM=1 $(PYTHON) targets/qemu/test_ladybird.py

# Upstream ARM64 Linux baseline. Run this inside a Linux QEMU guest.
.PHONY: test-ladybird-youtube-reference
LADYBIRD_YOUTUBE_URL ?= https://www.youtube.com/watch?v=YE7VzlLtp-4
LADYBIRD_YOUTUBE_SECONDS ?= 20
test-ladybird-youtube-reference:
	mkdir -p build
	$(PYTHON) targets/qemu/test_ladybird_youtube.py "$(LADYBIRD_YOUTUBE_URL)" \
	    --seconds "$(LADYBIRD_YOUTUBE_SECONDS)" --audio-monitor @DEFAULT_MONITOR@ \
	    --screenshot build/ladybird-youtube.png

.PHONY: test-wifi-app
test-wifi-app: qemu-image
	$(PYTHON) targets/qemu/test_wifi_app.py

# The voice agent: QR pairing, an encrypted install that survives a restart, reconnect
# and unpair, with the real Mac app (companion/mac) and a local relay.
test-agent:
	VIOLET_AGENT_RELAY=http://violet-relay.test VIOLET_AGENT_STUN=violet-relay.test:3478 VIOLET_OPEN_NO_PYTHON=1 $(MAKE) qemu-image
	$(PYTHON) targets/qemu/test_agent.py

# Uses the downloaded Mac app and its installed models. Runs locally only.
LOCOMO_APP ?= /Applications/Locomo Music.app
.PHONY: test-locomo
test-locomo:
	VIOLET_AGENT_RELAY=http://violet-relay.test VIOLET_AGENT_STUN=off VIOLET_OPEN_SKIP=ladybird VIOLET_OPEN_NO_PYTHON=1 $(MAKE) qemu-image
	$(PYTHON) targets/qemu/test_locomo.py "$(LOCOMO_APP)"

.PHONY: test-open-apps
test-open-apps: qemu-image
	$(PYTHON) targets/qemu/test_open_apps.py

# Host unit tests (CI): each test finishes in well under a second.
HOST_CARGO ?= $(or $(shell command -v cargo),$(dir $(HOST_RUSTC))cargo)
.PHONY: test-unit
test-unit:
	@mkdir -p build/tests
	"$(HOST_RUSTC)" --edition=2021 --test -O services/audio/mic.rs -o build/tests/mic
	build/tests/mic
	@printf '#![no_std]\nextern crate alloc;\nextern crate std;\n#[path = "../../runtime/open/inflate.rs"]\nmod inflate;\n' > build/tests/inflate_test.rs
	"$(HOST_RUSTC)" --edition=2021 --test -O build/tests/inflate_test.rs -o build/tests/inflate
	build/tests/inflate
	python3 apps/lib/python/test_violet.py
	cd apps && RUSTC="$(abspath $(HOST_RUSTC))" "$(abspath $(HOST_CARGO))" test --release --locked --lib --bins -p violet -p home -p settings -p bitchat --target-dir ../build/host-tests

# The QEMU tests CI runs: one boot of the supervisor and its services (touch,
# the watchdog, Home answering a tap), then Home, Settings and Music across
# two boots (a setting kept in /data across the restart).
.PHONY: test-ci
test-ci: qemu-image
	$(PYTHON) targets/qemu/test_guest.py --spi normal --watchdog-enabled
	OPEN_FORCE_OFFLINE=1 $(PYTHON) targets/qemu/test_open_apps.py

.PHONY: test-moto-diagnostics
test-moto-diagnostics: moto qemu-image
	python3 targets/moto-g-pure/test_boot.py --diagnostics-trial confirm
	python3 targets/moto-g-pure/test_boot.py --diagnostics-trial fault
	python3 targets/moto-g-pure/test_boot.py --diagnostics-trial timeout

.PHONY: test-moto-ui
test-moto-ui: moto qemu-image
	$(PYTHON) targets/moto-g-pure/test_boot.py
	$(PYTHON) targets/moto-g-pure/test_boot.py --stall-services
	$(PYTHON) targets/moto-g-pure/test_ui.py

.PHONY: moto-debug
moto-debug: build/moto-debug
	@$<

build/moto-debug: services/usb-diagnostics/host/moto_debug.c
	@mkdir -p build
	$(CC) -Wall -Wextra -Werror -std=c11 $< $$(pkg-config --cflags --libs libusb-1.0) -o $@

.PHONY: test-moto-radio
MOTO_WIFI_STOCK ?= build/xt2163dl/S3RHS32.20-42-13-2-3-2/stock/wifi
HOST_RUSTC ?= $(or $(shell command -v rustc),build/spike1/rustup/toolchains/1.91.1-aarch64-apple-darwin/bin/rustc)
test-moto-radio: moto
	$(PYTHON) targets/moto-g-pure/test_supplicant.py
	"$(HOST_RUSTC)" --edition=2021 --test targets/moto-g-pure/test_history.rs -o build/sel4/moto/test-history
	build/sel4/moto/test-history
	$(CC) -std=c11 -Wall -Wextra -Werror targets/moto-g-pure/test_radio_memory.c -o build/sel4/moto/test-radio-memory
	build/sel4/moto/test-radio-memory
	VIOLET_TEST_WIFI_BUNDLE="$(abspath build/sel4/moto/wifi-firmware.bin)" VIOLET_TEST_WLAN_IMAGE="$(abspath $(MOTO_WIFI_STOCK)/WIFI_RAM_CODE_soc1_0_1_1.bin)" "$(HOST_RUSTC)" --edition=2021 --test targets/moto-g-pure/test_radio.rs -o build/sel4/moto/test-radio
	build/sel4/moto/test-radio

.PHONY: test-moto-ip-live
test-moto-ip-live: moto
	$(PYTHON) targets/moto-g-pure/test_ip_live.py

# A Mac bitchat peer for over-the-air tests of apps/open/bitchat. macOS keeps
# its Bluetooth grant across rebuilds while it is signed with one identity:
# BITCHAT_PEER_IDENTITY, by default the first Apple Development identity (else ad hoc).
.PHONY: bitchat-peer
bitchat-peer: build/BitchatPeer.app/Contents/MacOS/bitchat-peer
build/BitchatPeer.app/Contents/MacOS/bitchat-peer: tools/bitchat-peer/main.swift tools/bitchat-peer/Info.plist
	@mkdir -p $(@D)
	cp tools/bitchat-peer/Info.plist build/BitchatPeer.app/Contents/Info.plist
	swiftc -O $< -o $@
	codesign --force -s "$${BITCHAT_PEER_IDENTITY:-$$(security find-identity -v -p codesigning | awk -F'"' '/Apple Development/{print $$2; exit}')}" build/BitchatPeer.app 2>/dev/null || codesign --force -s - build/BitchatPeer.app

build/moto-package: services/usb-diagnostics/host/moto_package.c
	@mkdir -p build
	$(CC) -Wall -Wextra -Werror -std=c11 $< $$(pkg-config --cflags --libs libusb-1.0) -o $@

.PHONY: moto-component
moto-component: setup
	@test -n "$(NAME)" || (echo "Set NAME to the Moto component (e.g. radio)"; exit 1)
	VIOLET_COMPONENT_ONLY="$(NAME)" $(PYTHON) tools/build_sel4.py moto

# Violet Shield (docs/violet-shield-poc.md).
SHIELD_PY := build/shield/venv/bin/python
.PHONY: shield-setup shield-train test-shield-unit test-shield moto-shield
$(SHIELD_PY):
	$(PYTHON) -m venv build/shield/venv
	$(SHIELD_PY) -m pip install -q torch "transformers>=5.24" sentence-transformers coremltools numpy cryptography pillow
shield-setup: $(SHIELD_PY)
shield-train: $(SHIELD_PY)
	$(SHIELD_PY) tools/shield-trainer/train.py
test-shield-unit: $(SHIELD_PY)
	SHIELD_PYTHON=$(SHIELD_PY) HOST_RUSTC="$(HOST_RUSTC)" targets/qemu/test_shield_unit.sh
test-shield: qemu-image $(SHIELD_PY)
	$(SHIELD_PY) targets/qemu/test_shield.py
# Moto image with the demo apps for use with the Mac companion.
moto-shield: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath apps/shield-demo) $(PYTHON) tools/build_sel4.py moto
