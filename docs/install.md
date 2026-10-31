# Installing Atidarla (FR-H5)

| Platform | Installer | Built by |
|---|---|---|
| macOS (Apple silicon) | `Atidarla-<version>-macos-arm64.dmg` | `packaging/macos/build-dmg.sh` |
| Windows (x64) | `Atidarla-<version>-windows-x64-setup.exe` (recommended) and `Atidarla-<version>-windows-x64.msi` | `packaging/windows/build-msi.ps1` |
| Linux (best-effort) | `Atidarla-<version>-linux-x86_64.AppImage` | `packaging/linux/build-appimage.sh` |

Each installs the app (`atidarla-app`), the headless CLI (`atidarla`) beside it, the example
projects, the licences and its own Python (below). The scripts write to `dist/` (not committed)
and never install anything or need administrator rights to build.

## Prerequisites on the target machine

Only a GPU with Metal (macOS 13+), DX12 (Windows 10/11) or Vulkan (Linux). Nothing else needs
installing first (PRD A68):

- **Python is bundled.** Every installer carries CPython 3.13 from
  [python-build-standalone](https://github.com/astral-sh/python-build-standalone) (PSF licence,
  shipped as `Python-License.txt`), pinned with its SHA-256 in `packaging/python-runtime.env`.
  It lives in `Atidarla.app/Contents/Resources/python` (macOS), `<install folder>\python`
  (Windows) or `usr/lib/atidarla/python` (AppImage); the app starts it from there and ignores any
  other Python on the machine. Python operators, expressions, the Script DAT and the Textport use
  it; packages you `pip install` elsewhere are not seen.
- **Windows:** the setup `.exe` installs the Microsoft Visual C++ 2015–2022 runtime (x64) before
  the app, unless the same or a newer one is already there. The bare MSI does not: deploy
  `vc_redist.x64.exe /install /quiet /norestart` before it.
- **The AI Host is not bundled** (its ML packages run to gigabytes, and model weights are never
  shipped). Set it up once with [uv](https://docs.astral.sh/uv/) from an Atidarla source folder,
  using the bundled Python as its base so no other Python is needed:

  ```sh
  cd ai-host
  uv sync --python /Applications/Atidarla.app/Contents/Resources/python/bin/python3   # macOS
  uv sync --python "C:\Program Files\Atidarla\python\python.exe"                      # Windows
  ```

  To have an installed app start that host, set `ATD_AI_HOST_CMD` to its command, for example
  `ATD_AI_HOST_CMD="<atidarla source>/ai-host/.venv/bin/python -m atd_ai_host"`.

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
and validates it. The script signs the bundled Python's libraries and program first (inside out).
The entitlements allow loading Python extension modules not signed by our team and ask for the
microphone and camera (Audio In, Video In).

The script builds the app against the bundled Python (`PYO3_CONFIG_FILE` from
`packaging/fetch-python.sh`), precompiles its standard library so nothing is written into the
signed bundle, and stops if a binary links a Python outside the bundle. `ATD_SKIP_BUILD=1` needs
binaries built the same way.

## Windows

Double-click `Atidarla-<version>-windows-x64-setup.exe`. It installs the Visual C++ runtime when
needed, then Atidarla. For a lab golden image, install silently:

```bat
rem Everything (VC++ runtime + Atidarla), no UI, no restart, with a log
Atidarla-1.0.0-windows-x64-setup.exe /quiet /norestart /log "%TEMP%\atidarla-setup.log"

rem The same with the properties below
Atidarla-1.0.0-windows-x64-setup.exe /quiet /norestart INSTALLFOLDER="D:\Apps\Atidarla" DESKTOPSHORTCUT=1

rem Uninstall silently (the VC++ runtime stays: other programs share it)
Atidarla-1.0.0-windows-x64-setup.exe /uninstall /quiet /norestart
```

The setup exits 0 on success and 3010 when a restart is wanted. `/passive` shows progress without
asking anything.

The MSI is the same app without the VC++ runtime, for deployment tools (Intune, SCCM, Group Policy)
that want an MSI and deploy the runtime themselves:

```bat
vc_redist.x64.exe /install /quiet /norestart
msiexec /i Atidarla-1.0.0-windows-x64.msi /qn /norestart /l*v "%TEMP%\atidarla-install.log"
```

Both take these properties (on the setup's command line or msiexec's):

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
`msiexec` exits 0 on success and 3010 when a restart is wanted. A golden-image recipe: the setup
`.exe` silently (nothing else first); check with `atidarla --version` and
`atidarla check --gpu "C:\Program Files\Atidarla\examples\stage-deck.atidarla"`.

### Building the installers

On Windows with Rust (MSVC), the .NET SDK and WiX v5 (`dotnet tool install --global wix
--version 5.0.2`; the script adds the `WixToolset.Bal.wixext` and `WixToolset.Util.wixext`
5.0.2 extensions itself):

```powershell
pwsh packaging/windows/build-msi.ps1            # release build, then dist\Atidarla-<version>-windows-x64.msi and -setup.exe
pwsh packaging/windows/build-msi.ps1 -SkipBuild
```

The script downloads the pinned Python (`packaging/python-runtime.env`, checked by SHA-256) and
links the build against it, and downloads `vc_redist.x64.exe` from Microsoft into `dist\tools`
(or takes `ATD_VCREDIST`). Microsoft's link always serves the latest 14.x runtime, so the script
checks its Microsoft Authenticode signature rather than a fixed hash; set `ATD_VCREDIST_SHA256` to
pin one as well.

Signing (signtool from the Windows SDK, SHA-256 with an RFC 3161 timestamp) signs both
executables, the MSI, the setup bundle's engine and the bundle when one of these is set:

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

The AppImage carries the app, the CLI, the examples and Python; the GPU driver (Vulkan), ALSA,
X11 or Wayland come from the system. Build with `packaging/linux/build-appimage.sh`
(it downloads `appimagetool` into `dist/tools` unless `APPIMAGETOOL` points at one).

## Release builds in CI

`.github/workflows/release.yml` builds all three on a version tag (`git tag v1.0.0 && git push
origin v1.0.0`), signs where the secrets are set, and drafts a GitHub release with the
installers and `SHA256SUMS.txt`. Started by hand (Actions > Release > Run workflow) it is a dry
run: it builds unsigned installers and keeps them as workflow artefacts only; publishing happens
only for a tag. It never runs on ordinary pushes. GitHub offers Run workflow (and `gh workflow run
release.yml --ref <branch>`) only once the workflow file is on the default branch. The secrets are
listed at the top of the workflow.
