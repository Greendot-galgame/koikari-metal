# 可选追加归档兼容

> 本文保留追加归档接入阶段的历史验证。后续 124 项测试及基础 95 章 / 追加模式 124 章的覆盖边界见[全面检查](<COMPREHENSIVE-AUDIT-2026-11-11.md>)；当前构建与测试见[构建说明](<../BUILD-AND-RUN.md>)和[测试说明](<TESTING.md>)。真实资源测试不在普通 CI 中，本次文档更新没有运行、展示或播放游戏内容。
>
> 代码采用[根 MIT 许可证](<../../LICENSE>)，不包括图片、游戏截图与原始游戏资源；追加归档须由使用者合法自备，仓库不分发。

## 加载方式

启动时会自动发现 `--root` 或 `KOIKARI_GAME_ROOT` 指定的自有游戏目录中的 `adult.xp3`。资源覆盖顺序由低到高：

```text
data.xp3 → adult.xp3（可选）→ patch.xp3（可选）
```

高优先级补丁的别名仍可覆盖低优先级的完整路径；遇到歧义、损坏或无法读取的已有归档，会明确报错，不会悄悄回落。没有追加包时仍按原来的基础资源模式工作。

所有归档只读，不重打包、复制原始资源或执行包内的 TJS、DLL、Windows 程序。

## 使用

先按[构建说明](<../BUILD-AND-RUN.md>)从 GitHub 获取源码，在项目子目录构建；公开仓库不提供预编译程序。推荐已验证的 Rust 1.95.0、macOS / Apple Silicon，启动前先退出旧游戏窗口：

```sh
cargo +1.95.0 build --release --locked
export KOIKARI_GAME_ROOT="/path/to/owned-game"
./target/release/koikari-metal --root "$KOIKARI_GAME_ROOT"
```

若三份归档都存在，启动日志会依次列出它们。历史所测追加包为 **4,554 项**，合并资源命名空间 **19,513 项**。其中基础模式缺失的 `vo5_1089`、`vo5_1169` 可从该追加包读取，当时已验证能解码为非空 PCM，没有使用替代录音；其他资源版本须自行验证。

## 原存档兼容

追加包替换了部分脚本和章节元数据（所测基础资源为 **95 章**，追加模式为 **124 章**），旧存档的脚本位置不能直接移植到新版。默认存档目录因此分开：

```text
原资源模式：~/Library/Application Support/KoikariMetal/quicksave.json
追加包模式：~/Library/Application Support/KoikariMetal/with-additional/quicksave.json
```

建议新模式从新游戏开始。要继续原来的快存，使用：

```sh
./target/release/koikari-metal --root "$KOIKARI_GAME_ROOT" --base-only
```

`--base-only` 完全忽略追加包，保留原来的资源指纹、章节表和默认存档位置。显式设置 `KOIKARI_SAVE_DIR` 时仍尊重该目录，由使用者自行分开不同版本的存档。

## 验证范围

**该阶段历史记录为 87 项测试全部通过**，不是当前测试总数。日志只在本机生成、被版本控制排除，不随公开仓库分发。源回归见[追加归档状态测试](<../tests/archive_additional.rs>)、[合成资源覆盖测试](<../tests/archive_synthetic.rs>)和[音频内部测试](<../src/audio.rs>)。

- 真实追加归档索引可解析，选取的语音载荷通过 Adler-32 校验与 Opus 解码。
- 三层资源覆盖、别名、缺包、坏包、非文件、歧义及原模式指纹兼容均有合成回归。
- 新章节元数据能加载，常规开场能正常运行；不执行原始 TJS。
- 原有基础资源模式的图层、29 条选择分支、音频提示及 Metal 像素回归仍通过。
- 三份归档读取前后的文件大小与修改时间不变。

**这一阶段验证的是归档接入、资源覆盖和常规开场，不代表新增的所有剧情、图片或演出均已逐一验证。** 后续章节审计也不等于逐帧或人工验收。影片、复杂动画时序与原版场景回想仍未实现，留待后续工作；B 仅显示最近 6 条对话历史。受限脚本边界仍然存在。

## 本机复现

在项目子目录中，只有合法持有追加归档时才选择对应资源目录。以下状态/索引测试不输出台词或渲染图片，也不播放媒体；它不代替音频选样解码和 Metal 验证：

```sh
export KOIKARI_GAME_ROOT="/path/to/owned-game"
export KOIKARI_ASSET_ROOT="$KOIKARI_GAME_ROOT"
mkdir -p ../diagnostics
cargo +1.95.0 test --locked --test archive_additional -- --nocapture \
  > ../diagnostics/additional-archive-tests.log 2>&1
```

检查退出状态及本机日志；未选择资源或没有追加归档时，测试可能提前返回。更完整的可选验证见[测试说明](<TESTING.md>)与[全面检查复现步骤](<COMPREHENSIVE-AUDIT-2026-11-11.md#本机复现>)。不要公开上传游戏归档、台词、媒体或用户存档。
