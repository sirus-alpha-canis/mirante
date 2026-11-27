# Plan: Vibely visual identity for the Mirante cockpit

> **For agentic workers:** use superpowers:subagent-driven-development. One committing agent at a time. Commit author for this repo: `Pedro Cândido <32328995+pedrincandido@users.noreply.github.com>` (already set in the local git config).

**Goal:** the cockpit takes Vibely's look and logo. The product name stays **Mirante**: the CLI, binaries, `~/.mirante`, `MIRANTE_*`, the repo and the Homebrew formula are unchanged.

**Decision (founder, 2026-11-25):** "Look + logo only". The Vibely ribbon mark replaces the ProjectDNA Orb-Dot. The wordmark next to it reads **Mirante**, not "Vibely™".

**Source of truth:** `~/Documents/vibely-app/src/theme/` (colors.ts, light.ts, dark.ts, fonts.ts, shadows.ts, styles.ts). The logo masters are in `~/Documents/vibely-landing-web/src/assets/svg/` (`vibely.svg`, `vibelyWhite.svg`) and `~/Documents/vibely-app/src/assets/images/loadingLogo.svg`. Vibely has no Figma file or brand doc.

---

## Decisions this plan makes

1. **Only what Mirante's rules allow comes over.** Color is used only for state; there is AA contrast in both themes; the UI stays dense, with 26px buttons and 30px rows. Vibely's 61px buttons, floating labels, pill radii, the 3-stop "Viby" gradient and solid-900 dark status cards stay out.
2. **Red is the brand and "danger" — never an accent.** `--accent` stays neutral, like Vibely's `normal` button (neutral900). Red appears only in the mark and in failed/danger states.
3. **The state vocabulary maps to Vibely's semantic scales.** Where Vibely's own step fails AA, a darker or interpolated value is used and marked `(new)` in `styles.css`.
   - needs you → alert (amber)
   - working → information (blue; this replaces cyan)
   - ready → success (green)
   - failed → primary red
4. **Fonts:**
   - **Nunito Sans** (variable woff2 from google/fonts, SIL OFL 1.1) replaces Inter.
   - **JetBrains Mono** stays, since Vibely has no mono.
   - Mirante's type scale (12/13/15/20) is kept; only the family changes.
   - **Gotham Rounded is never bundled.** It is commercial (Hoefler & Co. EULA) and Mirante is open source.
