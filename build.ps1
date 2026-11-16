# Compila a lib Rust fogusmartusb para o Windows e grava a DLL em
# native/windows-<arch>/, onde o _native.fg a procura ao importar a lib.
# Para .so (Linux) e .dylib (macOS) use o build.sh (WSL, Linux ou macOS).
#   ./build.ps1                  arquitetura desta máquina
#   ./build.ps1 -Arch arm64      outra arquitetura (precisa do target no rustup)
#   ./build.ps1 -All             x64, x86 e arm64 (os targets instalados no rustup)
param(
    [ValidateSet("x64", "x86", "arm64")][string]$Arch = "",
    [switch]$All
)
$ErrorActionPreference = "Stop"
$crate = "fogusmartusb"
$targets = @{ "x64" = "x86_64-pc-windows-msvc"; "x86" = "i686-pc-windows-msvc"; "arm64" = "aarch64-pc-windows-msvc" }

if (-not $Arch) {
    $Arch = switch ($env:PROCESSOR_ARCHITECTURE) { "ARM64" { "arm64" } "x86" { "x86" } default { "x64" } }
}
$archs = if ($All) {
    $installed = rustup target list --installed
    @("x64", "x86", "arm64") | Where-Object { $installed -contains $targets[$_] }
} else { @($Arch) }

foreach ($a in $archs) {
    $target = $targets[$a]
    Write-Host "== windows-$a ($target)"
    cargo build --release --target $target --manifest-path "$PSScriptRoot\$crate\Cargo.toml"
    if ($LASTEXITCODE -ne 0) { throw "cargo build falhou para $target" }
    $out = "$PSScriptRoot\native\windows-$a"
    New-Item -ItemType Directory -Force $out | Out-Null
    $dll = "$out\$crate.dll"
    # Uma DLL aberta por um programa Fogu não pode ser sobrescrita, mas pode ser
    # renomeada: a antiga vira .old-* e é apagada num build seguinte.
    Get-ChildItem "$out" -Filter "$crate.dll.old-*" -ErrorAction SilentlyContinue | ForEach-Object {
        Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue
    }
    try {
        Copy-Item "$PSScriptRoot\$crate\target\$target\release\$crate.dll" $dll -Force
    } catch {
        Rename-Item $dll "$crate.dll.old-$(Get-Date -Format yyyyMMddHHmmss)"
        Copy-Item "$PSScriptRoot\$crate\target\$target\release\$crate.dll" $dll -Force
    }
    Write-Host "  native\windows-$a\$crate.dll atualizado"
}
