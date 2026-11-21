# seL4 kernel

seL4 schedules threads and enforces the memory mappings and communication rights that isolate VioletOS processes. The current [build script](build.py) pins upstream sources and applies the [board configuration](../../platform/sel4/board/). Run `make test-isolation` to check that the UI cannot directly access the tested touch and storage regions.
