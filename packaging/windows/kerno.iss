; Kerno's Windows installer (Inno Setup 6, docs/DECISIONS.md D-139): for the current user only, no
; administrator, into %LOCALAPPDATA%\Programs\Kerno. Built by scripts/bundle-windows.sh from the
; folder it zips:
;   ISCC /DAppVersion=2026.11.3 /DSource=<that folder> /DOutputDir=<dist> packaging\windows\kerno.iss
; Kerno's data (%APPDATA%\Kerno) is never the installer's: uninstalling leaves it.
;
; An update from the app (crates/kerno/src/update.rs) runs it with
;   /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /NOCLOSEAPPLICATIONS /relaunch=1 /pid=<n> /kerno=<exe> /LOG=<file>
; and with TEMP and TMP set to the download's own folder (its stub unpacks its code there), the
; user's own two in KERNO_SETUP_TEMP and KERNO_SETUP_TMP, put back at once. It waits for that
; Kerno (process <n>) to quit, up to /wait=<seconds> (600), refuses to start while another Kerno
; runs from the install folder, starts the new Kerno when it is done, and starts <exe> (the Kerno
; that asked) again when it is not. The model is the owner's Recto (packaging/windows/recto.iss,
; MIT), which waits on a named mutex instead: Kerno has none (no `unsafe` for one), so the process.

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
en.OtherKerno=Another Kerno is still running from %1. Quit it, then update again.
en.KernoHung=Kerno did not quit within %1 seconds: nothing was installed. Once that Kerno is ended and started again, it says what happened.

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
; kerno.exe first: when it cannot be replaced no other file of Kerno's was.
Source: "{#Source}\kerno.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\README.md"; DestDir: "{app}"; Flags: ignoreversion

[UninstallDelete]
; What updates from the app downloaded (K70, V4): Kerno's own folder of downloads, never its data.
Type: filesandordirs; Name: "{localappdata}\Kerno\data\updates"

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
  { The Kerno that asked still runs: it is not started a second time. }
  AskerRuns: Boolean;

function OpenProcess(Access: Cardinal; Inherit: Boolean; Pid: Cardinal): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function WaitForSingleObject(Handle: THandle; Milliseconds: Cardinal): Cardinal;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';
function SetEnvironmentVariable(Name: String; Value: String): Boolean;
  external 'SetEnvironmentVariableW@kernel32.dll stdcall';
{ The same function with a null value: the variable is removed. }
function RemoveEnvironmentVariable(Name: String; Null: Cardinal): Boolean;
  external 'SetEnvironmentVariableW@kernel32.dll stdcall';

{ Started by Kerno's Update. }
function RelaunchAsked(): Boolean;
begin
  Result := ExpandConstant('{param:relaunch|0}') = '1';
end;

{ Kerno started this Setup with TEMP and TMP set to the update's private folder, where the stub has
  already unpacked Setup's own code (K70, V1). The user's own two are in KERNO_SETUP_TEMP and
  KERNO_SETUP_TMP (an unset or empty one is absent): put back now, so that the Kerno started at the
  end (the new one, or the old one again) gets them, and not a folder it removes at its start (W1). }
procedure RestoreTemporaryFolder(const Name: String);
var
  Value: String;
begin
  Value := GetEnv('KERNO_SETUP_' + Name);
  if Value <> '' then
    SetEnvironmentVariable(Name, Value)
  else
    RemoveEnvironmentVariable(Name, 0);
  RemoveEnvironmentVariable('KERNO_SETUP_' + Name, 0);
  Log(Format('%s for the Kerno started at the end: %s', [Name, GetEnv(Name)]));
end;

function InitializeSetup(): Boolean;
begin
  Result := True;
  if GetEnv('KERNO_SETUP_ENV') = '1' then
  begin
    RestoreTemporaryFolder('TEMP');
    RestoreTemporaryFolder('TMP');
    RemoveEnvironmentVariable('KERNO_SETUP_ENV', 0);
  end;
end;

{ Waits for process Pid to end: the Kerno that asked has decided to quit. Its quit can take a while
  (connections to close); after Seconds (/wait=, 600 by default) Setup gives up rather than stay
  invisible for ever, installs nothing and does not start that Kerno a second time; started again
  once it has ended, it says what happened (K70, V6, W3). Logged each minute. True when it ended. A
  process that cannot be opened has ended (or is not one this user may wait on: then the file check
  below still stops Setup if it holds kerno.exe). }
function WaitForProcess(Pid: Integer; Seconds: Integer): Boolean;
var
  Process: THandle;
  Waited: Integer;
  Step: Cardinal;
begin
  Result := True;
  if Pid <= 0 then
    Exit;
  Process := OpenProcess(Synchronize, False, Pid);
  if Process = 0 then
    Exit;
  Waited := 0;
  Result := False;
  while (not Result) and (Waited < Seconds) do
  begin
    Step := 60;
    if Seconds - Waited < Step then
      Step := Seconds - Waited;
    Result := WaitForSingleObject(Process, Step * 1000) <> WaitTimeout;
    Waited := Waited + Step;
    if not Result then
      Log(Format('The Kerno that asked (process %d) still runs after %d s: waiting.', [Pid, Waited]));
  end;
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
  Waited, Limit: Integer;
  Exe: String;
begin
  Result := '';
  if not RelaunchAsked() then
    Exit;
  Limit := StrToIntDef(ExpandConstant('{param:wait|600}'), 600);
  if not WaitForProcess(StrToIntDef(ExpandConstant('{param:pid|0}'), 0), Limit) then
  begin
    AskerRuns := True;
    Result := FmtMessage(CustomMessage('KernoHung'), [IntToStr(Limit)]);
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
  with none; it reads why in its data folder (update::after_install) and says so. Not when it
  still runs (W3). }
procedure DeinitializeSetup();
var
  Old: String;
  Code: Integer;
begin
  if RelaunchAsked() and not Installed and not AskerRuns then
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
