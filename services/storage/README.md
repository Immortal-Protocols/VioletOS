# Storage service

The [QEMU storage process](storage.rs) saves one durable value per app. Runtime selects the journal using the sender identity assigned by seL4. The stopwatch keeps its original disk format in sectors 0 and 1. The launcher stores its airplane-mode preference in sectors 2 and 3.

Run `make test-storage` to check that both apps recover their own values after reboot, including a torn checkpoint. General files and a handset block driver remain future work.
