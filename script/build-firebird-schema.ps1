# Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
#requires -Version 5.1
<#
.SYNOPSIS
Build the Firebird-enabled schema Wasm package with native Windows tools.
.DESCRIPTION
Uses an already installed Rust 1.97.1 toolchain and wasm32-unknown-unknown
target. No tools are installed. Packaging mirrors prisma-schema-wasm/scripts/
install.sh: package.json, README.md, and wasm-bindgen --target nodejs output.
.EXAMPLE
 .\script\build-firebird-schema.ps1 -CargoHome ..\.tools\cargo `
   -RustupHome ..\.tools\rustup -WasmBindgenPath C:\tools\wasm-bindgen.exe
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$CargoHome,

    [Parameter(Mandatory = $true)]
    [string]$RustupHome,

    [Parameter(Mandatory = $true)]
    [string]$WasmBindgenPath,

    [Alias('Profile')]
    [ValidateSet('dev', 'release')]
    [string]$BuildProfile = 'dev',

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
$cargoExecutable = Join-Path $cargoBin 'cargo.exe'

foreach ($executable in @($rustupExecutable, $cargoExecutable, $resolvedWasmBindgen)) {
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw "Required executable does not exist: $executable"
    }
}

if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $repositoryRoot 'target\prisma-schema-wasm'
} elseif (-not [System.IO.Path]::IsPathRooted($OutputPath)) {
    $OutputPath = Join-Path $repositoryRoot $OutputPath
}
$OutputPath = [System.IO.Path]::GetFullPath($OutputPath)

if ([string]::IsNullOrWhiteSpace($TargetDirectory)) {
    $TargetDirectory = Join-Path $repositoryRoot 'target'
} elseif (-not [System.IO.Path]::IsPathRooted($TargetDirectory)) {
    $TargetDirectory = Join-Path $repositoryRoot $TargetDirectory
}
$TargetDirectory = [System.IO.Path]::GetFullPath($TargetDirectory)

$lockContents = [System.IO.File]::ReadAllText((Join-Path $repositoryRoot 'Cargo.lock'))
$bindgenLock = [regex]::Match(
    $lockContents,
    '(?m)^name = "wasm-bindgen"\r?\nversion = "([^"]+)"'
)
if (-not $bindgenLock.Success) {
    throw 'Cannot determine the wasm-bindgen crate version from Cargo.lock.'
}
$expectedBindgenVersion = $bindgenLock.Groups[1].Value

$savedCargoHome = $env:CARGO_HOME
$savedRustupHome = $env:RUSTUP_HOME
$savedRustupToolchain = $env:RUSTUP_TOOLCHAIN
$savedPath = $env:PATH
$savedRustflags = $env:RUSTFLAGS
$savedEncodedRustflags = $env:CARGO_ENCODED_RUSTFLAGS

Push-Location -LiteralPath $repositoryRoot
try {
    $env:CARGO_HOME = $resolvedCargoHome
    $env:RUSTUP_HOME = $resolvedRustupHome
    # Override the project toolchain file before even querying rustup. Its
    # additional components and cross-platform targets are unnecessary here.
    $env:RUSTUP_TOOLCHAIN = $toolchainVersion
    $env:PATH = "$cargoBin;$savedPath"
    # Host CRT flags from a native Windows build do not apply to this target.
    $env:RUSTFLAGS = $null
    $env:CARGO_ENCODED_RUSTFLAGS = $null

    $installedToolchains = @(& $rustupExecutable toolchain list)
    if ($LASTEXITCODE -ne 0) {
        throw 'Unable to inspect installed Rust toolchains.'
    }
    if (-not ($installedToolchains | Where-Object { $_ -match '^1\.97\.1-x86_64-pc-windows-msvc(?:\s|$)' })) {
        throw "Rust $toolchainVersion is not installed in $resolvedRustupHome. Install it before running this script."
    }

    $installedTargets = @(& $rustupExecutable target list --installed --toolchain $toolchainVersion)
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to inspect targets for Rust $toolchainVersion."
    }
    if ($installedTargets -notcontains $wasmTarget) {
        throw "The $wasmTarget target is not installed for Rust $toolchainVersion. Install it before running this script."
    }

    $bindgenVersionOutput = @(& $resolvedWasmBindgen --version)
    if ($LASTEXITCODE -ne 0) {
        throw 'Unable to run the supplied wasm-bindgen executable.'
    }
    $actualBindgenVersion = ($bindgenVersionOutput -join "`n").Trim()
    if ($actualBindgenVersion -ne "wasm-bindgen $expectedBindgenVersion") {
        throw "Cargo.lock requires wasm-bindgen-cli $expectedBindgenVersion; supplied executable reports '$actualBindgenVersion'."
    }

    $cargoArguments = @(
        'run', $toolchainVersion, 'cargo', 'build', '--locked',
        '--profile', $BuildProfile, '--target', $wasmTarget,
        '--target-dir', $TargetDirectory, '-p', 'prisma-schema-build'
    )
    if ($Offline) {
        $cargoArguments += '--offline'
    }
    Write-Host "Building schema Wasm with Rust $toolchainVersion ($BuildProfile)..."
    & $rustupExecutable @cargoArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo schema Wasm build failed with exit code $LASTEXITCODE."
    }

    $profileDirectory = if ($BuildProfile -eq 'dev') { 'debug' } else { 'release' }
    $wasmInput = Join-Path $TargetDirectory "$wasmTarget\$profileDirectory\prisma_schema_build.wasm"
    if (-not (Test-Path -LiteralPath $wasmInput -PathType Leaf)) {
        throw "Cargo did not produce the expected Wasm module: $wasmInput"
    }

    $sourceOutput = Join-Path $OutputPath 'src'
    New-Item -ItemType Directory -Path $sourceOutput -Force | Out-Null
    & $resolvedWasmBindgen --target nodejs --out-dir $sourceOutput $wasmInput
    if ($LASTEXITCODE -ne 0) {
        throw "wasm-bindgen failed with exit code $LASTEXITCODE."
    }

    $packageSource = Join-Path $repositoryRoot 'prisma-schema-wasm\package.json'
    $packageMetadata = Get-Content -LiteralPath $packageSource -Raw | ConvertFrom-Json
    $packageMetadata.version = $PackageVersion
    $packageJson = $packageMetadata | ConvertTo-Json -Depth 20
    $utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText(
        (Join-Path $OutputPath 'package.json'),
        $packageJson + [Environment]::NewLine,
        $utf8WithoutBom
    )
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'prisma-schema-wasm\README.md') -Destination $OutputPath -Force

    foreach ($generatedFile in @('prisma_schema_build.js', 'prisma_schema_build_bg.wasm')) {
        $generatedPath = Join-Path $sourceOutput $generatedFile
        if (-not (Test-Path -LiteralPath $generatedPath -PathType Leaf)) {
            throw "Missing generated package file: $generatedPath"
        }
        if ((Get-Item -LiteralPath $generatedPath).Length -eq 0) {
            throw "Generated package file is empty: $generatedPath"
        }
    }

    Write-Host "Schema Wasm package: $OutputPath"
    Write-Host 'Run the actual Node API tests with PRISMA_SCHEMA_WASM set to this directory:'
    Write-Host 'node --test prisma-schema-wasm/tests/firebird-schema.test.cjs'
    Write-Host 'This package provides schema tooling; generated Client integration is verified separately.'
} finally {
    $env:CARGO_HOME = $savedCargoHome
    $env:RUSTUP_HOME = $savedRustupHome
    $env:RUSTUP_TOOLCHAIN = $savedRustupToolchain
    $env:PATH = $savedPath
    $env:RUSTFLAGS = $savedRustflags
    $env:CARGO_ENCODED_RUSTFLAGS = $savedEncodedRustflags
    Pop-Location
}
