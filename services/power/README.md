# Power service

The QEMU power process exposes battery level, sleep, wake and shutdown to Runtime. Tap **Sleep** on Home to blank the display and pause guest execution. The viewer resumes QEMU on the next press. For shutdown, open **Apps**, tap **Off**, then **Confirm**.

Run `make test-power` to send those controls through the guest UI. It checks the virtual machine’s paused and running states via QMP and waits for the guest process to exit after shutdown.
