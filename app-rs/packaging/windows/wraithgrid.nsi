; The GPUI app's Windows installer (#166 phase 6), laid out as electron-builder's assisted NSIS
; installer (electron-builder.yml `nsis:`), so installing it over the Electron app replaces it
; in place and electron-updater (latest.yml) can update into it:
;   - same product name, install folder (%LOCALAPPDATA%\Programs\Wraithgrid per user), exe
;     name (Wraithgrid.exe) and the host in resources\ (Electron's process.resourcesPath);
;   - same registry keys: Software\<guid> (InstallLocation, KeepShortcuts, ShortcutName) and
;     the Uninstall\<guid> entry, <guid> = UUID v5 of the appId in electron-builder's namespace;
;   - same uninstaller name and flags (/S, --updated, --keep-shortcuts, --delete-app-data,
;     /currentuser, /allusers), which the app's updater (update.rs) also looks for;
;   - same update flags (--updated, --force-run, /D=) and shortcuts (Start menu + desktop).
; Over an older version it removes that version's files itself (RemoveOldVersion), keeping
; the running pty host and its agents.
; Built by package.sh: makensis -DVERSION=… -DVERSION_WIN=… -DSTAGE=… -DICON=… -DOUTFILE=…
; Stock NSIS 3 only (no electron-builder plugins): shortcuts made here carry no
; AppUserModelID (the app sets its own at run time); ones kept from Electron keep theirs.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma
RequestExecutionLevel user

!macro Require NAME
  !ifndef ${NAME}
    !error "pass -D${NAME}=… (see package.sh)"
  !endif
!macroend
!insertmacro Require VERSION
!insertmacro Require VERSION_WIN
!insertmacro Require STAGE
!insertmacro Require ICON
!insertmacro Require OUTFILE

!define PRODUCT_NAME "Wraithgrid"
!define APP_ID "dev.cachewraith.wraithgrid"
; UUID.v5("dev.cachewraith.wraithgrid", 50e065bc-3134-11e6-9bab-38c9862bdaf3), as NsisTarget.
!define APP_GUID "4900cb6d-cc10-590d-bb2c-81ea5489b43f"
!define APP_EXE "Wraithgrid.exe"
!define HOST_EXE "wraithgrid-host.exe"
!define UNINSTALL_FILENAME "Uninstall Wraithgrid.exe"
!define INSTALL_REGISTRY_KEY "Software\${APP_GUID}"
!define UNINSTALL_REGISTRY_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_GUID}"
!define PUBLISHER "cachewraith"
!define DESCRIPTION "Run many claude CLI sessions side by side, each with its own account and project folder."
!define HOMEPAGE "https://github.com/cachewraith/wraithgrid"

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "x64.nsh"

Name "${PRODUCT_NAME}"
OutFile "${OUTFILE}"
BrandingText "${PRODUCT_NAME} ${VERSION}"
ShowInstDetails nevershow
ShowUninstDetails nevershow

VIProductVersion "${VERSION_WIN}"
VIAddVersionKey "ProductName" "${PRODUCT_NAME}"
VIAddVersionKey "FileDescription" "${PRODUCT_NAME}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "Copyright © 2026 ${PUBLISHER}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"

; "CurrentUser" or "all" (an Electron install made "for anyone who uses this computer").
Var installMode
Var keepShortcuts
Var oldDir

!macro HasOption PARAMS OPTION OUT
  ClearErrors
  ${GetOptions} "${PARAMS}" "${OPTION}" $R9
  ${If} ${Errors}
    StrCpy ${OUT} "0"
  ${Else}
    StrCpy ${OUT} "1"
  ${EndIf}
  ClearErrors
!macroend

