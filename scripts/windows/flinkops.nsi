; FlinkOps Windows 安装包（NSIS 3）。
;
; 由 scripts/dist.py --formats setup 调用：
;   makensis /DVERSION=0.1.0 /DVERSION4=0.1.0.0 /DTARGET=x86_64-pc-windows-msvc \
;            /DSTAGE=. /DOUTFILE="<输出文件>" scripts/windows/flinkops.nsi
;
; 按**当前用户**安装（不需要管理员、不弹 UAC）：装到 %LOCALAPPDATA%\Programs\FlinkOps，
; 注册 HKCU 卸载项；配置与数据留在安装目录（.env / data\）。

Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "StrFunc.nsh"
!include "FileFunc.nsh"

!ifndef VERSION
  !define VERSION "0.1.0"
!endif
!ifndef VERSION4
  !define VERSION4 "0.1.0.0"
!endif
!ifndef TARGET
  !define TARGET "x86_64-pc-windows-msvc"
!endif
!ifndef STAGE
  !error "缺少 /DSTAGE=<解包目录>"
!endif
!ifndef OUTFILE
  !define OUTFILE "flinkops-${VERSION}-${TARGET}-setup.exe"
!endif

${StrStr}
${UnStrStr}
${UnStrRep}

Name "FlinkOps ${VERSION}"
Caption "FlinkOps ${VERSION} 安装"
OutFile ${OUTFILE}
InstallDir "$LOCALAPPDATA\Programs\FlinkOps"
InstallDirRegKey HKCU "Software\FlinkOps" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
SetCompressorDictSize 32
BrandingText "FlinkOps ${VERSION} · state-flux"

VIProductVersion "${VERSION4}"
VIAddVersionKey "ProductName" "FlinkOps"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "CompanyName" "state-flux"
VIAddVersionKey "LegalCopyright" "Apache-2.0 / state-flux"
VIAddVersionKey "FileDescription" "Flink Kubernetes Operator 运维控制台"
VIAddVersionKey "Comments" "target: ${TARGET}"

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_TITLE "FlinkOps ${VERSION} 安装完成"
!define MUI_FINISHPAGE_TEXT "启动：$INSTDIR\flinkops.exe$\r$\n浏览器会自动打开 http://127.0.0.1:8080（默认 admin/admin，登录后请改密）。$\r$\n$\r$\n配置：$INSTDIR\.env   ·   数据：$INSTDIR\data$\r$\n卸载：设置 → 应用，或 $INSTDIR\uninstall.exe"
!define MUI_FINISHPAGE_NOREBOOTSUPPORT

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

Section "FlinkOps 控制台（flinkops + Web UI）" SEC_CORE
  SectionIn RO
  SetOutPath "$INSTDIR"
  SetOverwrite on
  File "${STAGE}\flinkops.exe"
  File /r "${STAGE}\web"
  File "${STAGE}\start.cmd"
  File "/oname=LICENSE" "${STAGE}\LICENSE"
  File "/oname=NOTICE" "${STAGE}\NOTICE"
  File "/oname=README.md" "${STAGE}\README.md"
  File "/oname=VERSION" "${STAGE}\VERSION"
  File "/oname=build-info.json" "${STAGE}\build-info.json"

  CreateDirectory "$INSTDIR\data"
  ; 零配置 .env：双击 exe 就直接能用；已存在则不覆盖，避免升级冲掉用户配置
  ${IfNot} ${FileExists} "$INSTDIR\.env"
    FileOpen $0 "$INSTDIR\.env" w
    FileWrite $0 "# 双击 flinkops.exe 即可启动，浏览器会自动打开 http://127.0.0.1:8080$\r$\n"
    FileWrite $0 "FLINKOPS_BIND=127.0.0.1:8080$\r$\n"
    FileWrite $0 "FLINKOPS_WEB_DIR=./web$\r$\n"
    FileWrite $0 "FLINKOPS_STORE=sqlite://./data/flinkops.db?mode=rwc$\r$\n"
    FileWrite $0 "FLINKOPS_OPEN_BROWSER=1$\r$\n"
    FileWrite $0 "RUST_LOG=info$\r$\n"
    FileClose $0
  ${EndIf}
SectionEnd

Section "命令行工具（flinkopsctl / flinkops-mcp）" SEC_TOOLS
  SetOutPath "$INSTDIR"
  File "${STAGE}\flinkopsctl.exe"
  File "${STAGE}\flinkops-mcp.exe"
SectionEnd

