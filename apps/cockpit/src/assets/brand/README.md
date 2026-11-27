# Brand marks

**The ribbon mark belongs to Vibely (its owner) and is used in Mirante by permission. It is
not covered by Mirante's code license (Apache-2.0): that license grants no right to use the
mark.** Forks and redistributions must remove it or get the owner's permission.

- `ribbon.svg`: the master, two-tone (#650C10 back band, #A8141B front).
- `ribbon-mono.svg`: one colour (#FAFAFA, the back band at 60%), for dark or tinted surfaces.

Both come from Vibely's `vibely.svg` / `vibelyWhite.svg` with the path data unchanged (exact
geometry). Only the drop and inner shadow filter is left out, and the viewBox is cropped to the
shapes (`10 3 772.4 298`). The "Vibely™" wordmark and lockup are never used: next to the mark
the app sets "Mirante" in Nunito Sans.

The same paths are inline in `src/components/Splash.tsx` (`RibbonMark`) and in `index.html`
(the launch splash); `src/lib/splash.test.ts` keeps the three identical. The app icon is
rendered from `ribbon.svg` (see `src-tauri/icons/source/`).
