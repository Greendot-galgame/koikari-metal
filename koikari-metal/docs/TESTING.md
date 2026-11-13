# 测试与 CI 边界

## CI 实际检查什么

[CI 配置](<../../.github/workflows/ci.yml>) 只在推送到 `main`、拉取请求和手动触发时运行；没有定时任务。同一工作流、同一分支引用（PR 使用其 PR 引用）的旧运行会被取消。

- 使用 GitHub-hosted `macos-15`，固定 Rust **1.95.0**，安装 `rustfmt` 和 `clippy`。这是本项目要求的已测工具链版本，不是对更低 MSRV 的验证。
- 所有 shell 步骤默认在项目目录执行。Checkout 固定到已知版本的提交，权限仅 `contents: read`，不持久化 Git 凭据。
- 使用 runner 自带的 Xcode/macOS SDK、Clang 和 CMake。原生 Objective-C 文本模块依赖 Apple 框架，Metal/音频依赖也必须在 macOS 上编译，不能用 Linux 编译通过代替。`SDKROOT`、`CC`、`CXX` 从当前选定的 Xcode 工具链解析；旧 Opus 原生依赖的 CMake 配置设置兼容策略下限 `3.5`，以兼容 CMake 4。
- 顺序执行格式检查、Clippy、`cargo test --locked --all-targets`、`cargo build --locked --release`。依赖遵循现有 [Cargo.lock](<../Cargo.lock>)，不自动更新锁文件。
- Clippy 将 `correctness`、`suspicious` 组作为错误；允许旧 `objc` 宏的 `unexpected_cfgs`，以及 `style`、`complexity`、`perf` 组。**这不表示所有风格/性能警告已经清零**，也不使用全局 `-D warnings`。
- 不获取游戏资源，不设置 `KOIKARI_GAME_ROOT`、`KOIKARI_ASSET_ROOT` 或 `KOIKARI_METAL_TESTS=1`，不启用 GPU/真实资源验收。不上传构建 artifact、游戏资源、截图、存档或本机日志。

**CI 绿色不等于“124 项真实资源 + Metal 完整验收已通过”。** 部分测试在缺少环境变量、私有归档或 Metal 开关时直接早退；Rust 测试框架仍可能把这些用例记作 `ok`，而不是 `ignored`。历史测试数量和历史本机结果不能当作本次 CI 的真实覆盖证据。CI 验证的是无资源条件下可运行的逻辑回归、macOS 原生依赖可编译，以及 release 构建成功。

构建使用 runner 的原生硬件架构，日志记录 `uname -m` 和 Rust host；不是 universal 二进制，也不承诺同时覆盖 Intel 与 Apple Silicon。工作流不执行 Developer ID 签名或 Apple 公证，不生成可直接发布的签名应用包。

## 本机准备：从仓库根进入项目

以下命令在 macOS 上、从本仓库根目录开始执行。需要 Xcode 或 Xcode Command Line Tools、可用的 macOS SDK、CMake 和 Rustup；完整 Metal 检查还需要支持 Metal 的本机设备及可用的图形会话。普通 GitHub-hosted CI 不替代这一环境。

```bash
cd koikari-metal

# 缺少 Apple 开发工具时，先安装并完成系统交互，再继续。
# xcode-select --install
xcode-select --print-path
xcrun --sdk macosx --show-sdk-path
xcrun --sdk macosx --find clang
cmake --version

rustup toolchain install 1.95.0 --profile minimal --component rustfmt --component clippy --no-self-update
export RUSTUP_TOOLCHAIN=1.95.0
export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
export CC="$(xcrun --sdk macosx --find clang)"
export CXX="$(xcrun --sdk macosx --find clang++)"
export CMAKE_POLICY_VERSION_MINIMUM=3.5
rustc --version --verbose
cargo --version
```

下面两节都接着使用这个项目目录。命令示例仅供手动运行；不要把私有资源路径写入仓库、CI 变量或公开日志。

## 无资源验证：接近 CI

先使用**不含游戏资源的工作副本**。原始归档测试和部分运行时测试还会回退到项目父目录（即仓库根）寻找归档，**只清除环境变量并不能保证它们跳过**。不需要移动、删除或复制你的合法原始资源；使用另一个纯源码工作副本即可。

