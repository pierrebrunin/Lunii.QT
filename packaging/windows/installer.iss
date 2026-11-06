; Windows installer (Inno Setup 6), built by package.ps1:
;   ISCC.exe /DAppVersion=v3.1.4 /DSourceDir=<binaries> /DOutputDir=<dist> installer.iss
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
AppName=Lunii.QT
AppVersion={#AppVersion}
AppVerName=Lunii.QT {#AppVersion}
AppPublisher=o-daneel
AppPublisherURL=https://github.com/o-daneel/Lunii.QT
AppSupportURL=https://github.com/o-daneel/Lunii.QT/issues
AppUpdatesURL=https://github.com/o-daneel/Lunii.QT/releases
DefaultDirName={autopf}\Lunii.QT
DefaultGroupName=Lunii.QT
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=Lunii.Qt-{#AppVersion}-win64_setup
SetupIconFile=..\..\res\lunii.ico
UninstallDisplayIcon={app}\lunii-qt.exe
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
Source: "{#SourceDir}\lunii-qt.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\lunii-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"

[Icons]
Name: "{group}\Lunii.QT"; Filename: "{app}\lunii-qt.exe"
Name: "{autodesktop}\Lunii.QT"; Filename: "{app}\lunii-qt.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\lunii-qt.exe"; Description: "{cm:LaunchProgram,Lunii.QT}"; Flags: nowait postinstall skipifsilent
