# Settings

One app for the phone's settings, opened from its icon on Home:

- **Wi-Fi** (below).
- **Bluetooth**: whether the controller is on, and whether Bitchat is using it.
- **Phone Link**: Violet Shield's link to your iPhone, and Phone Internet: a pairing code to type into Violet Relay, **Turn on**/**Turn off** and **New code**. The choice and code are kept in `/data/relay`, and the Open Runtime turns Phone Internet back on after a restart.
- **Date & Time**: the city whose date Home shows (the shared setting `time.city`), and the time there now. That time is the system clock, once the network has set it, shifted by the city's own rule ([violet::time](../../lib/violet/src/time.rs): a POSIX TZ string per city, daylight saving included, checked against tzdata 2026f for 2026 to 2031); nothing is fetched. Until the clock is set the page says so instead of showing the build time. Home still takes its own time from wttr.in.
- **Agent Apps**: every app Violet Agent installed, with its size; **Delete** (tap twice to confirm) removes one, **Delete all** removes them all.
- **Updates** (last row): the running system's version and slot, and **Download and install**, which fetches a signed system update over HTTPS from the server the image was built with (`VIOLET_UPDATE_URL`; without one the page says so) and stages it in the other boot slot; the next start tries it ([updates.rs](src/updates.rs), [fetch.rs](src/fetch.rs); what is checked where is in the [package manager](../../../system/package-manager/README.md#full-system-updates-ota)). TLS trusts the Mozilla roots, plus a test CA only in an image built with `VIOLET_UPDATE_TEST_ROOT` (`make test-ota`).
- **Passcode**: **Set passcode**, **Change passcode** and **Turn off**. Settings only asks the Open Runtime (`passcode set`, `passcode off`); the runtime takes the screen (the band at the top turns violet and says `VIOLET SYSTEM`) and asks for the digits on its own keypad, so Settings never sees them. The phone then asks for the passcode at start and when the screen wakes ([Trusted system UI](../../../runtime/open/README.md)). No fingerprint or face unlock.
- **Power**: **Power off** (tap twice to confirm). Holding the power button offers the same from Home's power sheet.
- **About**: the system, battery, volume and network.

`SETTINGS_PREVIEW=dir cargo test -p settings` (in `apps/`) draws every page as raw 720 × 1600 RGBA, `WIFI_PREVIEW=dir` the Wi-Fi pages.

## Wi-Fi

Settings › Wi-Fi scans when it starts and lists the networks it found, one row per name, with a lock on WPA2 networks. Tap a network to join it:

- **WPA2-Personal**: type the password on the on-screen keyboard (letters, **123** and **#+=** cover every printable character; **Show** reveals it) and tap **Connect**. A wrong password is reported under the field.
- **Open** networks join with one tap. (On the handset this is new and not yet checked against a real router.)
- Networks marked **Not supported** use security Radio can't join yet (WPA3, Enterprise, WEP).

The password never leaves the app: it becomes the network's key (PBKDF2-SHA1 with the SSID, as WPA2 defines) and only the key goes to Radio. Networks that joined are kept in `/data/networks`, most recently joined first, so a known network joins with one tap; tap the connected network to **Disconnect** or **Forget** it.

Saved networks rejoin by themselves: a few seconds after boot, and again while none is joined, the Open Runtime scans and joins the most recently joined saved network in range (see [wifi.rs](../../../runtime/open/wifi.rs)). **Disconnect** pauses that until you join a network again. A saved key is never thrown away because a join failed. Only when the access point keeps rejecting the handshake (reason 52) does the app ask for the password again, and the new one replaces the old only once it works. A timeout or weak signal (reason 57) keeps the key.

`make test-wifi-app` drives Wi-Fi by touch in QEMU, across four boots: joining, rejoining by itself after a restart, and keeping the key through a handshake timeout. The app talks to the system through the `wifi` commands of [`/dev/violet/sys`](../../../runtime/open/README.md).
