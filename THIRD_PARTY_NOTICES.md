# Third-party notices

Mirante's own code is licensed under Apache-2.0 (see LICENSE).

No third-party source code is included. Design patterns were studied from
MIT/Apache projects (T3 Code, Paradev, Emdash, vibe-kanban, opencode) and
reimplemented from prose descriptions. GPL/AGPL/BUSL projects were used for
ideas only. Dependencies are listed in Cargo.lock under their own licenses.

## Fonts bundled in the desktop app (apps/cockpit)

The cockpit ships two font families under the SIL Open Font License,
Version 1.1. The full license texts are next to the files, in
`apps/cockpit/src/assets/fonts/`. JetBrains Mono is unmodified. Nunito Sans is subset (a
Modified Version under the OFL; the font declares no Reserved Font Name) and keeps its
copyright in the font's name table.

- **Nunito Sans** 3.101 (`NunitoSans-Variable.woff2`, the upright variable font subset to
  Latin and Latin Extended). Copyright 2016 The Nunito Sans Project Authors. SIL OFL 1.1
  (`NunitoSans-OFL.txt`). Source: https://github.com/googlefonts/NunitoSans (as shipped
  by google/fonts, upstream commit 058bd7a).
- **JetBrains Mono** 2.304 (`JetBrainsMono-Regular.woff2`, `JetBrainsMono-Bold.woff2`).
  Copyright 2020 The JetBrains Mono Project Authors. SIL OFL 1.1
  (`JetBrainsMono-OFL.txt`). Source: https://github.com/JetBrains/JetBrainsMono
  (release v2.304).

## Marks (not licensed under Apache-2.0)

- **The Vibely ribbon mark** (`apps/cockpit/src/assets/brand/ribbon.svg`,
  `ribbon-mono.svg`, inline in `apps/cockpit/index.html` and
  `apps/cockpit/src/components/Splash.tsx`, and the app icon in
  `apps/cockpit/src-tauri/icons/`) belongs to Vibely, its owner, and is used in the Mirante
  app by permission. Mirante's license does not grant any right to use it.
- The "Mirante" wordmark is set in Nunito Sans (above) and outlined to a path.
