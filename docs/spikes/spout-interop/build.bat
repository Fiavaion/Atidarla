@echo off
rem Builds the Spout SDK interop peers (Atidarla, MIT OR Apache-2.0).
rem usage: build.bat <path to Spout2 SPOUTSDK folder> [output folder]
rem Compiles the same sources as the SDK's SpoutDX_static CMake target
rem (SpoutDirectX\SpoutDX\CMakeLists.txt) with MSVC, then links each peer.
setlocal
set "SDK=%~1"
set "SRC=%~dp0"
set "OUT=%~2"
if "%SDK%"=="" (
  echo usage: build.bat ^<SPOUTSDK folder^> [output folder]
  exit /b 2
)
if "%OUT%"=="" set "OUT=%~dp0"
if not exist "%SDK%\SpoutDirectX\SpoutDX\SpoutDX.cpp" (
  echo not a SPOUTSDK folder: %SDK%
  exit /b 2
)
for /f "usebackq delims=" %%i in (`"%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VS=%%i"
if "%VS%"=="" (
  echo MSVC x64 build tools not found
  exit /b 1
)
call "%VS%\VC\Auxiliary\Build\vcvars64.bat" >nul 2>&1 || exit /b 1
if not exist "%OUT%\obj" mkdir "%OUT%\obj"
set "FLAGS=/nologo /EHsc /O2 /MD /std:c++17 /W3 /DNDEBUG /I"%SDK%\SpoutGL" /I"%SDK%\SpoutDirectX\SpoutDX""
cl %FLAGS% /c /Fo"%OUT%\obj\\" ^
  "%SDK%\SpoutDirectX\SpoutDX\SpoutDX.cpp" ^
  "%SDK%\SpoutGL\SpoutCopy.cpp" ^
  "%SDK%\SpoutGL\SpoutDirectX.cpp" ^
  "%SDK%\SpoutGL\SpoutFrameCount.cpp" ^
  "%SDK%\SpoutGL\SpoutSenderNames.cpp" ^
  "%SDK%\SpoutGL\SpoutSharedMemory.cpp" ^
  "%SDK%\SpoutGL\SpoutUtils.cpp" || exit /b 1
for %%p in (peer_receive peer_send peer_list) do (
  cl %FLAGS% /Fo"%OUT%\obj\\" /Fe"%OUT%\%%p.exe" "%SRC%%%p.cpp" ^
    "%OUT%\obj\SpoutDX.obj" "%OUT%\obj\SpoutCopy.obj" "%OUT%\obj\SpoutDirectX.obj" ^
    "%OUT%\obj\SpoutFrameCount.obj" "%OUT%\obj\SpoutSenderNames.obj" ^
    "%OUT%\obj\SpoutSharedMemory.obj" "%OUT%\obj\SpoutUtils.obj" ^
    /link d3d11.lib dxgi.lib winmm.lib opengl32.lib user32.lib gdi32.lib || exit /b 1
)
echo built peers in %OUT%
