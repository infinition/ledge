# Ledge

A docked sidebar for Windows. It sits on the edge of your screen and holds your
pinned apps, live system gauges, and small HTML widgets.

Ledge reserves screen space through the Windows AppBar API, the same mechanism
the taskbar uses. A maximized window stops at the edge of the bar instead of
hiding behind it.

## Features

- Pin apps, folders, URLs, and shell protocols (for example `ms-settings:`).
- Real application icons extracted from the executables, not emoji.
- Live gauges for CPU, RAM, and NVIDIA GPU (usage and VRAM).
- Taskbar-style control of pinned app windows: live thumbnails on hover, plus
  restore, minimize, and close. Those windows are removed from the Windows
  taskbar so you do not see them twice.
- Custom widget blocks written in plain HTML, CSS, and JavaScript.
- Multiple bars.
- Drag and drop to add and reorder items.
- Light and dark theme.
- Optional launch at startup.

## Requirements

- Windows 10 or 11.
- [WebView2 runtime](https://developer.microsoft.com/microsoft-edge/webview2/),
  which ships with current Windows versions.

## Install

Download `ledge.exe` from the [latest release](https://github.com/infinition/ledge/releases/latest)
and run it. There is no installer.

## Build from source

Requires the stable Rust toolchain for `x86_64-pc-windows-msvc`.

```bash
cargo build --release
```

The binary is written to `target/release/ledge.exe`.

## Configuration

Settings are stored as JSON and can be edited from the bar itself (right click
for the menu). The files live under:

```
%APPDATA%\ledge\config.json
%APPDATA%\ledge\widgets\
```

## Stopping

Close the bar from its right click menu, or end the `ledge.exe` process.
Ledge unregisters its AppBar slot on a clean exit, which returns the reserved
screen space. If it is killed without cleanup, restart Ledge or `explorer.exe`
to restore the work area.

## License

MIT. See [LICENSE](LICENSE).
