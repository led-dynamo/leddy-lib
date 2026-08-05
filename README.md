# leddy-lib

Shared rendering primitives for Arduino-class simulations, Raspberry Pi agents,
servers, and tests. The initial renderer provides a monochrome framebuffer, a
compact 5×7 font, serpentine addressing, and time-based scrolling for text of
arbitrary length.

```sh
cargo test
```

This repository is a Zed package. Its resolver-generated `.zpkg.lock` will be
committed only after `leddy-interfaces` is published and resolvable.
