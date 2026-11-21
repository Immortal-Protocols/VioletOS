# Display service

Today the [UI](../../platform/sel4/stopwatch/ui.c) draws into a mapped framebuffer. The proposed display process would provide writable surfaces through Runtime and accept commands to present one. A test should draw a known pattern and verify which surface appears on screen.
