; Inno Setup script for Ariadne (unsigned).
; Build with: iscc /DAppVersion=0.1.0 /O"output" dist\windows\app.iss
#define MyAppName "Ariadne"
#define MyAppExeName "ariadne.exe"
#define MyAppId "{{A3F1B2C4-9D8E-4F6A-7B5C-1D2E3F4A5B6C}"

[Setup]
AppId={#MyAppId}
AppName={#MyAppName}
AppVersion={#AppVersion}
AppPublisher=Craft Anything
DefaultDirName={autopf}\Ariadne
DefaultGroupName=Ariadne
OutputBaseFilename=Ariadne-Setup-{#AppVersion}-x64
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName=Ariadne
SetupIconFile=..\..\app\assets\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}

[Files]
Source: "..\..\app\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Ariadne"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\Ariadne"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,Ariadne}"; Flags: nowait postinstall skipifsilent
