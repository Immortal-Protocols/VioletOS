# Built-in app catalog

[components.json](components.json) lists every package by its component ID: the boot launcher, Runtime, the services (touch, display, storage, radio, power, audio) and the Open Runtime. The handset build adds USB Diagnostics (21). The [top-level README](../../README.md#components) describes each one. The factory image embeds their validated packages. Staged catalogs can replace components and omit optional ones.

The phone's apps are not catalog packages: they are open apps in `/apps` (see the [Open Runtime](../../runtime/open/README.md)).
