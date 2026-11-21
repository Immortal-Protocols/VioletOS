# Storage service

Each app gets a private directory selected from its kernel-assigned identity. The [SDK](../../runtime/sdk.rs) can list, read, write, truncate and delete named files. Each directory currently holds four files of up to 4 KiB each. The [filesystem](files.rs) writes data into an inactive bank and flushes it before committing a checksummed directory entry. Private checkpoint values retain their original two-sector disk format.

Run `make test-storage` for checkpoint recovery and `make test-apps` to save and reopen an installed app’s file across VM restarts. System package updates use a separate boot-accessible volume, so the supervisor can recover the storage service itself.
