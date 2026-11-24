# Fills tools/ with the third-party programs Yakusu drives. build.rs packs
# them into Yakusu.exe. Run on Windows:
#   powershell -ExecutionPolicy Bypass -File packaging\fetch_tools.ps1
# Needs Rust (cargo) and git. Every version is pinned.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path -Parent $PSScriptRoot
$tools = Join-Path $root 'tools'
New-Item -ItemType Directory -Force -Path $tools, "$tools\kirikiri", "$tools\VNTextPatch" | Out-Null
$tmp = Join-Path $env:TEMP 'yakusu-tools'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
function Check($what) { if ($LASTEXITCODE -ne 0) { throw "$what failed (exit $LASTEXITCODE)" } }

# 1. msg-tool (lifegpc/msg-tool, GPL-3.0): scripts + archives for ~20 engines.
#    Built with two small patches: msg-tool-safe-paths.patch keeps every
#    unpacked archive entry inside the output folder, and
#    msg-tool-tolerant-hcb.patch reads FAVORITE scripts that carry stray bytes.
$MSGTOOL_REF = '64856108ef5aa18d15f5debc67ff2ade2fda2c45'
if (-not (Test-Path "$tmp\msg-tool\.git")) { git -c core.autocrlf=false clone https://github.com/lifegpc/msg-tool "$tmp\msg-tool"; Check 'git clone msg-tool' }
Push-Location "$tmp\msg-tool"
git fetch origin; Check 'git fetch'
git checkout --force $MSGTOOL_REF; Check 'git checkout'
git apply --ignore-whitespace "$root\packaging\msg-tool-safe-paths.patch"; Check 'patch msg-tool'
git apply --ignore-whitespace "$root\packaging\msg-tool-tolerant-hcb.patch"; Check 'patch msg-tool (hcb)'
cargo build --release --no-default-features --features "all-script,all-arc"; Check 'msg-tool build'
Copy-Item target\release\msg_tool.exe "$tools\msg-tool.exe" -Force
Pop-Location

# 2. VNTextPatch (arcusmaximus/VNTranslationTools v0.0.41, MIT).
$zip = "$tmp\VNTranslationTools.zip"
Invoke-WebRequest 'https://github.com/arcusmaximus/VNTranslationTools/releases/download/v0.0.41/VNTranslationTools.zip' -OutFile $zip
Expand-Archive $zip -DestinationPath "$tools\VNTextPatch" -Force
if (-not (Get-ChildItem "$tools\VNTextPatch" -Recurse -Filter 'VNTextPatch.exe')) { throw 'VNTextPatch.exe not found in VNTranslationTools.zip' }
# Yakusu looks for tools\VNTextPatch\VNTextPatch.exe: flatten one level if the zip has a folder.
$exe = Get-ChildItem "$tools\VNTextPatch" -Recurse -Filter 'VNTextPatch.exe' | Select-Object -First 1
if ($exe.DirectoryName -ne "$tools\VNTextPatch") { Get-ChildItem $exe.DirectoryName | Move-Item -Destination "$tools\VNTextPatch" -Force }

# 3. KirikiriUnencryptedArchive version.dll (arcusmaximus/KirikiriTools 1.7, MIT).
Invoke-WebRequest 'https://github.com/arcusmaximus/KirikiriTools/releases/download/1.7/version.dll' -OutFile "$tools\kirikiri\version.dll"

# 4. siglus-static-key-tool (xmoezzz/siglus_static_key_tool, MPL-2.0): recovers
#    the 16-byte key an encrypted SiglusEngine Scene.pck needs, from
#    Scene.pck + Gameexe.dat or from the game's .exe.
$SIGLUSKEY_REF = 'd1bcd97862a24b511217e3ea930599ed99e0a053'
if (-not (Test-Path "$tmp\siglus_static_key_tool\.git")) { git -c core.autocrlf=false clone https://github.com/xmoezzz/siglus_static_key_tool "$tmp\siglus_static_key_tool"; Check 'git clone siglus_static_key_tool' }
Push-Location "$tmp\siglus_static_key_tool"
git fetch origin; Check 'git fetch'
git checkout --force $SIGLUSKEY_REF; Check 'git checkout'
cargo build --release --locked; Check 'siglus-static-key-tool build'
Copy-Item target\release\siglus-static-key-tool.exe "$tools\siglus-static-key-tool.exe" -Force
Pop-Location

Write-Host "Tools ready in $tools"
Get-ChildItem $tools -Recurse -File | Select-Object FullName, Length
