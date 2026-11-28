.DEFAULT_GOAL := moto
.PHONY: build moto emulator qemu-image test-emulator sim setup
PYTHON ?= python3
VENV := build/sel4/venv
build: moto
moto: setup
	$(PYTHON) tools/build_sel4.py moto
qemu-image: setup
	$(PYTHON) tools/build_sel4.py qemu
# The AArch64 bring-up image (targets/qemu64/README.md). Local only: CI neither builds nor boots it.
# QEMU64_STACK=1 also builds the VioletOS system for AArch64 (targets/qemu64/stack.py,
# build/sel4/qemu-aarch64) and `make qemu64` then checks the layers it reaches
# (targets/qemu/test_system.py --arch aarch64) and drives Home, Settings and a
# setting kept across a restart (test_open_apps.py --arch aarch64, two boots).
.PHONY: qemu64-image qemu64
qemu64-image: setup
	$(PYTHON) tools/build_sel4.py qemu64
	$(if $(QEMU64_STACK),$(PYTHON) tools/build_sel4.py qemu-aarch64)
qemu64: qemu64-image
	$(PYTHON) targets/qemu64/run.py --expect
	$(if $(QEMU64_STACK),$(PYTHON) targets/qemu/test_system.py --arch aarch64)
	$(if $(QEMU64_STACK),OPEN_FORCE_OFFLINE=1 $(PYTHON) targets/qemu/test_open_apps.py --arch aarch64)
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

