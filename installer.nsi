!include MUI2.nsh

Name "SkyRC MC3000 BLE"

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES

!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_NOTCHECKED
!define MUI_FINISHPAGE_RUN_TEXT "Start application"
!define MUI_FINISHPAGE_RUN_FUNCTION "LaunchLink"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_LANGUAGE "English"

OutFile "dist/mc3000ble-install.exe"

InstallDir "$PROGRAMFILES64\mc3000ble"
RequestExecutionLevel admin

Section "Dummy Section" SecDummy
    SetOutPath $INSTDIR
    RMDir /r $INSTDIR
    File /r dist\mc3000ble\*.*
    WriteUninstaller "$INSTDIR\uninstall.exe"
    CreateDirectory "$SMPROGRAMS\MC3000 BLE"
    CreateShortcut "$SMPROGRAMS\MC3000 BLE\Uninstall.lnk" "$INSTDIR\uninstall.exe"
    CreateShortcut "$SMPROGRAMS\MC3000 BLE\MC3000 BLE.lnk" "$INSTDIR\mc3000ble.exe"
SectionEnd

Section "Uninstall"
    RMDir /r "$SMPROGRAMS\MC3000 BLE"
    RMDir /r $INSTDIR
SectionEnd

Function LaunchLink
  ExecShell "" "$SMPROGRAMS\MC3000 BLE\MC3000 BLE.lnk"
FunctionEnd