5. **Logo:**
   - The two-tone ribbon mark (#A8141B front, #650C10 back) plus a "Mirante" wordmark set in Nunito Sans (outlined to paths for the splash, so no font is needed before load).
   - A white/mono variant for dark surfaces.
   - Never the "Vibely™" lockup.
   - Third-party icons in Vibely's asset folders (Google, Apple, Facebook, Instagram, WhatsApp, Pix) are not copied.
6. **App icon:** generated from the ribbon SVG master into `.icns` and all Tauri icon sizes. Vibely only has raster app icons, so the vector is the cleaner source. It uses a light background like Vibely's iOS icon, with a dark variant if Tauri/macOS needs one.
7. **Icons:** Mirante's current icons stay. Phosphor (MIT) is an option later, not part of this plan.

## Token map (light / dark)

`(new)` means the value is not in Vibely's palette; it was adjusted to pass AA.

| Token | Light | Dark |
|---|---|---|
| --bg | #EFEFEF (new) | #111111 n1000 |
| --panel | #FAFAFA n000 | #1A1A1A |
| --panel-2 | #FFFFFF | #1E1E1E n900 |
| --panel-3 / --line | #E5E5E5 n100 | #2A2A2A (new) |
| --text | #1E1E1E | #FAFAFA |
| --muted | #3B3B3B | #BBBBBB |
| --faint | #666666 | #909090 |
| --accent / --accent-text | #1E1E1E on #FFFFFF | #FAFAFA on #111111 |
| --needs / --needs-bg | #8F4E19 (new) / #FEECC8 | #FBBB3C / #3D2E10 (new) |
| --work / --work-bg | #2659BE / #EAF1FD | #7AA3F4 / #18264A (new) |
| --review, --add / --review-bg | #1C6E53 / #E8FBF5 | #36D39F / #123A2D (new) |
| --danger, --del / --on-danger | #A8141B / #FAFAFA | #D9787C (new) / #111111 |
| --r-sm / --r / --r-lg | 4 / 8 / 12 | same |
| --shadow | 0 4px 6px rgb(0 0 0 / .19) | same |
| --sans | "Nunito Sans" | same |
| --mono | "JetBrains Mono" | same |
| splash mark | #650C10 back / #A8141B front | #650C10 / #A8141B on #111111 |

Every text/background pair is checked by `styles.test.ts`: 4.5:1 for text and 3:1 for the focus ring.

**As built (Batch A, Task 1):**
- Dark `--faint` is **#919191 (new)**, not #909090 (n400): #909090 on `--panel-3` #2A2A2A is 4.496:1, just under AA; #919191 gives 4.55:1. Every other value is as in the map.
- `--focus` (not in the map) is n800 #3B3B3B light / n200 #BBBBBB dark: 8.9:1 and 7.5:1 on `--panel-3`, its lowest surface.
- The window backdrop (`src-tauri/src/lib.rs`) and the splash background match `--panel` (#FAFAFA / #1A1A1A), not `--bg`: that is what `body` paints, so the window shows no flash. Unchanged design; only the values moved.
- The terminal keeps its own always-dark xterm theme (#141519); it is not a token.

## Batch A — Tokens and fonts

**Task 1: tokens**
- Rewrite the `:root` light and dark blocks in `apps/cockpit/src/styles.css` with the map above.
- Extend `styles.test.ts` with every new pair, both themes.
- Update the Rust window backdrop colours in `src-tauri/src/lib.rs` (they must match `--bg`) and their test.

**Task 2: Nunito Sans**
- Add `NunitoSans[YTLC,opsz,wdth,wght].woff2` (variable) and `NunitoSans-OFL.txt` under `src/assets/fonts/`, with an `@font-face` for weights 200–1000.
- Remove the Inter files and their `@font-face`.
- Update `fonts.test.ts`, `security.test.ts` (no remote fonts) and the `THIRD_PARTY_NOTICES.md` font entries (drop Inter, add Nunito Sans).
- If the download is denied, the founder fetches the google/fonts release and the agent bundles it from `/tmp`.

## Batch B — Logo, splash, app icon

**Task 3: mark components**
- `OrbMark` becomes `RibbonMark` (inline SVG paths from `vibely.svg`, with a `still` prop and a mono variant).
- The top-bar Home button becomes ribbon + "Mirante".
- Update the tests that pin the mark shapes, so the splash and React marks stay identical.

**Task 4: splash**
- Replace the inline SVG in `apps/cockpit/index.html` with the ribbon plus an outlined "Mirante" wordmark.
- The animation is our own: a subtle draw-in or a sheen along the ribbon, 120–600ms, and none under `prefers-reduced-motion`.
- No remote resources; the theme-boot script is unchanged.

**Task 5: app icon**
- Build a 1024² master from the ribbon SVG: on #FFFFFF with the iOS-style safe area, plus a dark variant.
- Generate all Tauri icon sizes and the `.icns` (e.g. `pnpm tauri icon <master.png>`).
- Commit the master SVG and PNG under `apps/cockpit/src-tauri/icons/source/`.
- Remove the ProjectDNA-based icons.

**As built (Task 2):**
- Nunito Sans **3.101** (name table; METADATA.pb carries upstream commit 058bd7a, no version), upright only, subset to Latin + Latin Extended by the founder (118,764 B; Inter was 352,240 B). Its wght axis is 200–1000 (default 200), so `@font-face` says `font-weight: 200 1000`. The subset drops the OFL name records (IDs 13/14); the license text sits next to the file and THIRD_PARTY_NOTICES says so. No Reserved Font Name.

**As built (Batch B):**
- Ribbon paths are Vibely's `vibely.svg` path data unchanged; only the shadow filter is dropped and the viewBox cropped to the shapes (`10 3 772.4 298`). Masters in `src/assets/brand/` (+ a README saying the mark is Vibely's, by permission, outside the code license).
- The mark stays two-tone in dark too (the token map's choice; the front red carries the silhouette on #111111). `RibbonMark tone="mono"` (currentColor, back at 60%, as `vibelyWhite.svg`) exists but nothing uses it yet.
- The wordmark is "Mirante" outlined with fontTools from the variable TTF instanced at wght 700 / wdth 100 / opsz 12 / YTLC 500, with GPOS pair kerning (r·a −5, t·e −5): one 2 KB path. Used in the launch splash and the in-app waiting splash; the top bar keeps "Mirante" as live text in Nunito Sans.
- Splash motion: the ribbon draws in from its centre (clip-path, 420ms), the word fades up (300ms after 180ms); once, none with reduced motion. The old infinite orbit is gone.
- App icon: `icons/source/render-icon.swift` (AppKit's own SVG support, offline) renders the master on #FFFFFF with a 10% margin (and a #111111 dark variant, source only), then `tauri icon`; only the five files `tauri.conf.json` lists are kept.

## Batch C — Components

**Task 6: buttons, inputs, chips, cards, toasts, modals in Vibely's language, at Mirante density**
- Radius goes to 8 (cards and modals 12).
- Inputs get a 1px border in `--line`, with labels kept above the field (no floating labels).
- Chips become tinted state pairs (`*-bg` with `*` text).
- The toast gets the amber left stripe for info, like Vibely's.
- The modal overlay goes to rgba(0,0,0,.6) (Vibely uses .8, which is too heavy for a desktop tool).
- Primary buttons stay neutral (#1E1E1E fill, #FAFAFA text). Destructive buttons use `--danger`.
- No gradients.

**Task 7: state colours in every view**
- Sweep the views (Home, Detail, Grid, Brain, Skills, Rules, side pane, fan-out/thread cards and the status line).
- Working uses blue now, so update the legends, the Brain node colours and the i18n strings that mention colours, if any.

## Batch D — Review in the app

**Task 8: in the app**
- Rebuild the release app.
- Capture window-only screenshots (never full screen) against the fake daemon, in light and dark, of: Home, Detail with the side pane, Grid, Brain, Skills, Rules, the splash and the app icon in the Dock.
- Run a contrast spot-check.
- Write a dogfood checklist section for the founder to eyeball.
- Update the README and the cockpit README where they describe the look, plus the CHANGELOG (Unreleased → Changed).

## Risks

- **Brand coupling:** the Vibely mark on an open-source product makes Mirante read as a Vibely product. The founder accepted this. A trademark note goes in the README or NOTICE stating the mark is used by its owner, so the code license doesn't grant use of the mark.
- **Font metrics:** Nunito Sans is rounder and wider than Inter, so check truncation in the rail, chips and the status line, and the xterm unaffected.
- **Colour identity:** "working" moves from cyan to blue, which some users of the old look will notice.

## Open questions

None blocking. The defaults above apply unless the founder changes them:
- "working" = blue;
- the `(new)` interpolated values are acceptable;
- Nunito Sans replaces Inter.
