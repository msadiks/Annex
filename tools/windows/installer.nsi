; Annex setup for Windows (NSIS 3). Built by tools/windows/package.sh:
;   makensis -DVERSION=1.2.3 -DEXE=<path to annex.exe> -DOUTFILE=<setup.exe> installer.nsi
;
; A normal wizard: welcome, options, then Install. Annex installs for the
; current user (%LOCALAPPDATA%\Programs\Annex), so setup itself needs no
; admin rights. After Install is clicked, Windows asks for admin once, only to
; add the firewall rules the Mac needs; if that's declined, Annex is still
; installed and offers the same fix in its window. Settings in %APPDATA%\Annex
; are kept on uninstall.

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
InstallDir "$LOCALAPPDATA\Programs\Annex"
InstallDirRegKey HKCU "Software\Annex" "InstallDir"
RequestExecutionLevel user
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

Var FinishText

!define MUI_WELCOMEPAGE_TITLE "Annex ${VERSION}"
!define MUI_WELCOMEPAGE_TEXT "Annex turns this PC's monitors into extra displays for your Mac, over your local network.$\r$\n$\r$\nSetup installs Annex for your Windows account and lets it through Windows Firewall for devices on your local network, so your Mac can connect.$\r$\n$\r$\nClick Next to continue."
!insertmacro MUI_PAGE_WELCOME
!define MUI_PAGE_HEADER_TEXT "Ready to install"
!define MUI_PAGE_HEADER_SUBTEXT "Choose how Annex starts, then click Install."
!define MUI_COMPONENTSPAGE_TEXT_TOP "When you click Install, Windows asks for permission once, so Annex can let your Mac through the firewall."
!define MUI_COMPONENTSPAGE_TEXT_COMPLIST "Options:"
!define MUI_PAGE_CUSTOMFUNCTION_SHOW ShieldInstallButton
!insertmacro MUI_PAGE_COMPONENTS
!define MUI_PAGE_HEADER_TEXT "Installing Annex"
!define MUI_PAGE_HEADER_SUBTEXT "Please wait a moment."
!define MUI_INSTFILESPAGE_FINISHHEADER_TEXT "Done"
!define MUI_INSTFILESPAGE_FINISHHEADER_SUBTEXT "Annex is installed."
!define MUI_INSTFILESPAGE_ABORTHEADER_TEXT "Installation failed"
!define MUI_INSTFILESPAGE_ABORTHEADER_SUBTEXT "Annex wasn't installed. The details above say why."
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_TITLE "Annex is installed"
!define MUI_FINISHPAGE_TEXT "$FinishText"
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

; Runs Annex.exe elevated (Windows shows its admin prompt) with $0 as the
; arguments and waits for it. Sets $0 to its exit code, or -1 if the prompt
; was declined.
!macro RunElevated un
Function ${un}RunElevated
  ; SHELLEXECUTEINFOW, with SEE_MASK_NOCLOSEPROCESS
  System::Call '*(&l4, i 0x40, p $HWNDPARENT, w "runas", w "$INSTDIR\Annex.exe", w r0, p 0, i 0, p 0, p 0, p 0, p 0, i 0, p 0, p 0) p .r1'
  System::Call 'shell32::ShellExecuteExW(p r1) i .r2'
  ${If} $2 = 0
    StrCpy $0 -1
  ${Else}
    StrCpy $0 1
    System::Call '*$1(i, i, p, p, p, p, p, i, p, p, p, p, i, p, p .r3)'
    System::Call 'kernel32::WaitForSingleObject(p r3, i -1)'
    System::Call 'kernel32::GetExitCodeProcess(p r3, *i .r0)'
    System::Call 'kernel32::CloseHandle(p r3)'
  ${EndIf}
  System::Free $1
FunctionEnd
!macroend
!insertmacro RunElevated ""
!insertmacro RunElevated "un."

; The admin prompt comes right after Install; say so on the button.
Function ShieldInstallButton
  GetDlgItem $0 $HWNDPARENT 1
  SendMessage $0 0x160C 0 1 ; BCM_SETSHIELD
FunctionEnd

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

Function LaunchAnnex
  Exec '"$INSTDIR\Annex.exe"'
FunctionEnd

Section "Annex" SecMain
  SectionIn RO
  Call CloseAnnex
  DetailPrint "Copying Annex to $INSTDIR"
  SetOutPath "$INSTDIR"
  ClearErrors
  File "/oname=Annex.exe" "${EXE}"
  File "${ROOT}/LICENSE-MIT"
  File "${ROOT}/LICENSE-APACHE"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  ${If} ${Errors}
    DetailPrint "Couldn't write to $INSTDIR. Close Annex if it's open, then run setup again."
    Abort
  ${EndIf}

  CreateShortcut "$SMPROGRAMS\Annex.lnk" "$INSTDIR\Annex.exe" "" "$INSTDIR\Annex.exe" 0 SW_SHOWNORMAL "" "Use this PC's monitors as Mac displays"

  WriteRegStr HKCU "Software\Annex" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "Annex"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "Annex contributors"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\Annex.exe"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "URLInfoAbout" "https://github.com/msadiks/annex-app"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" 7000

  DetailPrint "Asking Windows to let your Mac through the firewall"
  StrCpy $0 "--allow-firewall"
  Call RunElevated
  ${If} $0 == 0
    DetailPrint "Windows Firewall now allows Annex on your local network."
    StrCpy $FinishText "Open Annex on your Mac too, then pair the two with the PIN Annex shows here."
  ${Else}
    ${If} $0 == -1
      DetailPrint "Windows Firewall wasn't changed: the permission prompt was declined."
    ${Else}
      DetailPrint "Couldn't change Windows Firewall (error $0)."
    ${EndIf}
    StrCpy $FinishText "Windows Firewall wasn't changed, so your Mac can't connect yet. When Annex opens, click Allow Annex in its window to fix this.$\r$\n$\r$\nThen open Annex on your Mac too and pair the two with the PIN Annex shows here."
  ${EndIf}
SectionEnd

Section "Start Annex when I sign in" SecAutostart
  ; Same value the app writes for its "Start with Windows" switch.
  WriteRegStr HKCU "${RUN_KEY}" "Annex" '"$INSTDIR\Annex.exe" --background'
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\Annex.lnk" "$INSTDIR\Annex.exe"
SectionEnd

Section "Uninstall"
  Call un.CloseAnnex
  DetailPrint "Removing the Windows Firewall rules (Windows asks for permission)"
  StrCpy $0 "--remove-firewall"
  Call un.RunElevated
  ${If} $0 != 0
    DetailPrint "The firewall rules were left in place. They do nothing without Annex."
  ${EndIf}

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

  Delete "$SMPROGRAMS\Annex.lnk"
  Delete "$DESKTOP\Annex.lnk"

  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "Software\Annex"
SectionEnd