# Rose as the agent, end to end (needs Codex signed in and companion/mac/violet_rose/setup.sh).
test-rose:
	VIOLET_AGENT_RELAY=http://violet-relay.test VIOLET_AGENT_STUN=violet-relay.test:3478 VIOLET_OPEN_NO_PYTHON=1 $(MAKE) qemu-image
	$(PYTHON) targets/qemu/test_rose.py

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
	@# The integration tests (tests/integration) first. They also run the unit
	@# tests inside inflate.rs, slots.rs, names.rs, loopback.rs, sockaddr.rs,
	@# sntp.rs, lifecycle.rs and the other modules open/mod.rs there builds.
	HOST_RUSTC="$(HOST_RUSTC)" tests/integration/run.sh
	"$(HOST_RUSTC)" --edition=2021 --test -O services/audio/mic.rs -o build/tests/mic
	build/tests/mic
	@mkdir -p build/tests/netcaps
	cc -std=gnu11 -O1 -Wall -Wextra -Wno-unused-parameter -I targets/moto-g-pure/ip/test targets/moto-g-pure/ip/test/test_sockets.c -o build/tests/netcaps/sockets_c
	build/tests/netcaps/sockets_c
	python3 tools/test_net_layout.py
	python3 tools/test_net_relay.py
	"$(HOST_RUSTC)" --edition=2021 --test -O services/power/policy.rs -o build/tests/power_policy
	build/tests/power_policy
	"$(HOST_RUSTC)" --edition=2021 --test -O -A dead_code runtime/open/tests/powerctl_host.rs -o build/tests/powerctl_host
	build/tests/powerctl_host
	@mkdir -p build/tests/power2
	cc -std=gnu11 -O1 -Wall -Wextra -I platform/sel4/include system/supervisor/tests/timekeeper_test.c -o build/tests/power2/timekeeper_test
	build/tests/power2/timekeeper_test
	@# The supervisor's AArch64 allocator (memory.h), against a model kernel.
	@mkdir -p build/tests/aarch64-supervisor
	cc -std=gnu11 -O1 -Wall -Wextra -I system/supervisor/tests/model -I platform/sel4/include system/supervisor/tests/memory_test.c -o build/tests/aarch64-supervisor/memory_test
	build/tests/aarch64-supervisor/memory_test
	"$(HOST_RUSTC)" --edition=2021 --test -O targets/moto-g-pure/test_power_tables.rs -o build/tests/power2/power_tables_test
	build/tests/power2/power_tables_test
	python3 tools/test_power_tables.py
	@mkdir -p build/tests/memprot
	"$(HOST_RUSTC)" --edition=2021 --test -O runtime/open/tests/memprot_host.rs -o build/tests/memprot/host
	build/tests/memprot/host
	@mkdir -p build/tests/scale
	"$(HOST_RUSTC)" --edition=2021 --test -O runtime/open/tests/paging_host.rs -o build/tests/scale/paging4
	build/tests/scale/paging4
	"$(HOST_RUSTC)" --edition=2021 --test -O --cfg page64 runtime/open/tests/paging_host.rs -o build/tests/scale/paging64
	build/tests/scale/paging64
	@printf '#![no_std]\nextern crate alloc;\nextern crate std;\n#[path = "../../../runtime/open/cores.rs"]\nmod cores;\n' > build/tests/scale/cores_test.rs
	"$(HOST_RUSTC)" --edition=2021 --test -O build/tests/scale/cores_test.rs -o build/tests/scale/cores
	build/tests/scale/cores
	"$(HOST_RUSTC)" --edition=2021 --test -O runtime/open/tests/iov_host.rs -o build/tests/scale/iov
	build/tests/scale/iov
	"$(HOST_RUSTC)" --edition=2021 --test -O runtime/open/tests/memres_host.rs -o build/tests/memprot/memres_host
	build/tests/memprot/memres_host
	python3 runtime/open/tests/test_page_rights.py
	@mkdir -p build/tests/tcbsplit
	"$(HOST_RUSTC)" --edition=2021 --test -O runtime/open/tests/tcb_host.rs -o build/tests/tcbsplit/tcb_host
	build/tests/tcbsplit/tcb_host
	"$(HOST_RUSTC)" --edition=2021 --test -O -A non_upper_case_globals runtime/open/tests/broker_host.rs -o build/tests/tcbsplit/broker_host
	build/tests/tcbsplit/broker_host
	cc -std=gnu11 -O1 -Wall -Wextra system/supervisor/test_open_recover.c -o build/tests/tcbsplit/open_recover
	build/tests/tcbsplit/open_recover
	python3 tools/test_open_contain.py
	@mkdir -p build/tests/abi
	"$(HOST_RUSTC)" --edition=2021 --test -O runtime/open/tests/abi_host.rs -o build/tests/abi/host
	build/tests/abi/host
	python3 tools/test_elf_check.py
	"$(HOST_RUSTC)" --edition=2021 --test -O targets/moto-g-pure/test_pmf.rs -o build/tests/wifi-pmf
	build/tests/wifi-pmf
	@mkdir -p build/tests/bt-wifi
	"$(HOST_RUSTC)" --edition=2021 --test -O runtime/open/tests/hci_host.rs -o build/tests/bt-wifi/hci_host
	build/tests/bt-wifi/hci_host
	"$(HOST_RUSTC)" --edition=2021 --test -O targets/moto-g-pure/test_sae.rs -o build/tests/bt-wifi/sae
	build/tests/bt-wifi/sae
	@mkdir -p build/tests/trustedui
	"$(HOST_RUSTC)" --edition=2021 --test -O runtime/open/tests/sysui_host.rs -o build/tests/trustedui/sysui_host
	build/tests/trustedui/sysui_host
	@mkdir -p build/tests/display
	"$(HOST_RUSTC)" --edition=2021 --test -O system/compositor/tests/host.rs -o build/tests/display/host
	build/tests/display/host
	@mkdir -p build/tests/power-key
	@printf '#![no_std]\nextern crate alloc;\nextern crate std;\n#[path = "../../../runtime/open/lock.rs"]\nmod lock;\n' > build/tests/power-key/lock_test.rs
	"$(HOST_RUSTC)" --edition=2021 --test -O -A dead_code build/tests/power-key/lock_test.rs -o build/tests/power-key/lock
	build/tests/power-key/lock
	python3 apps/lib/python/test_violet.py
	HOST_RUSTC="$(HOST_RUSTC)" targets/qemu/test_shield_host.sh
	HOST_RUSTC="$(HOST_RUSTC)" tools/shield-trainer/test_shield_data.sh
	HOST_RUSTC="$(HOST_RUSTC)" tools/shield-trainer/test_pairing.sh
	HOST_RUSTC="$(HOST_RUSTC)" targets/moto-g-pure/test_eap.sh
	python3 image/packages/test_package_format.py
	HOST_RUSTC="$(HOST_RUSTC)" system/package-manager/test_verify.sh
	HOST_RUSTC="$(HOST_RUSTC)" system/package-manager/test_ota.sh
	python3 kernel/sel4/test_delta.py
	HOST_RUSTC="$(HOST_RUSTC)" runtime/arch/test_host.sh
	@# Every service package compiles and links for AArch64 (3 s; skipped without the target).
	HOST_RUSTC="$(HOST_RUSTC)" python3 services/check_aarch64.py --if-available
	@mkdir -p build/tests/bootchain
	HOST_RUSTC="$(HOST_RUSTC)" boot/loader/test_verified.sh
	python3 tools/test_phone_image.py
	$(CC) -std=gnu11 -O1 -Wall -Wextra -Werror targets/moto-g-pure/test_ellis_smc.c targets/moto-g-pure/test_ellis_reference.c -o build/tests/bootchain/ellis-smc
	build/tests/bootchain/ellis-smc
	$(CC) -std=c11 -Wall -Wextra -Werror targets/moto-g-pure/test_radio_memory.c -o build/tests/bootchain/radio-memory
	build/tests/bootchain/radio-memory
	@mkdir -p build/tests/storage
	"$(HOST_RUSTC)" --edition=2021 --test -O services/storage/tests/host.rs -o build/tests/storage/host
	build/tests/storage/host
	@mkdir -p build/tests/crypt
	"$(HOST_RUSTC)" --edition=2021 --test -O services/storage/crypt/tests/host.rs -o build/tests/crypt/host
	build/tests/crypt/host
	python3 tools/test_root_key.py
	@mkdir -p build/tests/policy
	"$(HOST_RUSTC)" --edition=2021 --test -O runtime/open/tests/policy_host.rs -o build/tests/policy/host
	build/tests/policy/host
	@mkdir -p build/tests/sentinel
	"$(HOST_RUSTC)" --edition=2021 --test -O -A dead_code system/sentinel/tests/sentinel_host.rs -o build/tests/sentinel/sentinel_host
	build/tests/sentinel/sentinel_host
	python3 tools/test_manifests.py
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

