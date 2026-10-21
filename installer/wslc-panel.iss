; wslc-panel 的安装包定义（Inno Setup 6.3+）。
;
; 用法：
;   iscc /DAppVersion=0.10.0 /DExePath=<绝对路径>\wslc-panel.exe installer\wslc-panel.iss
; 两个 define 都由 scripts/build-installer.ps1 注入 —— 版本号只有一处来源
; （根 Cargo.toml / git tag），.iss 里不写死。
;
; 三条刻意的选择，理由见 docs/PLAN-install-update.md §3.1：
;   ① per-user（lowest）：本项目没有任何需要管理员的操作，per-user 装完目录
;      用户可写，将来真要自更新也不用弹 UAC；
;   ② 不走 Restart Manager，自己在 [Code] 里 taskkill（GPUI 窗口对 RM 的
;      关闭请求没有验证过，taskkill 是确定的）；
;   ③ 停进程**不加 /T**：面板会为"启动"的发行版吊着 wsl.exe 哨兵，
;      杀进程树会顺带停掉用户的发行版 —— 那不是安装该干的事。

#define AppName "wslc-panel"

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

; ⚠️ 这个 GUID 一旦发出去就不能再改：改了等于"另一个产品"，
; 升级装不上，旧版的卸载项也会留在"应用和功能"里。
#define AppId "{{8F3C2A61-5B7D-4E19-9A24-6C0D1E8B4F37}"

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=SilkKirk
AppPublisherURL=https://github.com/SilkKirk/wslc-panel
AppSupportURL=https://github.com/SilkKirk/wslc-panel/issues
AppUpdatesURL=https://github.com/SilkKirk/wslc-panel/releases
DefaultDirName={localappdata}\Programs\wslc-panel
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; per-user：不弹 UAC（见文件头 ①）
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=wslc-panel-{#AppVersion}-setup-x64
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\wslc-panel.exe
DirExistsWarning=no
CloseApplications=no
RestartApplications=no
; VersionInfoVersion 必须是四段数字，三段版本后面补 .0
VersionInfoVersion={#AppVersion}.0
VersionInfoProductName={#AppName}
VersionInfoDescription=WSL 容器与发行版管理面板

[Languages]
; ⚠️ 中文语言文件**随仓库带**（`installer\languages\ChineseSimplified.isl`），
; 不用 `compiler:Languages\...`。
;
; 这条是被实测推翻过的：CI runner 上装的是 **Chocolatey 版** Inno
; （`C:\ProgramData\Chocolatey\bin\ISCC.exe`），那个包**只装编译器**，
; 没有官方安装包里才有的 `Languages\` 语言文件。于是
; `compiler:Languages\ChineseSimplified.isl` 会报
; `Couldn't open include file ... The system cannot find the file specified.`，
; ISCC 退出码 **2**（v0.10.0 第一次发版就是这么红的）。
;
; 改成相对 .iss 的路径 + 把文件提交进仓库，就与 Inno 怎么装的无关了。
; 文件来自 issrc 的 `Files/Languages/`，内容未改（只把换行统一成 CRLF）。
Name: "zh"; MessagesFile: "languages\ChineseSimplified.isl"
; Default.isl 是**编译器内置**的，任何安装形态都有 —— 它仍然可以用 compiler:。
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; Flags: unchecked

[Files]
Source: "{#ExePath}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\*"; DestDir: "{app}\docs"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\wslc-panel.exe"
Name: "{group}\卸载 {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\wslc-panel.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\wslc-panel.exe"; Description: "启动 {#AppName}"; Flags: nowait postinstall skipifsilent

[Code]
// ---------------------------------------------------------------------------
// 停掉正在运行的实例
// ---------------------------------------------------------------------------

function IsRunning(): Boolean;
var
  Code: Integer;
  Started: Boolean;
begin
  // tasklist 找不到进程时退出码仍是 0（实测），所以借 find 把它翻译成退出码。
  // find 判的是**文本**，不受系统语言影响（找不到时 tasklist 打的是
  // "INFO: No tasks…"，里面没有映像名）。
  Started := Exec('cmd.exe',
    '/c tasklist /FI "IMAGENAME eq wslc-panel.exe" /NH | find /I "wslc-panel.exe"',
    '', SW_HIDE, ewWaitUntilTerminated, Code);

  // ⚠️ 分两步写，不要写成 `Started and (Code = 0)`：Inno 的 Pascal Script
  // 默认是**完整布尔求值**（不短路），Exec 失败时 Code 可能是未初始化的值。
  Result := False;
  if not Started then
    Exit;
  Result := (Code = 0);
end;

function StopRunning(): Boolean;
var
  Code, Waited: Integer;
begin
  if not IsRunning() then
  begin
    Result := True;
    Exit;
  end;

  // 不加 /T：见文件头 ③。
  Exec('cmd.exe', '/c taskkill /F /IM wslc-panel.exe', '', SW_HIDE,
    ewWaitUntilTerminated, Code);

  // 等它真的消失，最多 6 秒：进程退出与文件句柄释放之间有一小段窗口，
  // 不等的话 [Files] 复制会撞上"文件被占用"。
  Waited := 0;
  while (Waited < 6000) and IsRunning() do
  begin
    Sleep(250);
    Waited := Waited + 250;
  end;

  Result := not IsRunning();
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if not StopRunning() then
    Result := '无法关闭正在运行的 wslc-panel。请先手动退出它，再点"下一步"。';
end;

// ---------------------------------------------------------------------------
// 卸载
// ---------------------------------------------------------------------------

var
  RemoveData: Boolean;

function InitializeUninstall(): Boolean;
begin
  Result := StopRunning();
  if not Result then
  begin
    MsgBox('无法关闭正在运行的 wslc-panel，请先手动退出它再卸载。', mbError, MB_OK);
    Exit;
  end;

  RemoveData := False;

  // 静默卸载（/SILENT、winget、控制面板）时不弹框，默认**保留**数据。
  if UninstallSilent() then
  begin
    Result := True;
    Exit;
  end;

  // 数据目录里不止是偏好：db-credentials.json 存着用户数据库账号的**明文密码**，
  // 所以必须把"会删掉什么"说清楚，而不是一句"删除配置"。
  RemoveData := MsgBox(
    '是否同时删除面板数据？' + #13#10 + #13#10 +
    ExpandConstant('{localappdata}\wslc-panel') + #13#10 +
    '里面有：偏好设置、运行日志、部署记录、备份文件，以及' + #13#10 +
    '数据库账号（密码是明文保存的）。' + #13#10 + #13#10 +
    '选"否"则全部保留，以后重装还能接着用。',
    mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES;

  Result := True;
end;

procedure CurUninstallStepChanged(CurStep: TUninstallStep);
begin
  if CurStep = usPostUninstall then
  begin
    if RemoveData then
      DelTree(ExpandConstant('{localappdata}\wslc-panel'), True, True, True);
    // 安装目录本身由 Inno 按自己的记录删；这里兜底清掉运行期可能掉进去的东西。
    DelTree(ExpandConstant('{app}'), True, True, True);
  end;
end;
