# Added for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
#requires -Version 5.1
<#
.SYNOPSIS
Build the Firebird-enabled schema-engine Wasm package with native Windows tools.
.DESCRIPTION
Uses an already installed Rust 1.97.1 toolchain and wasm32-unknown-unknown
target. No tools are installed. Packaging mirrors schema-engine/schema-engine-wasm/build.sh: bundler glue,
Wasm, package metadata and matching TypeScript declarations.
.EXAMPLE
 .\script\build-firebird-schema-engine.ps1 -CargoHome ..\.tools\cargo `
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
    $OutputPath = Join-Path $repositoryRoot 'target\schema-engine-wasm'
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
        '--target-dir', $TargetDirectory, '-p', 'schema-engine-wasm', '--no-default-features', '--features', 'firebird,sqlite,postgresql'
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
    $wasmInput = Join-Path $TargetDirectory "$wasmTarget\$profileDirectory\schema_engine_wasm.wasm"
    if (-not (Test-Path -LiteralPath $wasmInput -PathType Leaf)) {
        throw "Cargo did not produce the expected Wasm module: $wasmInput"
    }

    $sourceOutput = $OutputPath
    New-Item -ItemType Directory -Path $sourceOutput -Force | Out-Null
    & $resolvedWasmBindgen --target bundler --out-name schema_engine --out-dir $sourceOutput $wasmInput
    if ($LASTEXITCODE -ne 0) {
        throw "wasm-bindgen failed with exit code $LASTEXITCODE."
    }

    $packageSource = Join-Path $repositoryRoot 'schema-engine\schema-engine-wasm\package.json'
    $packageMetadata = Get-Content -LiteralPath $packageSource -Raw | ConvertFrom-Json
    $packageMetadata.version = $PackageVersion
    $packageJson = $packageMetadata | ConvertTo-Json -Depth 20
    $utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText(
        (Join-Path $OutputPath 'package.json'),
        $packageJson + [Environment]::NewLine,
        $utf8WithoutBom
    )
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'schema-engine\schema-engine-wasm\README.md') -Destination $OutputPath -Force

    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'LICENSE') -Destination $OutputPath -Force
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'MODIFICATIONS.md') -Destination $OutputPath -Force
    $definitions = [IO.File]::ReadAllText((Join-Path $OutputPath 'schema_engine.d.ts'))
    [IO.File]::WriteAllText((Join-Path $OutputPath 'schema_engine_bg.d.ts'), $definitions + [Environment]::NewLine + 'export const __wbg_set_wasm: (val: any) => void;' + [Environment]::NewLine, $utf8WithoutBom)

    foreach ($generatedFile in @('schema_engine.js', 'schema_engine_bg.js', 'schema_engine_bg.wasm', 'schema_engine.d.ts', 'schema_engine_bg.d.ts')) {
        $generatedPath = Join-Path $sourceOutput $generatedFile
        if (-not (Test-Path -LiteralPath $generatedPath -PathType Leaf)) {
            throw "Missing generated package file: $generatedPath"
        }
        if ((Get-Item -LiteralPath $generatedPath).Length -eq 0) {
            throw "Generated package file is empty: $generatedPath"
        }
    }

    Write-Host "Schema Wasm package: $OutputPath"
    Write-Host 'Use this local package through the paired ORM schema-engine-wasm override.'
    Write-Host 'Run real introspection/migration integration only against owned synthetic databases.'
} finally {
    $env:CARGO_HOME = $savedCargoHome
    $env:RUSTUP_HOME = $savedRustupHome
    $env:RUSTUP_TOOLCHAIN = $savedRustupToolchain
    $env:PATH = $savedPath
    $env:RUSTFLAGS = $savedRustflags
    $env:CARGO_ENCODED_RUSTFLAGS = $savedEncodedRustflags
    Pop-Location
}
