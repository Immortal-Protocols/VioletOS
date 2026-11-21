# Runtime policy proofs

These proofs check that Runtime keeps apps apart when it routes touches, pixels, storage, radio and power requests. They use [Kani](https://model-checking.github.io/kani/), a model checker for Rust, on the same [router.rs](../router.rs) that ships in the Runtime package.

```sh
cd runtime/verify
cargo kani -Z stubbing -j 8 --output-format terse   # the proofs, about 10 minutes on 8 cores
cargo test        # concrete regression scenarios against the same model
```

## What is proven

For every request any caller can send, and every point at which a service worker can finish, starting from any state Runtime can reach:

| Policy | Checked in [model.rs](model.rs) |
| --- | --- |
| An app receives a touch only if it was read while that app was in front. | `reply`: a touch reply must be tagged with the receiving app. |
| Only the app in front, while awake, can ask the display to present, and only its own surface. | `check_request` on the display lane. The display worker also drops a present if focus changed after it was queued (`complete`). |
| A reply holds only the requesting app's service data, never another app's. This covers radio, storage, power and package replies. | `reply`: every service result is tagged with the app it was made for. |
| Storage requests carry the caller's identity, and different apps always reach different disk regions. | `check_request` on the storage lane, and `storage_regions_are_private`. |
| Only Wi-Fi Setup reaches the network. Only the launcher switches the radio, manages packages or changes the power state, apart from a tap waking the screen. | `check_request` on the radio, power and package lanes. |
| App switching follows the launcher rules. | `lifecycle`. |
| Every request gets exactly one reply, and nothing panics or overflows. | `reply`, and Kani's built-in checks. |

The proof is inductive. [proofs.rs](proofs.rs) shows that an invariant linking Runtime's state to the request cells holds after startup, and that it is preserved by the startup diagnostics step, by each request and by each worker step. So the policy holds after any number of steps, not only for short runs.

To keep solver runs short, each request harness fixes the calling app and the operation (7 apps × 6 operation groups), and starts from arbitrary state only in what that app's request can reach. This is not a weaker proof: the model fails if Runtime touches any other app's cells or client state, so everything outside stays as it was. `proofs.rs` explains the argument next to `local_invariant`.

There is one deliberate cross-app flow: every app's status screen shows **DISK READY** if the stopwatch checkpoint could be read at startup. `Out::stopwatch_checkpoint_loaded` marks it.

## Assumptions

The proof is only as good as these. Each one is a separate thing to check or trust.

- **seL4 and the supervisor:** the badge on each request identifies the caller, and only Runtime and the matching worker can write a lane's request page. The supervisor creates these pages fresh, so they start zeroed.
- **The request cells:** [lanes.rs](../lanes.rs) moves each cell through free, requested and completed using acquire and release ordering. The model treats each move as atomic. Kani cannot check memory ordering between threads.
- **The services:** each service answers a request using only the data it is meant to, and storage maps region numbers to separate disk areas. The touch service reports the panel's current state rather than a queue of older events.
- **Timing:** a present that the display worker has already started when focus changes can still finish. The next app's own present then replaces it.
- **Not covered:** the C transport ([transport.c](../transport.c)), the supervisor's update-record page read for the launcher, timing channels, and anything outside Runtime.
