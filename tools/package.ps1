# Build a release and pack it for players: dist\ScreamerRally-<version>.zip
#
# The package holds the program, its documentation and the CRT shader library - never game
# data (the player supplies their own CD image).
#
# The CRT shader library (crt-shaders.pak) is generated from a ShaderGlass checkout
# (https://github.com/mausimus/ShaderGlass; its generated headers hold the RetroArch shaders
# as precompiled DXBC): $env:SHADERGLASS, or work\thirdparty\ShaderGlass in the main checkout.
# It is not in the repository (large, third-party). Without a checkout the package has no
# library and the CRT tab says so.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
# the player's program: without the visibility editor (a developer tool, feature vis-editor)
cargo build --release --target i686-pc-windows-msvc -p screamer --no-default-features
if ($LASTEXITCODE -ne 0) { throw "build failed" }
cargo build --release --target i686-pc-windows-msvc -p srtool
if ($LASTEXITCODE -ne 0) { throw "build failed" }

$name = "ScreamerRally-$version"
# The zip is made from a clean staging folder. dist\<name> (where the program can be run
# from) only gets its program files replaced: the player's data\ and logs\ next to the
# program (settings, the remembered disc image, saves) are never touched.
$stage = Join-Path $root "dist\.stage\$name"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null

Copy-Item "target\i686-pc-windows-msvc\release\ScreamerRally.exe" $stage
Copy-Item "docs\USER_GUIDE.md" (Join-Path $stage "README.md")
Copy-Item "LICENSE" (Join-Path $stage "LICENSE.txt")

$sg = if ($env:SHADERGLASS) { $env:SHADERGLASS } else { Join-Path $root "work\thirdparty\ShaderGlass" }
if (-not (Test-Path (Join-Path $sg "ShaderGlass\Shaders\RetroArch.h"))) {
    # a worktree: the checkout is in the main working tree's work folder
    $main = (git rev-parse --path-format=absolute --git-common-dir 2>$null)
    if ($main) { $sg = Join-Path (Split-Path -Parent $main) "work\thirdparty\ShaderGlass" }
}
if (Test-Path (Join-Path $sg "ShaderGlass\Shaders\RetroArch.h")) {
    # RetroArch's crt category alone (108 presets, about 9 MB; the whole library is 70 MB)
    & "target\i686-pc-windows-msvc\release\srtool.exe" crt-pack $sg (Join-Path $stage "crt-shaders.pak") crt
    if ($LASTEXITCODE -ne 0) { throw "crt-pack failed" }
} else {
    Write-Warning "no ShaderGlass checkout at $sg (set SHADERGLASS): the package has no CRT shader library"
}

@"
Screamer Rally for Windows $version

This program is free software: you can redistribute it and/or modify it under the terms of
the GNU General Public License version 3 (LICENSE.txt). It comes with no warranty.
It contains no part of the original game: Screamer Rally and its logo (shown in the first-run
window) are (c) 1997 Milestone / Virgin Interactive; you need your own copy of the game.

CRT emulation (the CRT tab) is a port of ShaderGlass, https://github.com/mausimus/ShaderGlass,
Copyright (C) 2021-2025 mausimus (mausimus.net), GNU General Public License v3.0.

crt-shaders.pak holds the RetroArch slang shaders and presets of the crt category
(https://github.com/libretro/slang-shaders, by the libretro community and the many shader
authors credited in each shader's source) as ShaderGlass compiles them, with their lookup
textures. Each shader keeps the licence its authors gave it (most GPL, some MIT, public
domain or other free licences): see its source in the slang-shaders repository for its
authors and terms.

Third-party components compiled into the program (all MIT or MIT/Apache-2.0):
windows / windows-sys (Microsoft), iced-x86, and the Rust standard library.
"@ | Set-Content -Encoding utf8 (Join-Path $stage "NOTICE.txt")

$zip = Join-Path $root "dist\$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path "$stage\*" -DestinationPath $zip
Write-Host "created $zip"

$run = Join-Path $root "dist\$name"
New-Item -ItemType Directory -Force $run | Out-Null
# files earlier packages had and this one has not
foreach ($old in "Visibility Editor.cmd") {
    $p = Join-Path $run $old
    if (Test-Path $p) { Remove-Item -Force $p }
}
Copy-Item -Force "$stage\*" $run
Write-Host "updated the program files in $run (data and logs kept)"
