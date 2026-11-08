# Axo Terminal (`axo`)

[English](README.md) | [简体中文](README.zh-CN.md)

A lightweight, modular, GPU-accelerated (Vulkan via `ash`, Metal via `wgpu`) terminal emulator built on [aegle](https://github.com/antinomie1/aegle), supporting Linux, Windows, and macOS.

| Platform | Windowing / Input | PTY | Terminal Renderer |
| --- | --- | --- | --- |
| Linux | SDL3 (`aegle-platform-sdl3`, Wayland & X11) | `openpty` + `fork` | Vulkan (`ash`) |
| Windows 10 1809+ | Win32 (`aegle-platform-win32`, TSF IME) | ConPTY | Vulkan (`ash`) |
| macOS | AppKit (`aegle-platform-macos`, `NSTextInputClient`) | `openpty` + `fork` | wgpu (Metal) |

Backends are conditionally compiled based on the target platform: PTY differentiates between Unix and Windows inside `axo-term`, Vulkan is compiled only on Linux and Windows, and wgpu only on macOS, each drawing on the device aegle presents with.

## Features

- **High Performance & Zero Idle Overhead**: Hand-written zero-allocation VT state machine parser with an ASCII batch fast path and atomic dirty flag merging; paced by compositor frame callbacks with strictly 0% idle CPU and a ~2.4 MiB release binary.
- **GPU Rendering**: One WGSL shader and a single instanced draw pipeline, through `ash` on Vulkan (SPIR-V precompiled at build time) and `wgpu` on Metal; zero-copy off-screen sRGB compositing.
- **Tabs & Arbitrary Splits**: Support for multiple tabs and arbitrary nested horizontal/vertical terminal split trees.
- **Image Protocols (Kitty + Sixel)**: Unified graphics storage and rendering pipeline. Kitty protocol supports direct/chunked transfer, PNG, zlib, and multi-layer z-index; Sixel supports RGB/HLS palettes. Images anchor to line numbers, evict with scrollback history, and GPU textures cache on demand with idle cleanup.
- **Native In-Place IME**: Cursor-anchored pre-edit layout with wide-character alignment and cursor tracking, integrated across SDL3, Windows TSF, and macOS NSTextInputClient.
- **Typography & Pixel-Perfect Box Drawing**: On-demand system font rasterization and CJK fallback shaping; pixel-perfect rendering for box drawing and block elements (`U+2500..=U+259F`).
- **Themes & Protocol Support**: Supports 24-bit TrueColor, alternate screen buffer, mouse reporting, and bracketed paste; text or files dropped onto a terminal are pasted, files as shell-quoted paths; includes built-in schemes (Kitty, Catppuccin, Tokyo Night, etc.) and native Kitty theme file compatibility.

## Directory Structure

| Crate | Responsibility |
| --- | --- |
| `crates/axo-term` | PTY (Unix: `openpty`/`fork`; Windows: ConPTY), hand-written VT state machine, OSC 0/2/7/10/11, Kitty & Sixel image protocols, primary/alternate screen and circular scrollback, selections, keyboard & mouse protocol encoding |
| `crates/axo-render` | `Renderer` trait, color schemes (`palette`), quad layouts (`quads`), glyphs & box drawing (`glyphs`/`boxdraw`), `VulkanRenderer` (`ash`, image texture cache), `WgpuRenderer` (macOS) |
| `crates/axo` | Configuration read/write, split tree (`layout`), context menus (`menu`), terminal/settings blocks (`blocks`), tab workspaces, and keybindings |

## Building & Running

- Linux: Requires `libSDL3` (dynamically loaded at runtime), system fonts (Fontconfig / FreeType), and a Vulkan 1.1+ driver.
- Windows: Requires a Vulkan 1.1+ driver with `VK_KHR_external_memory_win32` for transparency, and Windows 10 1809+ (ConPTY). `axo.exe` is a GUI program and opens no console window; with `opacity` below 1 the window presents through DirectComposition, since Win32 Vulkan swapchains composite only opaquely.
- macOS: Requires Metal. Windows are always transparent-capable (a non-opaque window with a translucent Metal layer), so `opacity` applies at once.

```sh
cargo run --release -p axo
```

Run tests and clippy:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Keybindings

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+T` | New tab (disabled when tab bar is hidden) |
| `Ctrl+Shift+D` | Split right (new terminal block) |
| `Ctrl+Shift+E` | Split down (new terminal block) |
| `Ctrl+Shift+W` | Close current focused block (closes tab if it is the last block) |
| `Ctrl+,` | Open / focus settings block in current tab |
| `Alt+Left/Right/Up/Down` | Move focus between split blocks in current tab |
| `Ctrl+Tab` / `Ctrl+PageDown` | Next tab |
| `Ctrl+Shift+Tab` / `Ctrl+PageUp` | Previous tab |
| `Alt+1..9` | Switch to tab 1..9 |
| Middle-click tab | Close tab |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy selection / paste clipboard (also supports middle-click paste and bracketed paste `2004`) |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Increase / decrease / reset font size |
| `Ctrl+Scroll` (in terminal) | Increase / decrease font size (1pt step, synchronized across all terminals) |
| `Shift+PageUp` / `Shift+PageDown` | Scroll history page up / down (also supports mouse scroll wheel) |

## Configuration

Path: `$XDG_CONFIG_HOME/axo/axo.conf` on Unix (defaults to `~/.config/axo/axo.conf`), `%APPDATA%\axo\axo.conf` on Windows. Plain `key = value` format:

```ini
font_family = monospace
font_size = 14
line_height = 1
theme = kitty
opacity = 1
padding = 6
cursor = block
scrollback = 10000
tab_bar = true
shell =
```

## Color Schemes & Themes

`theme` supports built-in theme names (`kitty` by default), theme names in the `themes` directory (`$XDG_CONFIG_HOME/axo/themes/*.conf`, Kitty format), or an absolute path to a theme file.

Built-in themes: `kitty`, `axo-dark`, `axo-light`, `catppuccin-mocha`, `dracula`, `gruvbox-dark`, `gruvbox-light`, `nord`, `one-dark`, `solarized-dark`, `solarized-light`, `tokyo-night`.

Individual colors can be overridden in `axo.conf` (e.g. `color0`..`color15`, `background`, `foreground`, `cursor`) without copying the entire theme.

## License

Licensed under the [BSD 2-Clause License](LICENSE).
