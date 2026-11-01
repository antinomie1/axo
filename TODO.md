# Axo Terminal — 设计与 TODO

Axo（`axo`）是基于 [aegle](https://github.com/antinomie1/aegle) 的 Vulkan 加速终端。目标：极致性能、轻量、模块化，不过度工程。

## 架构

```
┌──────────────────────────── aegle Window (Vulkan 渲染器, Wayland) ──────────────────────────┐
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
- 渲染器接口 `axo_render::Renderer`（trait）：块只依赖 trait；Vulkan 是其一个实现，可换 wgpu / CPU 实现而不改调用方 API。
- PTY 读线程解析 VT 序列并更新 `Term`（`Mutex`），通过原子脏标志合并 `UiProxy` 唤醒（空闲零 CPU、零轮询）。

## Crate 划分

| crate | 职责 | 依赖 |
| --- | --- | --- |
| `axo-term` | PTY（Unix: openpty/fork；Windows: ConPTY）、VT 解析（vte）、kitty 与 sixel 图像协议、网格/回滚、选择、键盘与鼠标编码 | `vte`、`unicode-width`、`png`、`miniz_oxide`、`libc`(unix)、`windows-sys`(windows) |
| `axo-render` | `Renderer` trait、字形与框线栅格化（aegle-text + aegle-glyph）、`vulkan` 实现（ash，WGSL→SPIR-V 构建期由 naga 编译） | aegle |
| `axo` (bin) | 配置、标签页、分屏布局、块（终端/设置）、快捷键、标记 UI | 上两者 + aegle |

## 对 aegle 的最小补充

- [x] `Window::scale()`：HiDPI 下终端纹理按物理像素渲染，避免模糊。
- [x] `Node::request_paste()` + `CanvasEvent::Paste(String)`：Canvas（终端）可读取剪贴板。
- [x] Pango 字体文件缓存按文件路径共享 `Blob`（避免 `.ttc` 字体集按 face 重复读入内存）。
- [x] Win32 消息循环把按键先交给 TSF（`ITfKeystrokeMgr::TestKeyDown/KeyDown`，在 `TranslateMessage` 之前）：被输入法吃掉的键不再变成 `WM_CHAR` 文本，否则 Windows 上的输入法收不到按键。
- [x] `Control::ime_caret` + `Node::request_ime_sync` + `Canvas::set_ime` / `CanvasEvent::Ime` / `CanvasEvent::ImeCancelled`：Canvas 也能作为原生输入法合成目标（无 surrounding text，只发布光标矩形），一次 `Done` 仍是一个原子事务。
- [x] 单行编辑框不再把自身文本当作可纵向滚动内容：`scroll_limit` 对单行返回 0，caret 显露不再挪动 `scroll.y`，滚轮经过内嵌输入框时滚动外层视图（多行编辑区仍滚自己的文字）；单行文字在控件内垂直居中，基线、指针映射与 IME 原点随之调整。

## 输入法

终端没有文档，所以 IME 的语义和文本框不同：

1. 只有聚焦块发布 `ImeRequest`，`surrounding` 为 `None`（`axo-term` 不把网格当成可编辑文档，输入法也无从 `delete_surrounding_text` 改动 shell 提示符），`cursor_rect` 为光标单元格的逻辑像素矩形，`multiline = false`。
2. 平台事务（`delete_*` + `commit` + `preedit`）到达 `CanvasEvent::Ime`：`preedit` 只画在本地（覆写光标处单元格、宽字符占两格、下划线、光标跟随插入点），`commit` 才 `session.write()` 进 PTY；`delete_*` 对终端无意义（字节已交给子进程），忽略。
3. 光标移动/滚动/输出后 `sync_ime()` 比较单元格，只在变化时 `request_ime_sync()`，空闲不产生任何平台往返；焦点离开（`CanvasEvent::ImeCancelled`）直接丢弃预编辑。

## 图像

kitty 与 sixel 的差别只在“文字如何对待图像”，其余共用一层：

1. `Pixels`（`Arc`，RGBA8）+ `Placement`（锚点、像素偏移、裁剪、单元格尺寸、`z`）。匿名图像与 sixel 只由放置持有，随放置一起释放；带 id 的 kitty 图像另存一份，按配额从旧到新淘汰。
2. 放置锚定绝对行号：环形缓冲下标 + 累计淘汰行数（`Grid::base`）。整屏滚入历史不触碰放置，只有在环内移动行的滚动（区域、备用屏）才遍历；部分滚出区域的图像继续完整绘制直到完全离开，不逐步裁剪。
3. kitty 图像是图层，文字不影响；sixel 是单元格内容，`Images::punch` 把被写入的格子从放置里切掉、拆成周围的裁剪块，渲染器无需支持。文本路径只多一次“有没有 sixel”的判断。
4. 渲染：实例按绘制顺序写入同一缓冲（低于背景的图像、背景、文字下的图像、文字、文字上的图像）。图像是单独的管线和 set 1，走双线性采样，并把采样钳在裁剪框内，避免相邻纹素串色。
5. GPU 纹理是缓存：以 `Weak<Pixels>` 感知释放，退役纹理等所有 surface 空闲后销毁（不阻塞渲染），只有超出 256 MiB 预算才按 LRU 淘汰视野外的。

解码（PNG/zlib）在读线程持 `Term` 锁时完成，大图解码期间 UI 取锁会短暂等待；真成问题时再把解码移出锁。

## 里程碑

### M0 工程
- [x] Cargo workspace、release profile（lto/strip）、aegle GitHub git 依赖（`desktop,vulkan`）
- [x] TODO.md 设计

### M1 终端核心 `axo-term`
- [x] PTY：openpty + fork/exec shell，`TIOCSWINSZ` 调整大小，子进程退出检测与会话 `SIGHUP` 清理
- [x] VT：打印、C0、CSI（CUP/CUU…/ED/EL/ICH/DCH/IL/DL/SU/SD/ECH/REP/DECSTBM/SGR/DSR/DA/DECSCUSR）、ESC（IND/RI/NEL/DECSC/DECRC/RIS/DEC 特殊图形字符集）、OSC 0/2 标题与 10/11 颜色查询
- [x] SGR：16/256/真彩色（分号与冒号子参数）、粗体/斜体/下划线/反显/删除线/暗淡
- [x] DEC 模式：备用屏（1049/47/1047）、光标可见（25）、应用光标键（1）、自动换行（7）、括号粘贴（2004）、鼠标上报（1000/1002/1003/1006）、备用屏滚轮（1007）、焦点事件（1004）
- [x] 宽字符（CJK）、回滚环形缓冲、原子脏位合并唤醒
- [x] 选择（单击按格、双击按词、三击按行）与复制文本（软换行自动拼接）
- [x] 键盘 → 字节编码（方向键、功能键、Ctrl/Alt/Shift 组合）

### M2 渲染 `axo-render`
- [x] `Renderer` trait（surface 创建/销毁/字体度量/绘制/场景合成）
- [x] 字形：aegle `TextSystem` 成形单字（含系统回退、CJK）、`GlyphCache` 栅格化、框线/块元素像素级自绘、单元格度量
- [x] Vulkan：共享图集（RGBA8，灰度/彩色字形）、实例化绘制（背景 run 合并 + 字形 + 光标/下划线/删除线/选择）、正确的子通道依赖与布局屏障
- [x] 配色方案（`palette`）：前景/背景、光标与光标文字色、选区与选区文字色、16 色 ANSI + 标准 256 色表；内置 kitty（默认）等 12 套方案，解析 kitty 主题文件，按背景亮度派生深/浅窗口主题
- [x] 默认背景透明（由窗口主题背景提供，支持实时调节窗口透明度）

### M3 应用 `axo`
- [x] 配置：`~/.config/axo/axo.conf`（`key = value`，无 serde）
- [x] 标签页（顶部，`ui/window.aegle`）：新建、关闭、切换；标题跟随焦点块（OSC 标题优先，否则前台命令 + 当前目录）
- [x] 块与分屏：每个标签页一棵块树；水平/垂直分屏、关闭块、焦点在块间按方向移动
- [x] 设置块（aegle 标记 `ui/settings.aegle`）：字体族、字号、行高、配色方案（内置方案与 `themes/*.conf` 列表）、窗口透明度、内边距、光标样式、回滚行数、标签栏开关、shell；即时应用并保存
- [x] 快捷键（见 README.md），含终端内 `Ctrl+滚轮` 缩放字号：每格滚轮 1pt、单事件最多 1 步、触控板按 10 逻辑像素累计，复用 `FontSize` 动作同步所有终端

### M3.5 性能优化
- [x] 屏幕 + 回滚合并为单一环形缓冲（`grid.rs`），整屏滚入历史仅 push/recycle 一行，无切片旋转
- [x] 可打印 ASCII 批量快路径（`term.rs`），跳过宽字符分支与 `unicode_width` 查表
- [x] 重绘按 compositor 帧回调节流（一次性 `on_frame`），洪峰输出每个显示帧至多渲染一次
- [x] 读线程缓冲提升到 128 KiB，脏位只在真正绘制后清除

### M4 验证
- [x] `cargo build --release`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`
- [x] 实机运行（含 Khronos Vulkan Validation Layer）：shell、vim（备用屏）、CJK/真彩色/框线、多分屏、多标签页、设置页与窗口透明
- [x] 空闲 CPU 为 0（10s 空闲 0 CPU tick，无逐帧回调）
- [x] 与 Alacritty 0.17 同条件对比（DSR 同步吞吐，2026-10 复测，输入法已接入）：ASCII 161.4 vs 140.7 MiB/s、SGR 185.6 vs 184.8、CJK 169.2 vs 167.1、备用屏 TUI 174.9 vs 171.5 MiB/s；启动 64.1 vs 114.1 ms；空闲 CPU 0 ticks/5s vs 7；PSS 58.8 vs 39.0 MiB、线程 7 vs 16
  - 复测时发现并修掉了一个真实回归：输入法在每次宿主状态变化后都会回一个空 `done`，把它当成 update 上报会让宿主再发一遍状态，两者以事件循环速率互相触发（实测洪水输出时 ~500 次/秒 text-input 事务），吞吐因此腰斩到 68.7 MiB/s；忽略"只确认状态"的空批次（仅当它结束合成时才上报）后回到 162.7 MiB/s，同时终端侧不再为空事务重绘

### M5 图像协议
- [x] kitty 图形协议：直传/文件/临时文件、分块、RGB/RGBA/PNG、zlib、放置、删除、查询、配额
- [x] sixel：批量解码、DECSDM、`?8452`、DA1 与 `XTSMGRAPHICS` 上报、`CSI 14/16 t` 像素尺寸
- [x] 复用：放置模型、滚动、裁剪由两种协议共用；`Transfer` 同时服务字形图集与图像上传
- [x] 实机读回验证（Khronos Validation Layer 零报告）：原尺寸逐像素对齐、PNG、缩放与裁剪、`z` 三层、文字打穿 sixel、滚动与 padding 裁剪、清屏与删除；512×512 每帧换图 300 帧显存平稳
- [x] 无图像时端到端吞吐（5 次中位，对比上一提交）：ASCII 533 → 539 MiB/s、SGR 265 → 258、CJK 205 → 206；解码：sixel 448 Mpx/s，kitty 1080p RGBA 8.6 ms/帧
- [ ] kitty：共享内存传输（`t=s`）、动画（`a=f/a/c`）、虚拟放置（`U=1`，tmux 内用）、相对放置（`P/Q`）

## 平台支持

- [x] Linux（Wayland）：`aegle-platform-wayland`，Vulkan/ash 渲染，`text-input-v3` 输入法。
- [x] Windows：`aegle-platform-win32`（Win32 窗口 + TSF 输入法），ConPTY（`CreatePseudoConsole` + `CreateProcessW`，需 Windows 10 1809+），Vulkan/ash 渲染；前台进程名与 `/proc` 相关工作目录在 Windows 上分别退化为“无”和 OSC 7。
- [x] macOS：`aegle-platform-macos`（AppKit + `NSTextInputClient`）与 `openpty` PTY 均可编译，共享同一套 IME 与渲染器接口。
- [ ] macOS 渲染器：aegle 在 macOS 上没有 GPU 后端（`vulkan` 无 ash，`wgpu`/Metal 未接入），`axo` 在 macOS 上暂不打开窗口。

## 后续（未做）
- [ ] macOS 渲染器接入（wgpu/Metal 或 CPU 后备），随后才能实机验证 macOS 输入法
- [ ] 回滚的重排（resize reflow）；当前 resize 截断/填充并保留光标行
- [ ] 连字（ligatures）、Kitty 键盘协议
- [ ] 其他渲染器实现（wgpu / CPU）——`axo_render::Renderer` 接口已就绪
- [ ] 搜索、URL 识别
- [ ] 配色方案的实时预览（设置页显示当前方案色板）与 `theme` 下拉的搜索
