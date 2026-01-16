# xpose

An Expose-like window switcher for X11 compatible with TWM.

By DARKGuy

## What is this?

A lightweight window switcher that displays all windows as scaled thumbnails in a grid layout, similar to macOS Expose. Made 100% with Claude. Works with TWM and other minimal window managers that don't support EWMH.

Features:
- Real-time window thumbnails using XComposite/XRender
- Live updates via XDamage extension
- Click to select and focus window
- Hover highlighting with cyan border
- Auto-scaling grid layout
- Works with TWM and similar minimal WMs

## Building

```bash
make
```

## Installation

```bash
sudo make install
```

Or install to a custom location:
```bash
make PREFIX=~/.local install
```

## Uninstall

```bash
sudo make uninstall
```

## Usage

```bash
# Run (shows all windows, click to select)
xpose

# Debug mode
RUST_LOG=debug xpose
```

Press Escape to dismiss without selecting a window.

## Keybindings with TWM

Add to your `.twmrc`:

```
"Tab" = mod4 : all : !"xpose"
```

## License

MIT
