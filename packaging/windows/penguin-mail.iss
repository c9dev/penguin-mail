; Penguin Mail's installer: the staged prefix tree, the Start Menu entry
; with the app's AppUserModelID, the mailto/.eml/.ics capabilities Windows
; lists in Default Apps, the COM class Windows starts to deliver a toast
; click after the app quit, a wait for the running app on updates, and a
; WebView2 runtime check for the odd machine without it. Per-user install;
; admin rights never come up.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "stage"
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

[Setup]
AppId={{8B5C2E1A-4D3F-4A6B-9E7C-2F1D5A8B3C6E}
; The app holds this mutex while it runs; setup and uninstall wait for it.
AppMutex=PenguinMailRunning
AppName=Penguin Mail
AppVersion={#AppVersion}
AppPublisher=Pivotd
AppPublisherURL=https://penguin-mail.com
AppSupportURL=https://github.com/c9dev/penguin-mail/issues
AppUpdatesURL=https://github.com/c9dev/penguin-mail/releases
DefaultDirName={localappdata}\Programs\Penguin Mail
DefaultGroupName=Penguin Mail
OutputDir={#OutputDir}
OutputBaseFilename=penguin-mail-{#AppVersion}-setup
SetupIconFile=..\..\app\data\win32\penguin-mail.ico
UninstallDisplayIcon={app}\bin\penguin-mail.exe
Compression=lzma2/ultra64
SolidCompression=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesAssociations=yes
WizardStyle=modern

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion
#ifdef WebView2Bootstrapper
Source: "{#WebView2Bootstrapper}"; DestDir: "{tmp}"; DestName: "MicrosoftEdgeWebview2Setup.exe"; Flags: deleteafterinstall
#endif

[Icons]
Name: "{userprograms}\Penguin Mail\Penguin Mail"; Filename: "{app}\bin\penguin-mail.exe"; AppUserModelID: "io.github.c9dev.PenguinMail"; AppUserModelToastActivatorCLSID: "7B4D8E4A-3C1F-4F6E-9A2B-5D1E0C9F8A61"

[Registry]
; Preferences creates this value only when start at sign-in is enabled.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "Penguin Mail"; Flags: uninsdeletevalue
; The capabilities record Windows reads for Default Apps. Choosing the
; actual default stays the person's, as it must since Windows 8. Setup
; leaves registry entries behind at uninstall unless told otherwise;
; these subtrees are ours wholesale, so they go entirely.
Root: HKCU; Subkey: "Software\PenguinMail"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\PenguinMail\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "Penguin Mail"
Root: HKCU; Subkey: "Software\PenguinMail\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Mail and calendar"
Root: HKCU; Subkey: "Software\PenguinMail\Capabilities\URLAssociations"; ValueType: string; ValueName: "mailto"; ValueData: "PenguinMail.mailto"
Root: HKCU; Subkey: "Software\PenguinMail\Capabilities\FileAssociations"; ValueType: string; ValueName: ".eml"; ValueData: "PenguinMail.eml"
Root: HKCU; Subkey: "Software\PenguinMail\Capabilities\FileAssociations"; ValueType: string; ValueName: ".ics"; ValueData: "PenguinMail.ics"
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "PenguinMail"; ValueData: "Software\PenguinMail\Capabilities"; Flags: uninsdeletevalue

; Windows starts this server when a toast is clicked after the app quit.
; The CLSID matches app/src/app/notifications/windows.rs and the shortcut above.
Root: HKCU; Subkey: "Software\Classes\CLSID\{{7B4D8E4A-3C1F-4F6E-9A2B-5D1E0C9F8A61}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\CLSID\{{7B4D8E4A-3C1F-4F6E-9A2B-5D1E0C9F8A61}\LocalServer32"; ValueType: string; ValueData: """{app}\bin\penguin-mail.exe"""

Root: HKCU; Subkey: "Software\Classes\PenguinMail.mailto"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\PenguinMail.mailto"; ValueType: string; ValueData: "URL:MailTo Protocol"
Root: HKCU; Subkey: "Software\Classes\PenguinMail.mailto"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\PenguinMail.mailto\DefaultIcon"; ValueType: string; ValueData: "{app}\bin\penguin-mail.exe,0"
Root: HKCU; Subkey: "Software\Classes\PenguinMail.mailto\shell\open\command"; ValueType: string; ValueData: """{app}\bin\penguin-mail.exe"" ""%1"""

Root: HKCU; Subkey: "Software\Classes\PenguinMail.eml"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\PenguinMail.eml"; ValueType: string; ValueData: "Email message"
Root: HKCU; Subkey: "Software\Classes\PenguinMail.eml\DefaultIcon"; ValueType: string; ValueData: "{app}\bin\penguin-mail.exe,0"
Root: HKCU; Subkey: "Software\Classes\PenguinMail.eml\shell\open\command"; ValueType: string; ValueData: """{app}\bin\penguin-mail.exe"" ""%1"""

Root: HKCU; Subkey: "Software\Classes\PenguinMail.ics"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\PenguinMail.ics"; ValueType: string; ValueData: "iCalendar file"
Root: HKCU; Subkey: "Software\Classes\PenguinMail.ics\DefaultIcon"; ValueType: string; ValueData: "{app}\bin\penguin-mail.exe,0"
Root: HKCU; Subkey: "Software\Classes\PenguinMail.ics\shell\open\command"; ValueType: string; ValueData: """{app}\bin\penguin-mail.exe"" ""%1"""

[UninstallDelete]
; Also remove launchers written by the app before this path was shared.
Type: files; Name: "{userprograms}\Penguin Mail.lnk"
Type: files; Name: "{userprograms}\Penguin Mail\Penguin Mail.lnk"
Type: dirifempty; Name: "{userprograms}\Penguin Mail"


[Run]
Filename: "{app}\bin\penguin-mail.exe"; Description: "Open Penguin Mail"; Flags: nowait postinstall skipifsilent
; An update started by the app passes /RELAUNCH=background or /RELAUNCH=window,
; and the app comes back the way it was.
Filename: "{app}\bin\penguin-mail.exe"; Parameters: "--background"; Flags: nowait runasoriginaluser; Check: Relaunch('background')
Filename: "{app}\bin\penguin-mail.exe"; Flags: nowait runasoriginaluser; Check: Relaunch('window')

[Code]
{ The app quits right after it starts an update, so the mutex can outlive
  the call for a moment. Waiting here keeps AppMutex from stopping a silent
  update. Thirty seconds is far longer than a quit takes. }
function InitializeSetup(): Boolean;
var
  Waited: Integer;
begin
  Waited := 0;
  while CheckForMutexes('PenguinMailRunning') and (Waited < 30000) do
  begin
    Sleep(250);
    Waited := Waited + 250;
  end;
  Result := True;
end;

function Relaunch(Mode: String): Boolean;
begin
  Result := CompareText(ExpandConstant('{param:RELAUNCH|}'), Mode) = 0;
end;

{ WebView2's evergreen runtime registers its version here when present. A
  per-machine install lands in the 32-bit view (WOW6432Node). }
function WebView2Installed(): Boolean;
var
  Version: String;
begin
  Result := RegQueryStringValue(HKLM32, 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', Version)
    or RegQueryStringValue(HKCU, 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', Version);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Code: Integer;
begin
  if CurStep = ssPostInstall then
  begin
#ifdef WebView2Bootstrapper
    if not WebView2Installed() then
      Exec(ExpandConstant('{tmp}\MicrosoftEdgeWebview2Setup.exe'), '/silent /install', '', SW_HIDE, ewWaitUntilTerminated, Code);
#endif
  end;
end;
