@echo off
if exist target\release\codenotch.exe (
    target\release\codenotch.exe doctor
    if exist "%APPDATA%\codenotch\doctor.log" type "%APPDATA%\codenotch\doctor.log"
) else (
    cargo run --bin codenotch -- doctor
)
pause
