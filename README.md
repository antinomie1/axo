# Axo Terminal (`axo`)

基于 [aegle](https://github.com/antinomie1/aegle) 的轻量化、模块化 Vulkan（`ash`）加速终端模拟器，支持 Linux、Windows 与 macOS。

| 平台 | 窗口/输入 | PTY | 终端渲染 |
| --- | --- | --- | --- |
| Linux | SDL3（`aegle-platform-sdl3`，支持 Wayland 与 X11） | `openpty` + `fork` | Vulkan（`ash`） |
| Windows 10 1809+ | Win32（`aegle-platform-win32`，TSF 输入法） | ConPTY | Vulkan（`ash`） |
| macOS | AppKit（`aegle-platform-macos`，`NSTextInputClient`） | `openpty` + `fork` | 暂缺（无 GPU 渲染） |

按目标平台选择后端编译：PTY 在 `axo-term` 内区分 Unix 与 Windows，Vulkan 仅在 Linux 与 Windows 编译。macOS 接入窗口与输入法，GPU 渲染器尚未接入（启动提示后退出）。

## 特性与架构

- **顶部标签页 + 分屏块树**：每个标签页维护一棵分屏树（`aegle::Splitter` 内部节点，`Block` 叶子节点），可混合终端块与 UI 块。
- **极简标签栏**：标题跟随焦点块（优先取 OSC 标题，缺省回退前台命令与路径）；单标签隐藏关闭按钮，支持中键关闭；可在设置中隐藏。
- **块右键菜单**：复制/粘贴、向右/向下分屏、关闭块、新建标签页与设置；程序占用鼠标右键时按住 `Shift` 可强制呼出。
- **Vulkan（ash）终端渲染**：复用 aegle 共享 Vulkan 设备与图形队列（`App::vulkan()`），SPIR-V 着色器由 `naga` 在构建期预编译；单次实例化绘制渲染至离屏 SRGB 纹理，零拷贝合成进 aegle 帧。
- **字形与框线**：基于 `aegle-text` 与 `aegle-glyph` 按需栅格化系统字体与 CJK 回退；制表符/框线字符（`U+2500..=U+259F`）像素级精确绘制。
- **原生输入法（IME）**：光标处原位显示预编辑文本（宽字符占双格、下划线、光标同步跟随），提交时写入 PTY。跨平台接入 SDL3、Windows TSF 与 macOS NSTextInputClient。
- **图像协议（Kitty + Sixel）**：共用图像存储与绘制管线。Kitty 支持直传/文件/分块/PNG/zlib/多层 z-index 与配额管理；Sixel 支持 RGB/HLS 调色板与批量解码。图像锚定绝对行号并随历史环形淘汰，显存按需缓存且空闲回收。
- **完整配色与主题**：内置 Kitty、Catppuccin、Dracula、Gruvbox、Nord、Tokyo Night 等配色方案，兼容读取 Kitty 主题文件并支持在配置中单色覆盖；窗口背景、控件与标签栏自动派生主题。
- **声明式 UI 块**：窗口布局与设置界面采用 aegle 标记语言编写；设置面板修改即时生效并持久化至配置文件。
- **极致性能与零空闲开销**：PTY 读线程在后台执行手写 VT 状态机解析（含可打印 ASCII 批量快路径），脏位合并唤醒；渲染节流至 compositor 帧刷新率，空闲 CPU 占用为 0%，Release 二进制约 2.4 MiB。

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
