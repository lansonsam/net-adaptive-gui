# 网卡自适应测速 (ARM64)

把原 `自适应网络测试_beta.sh` 重写为 Rust，面向 **aarch64 (ARM64) Linux**，含两种形态：

| 目录 | 形态 | 说明 |
|------|------|------|
| `net-adaptive-test/` | **命令行 (TUI)** | 单一静态二进制，终端交互，无需图形环境 |
| `net-adaptive-gui/`  | **图形界面 (Slint · Material)** | 桌面 GUI，暗色 Material 风格，需图形桌面 |

功能一致：选网卡 → 选/扫描目标 IP → 连通性预检 → MTU 二分探测 → 逐档强制协商
**100M / 10M / 1000M** 测丢包·延迟·抖动·网卡计数器 → 汇总 + 延迟折线图。

## 构建 (GitHub Actions, 推荐)

推到 GitHub 后，`.github/workflows/build.yml` 会在**原生 amd64 runner** 上编译两个二进制，
到 Actions run 的 **Artifacts** 里下载 `amd64-binaries`（内含 CLI 与 GUI）。

手动触发：Actions 页面选 `build-amd64` → Run workflow；或 `gh workflow run build-amd64`。

## 运行 (ARM 设备上)

```bash
chmod +x net-adaptive-test net-adaptive-gui
sudo ./net-adaptive-gui      # 图形界面 (需桌面环境)
sudo ./net-adaptive-test     # 命令行
```

> **sudo 密码**：代码不再内置密码。用 `sudo` 直接运行即已是 root，`ethtool` 改协商无需再输密码。
> 若以普通用户运行且 sudo 需要密码，可设 `NET_TEST_SUDO_PASS=你的密码` 环境变量。

运行时依赖设备已安装：`ethtool` `iputils-ping` `iproute2` `net-tools`，GUI 另需图形桌面
(X11 或 Wayland) 与 `fontconfig`（桌面系统一般都有）。

## 本地构建 (可选)

- 在 ARM 设备本机：进入任一目录 `cargo build --release`。
- CLI 也可从 Windows 用 `cargo-zigbuild` 交叉编译（见 `net-adaptive-test/build-arm64.ps1`）。
  GUI 因链接 `libfontconfig`，交叉编译较麻烦，推荐用 Actions 或在设备/Linux 上构建。
