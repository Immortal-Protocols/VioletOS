# Radio service

The launcher controls airplane mode. Wi-Fi Setup uses Runtime to associate with the modeled open network and request a [gateway test](gateway.rs). The radio process owns a virtio network device and a dedicated DMA page. It sends an ARP request for 10.0.2.2 and verifies the reply's sender, destination and operation. A missing response times out.

QEMU uses a restricted user-mode network. A passing test establishes local guest-to-gateway connectivity. Nearby Wi-Fi discovery, WPA authentication and hardware radio testing require a physical driver.
