# codex-proxy-plugin-candy-eval

`codex-proxy-rs` 官方插件 SDK（`gateway-plugin-sdk`）编写的 Codex 账号降智检测插件。

## 功能特性

- **完全兼容宿主指纹链**：通过宿主 `host.model.execute` 发起内部调用，完整复用 `codex-proxy-rs` 当前配置的 `CodexWireProfile`、`x-codex-installation-id`、代理、地区对齐与并发租约；插件本身不伪造任何第三方 client 指纹。
- **三种检测模式**：
  - **糖果题（逻辑推理检测）**：可配置单账号 1–10 次串行检验与 reasoning effort，统计通过率与明细。
  - **模型指纹归因（`lm-detector`）**：内置 Rust 版 `lm-detector` 校准权重与随机 3 条数字挑战，给出模型归因概率与是否降智/错模型偏差结论。
  - **画鹈鹕测试**：支持「原版 HTML+SVG」与「多合一 SVG」，结果页面在隔离沙箱 `<iframe sandbox="">` 中展示。
- **批量队列与每日定时检测**：借助宿主 `Maintenance`（`plugin.reconcile`）能力支持后台无人值守队列执行与指定时区每日定时触发。
- **零侵入密钥托管**：默认由插件通过 `host.groups.ensure` + `host.keys.ensure` 自动托管专用内部 Key，也可手动指定已有 Client Key。

## 安装

在 `codex-proxy-rs` 管理后台的「插件中心」通过 GitHub 安装：

```text
Light-yzc/codex-proxy-plugin-candy-eval
```

或从本仓库 [Releases](https://github.com/Light-yzc/codex-proxy-plugin-candy-eval/releases) 下载对应平台包（`light-yzc.candy-eval-<version>-<target>.tar.gz`）上传安装。

## 打包开发

```bash
pnpm --dir frontend install --frozen-lockfile
pnpm --dir frontend build
cargo test --manifest-path backend/Cargo.toml --locked
bash scripts/package aarch64-unknown-linux-gnu
```

## 致谢

- 数字指纹权重与评估套件来自 [Ikaleio/lm-detector](https://github.com/Ikaleio/lm-detector)（MIT License，见 [backend/data/lm-detector/LICENSE](backend/data/lm-detector/LICENSE)）。
- 糖果题与检测灵感参考自 [haowang02/cpa-plugin-codex-candy-eval](https://github.com/haowang02/cpa-plugin-codex-candy-eval)。
