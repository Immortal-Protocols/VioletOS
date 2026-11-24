# Built-in app catalog

[components.json](components.json) lists every package by its component ID: the built-in apps (launcher, stopwatch, Wi-Fi Setup, Sounds, Weather), Runtime, the services (touch, display, storage, radio, power, audio) and the Open Runtime. The handset build adds USB Diagnostics (21). The [top-level README](../../README.md#components) describes each one. The factory image embeds their validated packages. Staged catalogs can replace components and omit optional ones.

The guest package manager exposes the available stopwatch entry to the launcher through Runtime. Removing the stopwatch removes its launch button on the next boot. Run `make test-stack` to check removal and reinstallation.
