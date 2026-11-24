; Kerno's Windows installer (Inno Setup 6, docs/DECISIONS.md D-139): for the current user only, no
; administrator, into %LOCALAPPDATA%\Programs\Kerno. Built by scripts/bundle-windows.sh from the
; folder it zips:
;   ISCC /DAppVersion=2026.11.3 /DSource=<that folder> /DOutputDir=<dist> packaging\windows\kerno.iss
; Kerno's data (%APPDATA%\Kerno) is never the installer's: uninstalling leaves it.
;
; An update from the app (crates/kerno/src/update.rs) runs it with
;   /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /NOCLOSEAPPLICATIONS /relaunch=1 /pid=<n> /kerno=<exe> /LOG=<file>
; It waits for that Kerno (process <n>) to quit, refuses to start while another Kerno runs from the
; install folder, starts the new Kerno when it is done, and starts <exe> (the Kerno that asked)
; again when it is not. The model is the owner's Recto (packaging/windows/recto.iss, MIT), which
; waits on a named mutex instead: Kerno has none (no `unsafe` for one), so the process itself.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef Source
  #define Source "..\..\dist\.stage-windows\kerno"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

[Setup]
; Never change: Windows knows the installed app by it.
AppId={{A1106071-CFCF-4417-8D07-D209FC6B030D}
AppName=Kerno
AppVersion={#AppVersion}
AppVerName=Kerno {#AppVersion}
AppPublisher=Evan Pluchart
AppPublisherURL=https://github.com/EvanPluchart/kerno
AppSupportURL=https://github.com/EvanPluchart/kerno/issues
AppUpdatesURL=https://github.com/EvanPluchart/kerno/releases
; Per user, never an administrator prompt; the folder is fixed.
PrivilegesRequired=lowest
DefaultDirName={userpf}\Kerno
DisableDirPage=yes
DefaultGroupName=Kerno
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\..\assets\icon\kerno.ico
UninstallDisplayIcon={app}\kerno.exe
UninstallDisplayName=Kerno
; Both licences, one after the other (written by scripts/bundle-windows.sh).
LicenseFile={#Source}\LICENSE.txt
OutputDir={#OutputDir}
OutputBaseFilename=Kerno-{#AppVersion}-windows-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Installing over a running Kerno: Setup offers to close it (Windows' Restart Manager) and never
; starts it again by itself; kerno.exe is the first of Kerno's files replaced (only Setup's own
; uninstaller comes before it), so a refusal leaves the old program whole.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
en.LaunchKerno=Launch Kerno
en.DataKept=Kerno is removed. Your connections, settings, history and saved queries are kept in the Kerno folder of your profile's AppData\Roaming; delete that folder to remove them too. Saved passwords stay in Windows' Credential Manager.
en.StillRunning=The Kerno that asked for this update is still running after 30 seconds.
en.OtherKerno=Another Kerno is still running from %1. Quit it, then update again.

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
; kerno.exe first: when it cannot be replaced no other file of Kerno's was.
Source: "{#Source}\kerno.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\Kerno"; Filename: "{app}\kerno.exe"
Name: "{userdesktop}\Kerno"; Filename: "{app}\kerno.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\kerno.exe"; Description: "{cm:LaunchKerno}"; Flags: nowait postinstall skipifsilent
; An update from the app: the new Kerno starts once it is installed.
Filename: "{app}\kerno.exe"; Flags: nowait; Check: RelaunchAsked

[Code]
const
  Synchronize = $00100000;
  WaitTimeout = 258;

var
  Installed: Boolean;

function OpenProcess(Access: Cardinal; Inherit: Boolean; Pid: Cardinal): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function WaitForSingleObject(Handle: THandle; Milliseconds: Cardinal): Cardinal;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';

{ Started by Kerno's Update. }
function RelaunchAsked(): Boolean;
begin
  Result := ExpandConstant('{param:relaunch|0}') = '1';
end;

{ Waits up to Milliseconds for process Pid to end; False when it still runs. A process that
  cannot be opened has ended (or is not one this user may wait on: then the file check below
  still stops Setup if it holds kerno.exe). }
function ProcessEnded(Pid: Integer; Milliseconds: Cardinal): Boolean;
var
  Process: THandle;
begin
  Result := True;
  if Pid <= 0 then
    Exit;
  Process := OpenProcess(Synchronize, False, Pid);
  if Process = 0 then
    Exit;
  Result := WaitForSingleObject(Process, Milliseconds) <> WaitTimeout;
  CloseHandle(Process);
end;

{ Whether Path can be opened for writing: a running program's file cannot. }
function Replaceable(const Path: String): Boolean;
var
  Stream: TFileStream;
begin
  Result := True;
  if not FileExists(Path) then
    Exit;
  try
    Stream := TFileStream.Create(Path, fmOpenReadWrite or fmShareExclusive);
    Stream.Free;
  except
    Result := False;
  end;
end;

{ An update from the app: the Kerno that asked quits right after starting Setup. Another Kerno
  still running from the install folder is never closed from here (it may hold an open
  transaction): stop before anything is replaced. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Waited: Integer;
  Exe: String;
begin
  Result := '';
  if not RelaunchAsked() then
    Exit;
  if not ProcessEnded(StrToIntDef(ExpandConstant('{param:pid|0}'), 0), 30000) then
  begin
    Result := CustomMessage('StillRunning');
    Exit;
  end;
  Exe := ExpandConstant('{app}\kerno.exe');
  Waited := 0;
  while (not Replaceable(Exe)) and (Waited < 10000) do
  begin
    Sleep(250);
    Waited := Waited + 250;
  end;
  if not Replaceable(Exe) then
    Result := FmtMessage(CustomMessage('OtherKerno'), [ExpandConstant('{app}')]);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    Installed := True;
end;

{ An update that did not install: start the Kerno that asked again, so the user is not left
  with none; it reads why in its data folder (update::after_install) and says so. }
procedure DeinitializeSetup();
var
  Old: String;
  Code: Integer;
begin
  if RelaunchAsked() and not Installed then
  begin
    Old := ExpandConstant('{param:kerno|}');
    if (Old <> '') and FileExists(Old) then
      Exec(Old, '', '', SW_SHOWNORMAL, ewNoWait, Code);
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent() then
    MsgBox(CustomMessage('DataKept'), mbInformation, MB_OK);
end;
