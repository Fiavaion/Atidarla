# Spike (c): macOS signing + notarisation with embedded Python and a signed AI Host helper

**Status:** not run. Needs a Mac (Apple Silicon), Xcode command-line tools and an Apple Developer ID. · **Feeds:** FR-H5, RESEARCH §5.1 (deep-sign, hardened runtime), risk "macOS signing of Python/torch".

## Questions to answer
1. Can `Atidarla.app` (Rust binary plus PyO3 linked against a relocatable CPython 3.13 from python-build-standalone in `Contents/Frameworks`) be deep-signed with hardened runtime and notarised?
2. Which entitlements do we need? Expected: `com.apple.security.cs.disable-library-validation` only if third-party Python extensions are loaded in-process. Keep the main app free of it if possible.
3. Can the AI Host ship as a **separately signed helper bundle** (`Contents/Helpers/AtidarlaAIHost.app`) with its own uv-managed environment, notarised with the app, and launched by the supervisor without Gatekeeper prompts?
4. Does `spctl --assess` pass on a clean machine, and does first launch pass Gatekeeper with quarantine set?

## Procedure
1. Write an `xtask bundle-macos` command that builds the app bundle, copy python-build-standalone, and sign every Mach-O file inside-out with `codesign --options runtime --timestamp`.
2. `xcrun notarytool submit --wait`, then `xcrun stapler staple`.
3. Download the DMG on a second Mac (quarantined) and launch it. The Python REPL and the AI Host helper must both start.

## Record
Entitlements used · notarisation time · DMG size · any rejected binaries and how they were fixed · Gatekeeper result.