Section "开始菜单快捷方式" SEC_SHORTCUT
  CreateDirectory "$SMPROGRAMS\FlinkOps"
  CreateShortCut "$SMPROGRAMS\FlinkOps\FlinkOps.lnk" "$INSTDIR\flinkops.exe" "" "$INSTDIR\flinkops.exe" 0 SW_SHOWNORMAL
  CreateShortCut "$SMPROGRAMS\FlinkOps\配置 .env.lnk" "$INSTDIR\.env"
  CreateShortCut "$SMPROGRAMS\FlinkOps\数据目录.lnk" "$INSTDIR\data"
  CreateShortCut "$SMPROGRAMS\FlinkOps\卸载 FlinkOps.lnk" "$INSTDIR\uninstall.exe"
SectionEnd

Section "桌面快捷方式" SEC_DESKTOP
  CreateShortCut "$DESKTOP\FlinkOps.lnk" "$INSTDIR\flinkops.exe" "" "$INSTDIR\flinkops.exe" 0 SW_SHOWNORMAL
SectionEnd

Section "把 flinkopsctl 加到用户 PATH" SEC_PATH
  ReadRegStr $0 HKCU "Environment" "Path"
  ${StrStr} $1 "$0" "$INSTDIR"
  ${If} $1 == ""
    ${If} $0 == ""
      WriteRegExpandStr HKCU "Environment" "Path" "$INSTDIR"
    ${Else}
      WriteRegExpandStr HKCU "Environment" "Path" "$0;$INSTDIR"
    ${EndIf}
    SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000
  ${EndIf}
SectionEnd

Section -post
  WriteUninstaller "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "Software\FlinkOps" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\FlinkOps" "Version" "${VERSION}"
  WriteRegStr HKCU "Software\FlinkOps" "Target" "${TARGET}"

  !define UNKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\FlinkOps"
  WriteRegStr HKCU "${UNKEY}" "DisplayName" "FlinkOps ${VERSION}"
  WriteRegStr HKCU "${UNKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNKEY}" "Publisher" "state-flux"
  WriteRegStr HKCU "${UNKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNKEY}" "DisplayIcon" "$INSTDIR\flinkops.exe"
  WriteRegStr HKCU "${UNKEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNKEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNKEY}" "NoRepair" 1
  WriteRegStr HKCU "${UNKEY}" "URLInfoAbout" "https://github.com/state-flux/flinkops"

  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNKEY}" "EstimatedSize" "$0"
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_CORE} "控制面二进制 flinkops.exe 与 Web UI（必需）。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_TOOLS} "flinkopsctl 运维 CLI 与 flinkops-mcp（AI 客户端 MCP server）。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_SHORTCUT} "开始菜单里放 FlinkOps、配置与数据目录、卸载入口。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_DESKTOP} "在桌面创建 FlinkOps 快捷方式。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_PATH} "把安装目录写入当前用户 PATH，便于在任意终端用 flinkopsctl。"
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  SetShellVarContext current

  Delete "$INSTDIR\flinkops.exe"
  Delete "$INSTDIR\flinkopsctl.exe"
  Delete "$INSTDIR\flinkops-mcp.exe"
  RMDir /r "$INSTDIR\web"
  Delete "$INSTDIR\start.cmd"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\NOTICE"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\VERSION"
  Delete "$INSTDIR\build-info.json"
  Delete "$INSTDIR\.env"
  ; 数据目录默认保留：静默卸载不问，图形卸载问一次
  IfSilent keep_data
  MessageBox MB_YESNO|MB_ICONQUESTION "是否同时删除数据目录？$\r$\n$INSTDIR\data（含 SQLite 元数据库）" IDNO keep_data
    RMDir /r "$INSTDIR\data"
  keep_data:

  Delete "$SMPROGRAMS\FlinkOps\FlinkOps.lnk"
  Delete "$SMPROGRAMS\FlinkOps\配置 .env.lnk"
  Delete "$SMPROGRAMS\FlinkOps\数据目录.lnk"
  Delete "$SMPROGRAMS\FlinkOps\卸载 FlinkOps.lnk"
  RMDir "$SMPROGRAMS\FlinkOps"
  Delete "$DESKTOP\FlinkOps.lnk"

  ReadRegStr $0 HKCU "Environment" "Path"
  ${UnStrStr} $1 "$0" "$INSTDIR"
  ${If} $1 != ""
    ${UnStrRep} $0 "$0" ";$INSTDIR" ""
    ${UnStrRep} $0 "$0" "$INSTDIR;" ""
    WriteRegExpandStr HKCU "Environment" "Path" "$0"
    SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000
  ${EndIf}

  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\FlinkOps"
  DeleteRegKey HKCU "Software\FlinkOps"

  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
SectionEnd
