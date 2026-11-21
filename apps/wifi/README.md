# Wi-Fi Setup

Open **Wi-Fi Setup**, choose **Next Network**, then tap **Connect**. The app saves the selected profile in its private checkpoint. On QEMU, **Test Radio** checks the gateway and displays a fresh UUID from the configured HTTPS endpoint. You can use Home and Stopwatch while the test runs. [Radio service](../../services/radio/README.md)

On the Moto G Pure XT2163DL, provision a WPA2-Personal profile for the current boot with `tools/moto-wifi-profile`. Once connected, **Test Radio** fetches the UUID over the handset’s Wi-Fi link. The same request can be triggered and verified from the Mac over USB. [Moto setup and hardware validation](../../targets/moto-g-pure/README.md)
