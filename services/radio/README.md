# Radio service

The launcher controls airplane mode. Wi-Fi Setup uses Runtime to associate with the modeled open network and request a [gateway test](gateway.rs). The radio process owns a virtio network device and a dedicated DMA page. It sends an ARP request for 10.0.2.2 and verifies the reply's sender, destination and operation. A missing response times out.

QEMU uses a restricted user-mode network. After the gateway replies, the guest sends a tagged Ethernet request to the emulator peer. The peer fetches the fixed UUID endpoint over HTTPS, validates the response and returns it over the same virtual network. Nearby Wi-Fi discovery, WPA authentication and hardware radio testing require a physical driver.
