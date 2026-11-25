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
	$(CC) -std=c11 -O2 -Wall -Wextra $(shell sdl2-config --cflags) $< -o $@ $(shell sdl2-config --libs)
VIEWER := build/sel4/viewer
ifeq ($(shell uname -s),Darwin)
VIEWER := build/sel4/VioletOS.app/Contents/MacOS/viewer
$(VIEWER): build/sel4/viewer tools/emulator/Info.plist
	mkdir -p $(@D)
	cp tools/emulator/Info.plist build/sel4/VioletOS.app/Contents/
	cp $< $@
endif
emulator: qemu-image $(VIEWER)
	$(PYTHON) tools/run.py $(VIEWER)
test-emulator: qemu-image
	@set -e; for mode in normal delayed drop-before drop-after stuck setup reset1 reset2; do \
	  $(PYTHON) targets/qemu/test_guest.py --spi $$mode --watchdog-enabled; \
	done
build/simulator: src/screen.c src/screen.h src/glyphs.h targets/mac-simulator/simulator.c
	mkdir -p $(@D)
	$(CC) -std=c11 -O2 -Wall -Wextra $(shell sdl2-config --cflags) -Isrc src/screen.c targets/mac-simulator/simulator.c -o $@ $(shell sdl2-config --libs)
sim: build/simulator
	./build/simulator

.PHONY: test-storage test-isolation
test-storage: qemu-image
	$(PYTHON) targets/qemu/test_storage.py
test-isolation: setup
	VIOLET_TEST_ISOLATION=1 $(PYTHON) tools/build_sel4.py qemu
	@set -e; for region in touch storage dma scanout peer radio radio-dma; do \
	  $(PYTHON) targets/qemu/test_guest.py --probe $$region; \
	done

.PHONY: test-stack component
test-stack: qemu-image
	$(PYTHON) targets/qemu/test_packages.py
component: setup
	@test -n "$(NAME)" || (echo "Set NAME to the component name"; exit 1)
	VIOLET_COMPONENT_ONLY="$(NAME)" $(PYTHON) tools/build_sel4.py qemu

.PHONY: test-wifi
test-wifi: qemu-image
	$(PYTHON) targets/qemu/test_wifi.py

.PHONY: test-wifi-live
test-wifi-live: qemu-image
	$(PYTHON) targets/qemu/test_guest.py --wifi live

.PHONY: app
app: setup
	@test -n "$(APP)" || (echo "Set APP to an application directory"; exit 1)
	VIOLET_APP_DIR="$(abspath $(APP))" $(PYTHON) tools/build_sel4.py qemu

.PHONY: test-apps
test-apps: qemu-image
	$(MAKE) app APP=targets/qemu/test-app
	$(PYTHON) targets/qemu/test_library.py

.PHONY: test-power
test-power: qemu-image
	$(PYTHON) targets/qemu/test_power.py

.PHONY: test-audio
test-audio: qemu-image
	$(PYTHON) targets/qemu/test_audio.py

.PHONY: test-open
test-open: qemu-image
	$(PYTHON) targets/qemu/test_open.py

.PHONY: test-ladybird
test-ladybird: qemu-image
	$(PYTHON) targets/qemu/test_ladybird.py

.PHONY: test-camera
test-camera: qemu-image
	$(PYTHON) targets/qemu/test_camera.py
.PHONY: test-open-apps
.PHONY: test-wifi-app
test-wifi-app: qemu-image
	$(PYTHON) targets/qemu/test_wifi_app.py

# The voice agent: QR pairing, an encrypted install that survives a restart, reconnect
# and unpair, with the real Mac app (companion/mac) and a local relay.
test-agent:
	VIOLET_AGENT_RELAY=http://violet-relay.test $(MAKE) qemu-image
	$(PYTHON) targets/qemu/test_agent.py

test-open-apps: qemu-image
	$(PYTHON) targets/qemu/test_open_apps.py
	@mkdir -p build/tests
	@printf '#![no_std]\nextern crate alloc;\nextern crate std;\n#[path = "../../runtime/open/inflate.rs"]\nmod inflate;\n' > build/tests/inflate_test.rs
	"$(HOST_RUSTC)" --edition=2021 --test -O build/tests/inflate_test.rs -o build/tests/inflate
	build/tests/inflate

