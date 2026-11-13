# 交接文档 — 恋爱，我就借走咯 · macOS Rust + Metal 重制

> **历史交接记录，不是现行安装指南。** 本文记录早期实现阶段，当时为 **43 个测试通过**；旧环境约束、结构清单和测试数量不可视为最新版。当前构建与使用以[运行时 README](<../README.md>)和[构建说明](<../BUILD-AND-RUN.md>)为准；2026-11-11 后续[全面检查](<COMPREHENSIVE-AUDIT-2026-11-11.md>)记录了本机 124 项测试及基础 95 章 / 追加模式 124 章的验证边界。真实资源测试不在普通 CI 中。
>
> 本 GitHub 仓库只提供源码，不分发预编译可执行文件。代码采用[根 MIT 许可证](<../../LICENSE>)；图片、游戏截图与原始游戏资源不在代码许可范围内。影片、复杂动画时序与原版场景回想仍未实现，留待后续工作；B 仅为最近 6 条对话历史。

## 1. 目标与达成情况

把 Windows 版 KiriKiri2 / KAG 游戏用 Rust + Metal 重写成 macOS 原生运行时：

| 目标 | 状态 |
|---|---|
| 直接读取原始 `data.xp3` / `patch.xp3`（不复制、不修改、不重打包） | ✅ 只读流式读取，集成测试断言 size/mtime 不变 |
| 完全去掉 Steam | ✅ 不加载 `steam_api.dll` / `krkrsteam.dll` / `DrawDeviceForSteam.dll` / `steam_autocloud.vdf`，`achievement` 命令记录诊断后跳过 |
| 不运行 Windows PE / 原生插件 DLL | ✅ 全部用 Rust 重新实现 |
| 画面 | ✅ Metal 渲染背景 / 三层立绘 / 文本框 / 分支 / 标题 / 对话历史 / 菜单；不含原版场景回想 |
| 声音 | ✅ BGM/SE 走 Ogg Vorbis 流式，语音走 Ogg Opus 解码 |
| 可玩 | ✅ 首章 400+ 次推进、95 章编译、12 张选择表分支可达 |

## 2. 历史环境与现行建议

当时使用 macOS 27.0.1 / Apple Silicon / rustc 1.95.0 / Xcode Command Line Tools。推荐继续采用已验证的 Rust 1.95.0 和 macOS / Apple Silicon；Intel 未经人工验证，不承诺 iOS 或 Windows 支持。`libopus` 由 `opus` crate 链接。

早期交接涉及的外置卷写入故障、沙箱限制和临时目录布局只属于当时的开发环境，不是公开项目的构建要求。现行源码构建与可选的[Cargo 包装脚本](<../tools/cargo.sh>)见[构建说明](<../BUILD-AND-RUN.md>)；不要据此文修改本机卷权限或使用旧临时路径。

## 3. XP3 格式（已用真实归档逐字节确认）

```
偏移 0   11 字节  魔数 "XP3\r\n \n\x1a\x8bg\x01"
偏移 11  8 字节   LE u64 = 索引块偏移      ← 没有版本字段
```

索引块：`u8 flag`（bit0=zlib）→ `u64 stored` →（若 zlib）`u64 original` → 数据。
zlib 数据从 `索引偏移 + 17` 开始。

解压后的索引载荷：重复 `"File"` + `u64 body 长度` + body。
body 内是 `4 字节 tag + u64 长度 + 载荷`：

```
"info" : u32 flags(必须 0) · u64 原始大小 · u64 归档大小 · u16 名字长度 · UTF-16LE 文件名
"segm" : 每 28 字节 { u32 flag(0=原样,1=zlib), u64 偏移, u64 原始大小, u64 存储大小 }
"adlr" : u32 Adler-32（不是加密密钥）
```

**没有加密。** 实测：`data.xp3` 15,051 条 / 2,899,957,471 字节 / 索引 607,904→2,714,750；
`patch.xp3` 20 条 / 2,727,999 字节。扩展名：opus 10935、webp 3637、ogg 226、ks 164、
tjs 70、mp4 9、otf 6、png 2、asd 2。

