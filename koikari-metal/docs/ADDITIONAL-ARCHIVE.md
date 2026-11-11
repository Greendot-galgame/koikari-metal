# 可选追加归档兼容

## 加载方式

现在启动时会自动发现游戏目录里的[追加归档](</Users/greenhats/Documents/租借男友/adult.xp3>)。资源覆盖顺序由低到高：

```text
data.xp3 → adult.xp3（可选）→ patch.xp3（可选）
```

高优先级补丁的别名仍可覆盖低优先级的完整路径；遇到歧义、损坏或无法读取的已有归档，会明确报错，不会悄悄回落。没有追加包时仍按原来的基础资源模式工作。

所有归档只读，不重打包、复制原始资源或执行包内的 TJS、DLL、Windows 程序。

## 使用

先退出旧游戏窗口，再正常启动，原来的命令不用改：

```sh
cd "/Users/greenhats/Downloads/koikari-metal-delivery/koikari-metal"
./koikari-metal --root "/Users/greenhats/Documents/租借男友"
```

启动日志会列出三份归档。当前实测：追加包 **4,554 项**，合并资源命名空间 **19,513 项**。其中以前缺失的 `vo5_1089`、`vo5_1169` 现在可以从追加包读取，并已验证能解码为非空 PCM；没有使用替代录音。

## 原存档兼容

追加包替换了部分脚本和章节元数据（当前元数据由 95 章变为 124 章），旧存档的脚本位置不能直接移植到新版。默认存档目录因此分开：

```text
原资源模式：~/Library/Application Support/KoikariMetal/quicksave.json
追加包模式：~/Library/Application Support/KoikariMetal/with-additional/quicksave.json
```

建议新模式从新游戏开始。要继续原来的快存，使用：

```sh
./koikari-metal --root "/Users/greenhats/Documents/租借男友" --base-only
```

`--base-only` 完全忽略追加包，保留原来的资源指纹、章节表和默认存档位置。显式设置 `KOIKARI_SAVE_DIR` 时仍尊重该目录，由使用者自行分开不同版本的存档。

## 验证范围

**87 项测试全部通过**，见[测试日志](<../../diagnostics/additional-archive-tests.log>)。

- 真实追加归档索引可解析，选取的语音载荷通过 Adler-32 校验与 Opus 解码。
- 三层资源覆盖、别名、缺包、坏包、非文件、歧义及原模式指纹兼容均有合成回归。
- 新章节元数据能加载，常规开场能正常运行；不执行原始 TJS。
- 原有基础资源模式的图层、29 条选择分支、音频提示及 Metal 像素回归仍通过。
- 三份归档读取前后的文件大小与修改时间不变。

**这次验证的是归档接入、资源覆盖和常规开场，不代表新增的所有剧情、图片或演出均已逐一验证。** 原有的视频、复杂动画时序及受限脚本支持边界仍然存在。未展示或播放敏感内容。
