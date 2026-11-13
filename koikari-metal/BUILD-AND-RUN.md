# 从 GitHub 源码构建与运行

这是「恋爱，我就借走咯」的 Rust + Metal 原生 macOS 兼容运行时，不是完整的原版引擎。仓库**仅提供源码，不提供预编译可执行文件，也不包含原始游戏资源或 Steam 组件**。

## 1. 构建环境

推荐已验证的 **Rust 1.95.0、macOS / Apple Silicon**，并安装 Xcode Command Line Tools 和 CMake。Intel Mac 未经人工验证；不承诺 iOS 或 Windows 支持。

- 安装 Xcode Command Line Tools：`xcode-select --install`。
- 使用 [rustup](<https://rustup.rs/>) 安装 Rust，并准备 1.95.0 工具链：`rustup toolchain install 1.95.0 --profile minimal`。
- 安装 [CMake](<https://cmake.org/download/>)，用 `cmake --version` 确认在 `PATH` 中。旧 Opus 原生依赖需要下述兼容策略设置才能适配 CMake 4。
- 首次构建需要下载锁定的 Cargo 依赖。仓库使用的工具链配置位于[根工具链文件](<../rust-toolchain.toml>)。

## 2. 获取源码并编译

```sh
git clone https://github.com/Greendot-galgame/koikari-metal.git koikari-metal-source
cd koikari-metal-source/koikari-metal
export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
export CC="$(xcrun --sdk macosx --find clang)"
export CXX="$(xcrun --sdk macosx --find clang++)"
export CMAKE_POLICY_VERSION_MINIMUM=3.5
cargo +1.95.0 build --release --locked
```

后续构建和测试命令请在同一终端中执行，或重新设置上述 SDK / 编译器 / CMake 环境。更完整的前置检查见[测试环境准备](<docs/TESTING.md>)。

编译产物为 `target/release/koikari-metal`，由你的本机构建生成，不随源码仓库分发。可选的[构建包装脚本](<tools/cargo.sh>)把 Cargo 缓存及产物保存在项目内；使用标准 Cargo 命令无需调用它。

## 3. 指定自有游戏资源

资源无需移动，也不要提交到源码仓库。把下列占位路径替换为你合法拥有的游戏目录：

```sh
export KOIKARI_GAME_ROOT="/path/to/owned-game"
./target/release/koikari-metal --root "$KOIKARI_GAME_ROOT"
```

资源目录示例：

```text
/path/to/owned-game/
├─ data.xp3           # 必需，保持原始文件不变
├─ patch.xp3          # 可选补丁；历史真实资源回归使用了此文件
└─ adult.xp3          # 可选追加归档
```

显式传入 `--root` 优先于环境变量；均未提供时，程序会检查当前工作目录及可执行文件附近最多四层父目录。推荐始终显式指定，避免误选资源版本。覆盖顺序、存档隔离和 `--base-only` 详见[追加归档说明](<docs/ADDITIONAL-ARCHIVE.md>)。

更新源码后，先保存需要的进度并完全退出旧游戏进程，再重新构建、运行新产物；旧进程不会自动加载更新。基础资源模式下的旧 v1 快速存档与追加模式不能混用；继续旧基础资源进度时使用：

```sh
./target/release/koikari-metal --root "$KOIKARI_GAME_ROOT" --base-only
```

可用 `KOIKARI_SAVE_DIR` 指定存档目录，设置后须自行隔离不同资源版本；建议在验证时使用独立测试目录。原始归档始终只读，不执行 Windows 程序、原始 TJS 或插件 DLL。

## 4. 操作与兼容边界

空格 / 回车 / 左键推进 · 数字键选择 · Ctrl 快进 · A 自动 · **B 对话历史（最近 6 条，不是原版场景回想）** · H 隐藏文本 · F5 快存 · F9 快读 · Esc 菜单。

**影片播放、复杂动画时序和原版场景回想仍未实现，留待后续工作。** 现有有限动作和立即转场不能视为完整原作演出支持。更多限制见[运行时说明](<README.md#已知限制>)。

## 5. 测试与历史记录

现行测试分级、普通 CI 与可选本机验证命令见[测试说明](<docs/TESTING.md>)。

2026-11-11 的[全面检查](<docs/COMPREHENSIVE-AUDIT-2026-11-11.md>)记录了本机 **124 项测试通过**，包含自有真实资源与显式启用的 Metal 回归。基础 95 章、追加模式 124 章等数据是该次历史验证范围，不是普通 CI 的资源覆盖承诺。**真实资源测试不在普通 CI 中**；没有资源或没有启用相关开关时，部分测试会提前返回，因此测试总数不能代替覆盖证明。

历史[图层修复记录](<docs/FIXES-2026-11-11.md>)、[音频修复记录](<docs/AUDIO-FIX-2026-11-11.md>)和[交接记录](<docs/HANDOFF.md>)用于追溯，不是预编译交付或现行测试数量的依据。诊断日志只在本机复现时生成，不随仓库提供；各记录保留了源测试入口及复现说明。本次文档更新没有运行、展示或播放游戏内容。

已有[标题截图](<docs/docs-title.png>)、[游戏截图](<docs/docs-gameplay.png>)和[选择截图](<docs/docs-choice.png>)为历史截帧，不是此次重新生成，也不表示所有场景已人工验收。

## 6. 许可与资源权利

运行时代码采用[根 MIT 许可证](<../LICENSE>)。文档图片、游戏截图、原始游戏脚本、图像、音频及其他游戏资源不在代码许可范围内，权利属于各自权利人；请自行合法取得资源，不要把它们上传到仓库或附入问题报告。
