# 交接文档 — 恋爱，我就借走咯 · macOS Rust + Metal 重制

> 状态：**可运行**。窗口、渲染、音频、剧情、分支、存档全部打通，43 个测试通过。

## 1. 目标与达成情况

把 Windows 版 KiriKiri2 / KAG 游戏用 Rust + Metal 重写成 macOS 原生运行时：

| 目标 | 状态 |
|---|---|
| 直接读取原始 `data.xp3` / `patch.xp3`（不复制、不修改、不重打包） | ✅ 只读流式读取，集成测试断言 size/mtime 不变 |
| 完全去掉 Steam | ✅ 不加载 `steam_api.dll` / `krkrsteam.dll` / `DrawDeviceForSteam.dll` / `steam_autocloud.vdf`，`achievement` 命令记录诊断后跳过 |
| 不运行 Windows PE / 原生插件 DLL | ✅ 全部用 Rust 重新实现 |
| 画面 | ✅ Metal 渲染背景 / 三层立绘 / 文本框 / 分支 / 标题 / 记忆 / 菜单 |
| 声音 | ✅ BGM/SE 走 Ogg Vorbis 流式，语音走 Ogg Opus 解码 |
| 可玩 | ✅ 首章 400+ 次推进、95 章编译、12 张选择表分支可达 |

## 2. 环境约束（先看，否则会卡住）

1. **sandbox 是 `workspace-write`，而且本卷（exFAT）不允许创建目录**。
   `mkdir` 在 `/Volumes/Untitled/...` 下一律 `No such file or directory`（沙箱拒绝）。
   因此：
   - 构建必须把 `CARGO_TARGET_DIR` 指到 `/tmp` 下（cargo 会建很多子目录）；
   - `write` / `edit` 工具会失败（它们要建临时目录），改用 `cat > file <<'EOF'`；
   - 新文件只能放进**已存在**的目录（`src/`、`tests/`、`docs/`）。
2. 本卷写入速度约 **5 MB/s**，且偶发 `EIO`。大量编译请务必用 `/tmp`。
3. `tools/cargo.sh` 把 `CARGO_HOME`/`CARGO_TARGET_DIR` 指到项目内。
   在沙箱里跑会因为无法建目录而失败；沙箱外（用户自己）没问题。
4. 工具链：macOS 27.0.1 / Apple Silicon / rustc 1.95.0 / Xcode CLI tools。
   `libopus` 由 `opus` crate 链接。

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

1. **zlib 流式解压误报**（`archive.rs`）
   旧代码在 `remaining == 0` 时立刻传 `FlushDecompress::Finish`，即使输出缓冲区
   （32 KiB）还装不下剩余数据 —— miniz_oxide 会报 `invalid zlib stream`，
   表现为「脚本全读不出来」。正确条件：`remaining == 0 && input_pos == input_len`。
2. **Metal API 名字**（`renderer.rs`，原来根本没编译过）
   metal 0.32 用 `MTLRegion` / `MTLSamplerAddressMode` / `MTLTextureType`（`D2`）/
   `MTLTextureUsage::ShaderRead` / `MTLBlendFactor` / `MTLTextureMinMagFilter` /
   `MTLPixelFormat`；`color_attachments()` 返回引用而不是 `Option`；
   `get_function()` 返回 `Result<_, String>` 而不是 `anyhow`。
3. **预乘 alpha**（`main.rs`）
   管线混合 `source=One, dest=OneMinusSourceAlpha`（预乘），但 WebP/PNG 是直通 alpha。
   不预乘的话，立绘透明区域的 `RGB=255` 会按 `One` 加进画面，糊出一条**纯白横带**
   （最初以为是 stride 对齐问题，padding 之后像素完全没变才排除）。
4. **KAG 重复属性**（`script.rs`）
   `scenario/main/0_15.ks:111` 有一处 `@t_move lay=@t_move lay=5`（原作笔误）。
   KAG 用字典存属性、后者覆盖前者；旧解析器 `ensure!` 报错，导致 0_15 整章编译失败。

## 5. 代码结构

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

## 6. 构建与运行

```sh
cd koikari-metal
./tools/cargo.sh build --release
./target/release/koikari-metal                 # 自动向上找 data.xp3
./target/release/koikari-metal --root "<游戏目录>"
```

沙箱内开发时（不能建目录）：

```sh
export CARGO_HOME=/tmp/koikari/cargo-home
export CARGO_TARGET_DIR=/tmp/koikari/target
cargo test
cargo run --release --bin koikari-metal -- --capture shot.png --advance 2
```

## 7. 验证方式（下次改完请照做）

```sh
cargo test                                         # 43 个
cargo run --release -- --capture /tmp/a.png --advance 2      # 首章一帧
cargo run --release -- --capture /tmp/t.png --title          # 标题
cargo run --release -- --capture /tmp/c.png --advance 5000   # 第一个分支
KOIKARI_WINDOW_SHOT=/tmp/w.png ./target/release/koikari-metal  # 真实窗口截帧（跑几秒后杀掉）
```

对照 `docs/docs-gameplay.png`（立绘表情、腮红、文本框、金色人名）。

## 8. 已知限制 / 下一步

1. 转场（`ts`/`te`/`trans`/`wt`）、震动、立绘缓动：立即生效，不还原原始时长。
2. 开场 MP4 不播放（`openvideo`/`playvideo` 跳过 + 诊断）。
3. 不执行 TJS2：章节表、选择表、立绘宽度/位置表用受限解析读取，其余 `.tjs` 不执行。
4. 回忆界面只显示最近 6 条；菜单只有说明文字。
5. 存档只有快速存档一个槽位（`quicksave.json`），没有多槽 UI。
6. 缺少：BGM 淡入淡出、语音打断的精确时机、自动播放速度设置。

## 9. 注意事项

- `extracted/` 里全是 0 字节的旧解析器产物，**不要参考**。看剧本用 `--capture` 或
  `AssetStore::text()`。
- 卷上到处都是 macOS 的 `._*` AppleDouble 文件，不是源码。
- 原始 `data.xp3` / `patch.xp3` 必须字节不变；测试已在断言。
- 需要新的验证截图时，用 `--capture`（离屏）或 `KOIKARI_WINDOW_SHOT`（真实窗口）。
