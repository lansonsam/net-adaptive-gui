# 在 Windows 上交叉编译到 aarch64 (ARM64) Linux
# 依赖: rustup 目标 aarch64-unknown-linux-gnu、cargo-zigbuild、ziglang(pip)
# 首次准备(只需一次):
#   rustup target add aarch64-unknown-linux-gnu
#   cargo install cargo-zigbuild
#   python -m pip install ziglang
#
# 之后每次构建直接运行本脚本即可。

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

# 把 ziglang 自带的 zig.exe 和 cargo bin 加入本次会话 PATH
$zigdir = python -c "import ziglang, os; print(os.path.dirname(ziglang.__file__))"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$zigdir;$env:PATH"

# glibc 2.17 = 兼容绝大多数 ARM Linux(含较老系统)。设备较新可去掉 .2.17
$target = "aarch64-unknown-linux-gnu.2.17"

Write-Host "zig: $(zig version)  →  target: $target" -ForegroundColor Cyan
cargo zigbuild --release --target $target

$out = "target\aarch64-unknown-linux-gnu\release\net-adaptive-test"
if (Test-Path $out) {
    $kb = [math]::Round((Get-Item $out).Length / 1KB, 1)
    Write-Host "`n✔ 产物: $out  (${kb} KB)" -ForegroundColor Green
    Write-Host "  传到 ARM 设备后: chmod +x net-adaptive-test && sudo ./net-adaptive-test" -ForegroundColor Gray
}
