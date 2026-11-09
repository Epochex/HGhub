param([string]$Destination = $PSScriptRoot)
$ErrorActionPreference = 'Stop'
$archiveUrl = 'https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-9.0.2-essentials_build.zip'
$expectedHash = '60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba'
$destinationPath = [IO.Path]::GetFullPath($Destination)
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ('hghub-ffmpeg-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempRoot | Out-Null
$archive = Join-Path $tempRoot 'ffmpeg.zip'
Write-Host 'Downloading FFmpeg 9.0.2 from the Windows build provider linked by ffmpeg.org...'
Invoke-WebRequest -Uri $archiveUrl -OutFile $archive -UseBasicParsing
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expectedHash) {
    throw 'FFmpeg checksum mismatch. Nothing was installed.'
}
Expand-Archive -LiteralPath $archive -DestinationPath (Join-Path $tempRoot 'package')
$package = Join-Path $tempRoot 'package/ffmpeg-9.0.2-essentials_build'
$component = Join-Path $destinationPath 'ffmpeg'
New-Item -ItemType Directory -Path $component -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $package 'bin/ffmpeg.exe') -Destination (Join-Path $component 'ffmpeg.exe') -Force
Copy-Item -LiteralPath (Join-Path $package 'bin/ffprobe.exe') -Destination (Join-Path $component 'ffprobe.exe') -Force
foreach ($file in @('LICENSE','README.txt')) {
    if (Test-Path -LiteralPath (Join-Path $package $file)) { Copy-Item -LiteralPath (Join-Path $package $file) -Destination $component -Force }
}
@('Downloaded separately from '+$archiveUrl,
  'SHA256: '+$expectedHash,
  'Upstream source and build information: https://www.gyan.dev/ffmpeg/builds/',
  'FFmpeg is an independent GPLv3 program; HGhub does not embed or link its libraries.') | Set-Content -LiteralPath (Join-Path $component 'SOURCE.txt')
Write-Host ('Installed to '+$component)
Write-Host ('Temporary archive retained at '+$tempRoot)
