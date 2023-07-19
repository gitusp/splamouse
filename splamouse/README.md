<h3 align="center">
  English | <a href="https://github.com/gitusp/splamouse/blob/master/splamouse/README-ja.md">日本語</a>
</h3>

# Splamouse

This turns a Pro Controller / Joy-con into a mouse which supports gyro aiming!  
The behavior is heavily inspired by Splatoon.

https://github.com/gitusp/splamouse/assets/2055516/4228c0da-a27c-4db2-b749-54f1cd394ac9

## Features

| Control        | Behavior                                    |
|----------------|---------------------------------------------|
| Motion         | Mouse move(like gyro aiming)                |
| L-stick        | Mouse wheel                                 |
| R-stick        | Mouse move                                  |
| L              | ⌘ + R(browser reload)                       |
| R              | Right click on release                      |
| ZR             | Left click on release                       |
| ZL             | Drag                                        |
| A              | ⌘ + Right arrow(browser forward)            |
| B              | ⌘ + Left arrow(browser back)                |
| X              | ⌘                                           |
| Y              | Shift                                       |
| Minus          | ⌘ + w(browser tab close)                    |
| Plus           | ⌘ + t(browser tab open)                     |
| Left           | Control + Shift + Tab(browser previous tab) |
| Right          | Control + Tab(browser next tab)             |
| Down           | ⌘ + c(copy)                                 |
| Up             | ⌘ + v(paste)                                |
| L-stick(press) | Middle click                                |
| R-stick(press) | Enter                                       |
| Capture        | ⌘ + z(undo)                                 |
| Home           | ⌘ + Shift + z(redo)                         |

## How to use

- Connect your controller to your computer via Bluetooth.
- Download the latest binary from [Releases](https://github.com/gitusp/splamouse/releases).
    - There's for Apple Silicon(splamouse-macos-apple_silicon.zip) and for x86_64(splamouse-macos-x86_64.zip) binaries.
- Double click the downloaded zip to unarchive.
- Open the unarchived binary **from a context menu** by clicking it with the right mouse button.
    - A security dialog may pop up, allow the program to be opened.
    - A Terminal window should pop up once allowed.
    - Terminal app should request an accessibility permission for mouse operations, allow it.
- Then you can control the cursor with your controller. :tada:

### Options

Launching this app directly from the terminal, you can set the sensitivity by specifying the following arguments.  
(Values should be between -5.0 and 5.0. If nothing is specified, 0.0 will be used.)

```sh
splamouse --gyro=-3.0 --stick=4.5
```

## Tips

- Your controller may lose connection when there's no interaction for a while.
    - Try pushing buttons and wiggling sticks to reconnect.
- Reconnect your controller and restart this program if the connection problem still remains.
    - It is more reliable to delete the device once when reconnecting.
