# Launcher

Component 1, the boot process. It has no screen: as soon as Runtime answers, it hands the screen to the [Open Runtime](../../runtime/open/README.md), which starts [Home](../open/home) and the other apps. It stays running to restore QEMU's saved virtual-radio choice and, on the handset, to start the Wi-Fi scans that USB `join-wifi` requests need (Radio takes those only through this process).

Wi-Fi, Bluetooth, Phone Internet and the rest of the phone's settings are in the [Settings](../open/settings) app.
