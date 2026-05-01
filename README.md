# Pictray

Pictray is a small Rust tray app that keeps a hot history of copied images and dropped file paths.

It starts hidden, watches the clipboard for images, stores them locally, and lets you reopen or recopy recent items from a compact Slint window.

![Pictray screenshot](assets/readme-image.jpg)

## What it does

- Runs as a tray-first app with a hidden-on-start window
- Watches the clipboard and saves copied images automatically
- Deduplicates images and moves repeated items back to the front
- Copies the original filesystem path when you drop a file into the window
- Shows recent images, text files, and file paths in a small preview gallery
- Copies the latest stored item back to the clipboard with `Ctrl+Shift+C`
- Lets you paste from the open window with `Ctrl+V` or `Cmd+V`

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
4. Drag any file into the window to buffer its path and copy the full path to the clipboard.
5. Copy a stored image, text file, or file path back to the clipboard from the UI, tray menu, or `Ctrl+Shift+C`.

## Storage

Pictray stores files under your platform local data directory in:

- `pictray/originals`
- `pictray/thumbnails`
- `pictray/metadata`
- `pictray/file-refs`

## Stack

- Slint
- tray-icon
- global-hotkey
- arboard
- image
- blake3
