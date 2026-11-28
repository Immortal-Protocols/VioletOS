# Built-in app catalog

[components.json](components.json) lists every package by its component ID: the boot launcher, Runtime, the services (touch, display, storage, radio, power, audio) and the Open Runtime. The handset build adds USB Diagnostics (21). The [top-level README](../../README.md#components) describes each one. Each entry's `version` is the package's security version: it is signed into the package, and the supervisor refuses a package older than the one the boot image ships or one that has already run ([package manager](../../system/package-manager/README.md#signatures-and-versions)). Raise it when a release fixes a security bug. The factory image embeds their signed packages. Staged catalogs can replace components and omit optional ones.

The phone's apps are not catalog packages: they are open apps in `/apps` (see the [Open Runtime](../../runtime/open/README.md)).
