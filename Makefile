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
	  $(PYTHON) platform/sel4/stopwatch/test.py --spi $$mode --watchdog-enabled; \
	done
build/simulator: src/screen.c src/screen.h src/glyphs.h targets/mac-simulator/simulator.c
	mkdir -p $(@D)
	$(CC) -std=c11 -O2 -Wall -Wextra $(shell sdl2-config --cflags) -Isrc src/screen.c targets/mac-simulator/simulator.c -o $@ $(shell sdl2-config --libs)
sim: build/simulator
	./build/simulator

.PHONY: test-storage test-isolation
test-storage: qemu-image
	$(PYTHON) platform/sel4/stopwatch/test_storage.py
test-isolation: setup
	VIOLET_TEST_ISOLATION=1 $(PYTHON) tools/build_sel4.py qemu
	@set -e; for region in touch storage dma scanout peer radio radio-dma; do \
	  $(PYTHON) platform/sel4/stopwatch/test.py --probe $$region; \
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
	$(PYTHON) platform/sel4/stopwatch/test.py --wifi live

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
