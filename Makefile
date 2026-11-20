.PHONY: sim emulator build
build/simulator: src/screen.c src/screen.h src/glyphs.h src/simulator.c
	mkdir -p build
	$(CC) -std=c11 -O2 -Wall -Wextra $(shell sdl2-config --cflags) src/screen.c src/simulator.c -o $@ $(shell sdl2-config --libs)
sim: build/simulator
	./build/simulator
build:
	python3 tools/image.py
emulator: build
	python3 tools/run.py
