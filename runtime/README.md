# Runtime

QEMU apps call the [Rust SDK](sdk.rs). The [Runtime process](process.rs) uses the endpoint badge assigned by seL4 to identify each caller. It controls focus, routes requests to the appropriate service, and lets only the foreground app present pixels or receive touches.

| App operation | Completion |
| --- | --- |
| Subscribe to contact, radio, power, volume or playback | Poll a persistent ticket until canceled. Contact reports new presses. The others report their initial state and the latest observed change. |
| Get a surface, checkpoint or device state | Poll a single-use ticket for the value or error. Each app can hold one outstanding request per service plus one local request. |
| Execute an operation | Acceptance returns a ticket. Await completion or discard the result. Independent workers wait for service replies. A pending radio request leaves display, touch, storage and app switching available. |

The version-one [wire format](protocol.rs) uses 14 ARM words. An app can write its own mapped surface between calls. Present requests name the changed rows.

Each app's surface holds a full 720×1600 frame. Classic apps draw 240×400 logical pixels in its first rows and Display scales them 3×4. An app that sets `HD_PRESENT` in its present request draws at the panel's full resolution, with a pitch of 720 pixels, and Display copies its rows unscaled. Music is the first HD app.

Volume, playback and music go to the [Audio service](../services/audio/README.md). Runtime calls it directly, because Audio answers between hardware polls and never waits on a device. Any app may read them; only the app on screen may change the volume, play a sound or control the music.
