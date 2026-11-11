# 如何使用这份交付

这是「恋爱，我就借走咯」的 Rust + Metal 原生 macOS 重制版，**不含任何 Steam 组件**。

## 1. 放置

把 `koikari-metal/` 整个文件夹放到游戏目录下（和 `data.xp3`、`patch.xp3` 同级）：

```
恋爱，我就借走咯/
├─ data.xp3          ← 你的原始文件，不要改动
├─ patch.xp3
└─ koikari-metal/
   ├─ koikari-metal          ← 已编译好的 arm64 可执行文件
   ├─ src/ tests/ native/ docs/
   └─ Cargo.toml
```

## 2. 直接运行（无需编译）

```sh
cd 恋爱，我就借走咯/koikari-metal
./koikari-metal
```

程序会向上查找 `data.xp3`；也可以用 `--root <游戏目录>` 显式指定。**你当前的资源无需移动，使用：**

```sh
cd "/Users/greenhats/Downloads/koikari-metal-delivery/koikari-metal"
./koikari-metal --root "/Users/greenhats/Documents/租借男友"
```

**更新后须先退出旧游戏窗口再启动。** 已运行的旧进程不会自动加载新程序；v1 快速存档仍兼容。图层缓存、头像和选项命中修复详情见[修复说明](<docs/FIXES-2026-11-11.md>)。

> 如果系统提示「无法验证开发者」：右键 → 打开，或执行
> `xattr -dr com.apple.quarantine koikari-metal`。

## 3. 重新编译

```sh
cd koikari-metal
./tools/cargo.sh build --release     # 产物在 target/release/koikari-metal
```

需要 Xcode Command Line Tools 和 Rust 1.85+。

## 4. 操作

空格/回车/左键推进 · 数字键选择 · Ctrl 快进 · A 自动 · B 回忆 · H 隐藏文本
· F5 快存 · F9 快读 · Esc 菜单

## 5. 截图

`docs/docs-title.png`、`docs/docs-gameplay.png`、`docs/docs-choice.png` 是真实运行截帧。
