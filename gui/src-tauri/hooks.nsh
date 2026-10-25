!macro NSIS_HOOK_POSTINSTALL
  CopyFiles /SILENT "$INSTDIR\ZeroG.exe" "$INSTDIR\zerog-input.exe"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ExecWait 'net stop ZeroGInput'
  ExecWait '"$INSTDIR\ZeroG.exe" --uninstall-service'
  Delete "$INSTDIR\zerog-input.exe"
!macroend
