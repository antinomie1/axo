# Axo Terminal (`axo`)

基于 [aegle](https://github.com/antinomie1/aegle) 的轻量化、模块化 Vulkan（`ash`）加速终端模拟器。

## 特性与架构

- **顶部标签页 + 任意分屏块树**：每个标签页维护一棵分屏树（内部节点为 `aegle::Splitter`，叶子节点为 `Block`），同一标签页内可混合放置多个终端块与 aegle UI 块。
- **块右键菜单**：在终端或设置块上右键（`Menu` 键、`Shift+F10` 亦可）弹出 aegle 菜单，含复制/粘贴、向右/向下分屏、关闭块、新建标签页与设置；应用程序占用右键时按住 `Shift` 可强制打开。
- **Vulkan（ash）终端渲染 + 渲染器接口解耦**：
  - `axo_render::Renderer` trait 抽象表面生命周期、字体度量、终端帧渲染与场景合成；上层块只依赖该接口，可接入 wgpu / CPU 等其他渲染器而不更改调用方 API。
  - `VulkanRenderer` 复用 `aegle` 的共享 Vulkan 设备与图形队列（`App::vulkan()`），通过构建期 `naga` 编译的 SPIR-V 着色器、按需填充的共享字形图集与单次实例化绘制（背景连续 run 合并 + 字形 + 光标/选区/装饰线）渲染到离屏 `R8G8B8A8_SRGB` 纹理，并以 `TextureId` 零拷贝合成进 aegle 帧。
  - 字形通过 `aegle-text`（系统字体与 CJK 回退）与 `aegle-glyph` 按需栅格化；制表符/框线字符（`U+2500..=U+259F`）按单元格像素精确生成，无缝拼接。
- **aegle 标记语言块**：
  - 窗口骨架（`crates/axo/ui/window.aegle`）与设置块（`crates/axo/ui/settings.aegle`）均采用 `.aegle` 标记语言编写。
  - 设置块支持字体族、字号、行高、深/浅色主题、窗口透明度（实时预乘 alpha 合成）、内边距、光标样式、回滚行数与默认 Shell，即时生效并可持久化到 `$XDG_CONFIG_HOME/axo/axo.conf`。
- **极致轻量与零空闲开销**：
  - PTY 读线程在后台完成 `vte` 解析并通过原子脏位合并 `UiProxy` 唤醒；重绘绑定到 compositor 的帧回调（每个显示帧至多一次离屏渲染），无轮询、无常驻逐帧回调，空闲 CPU 占用为 **0%**，Release 二进制约 **3.0 MiB**。
  - 屏幕与回滚历史共用单一环形缓冲，滚屏不搬移行；可打印 ASCII 走批量快路径。

## 目录结构

| Crate | 职责 |
| --- | --- |
| `crates/axo-term` | PTY（`openpty`/`fork`/`TIOCSWINSZ`）、VT 状态机（`vte`）、主/备屏与环形回滚、选区、键盘与鼠标协议编码 |
| `crates/axo-render` | `Renderer` trait、后端无关图元布局（`quads`）、字形与框线栅格化（`glyphs`/`boxdraw`）、`VulkanRenderer`（`ash`） |
| `crates/axo` | 配置读写、分屏树（`layout`）、块右键菜单（`menu`）、块接口与实现（`blocks::terminal`、`blocks::settings`）、标签页工作区与快捷键 |

## 构建与运行

`aegle` 由 Cargo 直接从 GitHub 拉取（Linux Wayland 环境需 `libxkbcommon`、系统 Pango/Fontconfig/FreeType 及 Vulkan 1.1 驱动）：

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
| `Ctrl+Shift+T` | 新建标签页 |
| `Ctrl+Shift+D` | 向右分屏（新建终端块） |
| `Ctrl+Shift+E` | 向下分屏（新建终端块） |
| `Ctrl+Shift+W` | 关闭当前焦点块（最后一个块关闭标签页） |
| `Ctrl+,` | 在当前标签页打开/聚焦设置块 |
| `Alt+Left/Right/Up/Down` | 在当前标签页各分屏块之间移动焦点 |
| `Ctrl+Tab` / `Ctrl+PageDown` | 下一个标签页 |
| `Ctrl+Shift+Tab` / `Ctrl+PageUp` | 上一个标签页 |
| `Alt+1..9` | 切换到第 1..9 个标签页 |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | 复制选区 / 粘贴剪贴板（支持中键粘贴与括号粘贴 `2004`） |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | 放大 / 缩小 / 重置字号 |
| `Shift+PageUp` / `Shift+PageDown` | 翻页滚动历史回滚区（亦支持鼠标滚轮） |

## 右键菜单

在终端或设置块上按右键（`Menu` 键、`Shift+F10` 亦可）打开块菜单，执行与快捷键相同的命令：`Copy`、`Paste`（终端且有选区时可用）、`Split Right`、`Split Down`、`Close Pane`、`New Tab`、`Settings`，每项右侧标注对应快捷键。应用程序通过鼠标协议占用右键时（vim、htop 等），右键照旧上报给程序，按住 `Shift` 再右键可强制打开菜单。

## 配置文件

路径：`$XDG_CONFIG_HOME/axo/axo.conf`（默认 `~/.config/axo/axo.conf`），纯 `key = value` 格式（无额外序列化依赖）：

```ini
font_family = monospace
font_size = 14
line_height = 1
theme = dark
opacity = 1
padding = 6
cursor = block
scrollback = 10000
shell =
```

## 许可证

采用 [BSD 2-Clause License](LICENSE) 授权。
