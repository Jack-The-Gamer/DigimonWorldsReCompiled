@echo off
rem Digimon Worlds Re:Compiled - Windows launcher.
rem Starts the program; in a source checkout, builds it first if needed.
setlocal
cd /d "%~dp0"

if exist "DigimonWorldsReCompiled.exe" (
    start "" "DigimonWorldsReCompiled.exe"
    exit /b 0
)
if exist "target\release\DigimonWorldsReCompiled.exe" (
    start "" "target\release\DigimonWorldsReCompiled.exe"
    exit /b 0
)

where cargo >nul 2>nul
if errorlevel 1 (
    echo.
    echo  Digimon Worlds Re:Compiled needs to be built once before it can run.
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
)

echo Building Digimon Worlds Re:Compiled (first run only, takes a few minutes)...
cargo build --release -p dwrc
if errorlevel 1 (
    echo.
    echo Build failed. See the messages above.
    pause
    exit /b 1
)
start "" "target\release\DigimonWorldsReCompiled.exe"
