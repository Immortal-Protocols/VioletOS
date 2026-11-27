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
ifeq ($(shell uname -s),Darwin)
VIEWER_FLAGS := -x objective-c
VIEWER := build/sel4/VioletOS.app/Contents/MacOS/viewer
$(VIEWER): build/sel4/viewer tools/emulator/Info.plist tools/emulator/VioletOS.icns
	mkdir -p $(@D)
	mkdir -p build/sel4/VioletOS.app/Contents/Resources
	cp tools/emulator/Info.plist build/sel4/VioletOS.app/Contents/
	cp tools/emulator/VioletOS.icns build/sel4/VioletOS.app/Contents/Resources/
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

# Power's wake locks and governors, and network time (local, not in CI). The
# time test needs this Mac to reach public NTP servers over UDP.
.PHONY: test-power test-time
test-power: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath targets/qemu/power-test-apps) $(PYTHON) tools/build_sel4.py qemu
	$(PYTHON) targets/qemu/test_power.py
test-time: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath targets/qemu/power-test-apps) $(PYTHON) tools/build_sel4.py qemu
	$(PYTHON) targets/qemu/test_time.py

# Open apps' sockets (local, not in CI): one app holds its share of the socket
# slots, churns the resolver and leaves a connect hanging; a second app must
# still fetch a page at once. Servers are on this Mac's loopback.
# Radio's socket code against lwIP itself, on the host (local, not in CI: it
# needs lwIP's source, which an image build downloads). A second.
LWIP_SRC ?= build/sel4/lwip-STABLE-2_2_1_RELEASE/src
.PHONY: test-net-lwip
test-net-lwip:
	@test -f $(LWIP_SRC)/core/tcp.c || { echo "no lwIP source at $(LWIP_SRC): build an image first, or set LWIP_SRC"; exit 1; }
	@mkdir -p build/tests/netcaps
	cc -std=gnu11 -O1 -w -I targets/moto-g-pure/ip/test/real -I targets/moto-g-pure/ip -I $(LWIP_SRC)/include targets/moto-g-pure/ip/test/real/test_lwip.c $(addprefix $(LWIP_SRC)/core/,def.c dns.c inet_chksum.c init.c ip.c mem.c memp.c netif.c pbuf.c tcp.c tcp_in.c tcp_out.c timeouts.c udp.c ipv4/acd.c ipv4/dhcp.c ipv4/etharp.c ipv4/icmp.c ipv4/ip4.c ipv4/ip4_addr.c) $(LWIP_SRC)/netif/ethernet.c -o build/tests/netcaps/sockets_lwip
	build/tests/netcaps/sockets_lwip

# Radio's TLS client against a TLS server on this Mac's loopback (local, not in
# CI: it needs BearSSL's source, which an image build downloads). Seconds.
.PHONY: test-radio-tls
test-radio-tls:
	python3 services/radio/test_tls.py

.PHONY: test-net
test-net: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath targets/qemu/net-test-apps) $(PYTHON) tools/build_sel4.py qemu
	$(PYTHON) targets/qemu/test_net.py

# Open-app memory protection (local, not in CI): a probe app that writes to
# its code, runs its stack and heap, and asks for W+X memory must be stopped
# each time, and its stack, heap and mappings must move between launches.
.PHONY: test-wx
test-wx: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath targets/qemu/wx-test-apps) $(PYTHON) tools/build_sel4.py qemu
	$(PYTHON) targets/qemu/test_wx.py

.PHONY: test-ladybird
test-ladybird: qemu-image
	$(PYTHON) targets/qemu/test_ladybird.py

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
	@mkdir -p build/tests/netcaps
	@printf '#![no_std]\nextern crate alloc;\nextern crate std;\n#[path = "../../../runtime/open/slots.rs"]\nmod slots;\n#[path = "../../../runtime/open/names.rs"]\nmod names;\n' > build/tests/netcaps/open_net_test.rs
	"$(HOST_RUSTC)" --edition=2021 --test -O build/tests/netcaps/open_net_test.rs -o build/tests/netcaps/open_net
	build/tests/netcaps/open_net
	cc -std=gnu11 -O1 -Wall -Wextra -Wno-unused-parameter -I targets/moto-g-pure/ip/test targets/moto-g-pure/ip/test/test_sockets.c -o build/tests/netcaps/sockets_c
	build/tests/netcaps/sockets_c
	python3 tools/test_net_layout.py
	python3 tools/test_net_relay.py
	"$(HOST_RUSTC)" --edition=2021 --test -O services/power/policy.rs -o build/tests/power_policy
	build/tests/power_policy
	"$(HOST_RUSTC)" --edition=2021 --test -O -A dead_code runtime/open/tests/powerctl_host.rs -o build/tests/powerctl_host
	build/tests/powerctl_host
	@printf '#![no_std]\nextern crate alloc;\nextern crate std;\n#[path = "../../runtime/open/sntp.rs"]\nmod sntp;\n' > build/tests/sntp_test.rs
	"$(HOST_RUSTC)" --edition=2021 --test -O build/tests/sntp_test.rs -o build/tests/sntp
	build/tests/sntp
	@mkdir -p build/tests/memprot
	"$(HOST_RUSTC)" --edition=2021 --test -O runtime/open/tests/memprot_host.rs -o build/tests/memprot/host
	build/tests/memprot/host
	python3 tools/test_elf_check.py
	"$(HOST_RUSTC)" --edition=2021 --test -O targets/moto-g-pure/test_pmf.rs -o build/tests/wifi-pmf
	build/tests/wifi-pmf
	python3 apps/lib/python/test_violet.py
	HOST_RUSTC="$(HOST_RUSTC)" targets/qemu/test_shield_host.sh
	python3 image/packages/test_package_format.py
	HOST_RUSTC="$(HOST_RUSTC)" system/package-manager/test_verify.sh
	python3 kernel/sel4/test_delta.py
	@mkdir -p build/tests/storage
	"$(HOST_RUSTC)" --edition=2021 --test -O services/storage/tests/host.rs -o build/tests/storage/host
	build/tests/storage/host
	cd apps && RUSTC="$(abspath $(HOST_RUSTC))" "$(abspath $(HOST_CARGO))" test --release --locked --lib --bins -p violet -p home -p settings -p bitchat --target-dir ../build/host-tests

