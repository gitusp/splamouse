:warning: On macOS Sonoma and later, the Pro Controller has input lag over Bluetooth.  
Connecting it via USB avoids the lag.

# joy

Fork of [Yamakaky/joy](https://github.com/Yamakaky/joy) that maintains `splamouse` and the Nintendo Switch controller library it depends on.

## Tools

- [`splamouse`](splamouse/README.md): mouse emulator like Splatoon (macOS only).

To run it from source: `cargo run --release --bin splamouse`.

## Libraries

- [`joycon`](crates/joycon): minimal driver for the Joy-Con and the Pro Controller, with only what `splamouse` uses (buttons, sticks, motion, battery level and player lights).
