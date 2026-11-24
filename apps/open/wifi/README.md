# Wi-Fi

Open it from the **Wi-Fi** bar on Home (or from Programs). The app scans when it starts and lists the networks it found, one row per name, with a lock on WPA2 networks. Tap a network to join it:

- **WPA2-Personal**: type the password on the on-screen keyboard (letters, **123** and **#+=** cover every printable character; **Show** reveals it) and tap **Connect**. A wrong password is reported under the field.
- **Open** networks join with one tap (QEMU only for now: the handset's Radio joins WPA2 networks).
- Networks marked **Not supported** use security Radio can't join yet (WPA3, Enterprise, WEP).

The password never leaves the app: it becomes the network's key (PBKDF2-SHA1 with the SSID, as WPA2 defines) and only the key goes to Radio. Keys of networks that joined are kept in the app's `/data/networks`, so a known network joins with one tap; tap the connected network to **Disconnect** or **Forget** it.

`make test-wifi-app` drives all of this by touch in QEMU. The app talks to the system through the `wifi` commands of [`/dev/violet/sys`](../../../runtime/open/README.md).
