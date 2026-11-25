# Violet Relay (iPhone)

The companion app for VioletOS's phone relay. It joins the VioletOS network and accepts SOCKS5 connections from VioletOS. Each connection goes out over the iPhone's cellular data.

For Violet Shield monitoring and Home warnings, pair VioletOS with [Violet Agent for macOS](../mac/README.md).

On VioletOS, turn on **Phone Internet** in **Settings › Phone Link**. In Violet Relay, tap **Find Violet networks**, select the broadcast network, and enter the code from VioletOS. **Join by code** also works if you already have the code.

- **SOCKS5.** CONNECT only, for IPv4, IPv6 and domain targets, with username/password authentication. The username is `violet` and the password is the pairing secret. The app listens on port 1080 or the next free port up to 1089. iOS 27.2 holds 1080, so in practice it uses 1081.
- **Cellular only.** Outbound connections set `requiredInterfaceType = .cellular`. The listener refuses connections that arrive over cellular.
- **Joining.** `NEHotspotConfiguration` saves the network (`joinOnce = false`), and `NEHotspotNetwork.fetchCurrent` confirms the join.
- **Status.** `Documents/status.json` is updated every 2 s, and the app logs under the subsystem `com.immortal.violet.relay`.

`./run.sh` builds and installs the app, then runs a UI test that accepts the Local Network and Wi-Fi prompts. Other modes:
- `./run.sh launch`: restart the app.
- `./run.sh status`: print `status.json`.
- `JOIN_SSID=… JOIN_PASSPHRASE=… RELAY_SECRET=… ./run.sh join`: join a VioletOS network.
- `FORGET_SSID=… ./run.sh forget`: remove a saved network.

`services/usb-diagnostics/host/moto-relay` writes the matching values to `build/relay-pairing.json`.
