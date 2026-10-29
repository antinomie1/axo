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
| `axo-term` | PTY（openpty/fork）、VT 解析（vte）、网格/回滚、选择、键盘与鼠标编码 | `vte`、`libc`、`unicode-width` |
| `axo-render` | `Renderer` trait、字形与框线栅格化（aegle-text + aegle-glyph）、`vulkan` 实现（ash，WGSL→SPIR-V 构建期由 naga 编译） | aegle |
| `axo` (bin) | 配置、标签页、分屏布局、块（终端/设置）、快捷键、标记 UI | 上两者 + aegle |

## 对 aegle 的最小补充

- [x] `Window::scale()`：HiDPI 下终端纹理按物理像素渲染，避免模糊。
- [x] `Node::request_paste()` + `CanvasEvent::Paste(String)`：Canvas（终端）可读取剪贴板。
- [x] Pango 字体文件缓存按文件路径共享 `Blob`（避免 `.ttc` 字体集按 face 重复读入内存）。
- [x] 单行编辑框不再把自身文本当作可纵向滚动内容：`scroll_limit` 对单行返回 0，caret 显露不再挪动 `scroll.y`，滚轮经过内嵌输入框时滚动外层视图（多行编辑区仍滚自己的文字）；单行文字在控件内垂直居中，基线、指针映射与 IME 原点随之调整。

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
- [x] 与 Alacritty 0.17 同条件对比（DSR 同步吞吐）：ASCII 157.8 vs 140.8 MiB/s、SGR 168.8 vs 193.4、CJK 160.5 vs 163.5、备用屏 TUI 163.4 vs 162.2 MiB/s；启动 64 vs 114 ms

## 后续（未做）
- [ ] 终端内输入法（IME）预编辑：需要 aegle Canvas 接入 IME 事务
- [ ] 回滚的重排（resize reflow）；当前 resize 截断/填充并保留光标行
- [ ] 连字（ligatures）、Kitty 键盘协议、图像协议（sixel/kitty）
- [ ] 其他渲染器实现（wgpu / CPU）——`axo_render::Renderer` 接口已就绪
- [ ] 搜索、URL 识别
- [ ] 配色方案的实时预览（设置页显示当前方案色板）与 `theme` 下拉的搜索
