@echo off
rem One-time setup: register a highest-privilege scheduled task (single UAC prompt).
rem Output is written to %TEMP%\whoholds_install.txt for verification.
schtasks /Create /F /TN "whoholds_admin" /TR "cmd.exe /c call \"%TEMP%\whoholds_req.cmd\"" /SC ONCE /ST 00:00 /RL HIGHEST > "%TEMP%\whoholds_install.txt" 2>&1
echo %errorlevel%>> "%TEMP%\whoholds_install.txt"
