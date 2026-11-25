# Notes for coding agents

Start with [README.md](README.md): how to build, run and test, the architecture, and where to change the code.

- **Keep CI under 5 minutes.** CI runs only `make test-unit` and `make test-ci` (two QEMU tests). Don't add QEMU suites to `test-ci` or new jobs to `.github/workflows/ci.yml`. Put new QEMU tests behind their own `make` target and run them locally before pushing. If CI must cover something, extend an existing test or add a host unit test that takes seconds. Leave apps CI doesn't need out of its build (`VIOLET_OPEN_SKIP`).
- Rust is the preferred language for new VioletOS code.
- Handset work: `make moto`, then `services/usb-diagnostics/host/moto-flash`. `build/moto-package` talks to the phone over USB.
