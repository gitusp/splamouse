<h3 align="center">
  English | <a href="https://github.com/gitusp/splamouse/blob/master/splamouse/README-ja.md">日本語</a>
</h3>

# Splamouse

This turns a Pro Controller / Joy-con into a mouse which supports gyro aiming!  
The behavior is heavily inspired by Splatoon.

https://github.com/gitusp/splamouse/assets/2055516/4228c0da-a27c-4db2-b749-54f1cd394ac9

## Features

| Control | Behavior                                                                  |
|---------|---------------------------------------------------------------------------|
| Motion  | Mouse move(like gyro aiming)                                              |
| L-stick | Mouse wheel                                                               |
| R-stick | Mouse move                                                                |
| Buttons | Bound in [the config file](#configuration) (they do nothing by default)   |

## How to use

- Connect your controller to your computer.
    - **USB is recommended for the Pro Controller.** On macOS Sonoma and later, Bluetooth adds noticeable input lag.
        - Use a cable that supports data transfer. Charge-only cables don't work.
        - After plugging in the cable, press a button to turn the controller on.
    - Connect Joy-Cons via Bluetooth.
- Download the latest binary from [Releases](https://github.com/gitusp/splamouse/releases).
    - There's for Apple Silicon(splamouse-macos-apple_silicon.zip) and for x86_64(splamouse-macos-x86_64.zip) binaries.
- Double click the downloaded zip to unarchive.
- Open the unarchived binary **from a context menu** by clicking it with the right mouse button.
    - A security dialog may pop up, allow the program to be opened.
    - A Terminal window should pop up once allowed.
    - Terminal app should request an accessibility permission for mouse operations, allow it.
- Then you can control the cursor with your controller. :tada:
- To use the buttons, create a config file as described in [Configuration](#configuration).

### Configuration

Put a config file at `~/.config/splamouse/config.toml` to set the sensitivity and the button bindings (`$XDG_CONFIG_HOME/splamouse/config.toml` if `XDG_CONFIG_HOME` is set).  
No button is bound by default, so only the buttons you write do something. The sensitivity is 0.0 (the default) unless you write it.

Here is a config that binds the buttons as in the previous versions:

```toml
[sensitivity]
# Between -5.0 and 5.0. 0.0 is the default.
gyro = 0.0
stick = 0.0

[buttons]
a = "cmd+right"         # browser forward
b = "cmd+left"          # browser back
x = "cmd"
y = "shift"
l = "alt"
r = "click:right"       # right click on release
zl = "hold:left"        # drag
zr = "click:left"       # left click on release
minus = "cmd+w"         # close the browser tab
plus = "cmd+r"          # browser reload
left = "ctrl+shift+tab" # previous tab
right = "ctrl+tab"      # next tab
down = "cmd+c"          # copy
up = "cmd+v"            # paste
lstick = "none"
rstick = "enter"        # press the R-stick
capture = "cmd+z"       # undo
home = "cmd+shift+z"    # redo
```

The values you can bind:

| Value                    | Behavior                                                      |
|--------------------------|---------------------------------------------------------------|
| e.g. `"cmd+shift+z"`     | Hold the keys while pressed (joined with `+`)                 |
| `"hold:left"`            | Hold the mouse button while pressed (drag)                    |
| `"click:left"`           | Click on release                                              |
| `"center"`               | Smoothly move the pointer to the center of the main display   |
| `"none"`                 | Do nothing (same as not writing it)                           |

- Keys: `cmd` `shift` `ctrl` `alt` (`opt`), `a`-`z` `0`-`9` and symbols (sent as the key at that position on the US layout), `left` `right` `up` `down` `enter` `tab` `space` `esc` `backspace` (⌫) `delete` (⌦) `home` `end` `pageup` `pagedown` `f1`-`f20`
- Mouse buttons: `left` `right` `back` `forward`

splamouse reads the config file on launch. If it has an error, splamouse shows where it is and waits until it's fixed.

### Start automatically at login

You can keep splamouse running in the background, even while no controller is connected. Register it from the terminal:

```sh
splamouse install
```

- splamouse starts right away and at every login, and picks up the controller whenever it connects.
- macOS asks for the accessibility permission again, this time for splamouse itself instead of the terminal app. Allow it.
- Don't run splamouse from the terminal at the same time.
- Keep the binary where it was when you ran `install`. After moving or updating it, run `splamouse install` again.
    - After an update, the old accessibility permission may no longer apply. If the cursor doesn't move, remove splamouse from the list with "−" and allow it again.
- Logs go to `~/Library/Logs/splamouse.log`.
- To restart it: `launchctl kickstart -k gui/$(id -u)/com.github.gitusp.splamouse`
    - Restart it after editing the config file.
- To stop it: `splamouse uninstall`

## Tips

- On launch, splamouse prints the config file it loaded and whether the controller is connected via USB or Bluetooth.
- If the cursor doesn't move, make sure the terminal app running splamouse has the accessibility permission (System Settings > Privacy & Security > Accessibility).
- Your controller may lose connection when there's no interaction for a while.
    - Try pushing buttons and wiggling sticks to reconnect.
- Reconnect your controller and restart this program if the connection problem still remains.
    - It is more reliable to delete the device once when reconnecting.
