; Windows installer for the ScanSend computer app (Inno Setup 6).
; Built in CI:  iscc /DAppVersion=<v> /DSourceExe=<path to scansend.exe> /DOutputDir=<dir> ScanSend.iss
;
; The installer runs as administrator once, and adds a Windows Firewall rule for ScanSend
; (local network only), so the app never has to ask for the firewall itself.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\..\target\release\scansend.exe"
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

[Setup]
AppId={{3A53DFC3-D86B-4F21-8AA8-862B533CFA1C}
AppName=ScanSend
AppVersion={#AppVersion}
AppVerName=ScanSend {#AppVersion}
AppPublisher=Matt-DKPC
AppPublisherURL=https://github.com/Matt-DKPC/ScanSend
AppSupportURL=https://github.com/Matt-DKPC/ScanSend/issues
DefaultDirName={autopf}\ScanSend
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=ScanSend-{#AppVersion}-windows-setup
SetupIconFile=..\..\mobile\src-tauri\icons\icon.ico
UninstallDisplayIcon={app}\ScanSend.exe
UninstallDisplayName=ScanSend
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "ScanSend.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\ScanSend"; Filename: "{app}\ScanSend.exe"
Name: "{autodesktop}\ScanSend"; Filename: "{app}\ScanSend.exe"; Tasks: desktopicon

[Run]
; Replace any older ScanSend rules, then allow ScanSend.exe in from the local network only.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""ScanSend"""; Flags: runhidden; StatusMsg: "Allowing ScanSend through Windows Firewall..."
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=all program=""{app}\ScanSend.exe"""; Flags: runhidden
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""ScanSend"" dir=in action=allow program=""{app}\ScanSend.exe"" enable=yes profile=any remoteip=localsubnet"; Flags: runhidden
Filename: "{app}\ScanSend.exe"; Description: "Start ScanSend"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""ScanSend"""; Flags: runhidden; RunOnceId: "RemoveFirewallRule"
