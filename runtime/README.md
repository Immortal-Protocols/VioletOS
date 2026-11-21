# Runtime

QEMU apps call the [Rust SDK](sdk.rs). The [Runtime process](process.rs) uses the endpoint badge assigned by seL4 to identify each caller. Its [router](router.rs) controls focus, routes requests to the appropriate service, and lets only the foreground app present pixels or receive touches.

The [Runtime proofs](verify/README.md) check with Kani that, from any reachable state, no app receives another app's touches or service replies, presents while in the background, or reaches another app's storage. CI runs them on every change to `runtime/`.

| App operation | Completion |
| --- | --- |
| Subscribe to contact, radio or power | Poll a persistent ticket until canceled. Contact reports new presses. Radio and power report their initial state and the latest observed change. |
| Get a surface, checkpoint or device state | Poll a single-use ticket for the value or error. Each app can hold one outstanding request per service plus one local request. |
| Execute an operation | Acceptance returns a ticket. Await completion or discard the result. Independent workers wait for service replies. A pending radio request leaves display, touch, storage and app switching available. |

The version-one [wire format](protocol.rs) uses 14 ARM words. An app can write its own mapped surface between calls. Present requests name the changed rows.
