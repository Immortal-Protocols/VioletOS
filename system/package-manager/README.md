# Package manager

The proposed package manager checks compatibility, stages installs and replacements, and records intentional removals. It coordinates activation with the supervisor so a failed replacement can roll back and an uninstalled component stays stopped. Test both outcomes using disposable packages and a QEMU disk.
