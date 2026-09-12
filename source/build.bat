@echo off
echo [Codenotch] Building Windows Release and Installer...
cd /d "%~dp0codenotch"
call npx @tauri-apps/cli build
if %errorlevel% neq 0 (
    echo [Codenotch] Build failed!
    pause
    exit /b %errorlevel%
)
cd /d "%~dp0"
copy /y /b "target\release\codenotch.exe" "..\portable\Codenotch_Portable.exe"
for %%F in ("target\release\bundle\nsis\*.exe") do copy /y /b "%%F" "..\setup\Codenotch_Setup.exe"
echo [Codenotch] Build successful!
echo Portable updated at ..\portable\Codenotch_Portable.exe
echo Installer updated at ..\setup\Codenotch_Setup.exe
