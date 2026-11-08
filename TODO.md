# Axo Terminal — 设计与 TODO

Axo（`axo`）是基于 [aegle](https://github.com/antinomie1/aegle) 的 GPU 加速终端（Vulkan/ash；macOS 为 wgpu/Metal）。目标：极致性能、轻量、模块化，不过度工程。

## 架构

```
┌──────────────────────────── aegle Window (Vulkan 渲染器, SDL3) ──────────────────────────┐
│ 标签栏（顶部，aegle 控件）  [tab1] [tab2]  +                                ⚙ │
├──────────────────────────────────────────────────────────────────────────────────────────────┤
│ Tab 页面 = 块树（Layout）：叶子是块（Block），内部节点是 aegle Splitter（水平/垂直）         │
│  ┌───────────────┬──────────────┐                                                          │
│  │ TerminalBlock │ SettingsBlock│  TerminalBlock: Canvas + ash 离屏纹理（TextureId）        │
│  │ (Vulkan/ash)  │ (aegle 标记) │  其余块: aegle 标记语言 (.aegle)                          │
│  └───────────────┴──────────────┘                                                          │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

- aegle 用 `vulkan` 渲染器构建；`App::vulkan()` 给出共享设备（`raw_device()`）。
- 终端块：自有 ash 管线把网格画进离屏 `R8G8B8A8_SRGB` 图像，`register_texture` 后在 Canvas painter 中 `builder.texture(id, rect)` 合成进 aegle 帧。同一队列、先于 aegle 帧按脏画布提交，空闲终端零绘制。
- 渲染器接口 `axo_render::Renderer`（trait）：块只依赖 trait；Linux/Windows 为 Vulkan 实现，macOS 为 wgpu 实现。
- macOS：aegle 用 `wgpu` 渲染器构建；`App::wgpu()` 给出共享设备与队列。终端纹理（`Rgba8UnormSrgb`）`register_texture` 后同样经 `builder.texture` 合成。wgpu 自行跟踪资源依赖与生命周期，故无栅栏、无等待、无纹理退役；上传在下一次提交开头执行，所有表面共用一个实例缓冲。
- PTY 读线程解析 VT 序列并更新 `Term`（`Mutex`），通过原子脏标志合并 `UiProxy` 唤醒（空闲零 CPU、零轮询）。

## Crate 划分

| crate | 职责 | 依赖 |
| --- | --- | --- |
| `axo-term` | PTY（Unix: openpty/fork；Windows: ConPTY）、手写 VT 状态机、kitty 与 sixel 图像协议、网格/回滚、选择、键盘与鼠标编码 | `unicode-width`、`png`、`miniz_oxide`、`libc`(unix)、`windows-sys`(windows) |
| `axo-render` | `Renderer` trait、字形与框线栅格化（aegle-text + aegle-glyph）、`vulkan` 实现（ash，WGSL→SPIR-V 构建期由 naga 编译）、`wgpu` 实现（macOS，同一 WGSL 运行时编译） | aegle |
| `axo` (bin) | 配置、标签页、分屏布局、块（终端/设置）、快捷键、标记 UI | 上两者 + aegle |

## 对 aegle 的最小补充

- [x] `Window::scale()`：HiDPI 下终端纹理按物理像素渲染。
- [x] `Node::request_paste()` + `CanvasEvent::Paste(String)`：Canvas（终端）可读取剪贴板。
- [x] Pango 字体文件缓存按文件路径共享 `Blob`（避免 `.ttc` 字体集按 face 重复读入内存）。
- [x] Win32 消息循环将按键先交付 TSF 处理，防止 IME 吞键后漏出 `WM_CHAR`。
- [x] `Control::ime_caret` + `Node::request_ime_sync` + `Canvas::set_ime` / `CanvasEvent::Ime` / `CanvasEvent::ImeCancelled`：Canvas 原生输入法合成目标（无 surrounding text，发布光标矩形）。
- [x] 单行编辑框滚动限制与垂直居中，消除滚轮与 IME 偏移误动。

## 输入法

1. 聚焦块发布 `ImeRequest`，`surrounding` 为 `None`，`cursor_rect` 为光标单元格逻辑矩形，`multiline = false`。
2. 平台事务到达 `CanvasEvent::Ime`：`preedit` 本地覆写光标处单元格（宽字符占双格、下划线、光标同步跟随），`commit` 时写入 PTY。
3. `sync_ime()` 仅在单元格发生变化时触发同步；焦点离开直接丢弃预编辑。

## 图像

kitty 与 sixel 共用同一存储与绘制管线：

1. `Pixels`（`Arc`，RGBA8）+ `Placement`（锚点、像素偏移、裁剪、单元格尺寸、`z`）。
2. 放置锚定绝对行号（`Grid::base`），滚屏不触碰历史放置。
3. kitty 图像为独立图层；sixel 为单元格内容，文字写入时自动裁切擦除。
4. 绘制按图层顺序批处理写入缓冲，采样钳位在裁剪框内防串色。
5. GPU 纹理作为缓存，依 `Weak<Pixels>` 回收失效资源。

## 里程碑

### M0 工程
- [x] Cargo workspace、release profile（lto/strip）、aegle 依赖（`desktop, vulkan, sdl3`）
- [x] 模块架构设计

### M1 终端核心 `axo-term`
- [x] PTY：openpty/fork 与 ConPTY，`TIOCSWINSZ` 窗口同步，会话清理
- [x] 手写 VT 状态机：打印、C0、CSI、ESC、OSC 0/2 标题与 10/11 颜色查询
- [x] SGR 属性：16/256/真彩色、粗体/斜体/下划线/反显/删除线/暗淡
- [x] DEC 模式：备用屏、光标显隐、应用键盘、自动换行、括号粘贴、鼠标协议（1000/1002/1003/1006）、焦点事件
- [x] 宽字符（CJK）、回滚环形缓冲、原子脏位合并唤醒
- [x] 选区（单击/双击/三击）与复制，键盘字节编码

### M2 渲染 `axo-render`
- [x] `Renderer` trait 抽象
- [x] 字形栅格化（aegle-text + aegle-glyph）、框线像素级自绘、单元格度量
- [x] Vulkan 实例化绘制（背景 run 合并 + 字形 + 装饰线 + 选区）
- [x] 配色方案（`palette`）：16 色 ANSI + 256 色表，内置 12 套方案与 kitty 主题解析
- [x] 窗口透明与实时不透明度调节

### M3 应用 `axo`
- [x] 配置文件读写（`axo.conf`）
- [x] 标签页系统（新建/关闭/切换，标题动态跟随）
- [x] 任意分屏块树（水平/垂直，方向导航）
- [x] 设置面板（aegle 标记语言编写，即时生效并持久化）
- [x] 全局快捷键与终端内滚轮缩放字号

### M3.5 性能优化
- [x] 屏幕与回滚合并为单环形缓冲，滚屏零内存搬移
- [x] 可打印 ASCII 批量快路径
- [x] 渲染绑定 compositor 帧回调，合并脏位节流

### M4 验证
- [x] Release 构建、测试与 Clippy 零警告
- [x] 实机运行验证（Vulkan Validation Layer 零警告）
- [x] 空闲 CPU 占用 0%
- [x] 吞吐与启动性能基准测试

### M5 图像协议
- [x] kitty 图像协议：直传/文件/分块/PNG/zlib/放置/删除/查询/配额
- [x] sixel 协议：批量解码、DECSDM、DA1/XTSMGRAPHICS 上报、像素尺寸响应
- [x] 共享放置模型与显存缓存回收
- [ ] kitty 进阶特性：共享内存传输（`t=s`）、动画（`a=f/a/c`）、虚拟放置（`U=1`）

## 平台支持

- [x] Linux：`aegle-platform-sdl3`（SDL3，支持 Wayland 与 X11），Vulkan/ash 渲染，文本输入。
- [x] Windows：`aegle-platform-win32`（Win32 窗口 + TSF 输入法），ConPTY，Vulkan/ash 渲染；透明窗口经 DirectComposition 呈现，GUI 子系统无控制台窗口。
- [x] macOS：`aegle-platform-macos`（AppKit + `NSTextInputClient`），`openpty` PTY 编译支持。
- [x] macOS 渲染器：`WgpuRenderer`（wgpu/Metal，与 Vulkan 共用 WGSL、图元布局与图集打包）。lavapipe 上经 wgpu Vulkan 后端验证：200×50 满屏（9752 个图元）`render()` CPU 耗时 367 µs/帧，其中布局 114 µs。
- [x] macOS 实机验证（macOS 15 x86_64，Metal）：窗口显示、Retina 2× 缩放、Menlo 等宽网格、CJK 回退、空闲 CPU 0%；透明窗口随 `opacity` 即时生效。

## 后续规划

- [ ] 终端重排（resize reflow）
- [ ] 连字（ligatures）、Kitty 键盘协议
- [ ] 搜索与 URL 识别
- [ ] 设置页配色方案调色板预览
