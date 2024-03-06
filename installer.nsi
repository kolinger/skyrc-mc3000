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
    CreateShortcut "$SMPROGRAMS\MC3000 BLE\MC3000 USB Profiles.lnk" "$INSTDIR\mc3000ble.exe" "profiles"

    ; reinstall helper
    WriteRegStr HKLM Software\mc3000ble "Install_Dir" "$INSTDIR"

    ; uninstaller
    WriteUninstaller "$INSTDIR\uninstall.exe"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\mc3000ble" "DisplayName" "MC3000 BLE"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\mc3000ble" "DisplayIcon" "$INSTDIR\assets\img\icon.ico"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\mc3000ble" "UninstallString" '"$INSTDIR\uninstall.exe"'
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\mc3000ble" "NoModify" 1
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\mc3000ble" "NoRepair" 1

    ; edge webview2 runtime
    Call installEdgeWebView2
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

# Install edge webview2 by launching the bootstrapper
# See https://docs.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution#online-only-deployment
# MicrosoftEdgeWebview2Setup.exe download here https://go.microsoft.com/fwlink/p/?LinkId=2124703
Function installEdgeWebView2
	# If this key exists and is not empty then webview2 is already installed
	ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
	ReadRegStr $1 HKCU "Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
	DetailPrint "WebView2 machine version: $0"
	DetailPrint "WebView2 user version: $1"

	${If} $0 == ""
	${AndIf} $1 == ""
		SetDetailsPrint both
		DetailPrint "Installing: WebView2 Runtime, this may take a while, please wait..."
		SetDetailsPrint listonly

		InitPluginsDir
		CreateDirectory "$pluginsdir\webview2bootstrapper"
		SetOutPath "$pluginsdir\webview2bootstrapper"
		File "bin\MicrosoftEdgeWebview2Setup.exe"
		ExecWait '"$pluginsdir\webview2bootstrapper\MicrosoftEdgeWebview2Setup.exe" /silent /install'

		SetDetailsPrint both
	${Else}
	    DetailPrint "WebView2 is already installed"
	${EndIf}
FunctionEnd