# Storage on the host, longer than CI's run (local, about a minute): more
# power-cut images per cut point and more random operations against the model.
.PHONY: test-storage-long
test-storage-long:
	@mkdir -p build/tests/storage
	"$(HOST_RUSTC)" --edition=2021 --test -O services/storage/tests/host.rs -o build/tests/storage/host
	STORAGE_CRASH_SEEDS=8 VIOLETFS_FUZZ_ITERS=400 build/tests/storage/host

# Package signatures at boot (local, not in CI): staged catalogs with a
# patched, unsigned or foreign-signed Radio must not boot.
.PHONY: test-packages
test-packages: qemu-image
	$(PYTHON) system/package-manager/test_signed_boot.py

# The boot image's version floor only shows with a component above version 1
# (local, not in CI): build an image whose Radio is version 2, run the same
# suite against it, then build the normal image again.
.PHONY: test-packages-floor
test-packages-floor: setup
	VIOLET_PACKAGE_VERSIONS=radio=2 $(PYTHON) tools/build_sel4.py qemu
	$(PYTHON) system/package-manager/test_signed_boot.py; status=$$?; $(PYTHON) tools/build_sel4.py qemu && exit $$status

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
	HOST_RUSTC="$(HOST_RUSTC)" $(PYTHON) targets/moto-g-pure/test_supplicant.py
	"$(HOST_RUSTC)" --edition=2021 --test targets/moto-g-pure/test_history.rs -o build/sel4/moto/test-history
	build/sel4/moto/test-history
	$(CC) -std=c11 -Wall -Wextra -Werror targets/moto-g-pure/test_radio_memory.c -o build/sel4/moto/test-radio-memory
	build/sel4/moto/test-radio-memory
	VIOLET_TEST_WIFI_BUNDLE="$(abspath build/sel4/moto/wifi-firmware.bin)" VIOLET_TEST_WLAN_IMAGE="$(abspath $(MOTO_WIFI_STOCK)/WIFI_RAM_CODE_soc1_0_1_1.bin)" "$(HOST_RUSTC)" --edition=2021 --test targets/moto-g-pure/test_radio.rs -o build/sel4/moto/test-radio
	build/sel4/moto/test-radio

# The handset's WPA2 client (hostap plus targets/moto-g-pure/supplicant.c)
# against an independent authenticator, with and without 802.11w. Local, not
# in CI: it needs hostap's sources (kept by `make moto`, or fetched here).
HOSTAP := wpa_supplicant-2.11
HOSTAP_SHA256 := 912ea06f74e30a8e36fbb68064d6cdff218d8d591db0fc5d75dee6c81ac7fc0a
build/sel4/$(HOSTAP)/src/rsn_supp/wpa.c:
	@mkdir -p build/sel4/downloads
	curl -fL https://w1.fi/releases/$(HOSTAP).tar.gz -o build/sel4/downloads/$(HOSTAP).tar.gz
	echo "$(HOSTAP_SHA256)  build/sel4/downloads/$(HOSTAP).tar.gz" | shasum -a 256 -c - || { rm -f build/sel4/downloads/$(HOSTAP).tar.gz; exit 1; }
	tar -xzf build/sel4/downloads/$(HOSTAP).tar.gz -C build/sel4

.PHONY: test-wpa
test-wpa: build/sel4/$(HOSTAP)/src/rsn_supp/wpa.c
	HOST_RUSTC="$(HOST_RUSTC)" python3 targets/moto-g-pure/test_supplicant.py

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
.PHONY: shield-setup shield-train test-shield-unit test-shield test-shield-proxy moto-shield
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
SHIELD_PROXY ?= Mac
test-shield-proxy: qemu-image
	$(PYTHON) targets/qemu/test_shield_proxy.py "$(SHIELD_PROXY)"
# Moto image with the demo apps for use with the Mac companion.
moto-shield: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath apps/shield-demo) $(PYTHON) tools/build_sel4.py moto
# The phone's own enforcement (docs/violet-shield-poc.md). All local, none in CI
# beyond the host tests `make test-unit` already runs (targets/qemu/test_shield_host.sh).
.PHONY: shield-train-tiny shield-fp-report test-shield-enforce
# Retrain the on-device scorer (standard library only, seconds) and regenerate
# runtime/open/scorer_model.rs and the fixtures; commit what changes.
shield-train-tiny:
	python3 tools/shield-trainer/train_tiny.py
# False-positive report: replay benign traces through the real policy and scorer.
# TRACES="a.log b.log", REPLAY="--mode strict --added" to change what is measured.
TRACES ?= $(wildcard targets/qemu/shield-traces/benign/*.log)
shield-fp-report:
	HOST_RUSTC="$(HOST_RUSTC)" targets/qemu/test_shield_host.sh replay $(REPLAY) $(TRACES)
# QEMU: the demo apps against an image whose Shield starts in strict mode. The
# normal image is rebuilt afterwards so later tests do not run strict.
test-shield-enforce: $(SHIELD_PY)
	VIOLET_SHIELD_MODE=strict $(MAKE) qemu-image
	$(SHIELD_PY) targets/qemu/test_shield_enforce.py; code=$$?; $(MAKE) qemu-image; exit $$code
