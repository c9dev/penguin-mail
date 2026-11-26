# Installs Penguin Mail silently, checks what the installer put on the machine, and uninstalls it.
#
#   scripts/test-windows-installer.ps1                  compile a placeholder installer first
#   scripts/test-windows-installer.ps1 -Setup X.exe     test a real installer
#   scripts/test-windows-installer.ps1 -Setup X.exe -Launch -Screenshots shots
#
# Without -Setup the tree holds BusyBox in place of the app and stand-ins for the other
# files, so the run needs no GTK build and never starts the mail application. -Launch starts the
# installed app: it screenshots the window, checks that a second launch hands over to the first,
# and updates the app over itself the way the in-app updater does.
param(
    [string]$Setup,
    [switch]$Launch,
    [string]$Screenshots = 'screenshots'
)
$ErrorActionPreference = 'Stop'
$work = Join-Path ($env:RUNNER_TEMP ?? $env:TEMP) 'penguin-installer-test'
Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $work | Out-Null

$required = @(
    'bin\penguin-mail.exe', 'bin\gdbus.exe', 'bin\gspawn-win64-helper.exe', 'bin\WebView2Loader.dll',
    'libexec\gnupg\gpgv.exe', 'libexec\poppler\pdftotext.exe',
    'share\hunspell\en_US.dic', 'share\iso-codes\json\iso_639-2.json', 'share\iso-codes\json\iso_3166-1.json'
)
# The same value as app/src/app/notifications/windows.rs and packaging/windows/penguin-mail.iss.
$clsid = 'HKCU:\Software\Classes\CLSID\{7B4D8E4A-3C1F-4F6E-9A2B-5D1E0C9F8A61}'

if (!$Setup) {
    $compiler = @(
        "${env:ProgramFiles(x86)}\Inno Setup 7\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (!$compiler) { throw 'Inno Setup is not installed' }
    $stage = Join-Path $work 'stage'
    foreach ($path in $required) {
        $file = Join-Path $stage $path
        New-Item -ItemType Directory -Force (Split-Path $file) | Out-Null
        if ($path -eq 'bin\penguin-mail.exe') { Copy-Item 'busybox64u.exe' $file } else { Set-Content $file 'placeholder' }
    }
    & $compiler "/DSourceDir=$stage" "/DOutputDir=$work" '/DAppVersion=0.0.0' 'packaging/windows/penguin-mail.iss'
    if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed' }
    $Setup = Join-Path $work 'penguin-mail-0.0.0-setup.exe'
}

function Wait-For([scriptblock]$condition, [int]$seconds, [string]$what) {
    for ($i = 0; $i -lt $seconds; $i++) {
        if (& $condition) { return }
        Start-Sleep -Seconds 1
    }
    throw "Timed out waiting for $what"
}

function Get-App { @(Get-Process penguin-mail -ErrorAction SilentlyContinue) }

function Save-Screenshot([string]$name) {
    Add-Type -AssemblyName System.Windows.Forms, System.Drawing
    $area = [System.Windows.Forms.SystemInformation]::VirtualScreen
    $bitmap = New-Object System.Drawing.Bitmap $area.Width, $area.Height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen($area.Left, $area.Top, 0, 0, $bitmap.Size)
    New-Item -ItemType Directory -Force $Screenshots | Out-Null
    $bitmap.Save((Join-Path $Screenshots "$name.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $bitmap.Dispose()
}

$destination = Join-Path $work 'installed'
$silent = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=`"$destination`"")
$process = Start-Process $Setup -ArgumentList $silent -Wait -PassThru
if ($process.ExitCode -ne 0) { throw "Installation failed: $($process.ExitCode)" }
foreach ($path in $required) {
    if (!(Test-Path (Join-Path $destination $path))) { throw "The installer left out $path" }
}
$exe = Join-Path $destination 'bin\penguin-mail.exe'
$server = (Get-ItemProperty "$clsid\LocalServer32").'(default)'
if ($server -ne "`"$exe`"") { throw "The toast activator points at $server, not $exe" }
$programs = [Environment]::GetFolderPath('Programs')
$link = Join-Path $programs 'Penguin Mail\Penguin Mail.lnk'
$legacy = Join-Path $programs 'Penguin Mail.lnk'
if (!(Test-Path $link)) { throw 'The shared Start Menu shortcut is missing' }
Copy-Item $link $legacy
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
New-Item -Path $run -Force | Out-Null
New-ItemProperty -Path $run -Name 'Penguin Mail' -Value 'test autostart entry' -PropertyType String -Force | Out-Null

if ($Launch) {
    Start-Process $exe -ArgumentList '--demo'
    Wait-For { (Get-App | Where-Object { $_.MainWindowHandle -ne 0 }).Count -gt 0 } 120 'the app window'
    Start-Sleep -Seconds 5
    Save-Screenshot 'first-start'

    # A second launch must hand over to the running copy and exit.
    # The demo registers its own name, so the second copy must be a demo too.
    Start-Process $exe -ArgumentList '--demo', '--compose'
    Start-Sleep -Seconds 10
    if ((Get-App).Count -ne 1) { throw "A second launch left $((Get-App).Count) copies running" }
    Save-Screenshot 'second-launch'

    # An update: setup waits for the app to quit, installs, and starts it again.
    $update = Start-Process $Setup -PassThru -ArgumentList ($silent + '/RELAUNCH=window')
    Start-Sleep -Seconds 3
    Get-App | Stop-Process -Force
    $update | Wait-Process -Timeout 120
    if ($update.ExitCode -ne 0) { throw "The update failed: $($update.ExitCode)" }
    Wait-For { (Get-App | Where-Object { $_.MainWindowHandle -ne 0 }).Count -gt 0 } 60 'the app after the update'
    Start-Sleep -Seconds 10
    if ((Get-App).Count -ne 1) { throw "The update left $((Get-App).Count) copies running" }
    Save-Screenshot 'after-update'
    Get-App | Stop-Process -Force
    Start-Sleep -Seconds 2
}

$uninstall = Join-Path $destination 'unins000.exe'
$process = Start-Process $uninstall -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') -Wait -PassThru
if ($process.ExitCode -ne 0) { throw "Uninstall failed: $($process.ExitCode)" }
if ((Test-Path $link) -or (Test-Path $legacy)) { throw 'Uninstall left a Start Menu shortcut' }
if (Get-ItemProperty -Path $run -Name 'Penguin Mail' -ErrorAction SilentlyContinue) { throw 'Uninstall left the autostart entry' }
if (Test-Path $clsid) { throw 'Uninstall left the toast activator key' }
if (Test-Path (Join-Path $destination 'bin')) { throw 'Uninstall left the install folder' }
Write-Output 'Installer files, registry entries and uninstall cleanup passed'
