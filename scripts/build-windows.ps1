$ErrorActionPreference = "Stop"
$Target = "x86_64-pc-windows-msvc"
$Root = Split-Path $PSScriptRoot
$Desk = Join-Path $Root "apps/desk"
$Stage = Join-Path $Desk "src-tauri/binaries"
$Temp = Join-Path ([IO.Path]::GetTempPath()) ("fesen-" + [Guid]::NewGuid())
Push-Location $Root
try {
    New-Item -ItemType Directory -Force $Stage, $Temp | Out-Null
    $Zip = Join-Path $Temp "wintun.zip"
    Invoke-WebRequest "https://www.wintun.net/builds/wintun-0.14.1.zip" -OutFile $Zip
    if ((Get-FileHash -Algorithm SHA256 $Zip).Hash.ToLowerInvariant() -ne "07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51") {
        throw "Wintun SHA-256 校验失败"
    }
    Expand-Archive $Zip $Temp
    Copy-Item (Join-Path $Temp "wintun/bin/amd64/wintun.dll") (Join-Path $Stage "wintun.dll") -Force
    Copy-Item (Join-Path $Temp "wintun/LICENSE.txt") (Join-Path $Stage "Wintun-LICENSE.txt") -Force
    cargo build --locked --release -p fesen-core --bin fesen-core --target $Target
    if ($LASTEXITCODE) { throw "Rust 核心构建失败" }
    Copy-Item "target/$Target/release/fesen-core.exe" (Join-Path $Stage "fesen-core-$Target.exe") -Force
    pnpm -C $Desk install --frozen-lockfile
    if ($LASTEXITCODE) { throw "客户端依赖安装失败" }
    pnpm -C $Desk exec tauri build --target $Target --bundles nsis
    if ($LASTEXITCODE) { throw "安装包构建失败" }
} finally {
    Pop-Location
    if (Test-Path $Temp) { Remove-Item -Recurse -Force $Temp }
}
