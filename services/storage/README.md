# Storage service

Each app gets a private directory selected from its kernel-assigned identity. The [SDK](../../runtime/sdk.rs) can list, read, write, truncate and delete named files. Each directory holds four files of up to 4 KiB each, except the open apps' shared one, which holds 32 (the extra 28 sit after every directory's first four, in sectors 608–1112). The [filesystem](files.rs) writes data into an inactive bank and flushes it before committing a checksummed directory entry. Private checkpoint values retain their original two-sector disk format.

`make test-open-apps` checks that an open app's `/data` file survives a VM restart. System package updates use a separate boot-accessible volume, so the supervisor can recover the storage service itself.

**Installed apps.** The Open Runtime keeps programs installed by the agent in an extent of their own (the last 12 MiB of boot_b on the handset, beyond the flashed image; sectors 4096 onward in QEMU). Storage only reads and writes its sectors, up to 128 at a time, through a 64 KiB window shared with the Open Runtime (`APPSTORE` in [protocol.rs](../../runtime/protocol.rs)); the format is in [appstore.rs](../../runtime/open/appstore.rs).