## 4. 这次修掉的四个真 bug（都验证过）

1. **zlib 流式解压误报**（[archive.rs](<../src/archive.rs>)）
   旧代码在 `remaining == 0` 时立刻传 `FlushDecompress::Finish`，即使输出缓冲区
   （32 KiB）还装不下剩余数据 —— miniz_oxide 会报 `invalid zlib stream`，
   表现为「脚本全读不出来」。正确条件：`remaining == 0 && input_pos == input_len`。
2. **Metal API 名字**（[renderer.rs](<../src/renderer.rs>)，原来根本没编译过）
   metal 0.32 用 `MTLRegion` / `MTLSamplerAddressMode` / `MTLTextureType`（`D2`）/
   `MTLTextureUsage::ShaderRead` / `MTLBlendFactor` / `MTLTextureMinMagFilter` /
   `MTLPixelFormat`；`color_attachments()` 返回引用而不是 `Option`；
   `get_function()` 返回 `Result<_, String>` 而不是 `anyhow`。
3. **预乘 alpha**（[main.rs](<../src/main.rs>)）
   管线混合 `source=One, dest=OneMinusSourceAlpha`（预乘），但 WebP/PNG 是直通 alpha。
   不预乘的话，立绘透明区域的 `RGB=255` 会按 `One` 加进画面，糊出一条**纯白横带**
   （最初以为是 stride 对齐问题，padding 之后像素完全没变才排除）。
4. **KAG 重复属性**（[script.rs](<../src/script.rs>)）
   `scenario/main/0_15.ks:111` 有一处 `@t_move lay=@t_move lay=5`（原作笔误）。
   KAG 用字典存属性、后者覆盖前者；旧解析器 `ensure!` 报错，导致 0_15 整章编译失败。

## 5. 当时的代码结构（测试数量为早期快照）