# Host unit tests (CI): each test finishes in well under a second.
HOST_CARGO ?= $(or $(shell command -v cargo),$(dir $(HOST_RUSTC))cargo)
.PHONY: test-unit
test-unit:
	@mkdir -p build/tests
	"$(HOST_RUSTC)" --edition=2021 --test -O apps/stopwatch/state.rs -o build/tests/stopwatch-state
	build/tests/stopwatch-state
	"$(HOST_RUSTC)" --edition=2021 --test -O services/audio/mic.rs -o build/tests/mic
	build/tests/mic
	@printf '#![no_std]\nextern crate alloc;\nextern crate std;\n#[path = "../../runtime/open/inflate.rs"]\nmod inflate;\n' > build/tests/inflate_test.rs
	"$(HOST_RUSTC)" --edition=2021 --test -O build/tests/inflate_test.rs -o build/tests/inflate
	build/tests/inflate
	cd apps && RUSTC="$(abspath $(HOST_RUSTC))" "$(abspath $(HOST_CARGO))" test --release --locked --lib --bins -p violet -p home --target-dir ../build/host-tests

# The QEMU tests CI runs: one boot of the supervisor and its services (touch,
# storage, the watchdog, the classic launcher), then the open apps (Home,
# Stopwatch keeping its time across a restart, Sounds playing).
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

build/moto-package: services/usb-diagnostics/host/moto_package.c
	@mkdir -p build
	$(CC) -Wall -Wextra -Werror -std=c11 $< $$(pkg-config --cflags --libs libusb-1.0) -o $@

.PHONY: moto-component
moto-component: setup
	@test -n "$(NAME)" || (echo "Set NAME to the Moto component (e.g. radio)"; exit 1)
	VIOLET_COMPONENT_ONLY="$(NAME)" $(PYTHON) tools/build_sel4.py moto

# Violet Shield (docs/violet-shield-poc.md).
SHIELD_PY := build/shield/venv/bin/python
SHIELD_SIM ?= D4AE7E80-FFC5-4AD7-B810-33E3FFD396EA
.PHONY: shield-setup shield-train shield-companion test-shield-unit test-shield test-shield-ios shield-demo moto-shield test-shield-device shield-device-demo
$(SHIELD_PY):
	$(PYTHON) -m venv build/shield/venv
	$(SHIELD_PY) -m pip install -q torch "transformers>=5.24" sentence-transformers coremltools numpy cryptography pillow
shield-setup: $(SHIELD_PY)
shield-train: $(SHIELD_PY)
	$(SHIELD_PY) tools/shield-trainer/train.py
shield-companion:
	cd companion/ios && xcodegen generate -q && xcodebuild -project VioletRelay.xcodeproj -scheme VioletRelay \
	  -destination 'platform=iOS Simulator,id=$(SHIELD_SIM)' -derivedDataPath ../../build/dd-sim CODE_SIGN_IDENTITY=- -quiet build
	xcrun simctl install $(SHIELD_SIM) build/dd-sim/Build/Products/Debug-iphonesimulator/VioletRelay.app
test-shield-unit: $(SHIELD_PY)
	SHIELD_PYTHON=$(SHIELD_PY) HOST_RUSTC="$(HOST_RUSTC)" targets/qemu/test_shield_unit.sh
test-shield: qemu-image $(SHIELD_PY)
	$(SHIELD_PY) targets/qemu/test_shield.py --companion sim
test-shield-ios: qemu-image shield-companion
	VIOLET_SIM=$(SHIELD_SIM) $(SHIELD_PY) targets/qemu/test_shield.py --companion ios
shield-demo: qemu-image shield-companion
	VIOLET_SIM=$(SHIELD_SIM) $(SHIELD_PY) targets/qemu/test_shield.py --companion ios --record build/shield/demo
	$(SHIELD_PY) tools/shield-trainer/make_video.py build/shield/demo
# Real devices: the Moto image with the demo apps, then the run (see targets/moto-g-pure/test_shield_device.py).
moto-shield: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath apps/shield-demo) $(PYTHON) tools/build_sel4.py moto
test-shield-device: $(SHIELD_PY)
	$(SHIELD_PY) targets/moto-g-pure/test_shield_device.py
shield-device-demo: $(SHIELD_PY)
	$(SHIELD_PY) targets/moto-g-pure/test_shield_device.py --record build/shield/device-demo
	$(SHIELD_PY) tools/shield-trainer/make_device_video.py build/shield/device-demo