# The one QEMU end-to-end test of the local loop (not in CI): what needs the
# real kernel, MMU or boot path. One image (system version 2, a test CA for
# its update server, musl's loader and the ABI, W^X and policy test apps) and
# two boots through tools/ota_boot.py's model bootloader: the Linux ABI, W^X
# on real page tables and the policy through the real dispatch, a signed
# update staged in slot B; then B as a trial, marked successful. The normal
# image is built again afterwards. See targets/qemu/test_system.py and
# docs/test-map.md.
SYSTEM_TEST_APPS = $(abspath targets/qemu/abi-test-apps):$(abspath targets/qemu/wx-test-apps):$(abspath targets/qemu/policy-test-apps)
SYSTEM_TEST_ENV = VIOLET_SYSTEM_VERSION=2 VIOLET_UPDATE_URL=https://updates.violet.test/latest.vota VIOLET_OPEN_LDSO=1 VIOLET_OPEN_EXTRA_APPS=$(SYSTEM_TEST_APPS)
.PHONY: test-system
test-system: setup
	@mkdir -p build/tests/abi
	# One shell line: whatever step fails (the image build, the loader check
	# or QEMU), the normal image is built again, so no later run boots the
	# image that trusts the test update CA.
	$(SYSTEM_TEST_ENV) VIOLET_UPDATE_TEST_ROOT=$$($(PYTHON) targets/qemu/test_system.py --prepare) $(PYTHON) tools/build_sel4.py qemu \
	  && "$(HOST_RUSTC)" --edition=2021 --test -O runtime/open/tests/abi_host.rs -o build/tests/abi/host \
	  && VIOLET_ABI_REQUIRE_LIBC=1 build/tests/abi/host the_shipped_loader \
	  && $(PYTHON) targets/qemu/test_system.py; \
	  status=$$?; $(PYTHON) tools/build_sel4.py qemu && exit $$status

