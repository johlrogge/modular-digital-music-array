# ADR-029: Per-view narrowing, matched only in Rust

## Status

Accepted, 2026-11-16 (Joakim decided)

Related: ADR-009 §4, ADR-004, ADR-022 §4 (one fold).

## Context

- The redesigned web console (`projects/mdma-console`) narrows lists with a search text plus BPM, key and added filters. Text search runs on the server through `library_search`: `GET /api/tracks/match` and `GET /api/library/match`.
- The filters were one global object, `store.f`, while the search text was already per view (`store.nb[key]`). Clearing the Library also cleared Up next's filters, and filters set in one view silently narrowed another.
- Album, artist and playlist pages and a playlist workbench (library pane plus playlist pane) are next. Each would have its own narrowing, so the state model has to be settled first.
- A browser matcher (a second implementation of the text rules, possibly compiled to WASM) would have to track `library_search` exactly: initialisms, word breaks, case rules.

## Decision

### 1. Narrowing state is per view

- A view's narrowing is its search text and its filters (BPM, key, added). Both are keyed by the view key: `store.nb[key]` and `store.f[key]`.
- Clearing or changing one view never touches another.
- **View keys are per page kind, not per instance:** `lib`, `queue`, `album`, `artist`, `playlist`, `workbench-lib`, `workbench-pl`. When the instance changes (another album, another artist, another playlist), the page resets that key's text and filters. This keeps the store bounded by the number of kinds.
- **Sort:** the sort stays global (`store.sort`), but a view opts in. A view that keeps its own order, such as the workbench playlist pane (playlist order), does not use it and ignores sort changes.
- Persistence stores the map in sessionStorage under `mdma-f.v2`. On first load the old `mdma-f` (one global filters object) is read once as the Library's filters and then removed.
- Every narrowed view is built on one controller, `narrowedView`, which owns the narrowing bar, the stale-reply guard, the debounce disposal and the failure policy. Events: `filters` (always carries the view key) and `sort`.

### 2. Text matching happens only in Rust

- All text matching is `library_search`, on the server. That covers track text, artist, and playlist names (`LibraryRequest::PlaylistSearch`, answered by the library service and its stub with `StringMatcher`).
- The console binary only forwards. The browser never matches text.
- The Library view asks once per narrowing state: `GET /api/library/match` returns the matching tracks, the tracks passing the filters alone (only when text and filters are both on) and the matching playlist names (only with text).
- There is no separate playlist-name endpoint. A page that needs one adds it when it needs it.

### 3. The browser does set algebra on server results

- The browser intersects, unions and looks up in the sets the server returned: which tracks to list, which playlists to show, which rows of Up next to keep.
- Pure rules like that live in `logic.js` (for example `visiblePlaylists`).

### 4. WASM is not used, and when to revisit

- Revisit a WASM matcher only if one of these holds:
  - p95 latency of a match request on the Pi exceeds about 100 ms; or
  - an offline client appears that must narrow without the server.
- Until then there is exactly one matcher.

## Amendment 2026-11-17: the `marked` view and the mark facet

- **A new view key, `marked`,** for the Marked view (`#/marked/:mark?`, ADR-016 amendment). It follows the rules above: its own text and filters, global sort, no instance reset. Which mark the page shows (All, Liked, a macro) is the address, not narrowing state, so changing the chip keeps the text and filters.
- **A fourth filter group, Mark.** A view's filters are now BPM, key, added and mark (`f.mark`, `""` for any). It counts once in the Filters badge, shows as a removable chip, and is sent as `mark=` to `GET /api/tracks/match` and `GET /api/library/match`, where `SearchParams.mark` becomes `TrackQuery.mark` and the library matches it (`library_search`, so the one matcher still holds). A name the library cannot parse is a 400.
  - The sheet offers Any, Liked and the node's macro marks. Filters saved before this amendment have no `mark` and read as any.
  - Answers are cached per request string, so the cache is cleared when a mark changes.
- **Marks as set algebra.** The Marked view's pool (which tracks carry the mark) comes from the catalogue's `marks`; text and filters narrow it through the server, and the browser only intersects, as in §3.

## Amendment 2026-11-18: the key filter is a reach

- **The key filter becomes steps, shape and jump.** The "keys that mix well" checkbox (`f.compat`) is replaced by `f.steps` (0 = the key alone, 1 to 3), `f.shape` (`straight` or `any`, only meaningful from 2 steps) and `f.jump` (the +7 power jump). Filters saved with `compat: true` are migrated to `steps: 1`. The browser writes the key query with one helper, `keyReach(...)`, e.g. `8A+-2d,+7`; the grammar and its meaning live in `library_search` (`docs/data-model.md`, "Key query grammar"). `8A+-2` changed meaning from any path to Straight (a T shape); `±1` is unaffected.
- **Highlighting is the server's.** `GET /api/keys?reach=<key query>` marks each of the 24 keys `included` through `KeyQuery::matches`, so the browser never works the wheel out and the one-matcher rule holds. The plain list is in wheel order (`Key::camelot_wheel`, 1A..12A then 1B..12B) and the sheet shows it as two aligned rows.
- **Wire semantics.** A new CLI talking to an old node silently gets the old any-path meaning for `8A+-2`, while `d` and `,+7` fail loudly as unparseable; acceptable under redeploy-together.

## Alternatives considered

- Global filters with per-view overrides: two layers of state and a rule for which wins; clearing becomes ambiguous.
- A browser matcher or WASM: a second implementation of the text rules that must be kept identical, for no latency problem we have measured.

## Consequences

- One implementation of the text rules; changing them changes every client at once.
- `PlaylistSearch` is a new wire variant. Deploy `mdma-library` and `mdma-console` together: against an old library, text search answers 500 and the console shows its "search unavailable" note.
- A change to the `library_ipc_protocol` wire also needs `mdma-gateway` redeployed, because the gateway decodes `LibraryRequest` itself. An old gateway rejects `PlaylistSearch` as an invalid request (seen on stage2), so deploy `mdma-gateway` with `mdma-library` and `mdma-console`.
- Each keystroke is one request. For a new state with text and filters the console makes up to 3 sequential library calls (tracks, filters alone, playlist names). Answers are cached per narrowing state in the browser, so backspacing is instant, and a failed lookup shows a note instead of unfiltered rows.
- Narrowing state lives in sessionStorage, so it is lost when the tab closes.
- New pages get their own narrowing by choosing a view key; no shared state to untangle.
