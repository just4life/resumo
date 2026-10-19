; Resumo installer for Windows (NSIS 3, Unicode).
;
; Build:  makensis -DSRC=<folder with resumo.exe, libvlc*.dll, plugins> \
;                  -DVERSION=0.1.0 -DOUTFILE=Resumo-Setup.exe installer/resumo.nsi
;
; Installs per machine into Program Files, adds Start menu / desktop shortcuts,
; an uninstaller, and registers Resumo for video and audio files: ProgIDs,
; "Open with", and Default Apps capabilities. Windows 10/11 do not let a
; program make itself the default silently, so the finish page opens the
; Default Apps settings for Resumo, where one click confirms it.

; 64-bit installer: Resumo itself is x64-only.
Target amd64-unicode
SetCompressor /SOLID lzma

!ifndef VERSION
  !define VERSION "0.1.0"
!endif
!ifndef SRC
  !error "Pass -DSRC=<folder with resumo.exe and libVLC>"
!endif
!ifndef OUTFILE
  !define OUTFILE "Resumo-Setup.exe"
!endif

!define APP        "Resumo"
!define EXE        "resumo.exe"
!define PUBLISHER  "Resumo"
!define REGKEY     "Software\${APP}"
!define UNINSTKEY  "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"
!define PROG_VIDEO "Resumo.Video"
!define PROG_AUDIO "Resumo.Audio"

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$PROGRAMFILES64\${APP}"
InstallDirRegKey HKLM "${REGKEY}" "InstallDir"
RequestExecutionLevel admin
BrandingText "${APP} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "FileDescription" "${APP} Setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "GPL-2.0-or-later"

!include "MUI2.nsh"
!include "x64.nsh"
!include "LogicLib.nsh"

!define MUI_ICON "../assets/icon.ico"
!define MUI_UNICON "../assets/icon.ico"
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_SMALLDESC

!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Запустить Resumo"
; The "readme" checkbox is reused to open the Default Apps settings.
!define MUI_FINISHPAGE_SHOWREADME
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Открыть настройки «Приложения по умолчанию»"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION OpenDefaultApps

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "Russian"

; ---------------------------------------------------------------- helpers

; Registers .ext for ${PROGID}: "Open with" entry, Default Apps capability,
; and, if MAKE_DEFAULT is 1, the classic default (used until the user picks
; another app). The previous default is kept so the uninstaller can restore it.
!macro Assoc EXT PROGID MAKE_DEFAULT
  WriteRegStr HKLM "Software\Classes\.${EXT}\OpenWithProgids" "${PROGID}" ""
  WriteRegStr HKLM "${REGKEY}\Capabilities\FileAssociations" ".${EXT}" "${PROGID}"
  WriteRegStr HKLM "Software\Classes\Applications\${EXE}\SupportedTypes" ".${EXT}" ""
  ${If} ${MAKE_DEFAULT} == 1
    ReadRegStr $0 HKLM "Software\Classes\.${EXT}" ""
    ${If} $0 != "${PROGID}"
      WriteRegStr HKLM "Software\Classes\.${EXT}" "Resumo.Backup" "$0"
      WriteRegStr HKLM "Software\Classes\.${EXT}" "" "${PROGID}"
    ${EndIf}
  ${EndIf}
!macroend

!macro UnAssoc EXT PROGID
  DeleteRegValue HKLM "Software\Classes\.${EXT}\OpenWithProgids" "${PROGID}"
  DeleteRegKey /ifempty HKLM "Software\Classes\.${EXT}\OpenWithProgids"
  ReadRegStr $0 HKLM "Software\Classes\.${EXT}" ""
  ${If} $0 == "${PROGID}"
    ReadRegStr $1 HKLM "Software\Classes\.${EXT}" "Resumo.Backup"
    WriteRegStr HKLM "Software\Classes\.${EXT}" "" "$1"
  ${EndIf}
  DeleteRegValue HKLM "Software\Classes\.${EXT}" "Resumo.Backup"
!macroend

!macro VideoTypes MACRO ARG1 ARG2
  !insertmacro ${MACRO} "3gp"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "avi"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "flv"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "m2ts" ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "m4v"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "mkv"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "mov"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "mp4"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "mpeg" ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "mpg"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "mts"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "ogv"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "ts"   ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "vob"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "webm" ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "wmv"  ${ARG1} ${ARG2}
!macroend

!macro AudioTypes MACRO ARG1 ARG2
  !insertmacro ${MACRO} "aac"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "flac" ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "m4a"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "mka"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "mp3"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "ogg"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "opus" ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "wav"  ${ARG1} ${ARG2}
  !insertmacro ${MACRO} "wma"  ${ARG1} ${ARG2}
!macroend

; Two-argument wrapper so UnAssoc fits the list macros above.
!macro UnAssoc3 EXT PROGID UNUSED
  !insertmacro UnAssoc ${EXT} ${PROGID}
!macroend