; Waits for Wraithgrid.exe in $INSTDIR to exit (an updating app quits right after starting us);
; a running image can't be opened for writing. Silent runs then stop it, as electron-builder
; does; interactive ones ask. The pty host is left alone: agents keep running in it.
!macro WaitForAppBody
  StrCpy $R1 0
  ${Do}
    ${IfNot} ${FileExists} "$INSTDIR\${APP_EXE}"
      ${Break}
    ${EndIf}
    ClearErrors
    FileOpen $R0 "$INSTDIR\${APP_EXE}" a
    ${IfNot} ${Errors}
      FileClose $R0
      ${Break}
    ${EndIf}
    IntOp $R1 $R1 + 1
    ${If} $R1 = 20
      ${If} ${Silent}
        nsExec::Exec `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "Get-CimInstance Win32_Process -Filter \"Name='${APP_EXE}'\" | Where-Object { $$_.ExecutablePath -eq '$INSTDIR\${APP_EXE}' } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force }"`
        Pop $R0
      ${Else}
        MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "${PRODUCT_NAME} is running. Close it, then click Retry." IDRETRY +2
        Quit
        StrCpy $R1 0
      ${EndIf}
    ${ElseIf} $R1 > 40
      MessageBox MB_OK|MB_ICONSTOP "${PRODUCT_NAME} could not be closed." /SD IDOK
      SetErrorLevel 2
      Quit
    ${EndIf}
    Sleep 500
  ${Loop}
!macroend

; The pty host may still run agents from resources\ (it outlives the window): a running exe
; can't be replaced but can be renamed, so it moves to a temp name there and goes at reboot.
!macro MoveHostAsideBody
  ${If} ${FileExists} "$INSTDIR\resources\${HOST_EXE}"
    ClearErrors
    Delete "$INSTDIR\resources\${HOST_EXE}"
    ${If} ${Errors}
      GetTempFileName $R0 "$INSTDIR\resources"
      Delete $R0
      Rename "$INSTDIR\resources\${HOST_EXE}" $R0
      Delete /REBOOTOK $R0
    ${EndIf}
    ClearErrors
  ${EndIf}
!macroend

; Picks the install of this app that exists (per user first, then per machine, as Electron's
; multiUser.nsh) and the registry/shortcut context that goes with it.
!macro UseContext MODE
  StrCpy $installMode "${MODE}"
  ${If} "${MODE}" == "all"
    SetShellVarContext all
  ${Else}
    SetShellVarContext current
  ${EndIf}
!macroend

; A per-machine install needs admin rights: start again elevated (stock NSIS has no UAC plugin).
!macro ElevateIfNeeded EXTRA
  UserInfo::GetAccountType
  Pop $R0
  ${If} $R0 != "Admin"
    ${GetParameters} $R0
    ExecShell "runas" "$EXEPATH" "$R0${EXTRA}"
    SetErrorLevel 0
    Quit
  ${EndIf}
!macroend

; ---- Installer -------------------------------------------------------------------------------

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "${PRODUCT_NAME} needs 64-bit Windows." /SD IDOK
    Quit
  ${EndIf}
  SetRegView 64
  ; /D=<dir> (electron-updater passes it when the app knows its folder) is already in $INSTDIR.
  StrCpy $R2 $INSTDIR
  !insertmacro UseContext "CurrentUser"
  ReadRegStr $R1 HKCU "${INSTALL_REGISTRY_KEY}" InstallLocation
  ${If} $R1 == ""
    ReadRegStr $R1 HKLM "${INSTALL_REGISTRY_KEY}" InstallLocation
    ${If} $R1 != ""
      !insertmacro UseContext "all"
      !insertmacro ElevateIfNeeded ""
    ${EndIf}
  ${EndIf}
  ${If} $R2 != ""
    StrCpy $INSTDIR $R2
  ${ElseIf} $R1 != ""
    StrCpy $INSTDIR $R1
  ${Else}
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\${PRODUCT_NAME}"
  ${EndIf}
FunctionEnd

Function WaitForApp
  !insertmacro WaitForAppBody
FunctionEnd

Function MoveHostAside
  !insertmacro MoveHostAsideBody
FunctionEnd

Function SkipIfUpdated
  ${GetParameters} $R0
  !insertmacro HasOption $R0 "--updated" $R1
  ${If} $R1 == "1"
    Abort
  ${EndIf}
FunctionEnd

