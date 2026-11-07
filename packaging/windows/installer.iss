; Windows installer (Inno Setup 6), built by package.ps1:
;   ISCC.exe /DAppVersion=v4.0.0 /DSourceDir=<binaries> /DOutputDir=<dist> installer.iss
; Installs for the current user (no administrator rights needed), or for all
; users if chosen in the wizard.

#ifndef AppVersion
  #define AppVersion "dev"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\target\release"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

[Setup]
AppId={{26E656D1-CF77-4647-A68E-CD7CFA47D8E3}
AppName=Lunii
AppVersion={#AppVersion}
AppVerName=Lunii {#AppVersion}
AppPublisher=o-daneel
AppPublisherURL=https://github.com/o-daneel/Lunii.QT
AppSupportURL=https://github.com/o-daneel/Lunii.QT/issues
AppUpdatesURL=https://github.com/o-daneel/Lunii.QT/releases
DefaultDirName={autopf}\Lunii
DefaultGroupName=Lunii
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=Lunii-{#AppVersion}-win64_setup
SetupIconFile=..\..\res\lunii.ico
UninstallDisplayIcon={app}\lunii.exe
LicenseFile=..\..\LICENSE
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\lunii.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\lunii-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"

[Icons]
Name: "{group}\Lunii"; Filename: "{app}\lunii.exe"
Name: "{autodesktop}\Lunii"; Filename: "{app}\lunii.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\lunii.exe"; Description: "{cm:LaunchProgram,Lunii}"; Flags: nowait postinstall skipifsilent
