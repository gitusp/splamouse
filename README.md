:warning: On macOS Sonoma and later, the Pro Controller has input lag over Bluetooth.  
Connecting it via USB avoids the lag.

# joy

Fork of [Yamakaky/joy](https://github.com/Yamakaky/joy) that maintains `splamouse` and the Nintendo Switch controller libraries it depends on.

## External dependencies

On Linux, you'll need `libusb`, `libbluetooth` and `libudev`. On Ubuntu, you can install these by running:

```sh
sudo apt-get install libusb-1.0-0-dev libbluetooth-dev libudev-dev
```

## Tools

The tools can be run with `cargo run --bin <tool>`.

- [`splamouse`](splamouse/README.md): mouse emulator like Splatoon.

## Libraries

- [`joycon-sys`](https://yamakaky.github.io/joy/joycon_sys): decoding and encoding HID reports. Doesn't include any I/O.
- [`joycon`](https://yamakaky.github.io/joy/joycon): implements I/O and communication protocols on top of `joycon-sys`.
- `hid-gamepad-sys` / `hid-gamepad-types`: controller-agnostic gamepad traits and types used by `joycon`.
