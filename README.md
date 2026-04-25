# Pictray

Pictray is a small Rust tray app that keeps a hot history of added/copied images.

It starts hidden, watches the clipboard for images, stores them locally, and lets you reopen or recopy recent items from a compact Slint window.

![Pictray screenshot](assets/readme-image.jpg)

## What it does

- Runs as a tray-first app with a hidden-on-start window
- Watches the clipboard and saves copied images automatically
- Deduplicates images and moves repeated items back to the front
- Shows recent images in a small preview gallery
- Copies the latest stored image with `Ctrl+Shift+V`
- Lets you paste from the open window with `Ctrl+V` or `Cmd+V`
- Imports dropped image files, including GIFs

## Run

```bash
cargo run
```

## Build and test

```bash
cargo build
cargo test
cargo fmt --check
```

## How to use

1. Launch Pictray with `cargo run`.
2. Copy an image in another app.
3. Click the tray icon to open the window.
4. Copy a stored image back to the clipboard from the UI, tray menu, or `Ctrl+Shift+V`.

You can also drag image files into the window to import them.

## Storage

Pictray stores files under your platform local data directory in:

- `pictray/originals`
- `pictray/thumbnails`
- `pictray/metadata`

## Stack

- Slint
- tray-icon
- global-hotkey
- arboard
- image
- blake3