# Host integration tests (seconds; also run by test-unit, so by CI): the
# shipped sources of the Open Runtime, Storage, Power and the package
# manager built for this machine and joined by in-memory fakes of the kernel
# and hardware edges. See tests/integration/README.md.
.PHONY: test-integration
test-integration:
	HOST_RUSTC="$(HOST_RUSTC)" tests/integration/run.sh

# The local loop, everything to run before pushing: the host unit tests
# (with the integration tests), the one QEMU end-to-end test, then the two
# QEMU tests CI runs. test-system leaves the normal image behind, so test-ci
# builds nothing more.
.PHONY: test-local
test-local:
	$(MAKE) test-unit
	$(MAKE) test-system
	$(MAKE) test-ci

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

# WPA3-Personal SAE (targets/moto-g-pure/sae.rs) against hostap's own SAE on
# OpenSSL 3 as the access point, anti-clogging tokens included (local, not CI;
# a few seconds). Needs Homebrew's openssl@3 or OPENSSL_PREFIX.
OPENSSL_PREFIX ?= $(or $(shell brew --prefix openssl@3 2>/dev/null),/opt/homebrew/opt/openssl@3)
SAE_REF := common/sae common/dragonfly crypto/crypto_openssl crypto/sha256-prf crypto/sha256-kdf crypto/dh_groups utils/common utils/wpabuf utils/wpa_debug utils/os_unix ap/comeback_token
SAE_REF_FLAGS := -O1 -DCONFIG_SAE -DCONFIG_ECC -DCONFIG_SHA256 -DCONFIG_NO_RANDOM_POOL -DCONFIG_NO_STDOUT_DEBUG -Ibuild/sel4/$(HOSTAP)/src -Ibuild/sel4/$(HOSTAP)/src/utils -I$(OPENSSL_PREFIX)/include
.PHONY: test-sae
test-sae: build/sel4/$(HOSTAP)/src/rsn_supp/wpa.c
	@mkdir -p build/tests/bt-wifi/sae-hostap
	for f in $(SAE_REF); do cc $(SAE_REF_FLAGS) -w -c build/sel4/$(HOSTAP)/src/$$f.c -o build/tests/bt-wifi/sae-hostap/$$(basename $$f).o || exit 1; done
	cc $(SAE_REF_FLAGS) -Wall -c targets/moto-g-pure/test_sae_hostap.c -o build/tests/bt-wifi/sae-hostap/harness.o
	ar rcs build/tests/bt-wifi/sae-hostap/libsaeref.a build/tests/bt-wifi/sae-hostap/*.o
	"$(HOST_RUSTC)" --edition=2021 --test -O --cfg hostap targets/moto-g-pure/test_sae.rs -L build/tests/bt-wifi/sae-hostap -l static=saeref -L $(OPENSSL_PREFIX)/lib -l crypto -o build/tests/bt-wifi/sae-hostap/sae
	build/tests/bt-wifi/sae-hostap/sae

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
# The phone's own enforcement (docs/violet-shield-poc.md): host tests, all run by
# `make test-unit` (targets/qemu/test_shield_host.sh, tests/integration/scenarios/shield.rs).
.PHONY: shield-train-tiny shield-fp-report
# Retrain the on-device scorer (standard library only, seconds) and regenerate
# runtime/open/scorer_model.rs and the fixtures; commit what changes.
shield-train-tiny:
	python3 tools/shield-trainer/train_tiny.py
# False-positive report: replay benign traces through the real policy and scorer.
# TRACES="a.log b.log", REPLAY="--mode strict --added" to change what is measured.
TRACES ?= $(wildcard targets/qemu/shield-traces/benign/*.log)
shield-fp-report:
	HOST_RUSTC="$(HOST_RUSTC)" targets/qemu/test_shield_host.sh replay $(REPLAY) $(TRACES)
