# Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
#requires -Version 5.1
<#
.SYNOPSIS
Build the Firebird query compiler package with native Windows tools.
.DESCRIPTION
Uses installed Rust and wasm-bindgen tools, without installing anything. The
package contains only the Firebird compiler. For a complete ORM development
build, copy the unchanged official compilers for the other providers into this
package before using a workspace override. Both names are generated in dev
mode, without claiming separate optimized fast/small binaries.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$CargoHome,
    [Parameter(Mandatory = $true)][string]$RustupHome,
    [Parameter(Mandatory = $true)][string]$WasmBindgenPath,
    [Alias('Profile')][ValidateSet('dev', 'release')][string]$BuildProfile = 'dev',
    [string]$OutputPath,
    [string]$TargetDirectory,
    [string]$PackageVersion = '0.0.0-firebird3',
    [switch]$Offline
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$toolchainVersion = '1.97.1-x86_64-pc-windows-msvc'
$wasmTarget = 'wasm32-unknown-unknown'
$resolvedCargoHome = (Resolve-Path -LiteralPath $CargoHome).Path
$resolvedRustupHome = (Resolve-Path -LiteralPath $RustupHome).Path
$resolvedWasmBindgen = (Resolve-Path -LiteralPath $WasmBindgenPath).Path
$cargoBin = Join-Path $resolvedCargoHome 'bin'
$rustupExecutable = Join-Path $cargoBin 'rustup.exe'
foreach ($executable in @($rustupExecutable, (Join-Path $cargoBin 'cargo.exe'), $resolvedWasmBindgen)) {
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw "Missing tool: $executable" }
}
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $repositoryRoot 'target\query-compiler-wasm'
} elseif (-not [IO.Path]::IsPathRooted($OutputPath)) { $OutputPath = Join-Path $repositoryRoot $OutputPath }
$OutputPath = [IO.Path]::GetFullPath($OutputPath)
if ([string]::IsNullOrWhiteSpace($TargetDirectory)) {
    $TargetDirectory = Join-Path $repositoryRoot 'target'
} elseif (-not [IO.Path]::IsPathRooted($TargetDirectory)) { $TargetDirectory = Join-Path $repositoryRoot $TargetDirectory }
$TargetDirectory = [IO.Path]::GetFullPath($TargetDirectory)

$lock = [IO.File]::ReadAllText((Join-Path $repositoryRoot 'Cargo.lock'))
$bindgenLock = [regex]::Match($lock, '(?m)^name = "wasm-bindgen"\r?\nversion = "([^"]+)"')
if (-not $bindgenLock.Success) { throw 'Cannot determine wasm-bindgen version from Cargo.lock.' }
$savedEnvironment = @{}
foreach ($name in @('CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN', 'PATH', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
Push-Location -LiteralPath $repositoryRoot
try {
    $env:CARGO_HOME = $resolvedCargoHome
    $env:RUSTUP_HOME = $resolvedRustupHome
    # Set this before any rustup command to avoid installing project-file targets.
    $env:RUSTUP_TOOLCHAIN = $toolchainVersion
    $env:PATH = "$cargoBin;" + $savedEnvironment['PATH']
    $env:RUSTFLAGS = $null
    $env:CARGO_ENCODED_RUSTFLAGS = $null
    $targets = @(& $rustupExecutable target list --installed --toolchain $toolchainVersion)
    if ($LASTEXITCODE -ne 0 -or $targets -notcontains $wasmTarget) { throw "Install $wasmTarget for $toolchainVersion first." }
    $version = @(& $resolvedWasmBindgen --version)
    if ($LASTEXITCODE -ne 0 -or ($version -join "`n").Trim() -ne "wasm-bindgen $($bindgenLock.Groups[1].Value)") {
        throw 'Supplied wasm-bindgen version does not match Cargo.lock.'
    }
    $cargoArguments = @('run', $toolchainVersion, 'cargo', 'build', '--locked', '--no-default-features',
        '--features', 'firebird', '--profile', $BuildProfile, '--target', $wasmTarget,
        '--target-dir', $TargetDirectory, '-p', 'query-compiler-wasm')
    if ($Offline) { $cargoArguments += '--offline' }
    & $rustupExecutable @cargoArguments
    if ($LASTEXITCODE -ne 0) { throw "Query compiler build failed: $LASTEXITCODE" }
    $profileDirectory = if ($BuildProfile -eq 'dev') { 'debug' } else { 'release' }
    $wasmInput = Join-Path $TargetDirectory "$wasmTarget\$profileDirectory\query_compiler_wasm.wasm"
    if (-not (Test-Path -LiteralPath $wasmInput -PathType Leaf)) { throw "Missing compiler: $wasmInput" }
    $providerOutput = Join-Path $OutputPath 'firebird'
    New-Item -ItemType Directory -Path $providerOutput -Force | Out-Null
    foreach ($buildName in @('fast', 'small')) {
        & $resolvedWasmBindgen --target bundler --out-name "query_compiler_$buildName" --out-dir $providerOutput $wasmInput
        if ($LASTEXITCODE -ne 0) { throw "wasm-bindgen $buildName failed: $LASTEXITCODE" }
        foreach ($suffix in @('_bg.js', '_bg.wasm', '.js', '.d.ts')) {
            $generatedFile = Join-Path $providerOutput "query_compiler_$buildName$suffix"
            if (-not (Test-Path -LiteralPath $generatedFile -PathType Leaf) -or (Get-Item -LiteralPath $generatedFile).Length -eq 0) {
                throw "Missing or empty generated artifact: $generatedFile"
            }
        }
    }
    $metadata = Get-Content -LiteralPath (Join-Path $repositoryRoot 'query-compiler\query-compiler-wasm\package.json') -Raw | ConvertFrom-Json
    $metadata.version = $PackageVersion
    [IO.File]::WriteAllText((Join-Path $OutputPath 'package.json'), ($metadata | ConvertTo-Json -Depth 10) + "`n", (New-Object Text.UTF8Encoding($false)))
    Write-Host "Firebird query compiler package: $OutputPath"
    Write-Host "Build profile: $BuildProfile; both fast/small names derive from this one build."
} finally {
    foreach ($name in $savedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
    }
    Pop-Location
}
