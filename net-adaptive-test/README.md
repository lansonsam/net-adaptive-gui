# net-adaptive-test · 网卡自适应测速 (Rust 端口)

由 `自适应网络测试_beta.sh` 重写为单一静态二进制，目标 **aarch64 (ARM64) Linux**。

功能与原脚本一致：选网卡 → 选/扫描目标 IP → 连通性预检 → MTU 二分探测 →
逐档强制协商 **100M / 10M / 1000M** 测丢包·延迟·抖动·网卡计数器 → 汇总表 + 延迟折线图。

## 相比 Bash 版的优化
- **局域网扫描**用线程池（`SCAN_FANOUT` 个 worker）取代一次性 fork 254 个 `ping` 子进程，更快更省资源。
- ping/ethtool 输出解析全部**原生 Rust**，不再依赖 `grep/awk/sed/mktemp/bash`，精简系统也能跑。
- 编译为**单一静态二进制**（glibc 2.17），拷过去即用。
- `Ctrl+C` / `SIGTERM` 仍会自动把网卡恢复 `autoneg on`，避免卡在低速档。

## 运行时依赖（目标 ARM 设备上）
仍会调用系统命令：`ping` `ethtool` `ip` `arp` `ifconfig` `sudo` `stty`。
测速需 root（`ethtool -s` 改协商）——脚本内置 `sudo` 密码常量。

> 配置项（sudo 密码、发包数、并发数等）在 `src/main.rs` 顶部常量区，改后重新编译。

## 在 Windows 上交叉编译到 ARM64
一次性准备：
```powershell
rustup target add aarch64-unknown-linux-gnu
cargo install cargo-zigbuild
python -m pip install ziglang
```
构建：
```powershell
.\build-arm64.ps1
```
产物：`target\aarch64-unknown-linux-gnu\release\net-adaptive-test`

## 部署到 ARM 设备
```bash
scp net-adaptive-test user@设备IP:/tmp/
ssh user@设备IP
chmod +x /tmp/net-adaptive-test
sudo /tmp/net-adaptive-test        # 或直接运行, 内部用 sudo 提权
```

## 其他构建方式
- **在 ARM 设备本机编译**：装好 Rust 后 `cargo build --release`。
- **用 Docker (cross)**：`cargo install cross` 后 `cross build --release --target aarch64-unknown-linux-gnu`。
- **32 位 ARM (armv7)**：目标换成 `armv7-unknown-linux-gnueabihf`。
