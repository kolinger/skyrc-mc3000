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
InstallDirRegKey HKLM "Software\mc3000ble" "Install_Dir"
RequestExecutionLevel admin

Section "Dummy Section" SecDummy
    ; program files
    SetOutPath $INSTDIR
    RMDir /r $INSTDIR
    File /r dist\mc3000ble\*.*

    ; shortcuts
    CreateDirectory "$SMPROGRAMS\MC3000 BLE"
    CreateShortcut "$SMPROGRAMS\MC3000 BLE\Uninstall.lnk" "$INSTDIR\uninstall.exe"
    CreateShortcut "$SMPROGRAMS\MC3000 BLE\MC3000 BLE.lnk" "$INSTDIR\mc3000ble.exe"

    ; reinstall helper
    WriteRegStr HKLM Software\mc3000ble "Install_Dir" "$INSTDIR"

    ; uninstaller
    WriteUninstaller "$INSTDIR\uninstall.exe"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\mc3000ble" "DisplayName" "MC3000 BLE"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\mc3000ble" "DisplayIcon" "$INSTDIR\assets\img\icon.ico"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\mc3000ble" "UninstallString" '"$INSTDIR\uninstall.exe"'
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\mc3000ble" "NoModify" 1
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\mc3000ble" "NoRepair" 1
SectionEnd

Section "Uninstall"
    ; program files
    RMDir /r $INSTDIR

    ; shortcuts
    RMDir /r "$SMPROGRAMS\MC3000 BLE"

    ; registry
    DeleteRegKey HKLM "Software\mc3000ble"
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\mc3000ble"
SectionEnd

Function LaunchLink
  ExecShell "" "$SMPROGRAMS\MC3000 BLE\MC3000 BLE.lnk"
FunctionEnd
