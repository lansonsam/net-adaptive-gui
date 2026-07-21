# 在 Windows 上把 Slint GUI 交叉编译到 aarch64 (ARM64) Linux
# GUI 会链接 libfontconfig, 所以用 cross(Docker) 在 ARM64 sysroot 里构建。
#
# 首次准备(只需一次):
#   1) 安装并启动 Docker Desktop
#   2) cargo install cross
# 依赖的链接期原生库由 Cross.toml 的 pre-build 自动在容器内安装。

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

# 确认 Docker 在运行
$null = docker ps 2>$null
if ($LASTEXITCODE -ne 0) { Write-Error "Docker 未运行, 请先启动 Docker Desktop"; exit 1 }

Write-Host "cross 构建 aarch64-unknown-linux-gnu (首次会拉镜像+装 sysroot, 较慢)…" -ForegroundColor Cyan
cross build --release --target aarch64-unknown-linux-gnu

$out = "target\aarch64-unknown-linux-gnu\release\net-adaptive-gui"
if (Test-Path $out) {
    $kb = [math]::Round((Get-Item $out).Length / 1KB, 1)
    Write-Host "`n✔ 产物: $out  (${kb} KB)" -ForegroundColor Green
    Write-Host "  传到带图形桌面的 ARM 设备后: chmod +x net-adaptive-gui && ./net-adaptive-gui" -ForegroundColor Gray
}
