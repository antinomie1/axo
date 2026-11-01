# Axo Terminal (`axo`)

基于 [aegle](https://github.com/antinomie1/aegle) 的轻量化、模块化 Vulkan（`ash`）加速终端模拟器，支持 Linux（Wayland）、Windows 与 macOS。

| 平台 | 窗口/输入 | PTY | 终端渲染 |
| --- | --- | --- | --- |
| Linux | Wayland（`aegle-platform-wayland`，含 `text-input-v3`） | `openpty` + `fork` | Vulkan（`ash`） |
| Windows 10 1809+ | Win32（`aegle-platform-win32`，含 TSF 输入法） | ConPTY | Vulkan（`ash`） |
| macOS | AppKit（`aegle-platform-macos`，含 `NSTextInputClient`） | `openpty` + `fork` | 暂缺（见下） |

每个平台只编译自己的实现：PTY 在 `axo-term` 内按 `cfg(unix)`/`cfg(windows)` 分成两个模块，Vulkan 后端与着色器编译只在 Linux/Windows 上存在，`aegle` 的渲染器特性也按目标平台选择。macOS 目前只做到“能编译、能接输入法”：aegle 在 macOS 上还没有 GPU 后端（`vulkan` 无 ash 后端，`wgpu`/Metal 未接入），所以 `axo` 在 macOS 上会直接报告缺少渲染器而不打开窗口。

## 特性与架构

- **顶部标签页 + 任意分屏块树**：每个标签页维护一棵分屏树（内部节点为 `aegle::Splitter`，叶子节点为 `Block`），同一标签页内可混合放置多个终端块与 aegle UI 块。
- **极简标签栏**：标签栏以略微偏离终端背景的底色（深色主题略亮、浅色主题略暗）区分，选中标签以配色方案强调色（ANSI 蓝）的淡色底与高对比文字标出，关闭按钮在只剩一个标签时隐藏，中键点击标签即可关闭。标签与窗口标题跟随焦点块：优先使用程序设置的 OSC 标题，否则显示前台命令与当前目录（如 `vim — ~/Axo`）。可在设置中关闭标签栏：关闭后只保留当前标签页，`Ctrl+Shift+T` 新标签与右键菜单中的 `New Tab` 一并禁用。
- **块右键菜单**：在终端或设置块上右键（`Menu` 键、`Shift+F10` 亦可）弹出 aegle 菜单，含复制/粘贴、向右/向下分屏、关闭块、新建标签页与设置；应用程序占用右键时按住 `Shift` 可强制打开。
- **Vulkan（ash）终端渲染 + 渲染器接口解耦**：
  - `axo_render::Renderer` trait 抽象表面生命周期、字体度量、终端帧渲染与场景合成；上层块只依赖该接口，可接入 wgpu / CPU 等其他渲染器而不更改调用方 API。
  - `VulkanRenderer` 复用 `aegle` 的共享 Vulkan 设备与图形队列（`App::vulkan()`），通过构建期 `naga` 编译的 SPIR-V 着色器、按需填充的共享字形图集与单次实例化绘制（背景连续 run 合并 + 字形 + 光标/选区/装饰线）渲染到离屏 `R8G8B8A8_SRGB` 纹理，并以 `TextureId` 零拷贝合成进 aegle 帧。
  - 字形通过 `aegle-text`（系统字体与 CJK 回退）与 `aegle-glyph` 按需栅格化；制表符/框线字符（`U+2500..=U+259F`）按单元格像素精确生成，无缝拼接。
- **输入法（IME）预编辑**：终端把自己的光标单元格作为合成锚点交给平台，输入法的预编辑文本就地显示在光标处（宽字符占两格、带下划线、光标随预编辑插入点移动），提交（commit）时才写入 PTY；终端不向输入法公开 surrounding text，因此输入法不会去改动 shell 的提示符。三个平台共用同一套逻辑，差异全部由 aegle 的平台层吸收：Wayland 走 `text-input-v3`（`zwp_text_input_v3`，一次 `done` 为一个原子事务），Windows 走 TSF（`ITextStoreACP` 文档），macOS 走 `NSTextInputClient`（`setMarkedText:`/`insertText:`）。会话在焦点离开、切分屏/标签、光标滚出视野时自动结束，光标移动后按需重发锚点（不变则不重发）。
- **图像协议（kitty + sixel）**：两种协议共用一层图像存储与同一条绘制路径，没有图像时只多一次判空。
  - kitty 图形协议：直传、文件与临时文件传输，分块传输，24/32 位像素、PNG、zlib 压缩，放置（裁剪、缩放、像素偏移、`z` 三层、光标策略）、全部删除选择子、查询应答与 320 MiB 配额。共享内存传输、动画与虚拟放置（Unicode 占位符）明确回 `ENOTSUP`/`EINVAL`，客户端可以回退。
  - sixel：带重复、颜色寄存器（RGB/HLS，1024 个）与栅格属性的批量解码器，`?80`（DECSDM）、`?8452`，并通过 DA1 与 `XTSMGRAPHICS` 上报能力；`CSI 14/16 t` 报告像素尺寸。sixel 是单元格内容，文字与擦除会精确到格地打穿它，新图覆盖旧图时释放旧图；kitty 图像是独立图层，文字不影响它。
  - 图像锚定在绝对行号上，随文字滚入历史、随历史淘汰，滚屏不需要逐图更新；主屏与备用屏各有一份存储。GPU 纹理只是按需填充的缓存，终端释放像素后下一次渲染即回收，每帧换图的客户端（如视频）不会累积显存。
- **完整配色系统**：配色方案包含默认前景/背景、光标（含光标下的文字色）、选区（含选区文字色）与 16 个 ANSI 颜色，16–255 按标准色立方与灰阶生成；内置 kitty（默认）、Catppuccin Mocha、Dracula、Gruvbox、Nord、One Dark、Solarized、Tokyo Night 等方案，并可直接读取 kitty 主题文件（`$XDG_CONFIG_HOME/axo/themes/*.conf`），也可在配置中按 `color0..color15` 单独覆盖。窗口、标签栏、菜单与控件主题全部由该方案派生，程序查询颜色（OSC 10/11）返回同一组颜色。
- **aegle 标记语言块**：
  - 窗口骨架（`crates/axo/ui/window.aegle`）与设置块（`crates/axo/ui/settings.aegle`）均采用 `.aegle` 标记语言编写。
  - 设置块支持字体族、字号、行高、配色方案、窗口透明度（实时预乘 alpha 合成）、内边距、光标样式、回滚行数、标签栏开关与默认 Shell，即时生效并可持久化到 `$XDG_CONFIG_HOME/axo/axo.conf`。
- **极致轻量与零空闲开销**：
  - PTY 读线程在后台完成 `vte` 解析并通过原子脏位合并 `UiProxy` 唤醒；重绘绑定到 compositor 的帧回调（每个显示帧至多一次离屏渲染），无轮询、无常驻逐帧回调，空闲 CPU 占用为 **0%**，Release 二进制约 **3.0 MiB**。
  - 屏幕与回滚历史共用单一环形缓冲，滚屏不搬移行；可打印 ASCII 走批量快路径。

## 目录结构

| Crate | 职责 |
| --- | --- |
| `crates/axo-term` | PTY（Unix：`openpty`/`fork`/`TIOCSWINSZ`；Windows：ConPTY + `CreateProcessW`）、VT 状态机（`vte`）、OSC 0/2/7/10/11、kitty 与 sixel 图像协议、主/备屏与环形回滚、选区、键盘与鼠标协议编码 |
| `crates/axo-render` | `Renderer` trait、配色方案与 256 色解析（`palette`）、后端无关图元布局（`quads`）、字形与框线栅格化（`glyphs`/`boxdraw`）、`VulkanRenderer`（`ash`，含图像纹理缓存） |
| `crates/axo` | 配置读写、分屏树（`layout`）、块右键菜单（`menu`）、块接口与实现（`blocks::terminal`、`blocks::settings`）、标签页工作区与快捷键 |

## 构建与运行

`aegle` 由 Cargo 直接从 GitHub 拉取（固定 revision）。

- Linux（Wayland）：需要 `libxkbcommon`、系统 Pango/Fontconfig/FreeType 及 Vulkan 1.1 驱动。
- Windows：需要 Vulkan 1.1 驱动与 Windows 10 1809 及以上（ConPTY）；默认 shell 取 `%COMSPEC%`（`cmd.exe`）。
- macOS：可编译（`cargo check --target aarch64-apple-darwin`），但 GPU 渲染器尚未接入。

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
| `Ctrl+滚轮`（终端内） | 放大 / 缩小字号（每格滚轮 1pt，等价 `Ctrl+=` / `Ctrl+-`，所有终端同步） |
| `Shift+PageUp` / `Shift+PageDown` | 翻页滚动历史回滚区（亦支持鼠标滚轮） |

## 右键菜单

在终端或设置块上按右键（`Menu` 键、`Shift+F10` 亦可）打开块菜单，执行与快捷键相同的命令：`Copy`、`Paste`（终端且有选区时可用）、`Split Right`、`Split Down`、`Close Pane`、`New Tab`、`Settings`，每项右侧标注对应快捷键。应用程序通过鼠标协议占用右键时（vim、htop 等），右键照旧上报给程序，按住 `Shift` 再右键可强制打开菜单。

## 配置文件

路径：Unix 为 `$XDG_CONFIG_HOME/axo/axo.conf`（默认 `~/.config/axo/axo.conf`），Windows 为 `%APPDATA%\axo\axo.conf`；纯 `key = value` 格式（无额外序列化依赖）。`shell` 留空表示平台默认（`$SHELL`，Windows 上为 `%COMSPEC%`）：

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

`theme` 接受内置方案名（`kitty` 为默认）、`themes` 目录中的方案名，或主题文件路径；旧的 `dark`/`light` 保留为 `axo-dark`/`axo-light` 的别名。

内置方案：`kitty`、`axo-dark`、`axo-light`、`catppuccin-mocha`、`dracula`、`gruvbox-dark`、`gruvbox-light`、`nord`、`one-dark`、`solarized-dark`、`solarized-light`、`tokyo-night`。

自定义方案放进配置目录的 `themes/<name>.conf`（`theme = <name>` 即生效），或写 `theme = ~/dots/my-theme.conf`。文件就是 kitty 主题格式，`key value` 或 `key = value`，非颜色键会被忽略：

```ini
# ~/.config/axo/themes/my-theme.conf
foreground #dddddd
background #101010
cursor #ffcc66
cursor_text_color #101010
selection_background #334455
selection_foreground #ffffff
color0  #000000
# …color1–color15
```

在 `axo.conf` 里写同名颜色键会覆盖所选方案的单色（例如 `background = #0b0b0b`、`color4 = #5599ff`），适合只调一两个颜色而不复制整份主题；`cursor` 在取颜色值时是光标颜色覆盖，否则仍是光标样式（`block`/`bar`/`underline`）。`cursor_text_color` 与 `selection_foreground` 可写 `none`，表示沿用单元格自身的前景色。窗口背景、控件（surface/control/muted/border/divider）、强调色（ANSI 蓝）与标签栏底色都由方案自动派生，深色/浅色主题按背景亮度判定。

## 许可证

采用 [BSD 2-Clause License](LICENSE) 授权。
