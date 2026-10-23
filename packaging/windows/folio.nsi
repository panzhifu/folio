; folio 的 Windows 安装器（NSIS 3）。
;
; 调用形状（workflow 里那条）：
;   makensis /DVERSION=0.1.1 /DSRCDIR=<staging 目录> /DICONDIR=packaging/icons \
;            /DOUTDIR=<产物目录> packaging/windows/folio.nsi
; VERSION 与三个目录都必须由命令行给：**这里一个默认值都不给**，因为猜出来的版本号会做出
; 一个自称 0.1.0 的包，而它长得跟真的一模一样。
!ifndef VERSION
  !error "folio.nsi: 必须用 /DVERSION=<版本> 调用（不猜版本号）"
!endif
!ifndef SRCDIR
  !error "folio.nsi: 必须用 /DSRCDIR=<装 folio.exe 的目录> 调用"
!endif
!ifndef ICONDIR
  !error "folio.nsi: 必须用 /DICONDIR=packaging/icons 调用"
!endif
!ifndef OUTDIR
  !define OUTDIR "."
!endif

!include "MUI2.nsh"

Name "Folio"
OutFile "${OUTDIR}\folio-${VERSION}-windows-x64.exe"
; 64 位装到 Program Files，不是 (x86)。
!define INSTALL_DIR "$PROGRAMFILES64\Folio"
InstallDir "${INSTALL_DIR}"
InstallDirRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Folio" "InstallLocation"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

; ---- 图标 ----
!define MUI_ICON "${ICONDIR}\folio.ico"
!define MUI_UNICON "${ICONDIR}\folio.ico"
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TITLE "Folio ${VERSION}"
!define MUI_WELCOMEPAGE_TEXT "原生 GPU 渲染引擎的电子书阅读器。安装到：$INSTDIR"
!define MUI_FINISHPAGE_RUN "$INSTDIR\folio.exe"
!define MUI_FINISHPAGE_RUN_TEXT "现在启动 Folio"
!define MUI_FINISHPAGE_SHOWREADME ""
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
!define MUI_FINISHPAGE_SHOWREADME_TEXT "读一下第三方开源声明"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION openNotices

; ---- 页 ----
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

; ---- 卸载表里那个稳定标识 ----
;
; **NSIS 没有 MSI 的 `UpgradeCode` 那套东西**（计划里我按 WiX 的形状写了这条，实际形状在这儿）。
; 覆盖升级靠的是两件稳定的事：那条注册表键名 `Folio`（不带版本号 —— 带版本号就会每次装出一个
; 新条目，控制面板里躺着四个 Folio），以及上面那行 `InstallDirRegKey` 让第二次安装默认落回
; 同一个目录。所以**这个键名一旦发出去就不能改**。
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Folio"

Section "Folio"
  SetOutPath "$INSTDIR"
  File "${SRCDIR}\folio.exe"
  File "${SRCDIR}\THIRD-PARTY-NOTICES.txt"

  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateDirectory "$SMPROGRAMS\Folio"
  CreateShortCut "$SMPROGRAMS\Folio\Folio.lnk" "$INSTDIR\folio.exe"
  CreateShortCut "$SMPROGRAMS\Folio\Uninstall Folio.lnk" "$INSTDIR\uninstall.exe"

  ; 卸载表。DisplayVersion 用 ${VERSION}，所以版本号错传进来之时它在控制面板里就看得见 ——
  ; 这比让读者从安装目录里去猜要好。
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "Folio"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "Publisher" "panzhifu"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "URLInfoAbout" "https://github.com/panzhifu/folio-releases"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\folio.exe"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Function openNotices
  ExecShell "open" "$INSTDIR\THIRD-PARTY-NOTICES.txt"
FunctionEnd

Section "Uninstall"
  Delete "$INSTDIR\folio.exe"
  Delete "$INSTDIR\THIRD-PARTY-NOTICES.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"                       ; 目录非空时它自己不动手，这是对的
  Delete "$SMPROGRAMS\Folio\Folio.lnk"
  Delete "$SMPROGRAMS\Folio\Uninstall Folio.lnk"
  RMDir "$SMPROGRAMS\Folio"
  DeleteRegKey HKLM "${UNINSTALL_KEY}"
SectionEnd
