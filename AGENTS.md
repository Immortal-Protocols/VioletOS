# Notes for coding agents

Start with [README.md](README.md): how to build, run and test, the architecture, and where to change the code.

- **Keep CI under 5 minutes.** CI runs only `make test-unit` and `make test-ci` (two QEMU tests). Don't add QEMU suites to `test-ci` or new jobs to `.github/workflows/ci.yml`. If CI must cover something, extend an existing test or add a host test that takes seconds. Leave apps CI doesn't need out of its build (`VIOLET_OPEN_SKIP`).
- **Keep the local loop under 10 minutes.** `make test-local` is the loop: `test-unit` (37 s measured, the host integration tests 6 s of it), `test-system` (the one QEMU end-to-end test, two boots; 127 s measured before its last additions) and `test-ci` (38 s). The parts were timed separately; the loop as one command has not been. Run it before pushing.
- **Test with a host integration test, not a new QEMU suite.** Add a few lines to [tests/integration](tests/integration/README.md): each test builds its own phone in memory from the shipped sources, so tests run in parallel in milliseconds and in CI. Keep new logic callable from the host: put hardware and kernel access behind a narrow function or trait so the framework can fake only that edge. Only what needs the real kernel, page tables or boot path goes to QEMU, into [targets/qemu/test_system.py](targets/qemu/test_system.py) within its two boots. Do not add `targets/qemu/test_*.py` scripts or `make test-*` targets. [docs/test-map.md](docs/test-map.md) records where each old QEMU assertion went.
- Rust is the preferred language for new VioletOS code.
- Handset work: `make moto`, then `services/usb-diagnostics/host/moto-flash`. `build/moto-package` talks to the phone over USB.
