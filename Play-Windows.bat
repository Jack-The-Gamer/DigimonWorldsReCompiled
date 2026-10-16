@echo off
rem Digimon Worlds Re:Compiled - Windows launcher.
rem In a source checkout it rebuilds the program first (only what changed;
rem a few seconds when nothing did), so the newest code always runs.
rem In a release download it just starts the program.
setlocal
cd /d "%~dp0"
set "EXE=target\release\DigimonWorldsReCompiled.exe"

if exist "Cargo.toml" goto source
if exist "DigimonWorldsReCompiled.exe" (
    start "" "DigimonWorldsReCompiled.exe" %*
    exit /b 0
)

:source
set "CARGO=cargo"
where cargo >nul 2>nul
if not errorlevel 1 goto build
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
    set "CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"
    goto build
)
if exist "%EXE%" (
    echo Rust was not found, so the program cannot be updated. Starting the existing build.
    timeout /t 3 >nul
    start "" "%EXE%" %*
    exit /b 0
)
echo.
echo  Digimon Worlds Re:Compiled needs to be built before it can run.
echo.
echo  1. Install Rust from https://rustup.rs  ^(run rustup-init.exe and accept
echo     the defaults, including the Visual Studio C++ build tools^).
echo  2. Double-click Play-Windows.bat again.
echo.
echo  Or download a ready-made release zip instead.
echo.
start "" "https://rustup.rs"
pause
exit /b 1

:build
echo Building Digimon Worlds Re:Compiled...
echo ^(The first build, and the first one after an update, take a few minutes.^)
"%CARGO%" build --release -p dwrc
if errorlevel 1 (
    echo.
    echo Build failed - see the messages above.
    echo If Re:Compiled is still open, close it and try again.
    pause
    exit /b 1
)
start "" "%EXE%" %*
