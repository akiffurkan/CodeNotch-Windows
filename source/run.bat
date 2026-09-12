@echo off
if not exist target\release\codenotch.exe (
    echo [Codenotch] Release binary not found, building first...
    call build.bat
)
echo [Codenotch] Launching Codenotch...
start "" "target\release\codenotch.exe"
