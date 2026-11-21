# Built-in app catalog

[components.json](components.json) identifies the launcher, stopwatch, Wi-Fi Setup, Runtime and five services. The factory image embeds their validated packages. Staged catalogs can replace components and omit optional ones.

The guest package manager exposes the available stopwatch entry to the launcher through Runtime. Removing the stopwatch removes its launch button on the next boot. Run `make test-stack` to check removal and reinstallation.
