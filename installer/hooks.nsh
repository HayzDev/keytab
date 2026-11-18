; keytab NSIS hooks (wired up in tauri.conf.json).

!macro NSIS_HOOK_POSTINSTALL
  ; Start at login on first install only, so an update keeps the user's tray choice.
  ReadRegStr $0 HKCU "Software\keytab" "AutostartConfigured"
  ${If} $0 == ""
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "keytab" '"$INSTDIR\keytab.exe"'
    WriteRegStr HKCU "Software\keytab" "AutostartConfigured" "1"
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; A real uninstall (not the silent one an update runs) removes the login entry.
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "keytab"
    DeleteRegKey HKCU "Software\keytab"
  ${EndIf}
!macroend
