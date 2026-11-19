$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ($env:OS -ne "Windows_NT") { throw "package-windows: this package must be built on Windows" }

$Workspace = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $Workspace
$CargoBin = if ($env:CARGO) { $env:CARGO } else { "cargo" }
$VersionLine = Select-String -Path "Cargo.toml" -Pattern '^version = "([^"]+)"$' | Select-Object -First 1
if (-not $VersionLine) { throw "package-windows: workspace version unavailable" }
$Version = $VersionLine.Matches[0].Groups[1].Value
$OutputRoot = if ($args.Count -gt 0) { $args[0] } else { Join-Path $Workspace "dist" }
$PackageName = "Prism-Studio-$Version-windows-x86_64"
$PackageRoot = Join-Path $OutputRoot $PackageName
$Archive = Join-Path $OutputRoot "$PackageName.zip"

& $CargoBin build --release --locked -p prism-studio -p prism-cli
if ($LASTEXITCODE -ne 0) { throw "package-windows: release build failed" }

if (Test-Path $PackageRoot) { Remove-Item -Recurse -Force $PackageRoot }
New-Item -ItemType Directory -Force -Path (Join-Path $PackageRoot "bin") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $PackageRoot "licenses") | Out-Null
Copy-Item "target/release/prism-studio.exe" (Join-Path $PackageRoot "bin/Prism Studio.exe")
Copy-Item "target/release/prism-cli.exe" (Join-Path $PackageRoot "bin/prism-cli.exe")
Copy-Item "assets/brand/prism-icon-1024.png" (Join-Path $PackageRoot "PrismStudio.png")
Copy-Item "LICENSE" (Join-Path $PackageRoot "licenses/Apache-2.0.txt")
& $CargoBin run --release --locked -p prism-license-inventory -- (Join-Path $PackageRoot "licenses/THIRD_PARTY_NOTICES.txt")
if ($LASTEXITCODE -ne 0) { throw "package-windows: license inventory failed" }
& $CargoBin metadata --locked --format-version 1 | Set-Content -Encoding utf8 (Join-Path $PackageRoot "licenses/dependency-metadata.json")
if ($LASTEXITCODE -ne 0) { throw "package-windows: dependency inventory failed" }
$SourceRevision = (& git rev-parse HEAD 2>$null)
if ($LASTEXITCODE -ne 0) { $SourceRevision = "unknown" }
$SourceDirty = "false"
$SourceStatus = (& git status --porcelain --untracked-files=normal 2>$null)
if ($LASTEXITCODE -ne 0 -or $SourceStatus) { $SourceDirty = "true" }
$RustcCommand = if ($env:RUSTC) { $env:RUSTC } else { "rustc" }
$RustcVersion = (& $RustcCommand --version)
if ($LASTEXITCODE -ne 0) { throw "package-windows: compiler provenance failed" }
@(
    "product=Prism Studio"
    "version=$Version"
    "source_revision=$SourceRevision"
    "source_dirty=$SourceDirty"
    "rustc=$RustcVersion"
    "target=Windows-x86_64"
) | Set-Content -Encoding ascii (Join-Path $PackageRoot "RELEASE_BUILD.txt")

if ($env:PRISM_CODEC_DIR) {
    foreach ($Tool in @("ffmpeg.exe", "ffprobe.exe", "ffplay.exe")) {
        $SourceTool = Join-Path $env:PRISM_CODEC_DIR $Tool
        if (-not (Test-Path -PathType Leaf $SourceTool)) {
            throw "package-windows: missing audited codec tool $SourceTool"
        }
        Copy-Item $SourceTool (Join-Path $PackageRoot "bin/$Tool")
    }
}

if (Test-Path $Archive) { Remove-Item -Force $Archive }
Compress-Archive -Path $PackageRoot -DestinationPath $Archive -CompressionLevel Optimal
$Hash = (Get-FileHash -Algorithm SHA256 $Archive).Hash.ToLowerInvariant()
"$Hash  $([System.IO.Path]::GetFileName($Archive))" | Set-Content -Encoding ascii "$Archive.sha256"
Write-Host "package-windows: created $Archive"
Write-Host "package-windows: created $Archive.sha256"
