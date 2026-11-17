// Entry point. Importing a feature module registers its routes; this file only wires them up.
// It must not grow per-feature code: each phase edits its own module, not this one.
import { startRouter, loadCatalogue, loadPlaylists, loadMarksConfig, refreshPlayer, startSync } from "./core.js";
import "./player.js";
import "./queue.js";
import "./lists.js";
import "./narrow.js";
import "./views.js";
import "./collections.js";
import "./playlist.js";
import "./workbench.js";
import "./settings.js";

startRouter();
refreshPlayer();
loadCatalogue();
loadPlaylists();
loadMarksConfig();
startSync();