下列清单用于追溯早期实现，不代表当前文件清单或测试统计；现行结构见[README](<../README.md#实现结构>)。

```
koikari-metal/
├─ Cargo.toml            anyhow flate2 serde serde_json encoding_rs image(png,webp)
│                        macos: winit 0.30 metal 0.32 objc raw-window-handle
│                        core-graphics-types rodio 0.21(features: playback,vorbis,wav) ogg 0.9 opus 0.3
├─ build.rs              编译 native/text.m（ObjC 文本 shim）
├─ native/text.m         AppKit/CoreText → RGBA，唯一的 ObjC 代码
├─ src/archive.rs        ✓ XP3 只读解析 + 流式解压 + Adler-32
├─ src/assets.rs         ✓ patch 覆盖 data；别名解析（"bg1_0" → bgimage/bg1_0.webp）
├─ src/script.rs         ✓ KAG 词法/编译 + 受限表达式（安全，不执行 TJS）
├─ src/runtime.rs        ✓ 场景状态机（背景/立绘/对话/分支/音频效果/快照）
├─ src/save.rs           ✓ JSON 存档（版本 + 资源指纹）
├─ src/text.rs           ✓ CoreText 光栅化封装
├─ src/renderer.rs       ✓ Metal 单管线 quad + 纹理 LRU + 离屏渲染/回读
├─ src/audio.rs          ✓ Vorbis 流式 / Opus 解码；BGM/语音/SE 三总线
├─ src/main.rs           ✓ winit 窗口 + CAMetalLayer + 界面/输入
├─ tests/archive_synthetic.rs  20 个合成用例
├─ tests/archive_original.rs    2 个真归档只读用例
├─ tests/runtime_vm.rs          10 个合成用例
├─ tests/runtime_game.rs        3 个真归档用例
└─ docs/*.png            实际截帧
```

### 公开 API

```rust
assets::AssetStore::open(root) · read/text/resolve/names/archives/stats/fingerprint
runtime::Runtime::new(Arc<AssetStore>)
  · new_game()/start()/start_at(storage, label) · advance() -> Event
  · choose(usize) · snapshot()/restore(Snapshot) · take_effects() -> Vec<AudioEffect>
  · pub state: SceneState { background, sprites, speaker, text, choices, bgm, voice,
                            message_visible, message_style, font_size, warnings, ... }
renderer::Renderer::new() · upload_rgba() · begin_frame() · draw()/draw_uv()
  · present(drawable, clear) · present_capture(drawable, clear, path)
  · render_offscreen(w, h, clear) -> Vec<u8>   // 用于验证帧
audio::AudioEngine::new() · play_bgm/play_voice/play_se · stop_* · poll(assets)
```

## 6. 构建与运行（现行入口）

从 [GitHub 源码](<https://github.com/Greendot-galgame/koikari-metal>)构建，不使用早期本机交付的二进制或备份。先按[构建说明](<../BUILD-AND-RUN.md>)准备 Xcode / CMake，并在同一终端设置 SDK 与编译器环境，再执行：

```sh
git clone https://github.com/Greendot-galgame/koikari-metal.git koikari-metal-source
cd koikari-metal-source/koikari-metal
cargo +1.95.0 build --release --locked
export KOIKARI_GAME_ROOT="/path/to/owned-game"
./target/release/koikari-metal --root "$KOIKARI_GAME_ROOT"
```

完整前置要求、资源模式及存档说明以[构建说明](<../BUILD-AND-RUN.md>)为准。

## 7. 验证方式

现行验证分级与命令见[测试说明](<TESTING.md>)。普通 CI 不携带真实资源，不能复现本机资源审计或把资源测试提前返回理解为实际覆盖。

在不含原始游戏归档的源码检出中，不设置 `KOIKARI_GAME_ROOT`、`KOIKARI_ASSET_ROOT` 和 `KOIKARI_METAL_TESTS` 时，可运行合成回归：

```sh
cargo +1.95.0 test --locked
```

本机自有资源和 Metal 回归须明确选择资源目录及开关，详见[历史全面检查的复现步骤](<COMPREHENSIVE-AUDIT-2026-11-11.md#本机复现>)。早期截图和日志不是普通 CI 的输出；日志只在本机生成，不随仓库分发。本次文档更新没有运行、展示或播放游戏内容。

## 8. 已知限制 / 后续工作

这是早期交接的限制清单，现状以[README 的兼容边界](<../README.md#已知限制>)和[全面检查报告](<COMPREHENSIVE-AUDIT-2026-11-11.md#仍有的限制与未验证部分>)为准：

1. **影片、复杂动画时序和原版场景回想仍未实现，留待后续工作**。转场（`ts`/`te`/`trans`/`wt`）立即生效，震动和原始演出时长未复刻；有限立绘动作也不等于完整动画支持。
2. 开场 MP4 不播放（`openvideo`/`playvideo` 跳过并记录诊断）。
3. 不执行 TJS2：章节表、选择表、立绘宽度/位置表用受限解析读取，其余 `.tjs` 不执行。
4. B 显示最近 6 条**对话历史**，不是原版回想或场景重播；菜单只有说明文字。
5. 存档只有快速存档一个槽位（`quicksave.json`），没有多槽 UI。
6. 早期未完成项包括 BGM 淡入淡出、语音打断的精确时机、自动播放速度设置；不可从旧清单推断这些能力已新增。

## 9. 注意事项

- 不要依赖早期本机抽取目录、临时日志、二进制备份或卷上元数据，它们不是公开源码交付的一部分。
- 原始 `data.xp3` / `patch.xp3` 必须保持不变；真实资源测试检查读取前后的文件大小与修改时间，但不等于对全部载荷作密码学认证。
- 需要本机截图时才显式选择 `--capture`（离屏）或 `KOIKARI_WINDOW_SHOT`（真实窗口）；不要把游戏画面、台词、声音、归档或用户存档附入公开问题报告。