```bash
(
  set -euo pipefail
  unset KOIKARI_GAME_ROOT KOIKARI_ASSET_ROOT KOIKARI_METAL_TESTS
  unset KOIKARI_NOTICE_CAPTURE_DIR

  if [ -f ../data.xp3 ] || [ -f ../patch.xp3 ] || [ -f ../adult.xp3 ]; then
    printf '%s\n' '仓库根含游戏归档；请改用无资源源码工作副本。' >&2
    exit 1
  fi

  mkdir -p target/test-logs
  cargo fmt --all -- --check
  cargo clippy --locked --all-targets -- \
    -A unexpected_cfgs \
    -A clippy::style -A clippy::complexity -A clippy::perf \
    -D clippy::correctness -D clippy::suspicious \
    2>&1 | tee target/test-logs/no-assets-clippy.log
  cargo test --locked --all-targets -- --nocapture \
    2>&1 | tee target/test-logs/no-assets-tests.log
  cargo build --locked --release
)
```

`--nocapture` 便于看见部分测试的早退提示；有些早退没有提示，不能凭用例计数证明真实资源已被测试。`pipefail` 确保使用 `tee` 保存日志不会掩盖 Cargo 失败。

## 合法私有资源 + Metal：本机完整回归

仅使用自己合法持有的本地归档，放在源码仓库外，不上传、不随构建分发，也不执行原版 TJS/DLL。完整双资源模式需要同一私有目录中的 `data.xp3`、`patch.xp3` 和可选追加资源 `adult.xp3`；**若缺少追加包，只能声称基础资源模式通过，不能声称双模式全套通过**。不同游戏版本/归档布局可能不符合当前测试的固定断言。

先把下面的通用占位路径替换为自己的合法资源目录。`KOIKARI_GAME_ROOT` 与 `KOIKARI_ASSET_ROOT` 都要指向同一目录：不同集成测试读取的变量并不相同。确认设备支持 Metal、当前图形会话可用，并先将系统音频静音：现有 UI 回归会初始化音频设备并可能走正常音频路径，不能承诺仅解码而绝不输出声音。下面不启动游戏主程序，也不设置截图输出变量。

```bash
(
  set -euo pipefail
  export KOIKARI_GAME_ROOT="/path/to/private-owned-game"
  export KOIKARI_ASSET_ROOT="$KOIKARI_GAME_ROOT"
  export KOIKARI_METAL_TESTS=1
  unset KOIKARI_NOTICE_CAPTURE_DIR

  # 双资源模式完整检查：缺少任一归档就停止，不把早退当成验收。
  for archive in data.xp3 patch.xp3 adult.xp3; do
    test -f "$KOIKARI_GAME_ROOT/$archive"
  done

  mkdir -p target/test-logs
  cargo test --locked --all-targets -- --nocapture --test-threads=1 \
    2>&1 | tee target/test-logs/full-local-tests.log

  # 另在优化构建下运行坏档、运行时恢复及全资源模式回归。
  cargo test --locked --release \
    --test audit_save_validation \
    --test audit_runtime_regressions \
    --test full_compatibility_audit \
    -- --nocapture --test-threads=1 \
    2>&1 | tee target/test-logs/release-regressions.log
  cargo build --locked --release
)
```

设置 Metal 开关只是启用用例，不会替你提供 GPU。应检查日志确有基础/追加资源统计和 Metal 用例执行、没有资源跳过；即使所有现有测试通过，也不代表穷举全部剧情条件、逐帧目视验收、全量音频试听或所有硬件/窗口/设备切换验证。

## 日志与验收记录

上述 `target/test-logs/` 中的文件是**运行命令后在本机生成**的诊断，不是仓库自带附件，也没有预先存在的 GitHub 下载链接。不要把历史日志链接或“124”这个数当成本次通过证明。

复核时记录提交版本、macOS/Xcode/Rust 版本、CPU 架构、所用资源版本/是否包含追加包、是否启用 Metal、各命令退出状态和实际覆盖；私有路径、资源元数据或失败上下文可能出现在日志中，公开前先检查并脱敏。不要上传游戏归档、资源截图、原始剧情、个人存档或未经审核的本机日志。
