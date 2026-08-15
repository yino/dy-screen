!macro NSIS_HOOK_POSTINSTALL
  IfFileExists "$INSTDIR\resources\asr\runtime\windows-x86_64\vc_redist.x64.exe" 0 runtime_missing
  ExecWait '"$INSTDIR\resources\asr\runtime\windows-x86_64\vc_redist.x64.exe" /install /quiet /norestart' $0
  IntCmp $0 0 runtime_ready runtime_check_reboot runtime_check_reboot

  runtime_check_reboot:
    IntCmp $0 3010 runtime_ready runtime_check_newer runtime_check_newer

  runtime_check_newer:
    IntCmp $0 1638 runtime_ready runtime_failed runtime_failed

  runtime_failed:
    MessageBox MB_ICONSTOP|MB_OK "Microsoft Visual C++ x64 运行库安装失败，切片智能体无法启动本地语音识别。请重新运行安装程序。" /SD IDOK
    Abort

  runtime_missing:
    MessageBox MB_ICONSTOP|MB_OK "安装包缺少 Microsoft Visual C++ x64 运行库，无法完成本地语音识别安装。" /SD IDOK
    Abort

  runtime_ready:
!macroend
