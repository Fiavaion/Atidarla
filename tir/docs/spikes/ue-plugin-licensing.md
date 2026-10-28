# Spike: licensing and distribution of the TirBridge UE plugin

**Date:** 28 October 2026 · **Status:** desk check complete. **Not legal advice.** A legal review is still required before the first public binary release.
**Question:** can an MIT OR Apache-2.0 project publish a UE 5.8 editor plugin on GitHub, and how should students get compiled binaries?

**Limits of this check.** The live EULA text could not be fetched from this environment, because `unrealengine.com`, `cdn2.unrealengine.com` and the Steam EULA mirror are blocked. The EULA wording below comes from search-result quotes and forum posts that quote it. Some of those quotes are from older (UE4-era) EULA versions. Re-read the current EULA at https://www.unrealengine.com/eula/unreal before relying on any of this.

## What the EULA says (as quoted)

- **"Engine Tools"** means "(a) editors and other tools included in the Engine Code; (b) any code and modules in either the Developer or Editor folders, including in object code format, whether statically or dynamically linked", plus other software that can be used to make standalone products.
- **Public distribution of Engine Tools:** "Any public Distribution (i.e., intended for Engine Licensees generally) which includes Engine Tools must take place either through the Marketplace [now Fab] … or through a fork of Epic's GitHub UnrealEngine Network (e.g., for distributing source code)."
- **Engine Code / source:** may be shared only with other engine licensees. Snippets of up to 30 lines may be posted publicly.
- **Your own code:** an Epic answer says a work that incorporates UE may be distributed under your own licence, "with a few caveats under the EULA". Those caveats are the royalty and Epic-protection terms and a bar on combining with non-compatible (copyleft) licences.
- **Engine content:** an Epic staff answer (2018) refused a plan to ship copied editor icons alongside MIT code. Engine content cannot be redistributed in a way that permits reuse.
- **Royalties** apply to shipped products that earn revenue, not to editor tooling. An editor-only module (`"Type": "Editor"`) is never packaged into a game.

## Precedents (checked on GitHub, Oct 2026)

| Plugin | Licence | Source on GitHub | Binaries on GitHub Releases | Also on Fab |
|---|---|---|---|---|
| Cesium for Unreal | Apache-2.0 | yes | **yes**: release ZIPs, installed into `Engine/Plugins/Marketplace` | yes |
| Houdini Engine for Unreal | (LICENSE.md not readable here) | yes | **yes**: README says binaries are built for UE 5.8 and 5.7 | yes [U] |
| Gaea2Unreal | **Modified MIT**: no selling, mandatory "Terrains powered by QuadSpinner Gaea" credit, and no competing standalone product | yes | yes ("compiled version … in the Releases") | [U] |
| mod.io UE plugin | MIT / BSL | yes | [U] | [U] |

Several large vendors therefore ship compiled editor plugins on GitHub Releases. Some of them may hold separate agreements with Epic (Cesium received an Epic MegaGrant; SideFX is a long-standing partner). Their practice is evidence of tolerance, not of permission.

## Findings

1. **Source on GitHub under MIT OR Apache-2.0: low risk.**
   - This applies to our own code that uses engine headers and APIs but contains no copied Engine Code (snippets of 30 lines or fewer only), no engine icons and no engine content.
   - It matches Cesium, Gaea2Unreal and mod.io.
2. **Compiled binaries on GitHub Releases: grey area.**
   - The editor module links dynamically against engine Editor modules.
   - One reading treats the binary as distributing something that "includes Engine Tools".
   - Forum readings disagree on this, and Epic has not ruled publicly.
3. **Compiled binaries via Fab as a free listing: compliant channel.** This is the channel the EULA names.
4. **Python fallback script: no issue.**
   - A script that drives the editor's `unreal` module contains no Engine Code.
   - It can ship anywhere under our licence.
5. **Gaea2Unreal's code must not be reused.**
   - Its licence forbids competing standalone products and requires a Gaea credit, so it is incompatible with Tír.
   - Treat it like Hesiod: study its behaviour, never copy it.
6. **No royalty exposure.**
   - Keep every TirBridge module `"Type": "Editor"`, so nothing ends up in a packaged game.
   - Keep no runtime module.

## Decision proposed for the PRD (T16)

- **Source.**
  - `TirBridge` is published as source on GitHub under MIT OR Apache-2.0.
  - It must contain no copied Engine Code, icons or content; CI greps for engine copyright headers.
  - Every module is editor-only.
- **Binaries.**
  - Compiled binaries for UE 5.8 (Mac + Windows) go out as a **free Fab listing**. That is also the easiest install route for students, via the Epic launcher.
  - GitHub Releases carry binaries only after a written answer from Epic licensing, or a sign-off from counsel.
  - Until then, GitHub Releases carry source ZIPs and build instructions only.
- **Zero-install path.** The Python fallback (FR-U9b) needs neither channel, so labs can use it from day one.
- **Before the first public binary release:**
  - re-read the live EULA and the Fab distribution terms;
  - send Epic licensing a short question;
  - record the answer in this file.

## Sources

- Forum threads: https://forums.unrealengine.com/t/releasing-plugin-under-open-source-license/414873 · https://forums.unrealengine.com/t/dual-licensing-engine-plugins/336083 · https://forums.unrealengine.com/t/distributing-plug-ins-outside-of-unreal-marketplace/1216713 · https://forums.unrealengine.com/t/eula-question-distribution-of-precompiled-plugins-to-third-parties-outside-the-marketplace-or-github-network/852815
- Epic answers on answers.unrealengine.com: https://answers.unrealengine.com/questions/748082/view.html · https://answers.unrealengine.com/questions/321377/view.html
- EULA pages: https://www.unrealengine.com/eula/unreal (live text not fetched) · https://cdn2.unrealengine.com/Unreal+Engine/faq/UnrealEngineEULA_for_Publishing_v15-b9955ec2954f0e09fade11b2d9acecfc65cb6ab1.pdf
- Plugin repositories: https://github.com/CesiumGS/cesium-unreal · https://github.com/sideeffects/HoudiniEngineForUnreal · https://github.com/QuadSpinner/Gaea2Unreal (LICENSE read) · https://github.com/modio/modio-ue
