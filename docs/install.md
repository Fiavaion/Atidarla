# Installing Atidarla (FR-H5)

| Platform | Installer | Built by |
|---|---|---|
| macOS (Apple silicon) | `Atidarla-<version>-macos-arm64.dmg` | `packaging/macos/build-dmg.sh` |
| Windows (x64) | `Atidarla-<version>-windows-x64.msi` | `packaging/windows/build-msi.ps1` |
| Linux (best-effort) | `Atidarla-<version>-linux-x86_64.AppImage` | `packaging/linux/build-appimage.sh` |

Each installs the app (`atidarla-app`), the headless CLI (`atidarla`) beside it, the example
projects and the licences. The scripts write to `dist/` (not committed) and never install anything
or need administrator rights to build.

## Prerequisites on the target machine

- **Python 3.13** (the app embeds it through PyO3 and links it dynamically).
  - macOS: the build links the Python it was built against. The DMG built on a Mac with Homebrew
    expects Homebrew's `python@3.13` (`brew install python@3.13`); build on a machine with the
    python.org framework build to target that instead. Bundling Python in the app is not done yet.
  - Windows: python.org's 64-bit Python 3.13 with `python313.dll` on the `PATH`
    (`python-3.13.x-amd64.exe /quiet InstallAllUsers=1 PrependPath=1`).
- **Windows:** the Microsoft Visual C++ 2015–2022 Redistributable (x64)
  (`vc_redist.x64.exe /install /quiet /norestart`); most machines have it already.
- A GPU with Metal (macOS 13+), DX12 (Windows 10/11) or Vulkan (Linux).

## macOS

Open the DMG and drag Atidarla to Applications. The DMG also has an Examples folder.

An unsigned build (no identity set, the default) is signed ad hoc: the first launch is blocked
by Gatekeeper; Control-click the app > Open, or `xattr -dr com.apple.quarantine
/Applications/Atidarla.app`. Signed and notarised builds open normally.

The CLI is inside the bundle: `/Applications/Atidarla.app/Contents/MacOS/atidarla` (link it onto
your `PATH` if you use it often). Syphon is bundled in `Contents/Frameworks`.

### Building the DMG

```sh
packaging/macos/build-dmg.sh                 # release build, then dist/Atidarla-<version>-macos-<arch>.dmg
ATD_SKIP_BUILD=1 packaging/macos/build-dmg.sh # reuse target/release
```

Signing and notarisation are switched on by environment variables:

| Variable | Meaning |
|---|---|
| `ATD_MACOS_SIGN_IDENTITY` | `Developer ID Application: <Name> (<TEAMID>)` in the keychain. Signs the framework, both executables and the bundle with the hardened runtime and a timestamp (`packaging/macos/entitlements.plist`), then the DMG. |
| `ATD_NOTARY_PROFILE` | A `notarytool` keychain profile: `xcrun notarytool store-credentials atidarla --apple-id <id> --team-id <TEAMID>` once, then `ATD_NOTARY_PROFILE=atidarla`. |
| `ATD_NOTARY_APPLE_ID`, `ATD_NOTARY_TEAM_ID`, `ATD_NOTARY_PASSWORD` | The same without a keychain profile (CI): Apple ID, team ID and an app-specific password. |
| `ATD_BUNDLE_ID` | Bundle identifier (default `org.atidarla.Atidarla`). |
| `ATD_VERSION` | Version (default: the workspace version). |

With an identity and notary credentials, the script submits the DMG, waits, staples the ticket
and validates it. The entitlements allow loading the Python framework and its extension modules
(not signed by our team) and ask for the microphone and camera (Audio In, Video In).

## Windows

Double-click the MSI, or install silently, for example on a lab golden image:

```bat
msiexec /i Atidarla-1.0.0-windows-x64.msi /qn /norestart /l*v "%TEMP%\atidarla-install.log"
```

| Property | Default | Meaning |
|---|---|---|
| `INSTALLFOLDER` | `C:\Program Files\Atidarla` | Install folder |
| `ADDTOPATH` | `1` | Put the folder on the system `PATH` (so `atidarla` works in any terminal) |
| `DESKTOPSHORTCUT` | `0` | A desktop shortcut for all users (the Start menu shortcut is always made) |

```bat
rem Everything for a lab image: own folder, desktop shortcut, log
msiexec /i Atidarla-1.0.0-windows-x64.msi /qn /norestart INSTALLFOLDER="D:\Apps\Atidarla" DESKTOPSHORTCUT=1 /l*v C:\Logs\atidarla.log

rem Uninstall silently (by file or by product code)
msiexec /x Atidarla-1.0.0-windows-x64.msi /qn /norestart
```

The install is per machine (it needs an elevated prompt, as image builds have). A newer MSI
replaces an older one (major upgrade); installing an older one over a newer one is refused.
`msiexec` exits 0 on success and 3010 when a restart is wanted. A golden-image recipe in order:
the VC++ redistributable, Python 3.13 (all users, on the `PATH`), then the MSI; check with
`atidarla --version` and `atidarla check --gpu "C:\Program Files\Atidarla\examples\stage-deck.atidarla"`.

### Building the MSI

On Windows with Rust (MSVC), the .NET SDK and WiX v5 (`dotnet tool install --global wix
--version 5.0.2`):

```powershell
pwsh packaging/windows/build-msi.ps1            # release build, then dist\Atidarla-<version>-windows-x64.msi
pwsh packaging/windows/build-msi.ps1 -SkipBuild
```

Signing (signtool from the Windows SDK, SHA-256 with an RFC 3161 timestamp) signs both
executables and the MSI when one of these is set:

| Variable | Meaning |
|---|---|
| `ATD_WIN_SIGN_THUMBPRINT` | SHA-1 thumbprint of a code-signing certificate in the store or on a hardware token (EV and most OV certificates now live on tokens or in a cloud HSM) |
| `ATD_WIN_SIGN_PFX`, `ATD_WIN_SIGN_PFX_PASSWORD` | A `.pfx` file and its password |
| `ATD_WIN_TIMESTAMP_URL` | Timestamp server (default `http://timestamp.digicert.com`) |

Unsigned MSIs install, but SmartScreen warns on download.

## Linux (best-effort)

```sh
chmod +x Atidarla-*.AppImage
./Atidarla-1.0.0-linux-x86_64.AppImage                       # the app
./Atidarla-1.0.0-linux-x86_64.AppImage soak show.atidarla    # a CLI command runs the CLI
```

The AppImage carries the app, the CLI and the examples; the GPU driver (Vulkan), ALSA, X11 or
Wayland and Python 3.13 come from the system. Build with `packaging/linux/build-appimage.sh`
(it downloads `appimagetool` into `dist/tools` unless `APPIMAGETOOL` points at one).

## Release builds in CI

`.github/workflows/release.yml` builds all three on a version tag (`git tag v1.0.0 && git push
origin v1.0.0`), signs where the secrets are set, and drafts a GitHub release with the
installers and `SHA256SUMS.txt`. Started by hand (Actions > Release > Run workflow) it is a dry
run: it builds unsigned installers and keeps them as workflow artefacts only. It never runs on
ordinary pushes. The secrets are listed at the top of the workflow.
