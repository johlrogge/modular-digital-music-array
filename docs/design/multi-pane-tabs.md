# Multi-Pane Tabs — Design Proposal

## Status

Draft — awaiting Joakim's review before implementation. Branch: `feature/tui-search-and-panes`.

## Motivation

DJ workflow requires keeping several playlists open simultaneously: prepping a set while cross-referencing a "working ideas" playlist and a browser view for new tracks. The current TUI has a single pane per side (left/right), so switching between playlists means navigating back through `PlaylistsPane` each time.

Target UX: nnn-style tabs. `1`-`9` key switches the visible content of a pane. `1`-`5` bind to the left side, `6`-`9` bind to the right side.

## Current architecture (for reference)

- `projects/mdma-tui/src/app.rs:48-50` — `App` holds `left_pane: Box<dyn Pane>` and `right_pane: Box<dyn Pane>`, plus `active_side: Side`.
- `projects/mdma-tui/src/pane.rs:51-111` — `Pane` trait is already narrow (render, handle_key, selection state, etc). No shared expensive resources; each pane owns its own IPC handles and caches.
- `projects/mdma-tui/src/input.rs:22-127` — `handle_normal` dispatches Normal-mode keys; `1`-`9` are currently unbound.
- Existing pane types: `SearchPane`, `BrowserPane`, `QueuePane`, `PlaylistPane`, `PlaylistsPane`. `PlaylistPane` already carries its own `name: PlaylistName`, so multiple `PlaylistPane` instances pointed at different playlists coexist cleanly.

## Proposed changes

### Data model

```rust
struct App {
    left_tabs: Vec<Box<dyn Pane>>,
    left_tab_idx: usize,

    right_tabs: Vec<Box<dyn Pane>>,
    right_tab_idx: usize,

    active_side: Side,
    // ... rest unchanged
}
```

Accessors become helper methods:

- `active_pane()` / `active_pane_mut()` — returns `left_tabs[left_tab_idx]` or `right_tabs[right_tab_idx]`.
- `inactive_pane()` / `inactive_pane_mut()` — the other side's active tab.
- `switch_active_pane(pane: Box<dyn Pane>)` — replaces `tabs[idx]` for the current side, preserving other tabs.

Most call sites already go through accessors — the refactor is mostly mechanical.

### Keymap

In `handle_normal`, add:

```rust
KeyCode::Char(c @ '1'..='5') => {
    let idx = (c as u8 - b'1') as usize;
    if idx < app.left_tabs.len() {
        app.left_tab_idx = idx;
        app.active_side = Side::Left;
    }
}
KeyCode::Char(c @ '6'..='9') => {
    let idx = (c as u8 - b'6') as usize;
    if idx < app.right_tabs.len() {
        app.right_tab_idx = idx;
        app.active_side = Side::Right;
    }
}
```

Creating a new tab is a separate action (see open question on tab creation).

### Pane lifecycle

- Per-tab state (selection, filter, search query) stays with the tab between switches — switching tabs is cheap.
- Panes don't leak: an old tab's pane drops normally when the tab is closed.

### What stays untouched

- `Pane` trait — no new methods.
- Individual pane implementations — unchanged.
- Render layout — left/right split stays the same; just shows the *active* tab per side. Future: a thin tab-bar strip at the top of each pane showing tab numbers and titles.

## Open UX questions

1. **Startup tabs**. Two reasonable options:
   - *Single default tab per side* (left: `PlaylistsPane`, right: `QueuePane`) — matches current behaviour.
   - *Saved from last session* — persist the set of open tabs in a config file. Probably out of scope for v1.
   **Proposed v1**: single default tab per side. Joakim can create more on demand.

2. **Tab creation**. How does a user open a second tab?
   - *(a)* From `PlaylistsPane`, Enter drills into a playlist. Today that *replaces* the active pane. Change Enter to open in a new tab on the same side (pushing current tab index to len and making it active). `Shift+Enter` could keep the "replace" behaviour.
   - *(b)* An explicit "new tab" key like `t`. Opens an empty `SearchPane` or similar and makes it active.
   - *(c)* `:new playlist <name>` palette command.
   **Proposed v1**: Enter in `PlaylistsPane` opens in a new tab (push, not replace). Silently cap at 5 left / 4 right — if slots full, replace current tab.

3. **Tab closing**. Tabs accumulate otherwise.
   - *(a)* `x` closes current tab. If it's the only tab on that side, reset to the default pane type rather than leaving empty.
   - *(b)* `:close` palette command.
   **Proposed v1**: `x` key closes current tab. Last-tab-on-side resets to a default pane.

4. **Tab bar rendering**. Show tabs as a strip at the top of each pane?
   - *(a)* Small tab strip: `[1] Techno-80s   [2] House-90s*` (active marked).
   - *(b)* No strip — just trust the user to remember what's in each tab.
   **Proposed v1**: minimal strip showing `1 2 3 ...` with the active highlighted, plus a short title for each. Keeps the screen uncluttered but discoverable.

5. **Tab number on empty slot**. Pressing `3` when only tabs 1 and 2 exist:
   - *(a)* No-op.
   - *(b)* Auto-create an empty tab at that slot.
   **Proposed v1**: no-op. Creation is intentional (via Enter from `PlaylistsPane` or explicit `t`).

6. **Third side / center pane**. Joakim mentioned "useful when preparing playlists and sets" implying multiple workspaces. Does a center pane make sense?
   **Proposed v1**: no — left/right with 5+4 tabs gives 9 concurrent views, enough to cover prep + set-curation. Revisit if it feels cramped.

## Implementation scope

- **app.rs**: pane slots → Vec + index. Accessors. New `switch_active_pane`, `push_tab`, `close_tab`.
- **input.rs**: `1`-`9` dispatch. `x` to close. Interaction change on `PlaylistsPane` Enter.
- **ui.rs**: minimal tab strip. Active tab highlighting.
- **No changes needed**: pane implementations, Pane trait, library/playback IPC.

Estimated effort: one focused code-minion session; ~300-500 lines changed. All straightforward refactoring + new keymap.

## Once approved

When Joakim signs off the open questions above (or edits this doc), spin out a single code-minion task with the agreed behavior. The design keeps the Pane trait stable so existing work (SearchPane query grammar, quick-add, filters) lands transparently.
