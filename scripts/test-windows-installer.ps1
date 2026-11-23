# Exercise per-user uninstall cleanup without launching the mail application.
$ErrorActionPreference = 'Stop'
$compiler = @(
    "${env:ProgramFiles(x86)}\Inno Setup 7\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (!$compiler) { throw 'Inno Setup is not installed' }
$work = Join-Path $env:RUNNER_TEMP 'penguin-installer-test'
$stage = Join-Path $work 'stage'
$bin = Join-Path $stage 'bin'
New-Item -ItemType Directory -Force $bin | Out-Null
Copy-Item 'busybox64u.exe' (Join-Path $bin 'penguin-mail.exe')
& $compiler "/DSourceDir=$stage" "/DOutputDir=$work" '/DAppVersion=0.0.0' 'packaging/windows/penguin-mail.iss'
if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed' }
$destination = Join-Path $work 'installed'
$setup = Join-Path $work 'penguin-mail-0.0.0-setup.exe'
$process = Start-Process $setup -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=`"$destination`"") -Wait -PassThru
if ($process.ExitCode -ne 0) { throw "Installation failed: $($process.ExitCode)" }
$programs = [Environment]::GetFolderPath('Programs')
$link = Join-Path $programs 'Penguin Mail\Penguin Mail.lnk'
$legacy = Join-Path $programs 'Penguin Mail.lnk'
if (!(Test-Path $link)) { throw 'The shared Start Menu shortcut is missing' }
Copy-Item $link $legacy
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
New-Item -Path $run -Force | Out-Null
New-ItemProperty -Path $run -Name 'Penguin Mail' -Value 'test autostart entry' -PropertyType String -Force | Out-Null
$uninstall = Join-Path $destination 'unins000.exe'
$process = Start-Process $uninstall -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') -Wait -PassThru
if ($process.ExitCode -ne 0) { throw "Uninstall failed: $($process.ExitCode)" }
if ((Test-Path $link) -or (Test-Path $legacy)) { throw 'Uninstall left a Start Menu shortcut' }
if (Get-ItemProperty -Path $run -Name 'Penguin Mail' -ErrorAction SilentlyContinue) { throw 'Uninstall left the autostart entry' }
Write-Output 'Installer shortcuts and autostart cleanup passed'
