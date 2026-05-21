# New Agent Dialog — Redesign Plan

## Overview

Redesign the `CreateAgentDialog` to match the opencode visual style:
- Brighter dialog background to distinguish it from the rest of the UI (`BG1`)
- Solid rectangular buttons (no square-bracket simulation)
- Clean input fields (no brackets, cursor + tip text as the only visual indicator)
- Directory navigation via a vertical suggestion list instead of Tab completion
- Agent type selection via a vertical list instead of horizontal radio buttons
- Tab switches fields only; Up/Down navigates within fields

---

## Files Changed

- `src/app.rs` — state struct + key handler logic
- `src/ui/create_agent.rs` — rendering

---

## State Changes (`src/app.rs`)

### `CreateAgentState` struct

**Removed:**
- `tab_matches: Vec<String>`
- `tab_idx: usize`

**Added:**
- `dir_matches: Vec<String>` — up to 10 alphabetically sorted subdirs matching current `directory` input
- `dir_selected_idx: usize` — which suggestion is highlighted in the list

**Initialisation at dialog open:**
- `directory` pre-filled with `std::env::current_dir()` result
- `refresh_dir_matches()` called immediately to populate the list

### Removed method
- `handle_tab()` — replaced by `refresh_dir_matches()`

### New method: `refresh_dir_matches()`
Reads `self.directory`, treats it as a base path. Lists subdirectories of that path
(if it is a valid directory) or subdirs of the parent filtered by the last component
(if it's a partial path). Sorts alphabetically, caps at 10.

---

## Key Handler Changes (`handle_create_key`)

| Key | Old behaviour | New behaviour |
|-----|--------------|---------------|
| `Tab` | Tab-complete directory | Cycle focus: Name → Directory → AgentType → Name |
| `↑` / `↓` | Switch focused field | Navigate within-field (dir suggestions or agent list) |
| `←` / `→` | Cycle agent type when AgentType focused | Removed |
| `Enter` on Directory focus | — | Commit highlighted suggestion to `directory`, refresh matches |
| `Enter` on Name/AgentType | Validate + create | Unchanged |
| `Char(c)` on Directory | Push char, clear tab cache | Push char, call `refresh_dir_matches()`, reset `dir_selected_idx` |
| `Backspace` on Directory | Pop char, clear tab cache | Pop char, call `refresh_dir_matches()`, reset `dir_selected_idx` |

---

## Visual / Layout Changes (`src/ui/create_agent.rs`)

### Modal background
- After `Clear`, fill entire modal area with `BG1` (Rgb 60, 56, 54) background before rendering the border block.

### Input fields — new `render_field_row`
No brackets. Three visual states:

| State | Label | Value area |
|-------|-------|------------|
| Focused | `FG` | value in `FG` bold + `▌` cursor in `YELLOW` |
| Unfocused, empty | `GRAY` | placeholder tip text in `BG2` |
| Unfocused, has value | `GRAY` | value in `GRAY` |

### Directory suggestion list
Rendered directly below the directory input. Up to 10 rows:
- Highlighted item: `YELLOW` bold
- Non-highlighted: `GRAY`
- Input text does **not** change when navigating — only `Enter` commits the selection

### Agent type — vertical list
One agent type per row, replacing the horizontal radio selector:
- Selected + focused: `◉ label` in `GREEN` bold
- Selected + unfocused: `◉ label` in `GREEN`
- Non-selected: `○ label` in `GRAY`

### Buttons row — solid rectangles
```
 Launch    Cancel
```
- `" Launch "` — bg `ORANGE`, fg `BG` (dark text on colour)
- `" Cancel "` — bg `BG2`, fg `FG`
- Separated by two spaces; no brackets

### Dynamic modal height
```
height = 4              (blank + Name + blank + Directory)
       + dir_rows       (0–10, current suggestion count)
       + 2              (blank + "Agent:" label)
       + agent_rows     (number of available agent types)
       + 2              (blank + buttons row)
       + error_row      (0 or 1)
       + 2              (top/bottom border padding)
```

### Layout (vertical)
```
 ┌─ New Agent ──────────────────────────────────────┐
 │                                                  │
 │  Name      name value▌                           │
 │                                                  │
 │  Directory  /home/user/projects                  │
 │               foo/                               │  ← suggestions
 │               bar/                               │
 │               baz/                               │
 │                                                  │
 │  Agent                                           │
 │    ◉ opencode                                    │
 │    ○ claude                                      │
 │                                                  │
 │  ✗ error message (if any)                        │
 │   Launch    Cancel                               │
 │                                                  │
 └──────────────────────────────────────────────────┘
```
