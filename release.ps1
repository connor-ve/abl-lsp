# Builds a release of abl for winget: a zip with abl.exe and the VS Code
# extension (.vsix, which `abl i @code` installs), the licence and the third-party notices, plus the three winget
# manifest files, validated. Nothing is uploaded or published.
#
#   .\release.ps1            dist\abl-<version>-windows-x64.zip and dist\winget\<version>\,
#                            with the InstallerUrl of a GitHub release
#   .\release.ps1 -Local     the InstallerUrl points at http://localhost:<Port>; the zip is
#                            served there while winget installs from the manifest
#   .\release.ps1 -Local -Uninstall    serve it again, and uninstall with winget
#
# -Local needs local manifests allowed once, from an administrator prompt:
#   winget settings --enable LocalManifestFiles
param(
    [switch]$Local,
    [switch]$Uninstall,
    [switch]$SkipBuild,
    [int]$Port = 8765,
    [string]$Identifier = 'connor-ve.abl',
    [string]$Publisher = 'connor-ve',
    [string]$Repo = 'connor-ve/abl-lsp'
)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$dist = Join-Path $root 'dist'

$cargo = Get-Content (Join-Path $root 'Cargo.toml') -Raw
if ($cargo -notmatch '(?m)^version = "([^"]+)"') { throw 'no version in Cargo.toml' }
$version = $Matches[1]
$zipName = "abl-$version-windows-x64.zip"
$zip = Join-Path $dist $zipName
$manifestDir = Join-Path $dist "winget\$version"

if (-not $SkipBuild -and -not $Uninstall) {
    & (Join-Path $root 'build.ps1')
    $vsix = Join-Path $root "client\abl-lsp-$version.vsix"
    if (-not (Test-Path $vsix)) { throw "no $vsix after the build" }

    $stage = Join-Path $dist 'stage'
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Force $stage | Out-Null
    Copy-Item (Join-Path $root 'target\release\abl.exe') $stage
    Copy-Item $vsix $stage
    Copy-Item (Join-Path $root 'LICENSE') (Join-Path $stage 'LICENSE.txt')
    Copy-Item (Join-Path $root 'client\THIRD-PARTY-NOTICES.txt') $stage
    if (Test-Path $zip) { Remove-Item $zip -Force }
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
    Remove-Item $stage -Recurse -Force
}
if (-not (Test-Path $zip)) { throw "no $zip; run without -SkipBuild first" }

$sha = (Get-FileHash $zip -Algorithm SHA256).Hash
$url = if ($Local) {
    "http://localhost:$Port/$zipName"
} else {
    "https://github.com/$Repo/releases/download/v$version/$zipName"
}

# The three files winget-pkgs wants, under manifests\<first letter>\<publisher>\<name>\<version>\.
if (Test-Path $manifestDir) { Remove-Item $manifestDir -Recurse -Force }
New-Item -ItemType Directory -Force $manifestDir | Out-Null
$schema = '1.6.0'
$common = "PackageIdentifier: $Identifier`nPackageVersion: $version`n"
$files = @{
    "$Identifier.yaml" = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.version.$schema.schema.json
$common
DefaultLocale: en-US
ManifestType: version
ManifestVersion: $schema
"@
    "$Identifier.installer.yaml" = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.installer.$schema.schema.json
$common
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
- RelativeFilePath: abl.exe
  PortableCommandAlias: abl
Installers:
- Architecture: x64
  InstallerUrl: $url
  InstallerSha256: $sha
ManifestType: installer
ManifestVersion: $schema
"@
    "$Identifier.locale.en-US.yaml" = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.defaultLocale.$schema.schema.json
$common
PackageLocale: en-US
Publisher: $Publisher
PublisherUrl: https://github.com/$($Repo.Split('/')[0])
PackageName: abl
PackageUrl: https://github.com/$Repo
License: MIT
LicenseUrl: https://github.com/$Repo/blob/main/LICENSE
ShortDescription: Command line tools and language server for OpenEdge ABL (Progress 4GL).
Description: |-
  abl lints, formats, compiles, runs, tests and profiles OpenEdge ABL projects, manages
  development databases, installs packages from git, and serves the language server for
  editors. It needs a local OpenEdge install for the commands that run OpenEdge.
Moniker: abl
Tags:
- abl
- openedge
- progress
- 4gl
- lsp
ManifestType: defaultLocale
ManifestVersion: $schema
"@
}
foreach ($name in $files.Keys) {
    # winget wants UTF-8 without a byte order mark.
    [System.IO.File]::WriteAllText((Join-Path $manifestDir $name), $files[$name].Replace("`r`n", "`n"))
}

winget validate --manifest $manifestDir
if ($LASTEXITCODE -ne 0) { throw 'winget validate failed' }
Write-Host "Built $zip (sha256 $sha)"
Write-Host "Manifests in $manifestDir"

if (-not $Local) {
    Write-Host "To publish: attach $zipName to a GitHub release tagged v$version, then copy the"
    Write-Host "manifests to manifests\$($Identifier.Substring(0,1).ToLower())\$($Identifier.Replace('.', '\'))\$version\ in a fork of microsoft/winget-pkgs and open a pull request."
    return
}

# -Local: serve dist\ while winget downloads from it. A separate process, so
# it can be killed while it waits for a request (a job can't be stopped then).
$serve = @"
`$l = [System.Net.HttpListener]::new()
`$l.Prefixes.Add('http://localhost:$Port/')
`$l.Start()
while (`$true) {
    `$ctx = `$l.GetContext()
    `$file = Join-Path '$dist' (`$ctx.Request.Url.AbsolutePath.TrimStart('/'))
    if (Test-Path `$file -PathType Leaf) {
        `$bytes = [System.IO.File]::ReadAllBytes(`$file)
        `$ctx.Response.ContentType = 'application/zip'
        `$ctx.Response.OutputStream.Write(`$bytes, 0, `$bytes.Length)
    } else {
        `$ctx.Response.StatusCode = 404
    }
    `$ctx.Response.Close()
}
"@
$encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($serve))
$server = Start-Process powershell -ArgumentList '-NoProfile', '-EncodedCommand', $encoded -WindowStyle Hidden -PassThru
try {
    Start-Sleep -Seconds 2
    if ($Uninstall) {
        winget uninstall --manifest $manifestDir --disable-interactivity
    } else {
        winget install --manifest $manifestDir --accept-package-agreements --disable-interactivity
    }
    $code = $LASTEXITCODE
} finally {
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
}
if ($code -ne 0) {
    throw "winget exited with $code (local manifests need: winget settings --enable LocalManifestFiles, as administrator)"
}
if (-not $Uninstall) {
    Write-Host 'Installed. Open a new terminal and run: abl --version'
}
