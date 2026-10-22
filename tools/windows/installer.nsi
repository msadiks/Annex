; Annex setup for Windows (NSIS 3). Built by tools/windows/package.sh:
;   makensis -DVERSION=1.2.3 -DEXE=<path to annex.exe> -DOUTFILE=<setup.exe> installer.nsi
;
; Installs for all users into Program Files (so it asks for admin once),
; adds the Windows Firewall rule the Mac needs to connect, and registers an
; uninstaller. Settings in %APPDATA%\Annex are kept on uninstall.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma

!ifndef VERSION
  !error "Pass -DVERSION=x.y.z"
!endif
!ifndef EXE
  !error "Pass -DEXE=path/to/annex.exe"
!endif
!ifndef OUTFILE
  !define OUTFILE "Annex-windows-x64-setup.exe"
!endif
!define ROOT "../.."
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Annex"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

!include MUI2.nsh
!include LogicLib.nsh
!include WinMessages.nsh
!include x64.nsh

Name "Annex"
OutFile "${OUTFILE}"
InstallDir "$PROGRAMFILES64\Annex"
InstallDirRegKey HKLM "Software\Annex" "InstallDir"
RequestExecutionLevel admin
BrandingText "Annex ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "Annex"
VIAddVersionKey "FileDescription" "Annex setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "MIT OR Apache-2.0"

!define MUI_ICON "${ROOT}/crates/annex-win/resources/annex.ico"
!define MUI_UNICON "${ROOT}/crates/annex-win/resources/annex.ico"
!define MUI_WELCOMEFINISHPAGE_BITMAP "wizard.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "wizard.bmp"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_RIGHT
!define MUI_HEADERIMAGE_BITMAP "header.bmp"
!define MUI_HEADERIMAGE_UNBITMAP "header.bmp"
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_NODESC

!define MUI_WELCOMEPAGE_TITLE "Annex ${VERSION}"
!define MUI_WELCOMEPAGE_TEXT "Annex turns this PC's monitors into extra displays for your Mac, over your local network.$\r$\n$\r$\nSetup installs Annex and lets it through Windows Firewall for devices on your local network, so your Mac can connect.$\r$\n$\r$\nClick Next to continue."
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_TITLE "Annex is installed"
!define MUI_FINISHPAGE_TEXT "Open Annex on your Mac too, then pair the two with the PIN Annex shows here."
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Open Annex now"
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchAnnex
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; Asks a running Annex to quit (its hidden window takes WM_CLOSE as Quit) and
; waits up to 5 s. Then ends whatever is left: Annex 1.0 ignores WM_CLOSE.
!macro CloseAnnex un
Function ${un}CloseAnnex
  FindWindow $0 "AnnexHidden"
  ${If} $0 != 0
    DetailPrint "Closing Annex"
    SendMessage $0 ${WM_CLOSE} 0 0
    StrCpy $1 0
    wait:
      Sleep 250
      IntOp $1 $1 + 1
      FindWindow $0 "AnnexHidden"
      ${If} $0 != 0
      ${AndIf} $1 < 20
        Goto wait
      ${EndIf}
  ${EndIf}
  nsExec::Exec 'taskkill /F /IM Annex.exe'
  Pop $0
  ${If} $0 == 0
    Sleep 500
  ${EndIf}
FunctionEnd
!macroend
!insertmacro CloseAnnex ""
!insertmacro CloseAnnex "un."

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "Annex needs 64-bit Windows 10 or 11."
    Abort
  ${EndIf}
  SetRegView 64
FunctionEnd

Function un.onInit
  SetRegView 64
FunctionEnd

; Setup runs elevated; start Annex through Explorer so it runs as the user.
Function LaunchAnnex
  Exec '"$WINDIR\explorer.exe" "$INSTDIR\Annex.exe"'
FunctionEnd

Section "Annex" SecMain
  SectionIn RO
  Call CloseAnnex
  SetOutPath "$INSTDIR"
  File "/oname=Annex.exe" "${EXE}"
  File "${ROOT}/LICENSE-MIT"
  File "${ROOT}/LICENSE-APACHE"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  SetShellVarContext all
  CreateShortcut "$SMPROGRAMS\Annex.lnk" "$INSTDIR\Annex.exe" "" "$INSTDIR\Annex.exe" 0 SW_SHOWNORMAL "" "Use this PC's monitors as Mac displays"

  DetailPrint "Allowing Annex through Windows Firewall (local network)"
  nsExec::ExecToLog '"$INSTDIR\Annex.exe" --allow-firewall'
  Pop $0
  ${If} $0 != 0
    DetailPrint "Couldn't add the firewall rule ($0). Annex will offer to fix this when it opens."
  ${EndIf}

  WriteRegStr HKLM "Software\Annex" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "Annex"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "Annex contributors"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\Annex.exe"
  WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "URLInfoAbout" "https://github.com/msadiks/annex-app"
  WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKLM "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "EstimatedSize" 7000
SectionEnd

Section "Start Annex when I sign in" SecAutostart
  ; Same value the app writes for its "Start with Windows" switch.
  WriteRegStr HKCU "${RUN_KEY}" "Annex" '"$INSTDIR\Annex.exe" --background'
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  SetShellVarContext all
  CreateShortcut "$DESKTOP\Annex.lnk" "$INSTDIR\Annex.exe"
SectionEnd

Section "Uninstall"
  Call un.CloseAnnex
  DetailPrint "Removing the Windows Firewall rule"
  nsExec::ExecToLog '"$INSTDIR\Annex.exe" --remove-firewall'
  Pop $0

  ReadRegStr $0 HKCU "${RUN_KEY}" "Annex"
  StrLen $1 "$INSTDIR"
  StrCpy $2 $0 $1 1
  ${If} $2 == "$INSTDIR"
    DeleteRegValue HKCU "${RUN_KEY}" "Annex"
  ${EndIf}

  Delete "$INSTDIR\Annex.exe"
  Delete "$INSTDIR\LICENSE-MIT"
  Delete "$INSTDIR\LICENSE-APACHE"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"

  SetShellVarContext all
  Delete "$SMPROGRAMS\Annex.lnk"
  Delete "$DESKTOP\Annex.lnk"

  DeleteRegKey HKLM "${UNINST_KEY}"
  DeleteRegKey HKLM "Software\Annex"
SectionEnd
