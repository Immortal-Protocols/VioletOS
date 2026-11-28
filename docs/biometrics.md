# Biometrics

VioletOS has no fingerprint or face unlock. The only way to unlock the phone is the passcode. This page says what exists, what biometric unlock would need on the Moto G Pure, and why this project cannot build or test it.

## What exists

- **The passcode.** Set in Settings and entered on the trusted system keypad, which no app can draw over or read. It is stretched with PBKDF2-HMAC-SHA-256 and a random salt. The attempt counter is saved before an attempt is checked, and after five failures the wait doubles from 30 seconds up to an hour. See [Open Runtime](../runtime/open/README.md), Trusted system UI.
- **The rear camera** (Hi-1336, 1280 × 720) through the Camera service. No app ships for it.

## What does not exist

| Capability | State |
|---|---|
| Fingerprint sensor driver | None. The sensor is never powered or addressed. |
| Fingerprint matcher | None. |
| Front camera | Not brought up. Only the rear sensor works. |
| Face matcher | None. |
| Protected template storage | None. |
| Liveness or spoof detection | None. |
| A second credential type in the lock | None. The lock takes a passcode only. |

## What fingerprint unlock would need

**1. The sensor driver.** Sensors of this kind sit on an SPI bus with their own power and interrupt lines; which part this handset carries has not been identified here. The command protocol is the sensor vendor's and is not public. Under Android the kernel driver does little more than power and SPI pass-through; the protocol lives in a closed vendor library.

**2. The matcher.** Turning a capture into a template and comparing templates is the vendor's algorithm, shipped as a binary. On this phone it runs as a trusted application inside MediaTek's secure world, so that the normal world never sees a raw fingerprint image.

**3. Protected storage.** Templates must be sealed so that a compromised operating system cannot read or replace them. Under Android that is the secure world with replay-protected storage. VioletOS has no code in the secure world and no access to it.

**4. Liveness.** A matcher without spoof detection accepts a printed or moulded fingerprint.

## What face unlock would need

- The front camera brought up (its sensor, power sequence and tables are different from the rear one).
- A face matcher running on the device, and somewhere protected to keep its templates.
- A single ordinary camera has no depth. Face unlock from one is a convenience that a photograph can defeat, and would weaken the passcode it sits beside.

## Why this project cannot close the gap

- **It cannot be opened.** The sensor protocol, the matcher and the trusted application are closed binaries tied to the vendor's secure world. There is no source and no specification.
- **It cannot be tested here.** QEMU has no fingerprint sensor. Any test needs the handset, the sensor answering, and people's fingers.
- **A weak biometric is worse than none.** An unlock that a photograph or a lifted print defeats lowers the phone's security below the passcode alone.

## If the hardware becomes available

The rules a future biometric would have to follow, so it can be added behind the existing lock without weakening it:

- The passcode stays the root credential. A biometric is only a faster way back in.
- After every restart the first unlock is the passcode. No biometric before it.
- The passcode is required again after a set number of failed biometric attempts, after a set time without it, and for changing the passcode or any biometric.
- Biometric attempts go through the same attempt limiter as the passcode.
- Templates never leave protected storage, and no app sees a raw capture.
- Enrolment happens on the trusted system UI.

None of this is implemented. The lock takes only a passcode today.

## What must stay true in the product

- Settings offers no fingerprint or face option.
- Nothing describes the device as having biometric unlock.
