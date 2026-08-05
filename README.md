# leddy-lib

Shared rendering primitives for Arduino-class simulations, Raspberry Pi agents,
servers, and tests. The renderer provides a monochrome framebuffer, a compact
5×7 font, physical LED-chain addressing, and time-based scrolling for text of
arbitrary length.

## Core APIs

- `render_message_frame` turns a validated message and elapsed time into the
  current frame, including `once`, `forever`, and counted repeat behavior.
- `scroll_cycle_duration_ms` exposes the deterministic playback cycle length.
- `FrameBuffer::device_order` maps logical top-left pixels into the configured
  physical chain order for all four origins and optional serpentine wiring.
- `FrameBuffer::row_major` remains the canonical logical layout for previews
  and protocol-neutral tests.

```sh
cargo test
```

This repository is a Zed package. Its resolver-generated `.zpkg.lock` will be
committed only after `leddy-interfaces` is published and resolvable.