!macro ProgId PROGID DESCRIPTION
  WriteRegStr HKLM "Software\Classes\${PROGID}" "" "${DESCRIPTION}"
  WriteRegStr HKLM "Software\Classes\${PROGID}\DefaultIcon" "" "$INSTDIR\${EXE},0"
  WriteRegStr HKLM "Software\Classes\${PROGID}\shell" "" "open"
  WriteRegStr HKLM "Software\Classes\${PROGID}\shell\open" "FriendlyAppName" "${APP}"
  WriteRegStr HKLM "Software\Classes\${PROGID}\shell\open\command" "" '"$INSTDIR\${EXE}" "%1"'
!macroend

Function OpenDefaultApps
  ; Windows 11 opens Resumo's own page with a "Set default" button;
  ; Windows 10 ignores the parameter and shows the general Default Apps page.
  ExecShell "open" "ms-settings:defaultapps?registeredAppMachine=${APP}"
FunctionEnd

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "Resumo работает только в 64-битной Windows."
    Abort
  ${EndIf}
  SetRegView 64
  SetShellVarContext all
FunctionEnd

Function un.onInit
  SetRegView 64
  SetShellVarContext all
FunctionEnd

; ---------------------------------------------------------------- sections

Section "Resumo" SecMain
  SectionIn RO
  SetOutPath "$INSTDIR"
  ; Drop plugins from an older version so stale ones do not load.
  RMDir /r "$INSTDIR\plugins"
  File "${SRC}/${EXE}"
  File "${SRC}/libvlc.dll"
  File "${SRC}/libvlccore.dll"
  File /r "${SRC}/plugins"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${EXE}"

  ; App registration: "Open with" list, App Paths (Win+R "resumo"),
  ; and the Default Apps capabilities.
  !insertmacro ProgId ${PROG_VIDEO} "Видеофайл"
  !insertmacro ProgId ${PROG_AUDIO} "Аудиофайл"
  WriteRegStr HKLM "Software\Classes\Applications\${EXE}" "FriendlyAppName" "${APP}"
  WriteRegStr HKLM "Software\Classes\Applications\${EXE}\DefaultIcon" "" "$INSTDIR\${EXE},0"
  WriteRegStr HKLM "Software\Classes\Applications\${EXE}\shell\open\command" "" '"$INSTDIR\${EXE}" "%1"'
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\App Paths\${EXE}" "" "$INSTDIR\${EXE}"
  WriteRegStr HKLM "${REGKEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${REGKEY}\Capabilities" "ApplicationName" "${APP}"
  WriteRegStr HKLM "${REGKEY}\Capabilities" "ApplicationDescription" "Простой видеоплеер на движке VLC"
  WriteRegStr HKLM "${REGKEY}\Capabilities" "ApplicationIcon" "$INSTDIR\${EXE},0"
  WriteRegStr HKLM "Software\RegisteredApplications" "${APP}" "${REGKEY}\Capabilities"

  ; Every type is offered in "Open with" and Default Apps; the sections
  ; below decide which ones Resumo also claims as the default.
  !insertmacro VideoTypes Assoc ${PROG_VIDEO} 0
  !insertmacro AudioTypes Assoc ${PROG_AUDIO} 0

  WriteRegStr HKLM "${UNINSTKEY}" "DisplayName" "${APP}"
  WriteRegStr HKLM "${UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTKEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKLM "${UNINSTKEY}" "DisplayIcon" "$INSTDIR\${EXE},0"
  WriteRegStr HKLM "${UNINSTKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTKEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINSTKEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINSTKEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTKEY}" "NoRepair" 1
SectionEnd

Section "Ярлык на рабочем столе" SecDesktop
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\${EXE}"
SectionEnd

Section "Плеер по умолчанию для видео" SecVideo
  !insertmacro VideoTypes Assoc ${PROG_VIDEO} 1
SectionEnd

Section /o "Плеер по умолчанию для музыки" SecAudio
  !insertmacro AudioTypes Assoc ${PROG_AUDIO} 1
SectionEnd

Section "-Refresh"
  ; Tell Explorer that file associations changed (SHCNE_ASSOCCHANGED).
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecMain} "Сам плеер и движок VLC."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "Значок Resumo на рабочем столе."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecVideo} "Открывать mp4, mkv, avi и другие видеофайлы в Resumo."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecAudio} "Открывать mp3, flac, wav и другие аудиофайлы в Resumo."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; ---------------------------------------------------------------- uninstall

Section "Uninstall"
  !insertmacro VideoTypes UnAssoc3 ${PROG_VIDEO} 0
  !insertmacro AudioTypes UnAssoc3 ${PROG_AUDIO} 0
  DeleteRegKey HKLM "Software\Classes\${PROG_VIDEO}"
  DeleteRegKey HKLM "Software\Classes\${PROG_AUDIO}"
  DeleteRegKey HKLM "Software\Classes\Applications\${EXE}"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\App Paths\${EXE}"
  DeleteRegValue HKLM "Software\RegisteredApplications" "${APP}"
  DeleteRegKey HKLM "${REGKEY}"
  DeleteRegKey HKLM "${UNINSTKEY}"

  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  RMDir /r "$INSTDIR\plugins"
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\libvlc.dll"
  Delete "$INSTDIR\libvlccore.dll"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd
