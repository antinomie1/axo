# Axo Terminal (`axo`)

[English](README.md) | [简体中文](README.zh-CN.md)

基于 [aegle](https://github.com/antinomie1/aegle) 的轻量化、模块化 Vulkan（`ash`）加速终端模拟器，支持 Linux、Windows 与 macOS。

| 平台 | 窗口/输入 | PTY | 终端渲染 |
| --- | --- | --- | --- |
| Linux | SDL3（`aegle-platform-sdl3`，支持 Wayland 与 X11） | `openpty` + `fork` | Vulkan（`ash`） |
| Windows 10 1809+ | Win32（`aegle-platform-win32`，TSF 输入法） | ConPTY | Vulkan（`ash`） |
| macOS | AppKit（`aegle-platform-macos`，`NSTextInputClient`） | `openpty` + `fork` | 暂缺（无 GPU 渲染） |

按目标平台选择后端编译：PTY 在 `axo-term` 内区分 Unix 与 Windows，Vulkan 仅在 Linux 与 Windows 编译。macOS 接入窗口与输入法，GPU 渲染器尚未接入（启动提示后退出）。

## 特性

- **极致性能与零空闲开销**：手写零分配 VT 状态机解析（含 ASCII 批量快路径），原子脏位合并唤醒；帧回调驱动渲染，空闲 CPU 占用严格为 0%，Release 二进制约 2.4 MiB。
- **Vulkan GPU 加速渲染**：基于 `ash` 的单次实例化绘制管线，着色器构建期预编译，离屏 sRGB 纹理零拷贝合成。
- **多标签与任意分屏**：支持标签页及任意嵌套的水平/垂直终端分屏树。
- **图像协议（Kitty + Sixel）**：统一的图像存储与绘制管线；Kitty 支持直传/分块/PNG/zlib/多层 z-index；Sixel 支持 RGB/HLS 调色板；图像锚定行号并随滚动历史淘汰，显存按需缓存且空闲自动回收。
- **原生输入法（IME）**：光标处原位预编辑排版（宽字符对齐与光标同步），跨平台接入 SDL3、Windows TSF 与 macOS NSTextInputClient。
- **字形整形与精确框线**：按需栅格化系统字体与 CJK 回退；制表符与框线字符（`U+2500..=U+259F`）像素级精确绘制。
- **主题与协议兼容**：支持 24-bit TrueColor、主/备屏切换、鼠标报告与括号粘贴；内置 Kitty、Catppuccin、Tokyo Night 等配色方案，兼容读取 Kitty 主题文件。

## 目录结构

| Crate | 职责 |
| --- | --- |
| `crates/axo-term` | PTY（Unix：`openpty`/`fork`；Windows：ConPTY）、手写 VT 状态机、OSC 0/2/7/10/11、Kitty 与 Sixel 图像协议、主/备屏与环形回滚、选区、键盘与鼠标协议编码 |
| `crates/axo-render` | `Renderer` trait、配色方案（`palette`）、图元布局（`quads`）、字形与框线（`glyphs`/`boxdraw`）、`VulkanRenderer`（`ash`，图像纹理缓存） |
| `crates/axo` | 配置读写、分屏树（`layout`）、右键菜单（`menu`）、终端/设置块（`blocks`）、标签页工作区与快捷键 |

## 构建与运行

- Linux：依赖 `libSDL3`（运行时动态加载）、系统字体（Fontconfig/FreeType）及 Vulkan 1.1+ 驱动。
- Windows：依赖 Vulkan 1.1+ 驱动与 Windows 10 1809+（ConPTY）。
- macOS：支持编译检查（`cargo check --target aarch64-apple-darwin`），GPU 渲染器接入中。

```sh
cargo run --release -p axo
```

运行测试与静态检查：

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## 快捷键

| 快捷键 | 功能 |
| --- | --- |
| `Ctrl+Shift+T` | 新建标签页（标签栏关闭时禁用） |
| `Ctrl+Shift+D` | 向右分屏（新建终端块） |
| `Ctrl+Shift+E` | 向下分屏（新建终端块） |
| `Ctrl+Shift+W` | 关闭当前焦点块（最后一个块关闭标签页） |
| `Ctrl+,` | 在当前标签页打开/聚焦设置块 |
| `Alt+Left/Right/Up/Down` | 在当前标签页各分屏块之间移动焦点 |
| `Ctrl+Tab` / `Ctrl+PageDown` | 下一个标签页 |
| `Ctrl+Shift+Tab` / `Ctrl+PageUp` | 上一个标签页 |
| `Alt+1..9` | 切换到第 1..9 个标签页 |
| 中键点击标签 | 关闭该标签页 |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | 复制选区 / 粘贴剪贴板（支持中键粘贴与括号粘贴 `2004`） |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | 放大 / 缩小 / 重置字号 |
| `Ctrl+滚轮`（终端内） | 放大 / 缩小字号（每格 1pt，所有终端同步） |
| `Shift+PageUp` / `Shift+PageDown` | 翻页滚动历史回滚区（亦支持鼠标滚轮） |

## 配置文件

路径：Unix 为 `$XDG_CONFIG_HOME/axo/axo.conf`（默认 `~/.config/axo/axo.conf`），Windows 为 `%APPDATA%\axo\axo.conf`；纯 `key = value` 格式：

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

## 配色与主题

`theme` 支持内置方案名（`kitty` 为默认）、`themes` 目录中的方案名（`$XDG_CONFIG_HOME/axo/themes/*.conf`，Kitty 格式），或指定主题文件绝对路径。

内置方案：`kitty`、`axo-dark`、`axo-light`、`catppuccin-mocha`、`dracula`、`gruvbox-dark`、`gruvbox-light`、`nord`、`one-dark`、`solarized-dark`、`solarized-light`、`tokyo-night`。

可在 `axo.conf` 中单独指定 `color0`..`color15`、`background`、`foreground`、`cursor` 等覆盖单色，无需复制整份主题。

## 许可证

采用 [BSD 2-Clause License](LICENSE) 授权。
