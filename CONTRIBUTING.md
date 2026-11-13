# 贡献指南

感谢帮助完善 Koikari Metal。当前以 macOS Rust + Metal 兼容运行时为目标，不是完整原版引擎，也不承诺任意游戏脚本可直接运行。

## 开始前

- 阅读[首页](README.md)、[构建指南](koikari-metal/BUILD-AND-RUN.md)及[测试说明](koikari-metal/docs/TESTING.md)。
- 新功能请先开 Issue 确认语义和范围，尤其是影片、动画时间轴、回想导航与存档格式变化。
- 原始 TJS/DLL/EXE 不执行。对未知语义保留明确诊断，不用静默 no-op 或猜测分支掩盖问题。

## 开发与检查

Rust 工具链由[工具链配置](rust-toolchain.toml)固定为 1.95.0。命令在 Rust 项目目录执行：

```sh
cd koikari-metal
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- \
  -A unexpected_cfgs -A clippy::style -A clippy::complexity -A clippy::perf \
  -D clippy::correctness -D clippy::suspicious
cargo test --locked --all-targets
cargo build --release --locked
```

没有私有游戏资源时，相关集成测试会提前返回；不能将 CI 的绿色结果描述为真实剧情/媒体/Metal 全套验收。需要本机完整检查时，按[测试指南](koikari-metal/docs/TESTING.md)配置合法资源，并先确认媒体及输出设备的安全性。

## 回归与提交

1. 一个提交/PR 聚焦一个问题，说明触发条件、预期与实际行为。
2. 优先使用 `Runtime::from_scripts`、合成 XP3 和可控时钟构造最小回归，不嵌入原始剧情。
3. 修改存档、渲染或音频生命周期时，覆盖失败路径、旧格式兼容和资源释放。
4. 不删除失败断言、不全局禁用声音或强行跳章来使测试通过。
5. 说明哪些检查实际执行、哪些因为缺少资源/设备未执行；不要把早退当作完整集成验证。

## 不应提交的内容

不要提交 XP3、原始素材、完整剧情台词、原版插件、存档、账号信息、访问令牌、构建目录或本机日志。报告运行问题时，只提供必要指令名、行号、资源标识和已脱敏环境信息。截图应避免个人或敏感内容，并确保有权分享。

原创代码贡献在项目 [MIT 许可](LICENSE)下提交；第三方代码须保留来源及兼容许可，不得将游戏素材当作 MIT 内容。安全问题请使用[私密报告流程](SECURITY.md)，不要在公开 Issue 贴利用细节或秘密数据。
