# Runtime

QEMU apps call the [Rust SDK](sdk.rs). The [Runtime process](process.rs) uses the endpoint badge assigned by seL4 to identify each caller. It controls focus, routes requests to the appropriate service, and lets only the foreground app present pixels or receive touches. Who may ask for what is pinned by host tests at the end of [process.rs](process.rs) (run by `make test-unit` through [open/tests/broker_host.rs](open/tests/broker_host.rs)): only the launcher manages packages and power, an app can only hand the screen back to the launcher, only the app in front draws, plays or listens, and the complete list of what a background app can do.

| App operation | Completion |
| --- | --- |
| Subscribe to contact, radio, power, volume or playback | Poll a persistent ticket until canceled. Contact reports new presses. The others report their initial state and the latest observed change. |
| Get a surface, checkpoint or device state | Poll a single-use ticket for the value or error. Each app can hold one outstanding request per service plus one local request. |
| Execute an operation | Acceptance returns a ticket. Await completion or discard the result. Independent workers wait for service replies. A pending radio request leaves display, touch, storage and app switching available. |

The version-one [wire format](protocol.rs) uses 14 ARM words. An app can write its own mapped surface between calls. Present requests name the changed rows.

Volume and playback go to the [Audio service](../services/audio/README.md). Runtime calls it directly, because Audio answers between hardware polls and never waits on a device. Any app may read them; only the app on screen may change the volume or play a sound.
