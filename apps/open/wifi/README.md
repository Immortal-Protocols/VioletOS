# Wi-Fi

Open it from the **Wi-Fi** bar on Home (or from Programs). The app scans when it starts and lists the networks it found, one row per name, with a lock on WPA2 networks. Tap a network to join it:

- **WPA2-Personal**: type the password on the on-screen keyboard (letters, **123** and **#+=** cover every printable character; **Show** reveals it) and tap **Connect**. A wrong password is reported under the field.
- **Open** networks join with one tap. (On the handset this is new and not yet checked against a real router.)
- Networks marked **Not supported** use security Radio can't join yet (WPA3, Enterprise, WEP).

The password never leaves the app: it becomes the network's key (PBKDF2-SHA1 with the SSID, as WPA2 defines) and only the key goes to Radio. Networks that joined are kept in `/data/networks`, most recently joined first, so a known network joins with one tap; tap the connected network to **Disconnect** or **Forget** it.

Saved networks rejoin by themselves: a few seconds after boot, and again while none is joined, the Open Runtime scans and joins the most recently joined saved network in range (see [wifi.rs](../../../runtime/open/wifi.rs)). **Disconnect** pauses that until you join a network again. A saved key is never thrown away because a join failed. Only when the access point keeps rejecting the handshake (reason 52) does the app ask for the password again, and the new one replaces the old only once it works. A timeout or weak signal (reason 57) keeps the key.

`make test-wifi-app` drives all of this by touch in QEMU, across four boots: joining, rejoining by itself after a restart, and keeping the key through a handshake timeout. The app talks to the system through the `wifi` commands of [`/dev/violet/sys`](../../../runtime/open/README.md).
