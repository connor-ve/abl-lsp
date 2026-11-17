# Builds the server, bundles it into the VS Code extension and packages a .vsix.
#   .\build.ps1            build + package
#   .\build.ps1 -Install   also install the .vsix into VS Code
param([switch]$Install)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot

Push-Location "$root\server"
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
} finally { Pop-Location }

New-Item -ItemType Directory -Force "$root\client\bin" | Out-Null
Copy-Item "$root\server\target\release\abl-lsp.exe" "$root\client\bin\abl-lsp.exe" -Force

Push-Location "$root\client"
try {
    if (-not (Test-Path node_modules)) {
        npm install --no-audit --no-fund
        if ($LASTEXITCODE -ne 0) { throw 'npm install failed' }
    }
    npm run package
    if ($LASTEXITCODE -ne 0) { throw 'vsce package failed' }
    $vsix = Get-ChildItem *.vsix | Sort-Object LastWriteTime | Select-Object -Last 1
    Write-Host "Packaged $($vsix.FullName)"
    if ($Install) {
        code --install-extension $vsix.FullName --force
        Write-Host 'Installed. Reload VS Code windows to pick up the new server.'
    }
} finally { Pop-Location }