; Removes the installed version's files (Electron's or ours) where the registry says it is,
; as its uninstaller would with --updated: app data, registry keys and shortcuts stay. Done
; here rather than by running that uninstaller, because Electron's stops every process under
; the folder, the pty host and its agents included; this keeps the host running (renamed),
; as a Linux package upgrade does.
Function RemoveOldVersion
  ReadRegStr $oldDir SHCTX "${INSTALL_REGISTRY_KEY}" InstallLocation
  ${If} $oldDir == ""
    StrCpy $oldDir $INSTDIR
  ${EndIf}
  ; Only a folder that holds this app is emptied.
  ${IfNot} ${FileExists} "$oldDir\${APP_EXE}"
    Return
  ${EndIf}
  Push $INSTDIR
  StrCpy $INSTDIR $oldDir
  Call WaitForApp
  Call MoveHostAside
  Pop $INSTDIR
  ; A running (renamed) host can't go yet: RMDir removes everything else around it.
  SetOutPath $TEMP
  RMDir /r "$oldDir"
  ClearErrors
FunctionEnd

!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfUpdated
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfUpdated
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  ${GetParameters} $R8
  !insertmacro HasOption $R8 "--updated" $R7
  Call WaitForApp

  ; electron-builder keeps the user's shortcuts (even deleted ones stay deleted) on updates.
  StrCpy $keepShortcuts "false"
  ReadRegStr $R0 SHCTX "${INSTALL_REGISTRY_KEY}" KeepShortcuts
  ${If} $R7 == "1"
  ${AndIf} $R0 == "true"
  ${AndIf} ${FileExists} "$INSTDIR\${APP_EXE}"
    StrCpy $keepShortcuts "true"
  ${EndIf}

  Call RemoveOldVersion
  Call MoveHostAside

  SetOutPath "$INSTDIR"
  File /r "${STAGE}\*.*"
  WriteUninstaller "$INSTDIR\${UNINSTALL_FILENAME}"

  ; electron-builder's registryAddInstallInfo, value for value.
  WriteRegStr SHCTX "${INSTALL_REGISTRY_KEY}" InstallLocation "$INSTDIR"
  WriteRegStr SHCTX "${INSTALL_REGISTRY_KEY}" KeepShortcuts "true"
  WriteRegStr SHCTX "${INSTALL_REGISTRY_KEY}" ShortcutName "${PRODUCT_NAME}"
  ${If} $installMode == "all"
    StrCpy $R0 "/allusers"
  ${Else}
    StrCpy $R0 "/currentuser"
  ${EndIf}
  WriteRegStr SHCTX "${UNINSTALL_REGISTRY_KEY}" DisplayName "${PRODUCT_NAME} ${VERSION}"
  WriteRegStr SHCTX "${UNINSTALL_REGISTRY_KEY}" UninstallString '"$INSTDIR\${UNINSTALL_FILENAME}" $R0'
  WriteRegStr SHCTX "${UNINSTALL_REGISTRY_KEY}" QuietUninstallString '"$INSTDIR\${UNINSTALL_FILENAME}" $R0 /S'
  WriteRegStr SHCTX "${UNINSTALL_REGISTRY_KEY}" DisplayVersion "${VERSION}"
  WriteRegStr SHCTX "${UNINSTALL_REGISTRY_KEY}" DisplayIcon "$INSTDIR\${APP_EXE},0"
  WriteRegStr SHCTX "${UNINSTALL_REGISTRY_KEY}" Publisher "${PUBLISHER}"
  WriteRegStr SHCTX "${UNINSTALL_REGISTRY_KEY}" Comments "${DESCRIPTION}"
  WriteRegStr SHCTX "${UNINSTALL_REGISTRY_KEY}" HelpLink "${HOMEPAGE}"
  WriteRegStr SHCTX "${UNINSTALL_REGISTRY_KEY}" URLInfoAbout "${HOMEPAGE}"
  WriteRegStr SHCTX "${UNINSTALL_REGISTRY_KEY}" URLUpdateInfo "${HOMEPAGE}"
  WriteRegStr SHCTX "${UNINSTALL_REGISTRY_KEY}" Readme "${HOMEPAGE}"
  WriteRegDWORD SHCTX "${UNINSTALL_REGISTRY_KEY}" NoModify 1
  WriteRegDWORD SHCTX "${UNINSTALL_REGISTRY_KEY}" NoRepair 1
  ${GetSize} "$INSTDIR" "/S=0K" $R0 $R1 $R2
  IntFmt $R0 "0x%08X" $R0
  WriteRegDWORD SHCTX "${UNINSTALL_REGISTRY_KEY}" EstimatedSize "$R0"

  ${If} $keepShortcuts == "false"
    CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0 "" "" "${DESCRIPTION}"
    !insertmacro HasOption $R8 "--no-desktop-shortcut" $R0
    ${If} $R0 == "0"
      CreateShortCut "$DESKTOP\${PRODUCT_NAME}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0 "" "" "${DESCRIPTION}"
    ${EndIf}
    System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, i 0, i 0)'
  ${EndIf}

  ; An update (electron-updater, or the app's own updater: /S --updated --force-run) starts
  ; the app again; an interactive install offers it on the finish page.
  !insertmacro HasOption $R8 "--force-run" $R0
  ${If} $R0 == "1"
  ${AndIf} ${Silent}
    Exec '"$INSTDIR\${APP_EXE}"'
  ${EndIf}
SectionEnd

; ---- Uninstaller -----------------------------------------------------------------------------

Function un.onInit
  SetRegView 64
  ${GetParameters} $R8
  !insertmacro UseContext "CurrentUser"
  !insertmacro HasOption $R8 "/allusers" $R0
  ReadRegStr $R1 HKCU "${INSTALL_REGISTRY_KEY}" InstallLocation
  ${If} $R0 == "1"
  ${OrIf} $R1 != $INSTDIR
    ReadRegStr $R1 HKLM "${INSTALL_REGISTRY_KEY}" InstallLocation
    ${If} $R1 == $INSTDIR
    ${OrIf} $R0 == "1"
      !insertmacro UseContext "all"
      !insertmacro ElevateIfNeeded " _?=$INSTDIR"
    ${EndIf}
  ${EndIf}
FunctionEnd

Function un.WaitForApp
  !insertmacro WaitForAppBody
FunctionEnd

Function un.MoveHostAside
  !insertmacro MoveHostAsideBody
FunctionEnd

Section "Uninstall"
  ${GetParameters} $R8
  !insertmacro HasOption $R8 "--updated" $R7
  Call un.WaitForApp
  ${If} $R7 == "1"
    ; Updating: the new version installs next, and the host keeps its agents.
    Call un.MoveHostAside
    SetOutPath $TEMP
    RMDir /r "$INSTDIR"
  ${Else}
    ; Removing: stop what still runs from here (the host and its agents), as Electron's does.
    nsExec::Exec `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "Get-CimInstance Win32_Process | Where-Object { $$_.ExecutablePath -and $$_.ExecutablePath.StartsWith('$INSTDIR\', 'CurrentCultureIgnoreCase') } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force }"`
    Pop $R0
    Sleep 300
    SetOutPath $TEMP
    RMDir /r /REBOOTOK "$INSTDIR"
  ${EndIf}

  !insertmacro HasOption $R8 "--keep-shortcuts" $R0
  ${If} $R0 == "0"
    ReadRegStr $R1 SHCTX "${INSTALL_REGISTRY_KEY}" ShortcutName
    ${If} $R1 == ""
      StrCpy $R1 "${PRODUCT_NAME}"
    ${EndIf}
    Delete "$DESKTOP\$R1.lnk"
    Delete "$SMPROGRAMS\$R1.lnk"
    System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, i 0, i 0)'
  ${EndIf}

  ; App data stays unless asked (electron-builder's --delete-app-data; never on an update).
  !insertmacro HasOption $R8 "--delete-app-data" $R0
  ${If} $R0 == "1"
  ${AndIf} $R7 == "0"
    SetShellVarContext current
    RMDir /r "$APPDATA\${PRODUCT_NAME}"
    !insertmacro UseContext $installMode
  ${EndIf}

  DeleteRegKey SHCTX "${UNINSTALL_REGISTRY_KEY}"
  DeleteRegKey SHCTX "${INSTALL_REGISTRY_KEY}"
SectionEnd
