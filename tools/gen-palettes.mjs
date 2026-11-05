// Material 3 tonal-spot schemes (light + dark) for each pastel seed.
// Regenerate web/palettes.json (dev only, not part of the build):
//   npm install @material/material-color-utilities@0.3.0 && node tools/gen-palettes.mjs > web/palettes.json
import { argbFromHex, hexFromArgb, Hct, SchemeTonalSpot, MaterialDynamicColors as C } from "@material/material-color-utilities";

const seeds = { sage: "#9CAF88", dusty: "#8FA9C4", lavender: "#B8A9D9", blush: "#E8B4B8", peach: "#F4C2A1", butter: "#E9D58C" };
const roles = ["primary", "onPrimary", "primaryContainer", "onPrimaryContainer", "secondaryContainer", "onSecondaryContainer",
  "tertiaryContainer", "onTertiaryContainer", "surface", "surfaceContainerLow", "surfaceContainer", "surfaceContainerHigh",
  "surfaceContainerHighest", "onSurface", "onSurfaceVariant", "outline", "outlineVariant", "errorContainer", "onErrorContainer"];

const out = {};
for (const [name, hex] of Object.entries(seeds)) {
  out[name] = { seed: hex };
  for (const dark of [false, true]) {
    const scheme = new SchemeTonalSpot(Hct.fromInt(argbFromHex(hex)), dark, 0);
    out[name][dark ? "dark" : "light"] = Object.fromEntries(roles.map((r) => [r, hexFromArgb(C[r].getArgb(scheme))]));
  }
}
console.log(JSON.stringify(out));
