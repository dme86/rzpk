# rzpk

A deliberately small, keyboard-first file browser written in Rust.

Opens in the **current working directory** and shows a single, colorized file list with Nerd Font icons. Directories appear before files. The default long view shows the **filename first**, followed by **Unix permissions, human-readable sizes, and local modification timestamps**. Dates fade from vivid green (newest) to muted slate (oldest), similar to `lsd`.

## Run

```sh
cargo run --release
```

Install with `cargo install --path .`, then run `rzpk` from any directory. For the icons, use a [Nerd Font](https://www.nerdfonts.com/) in your terminal.

On narrower terminals the size, then date, then permission columns disappear automatically, leaving the file names visible.

## Metadata alignment

The default is `right`: icon + filename, then permissions, size and date. Metadata columns line up across files, even with names of different lengths.

To use the previous, metadata-first layout, create `~/.config/rzpk/config.toml`:

```toml
metadata_position = "left"
```

Set it to `"right"` to switch back. The XDG_CONFIG_HOME environment variable is supported. No config file is required for the default. Restart rzpk after changing it.

## Vertical split

Type `:v` and press Enter to open two side-by-side views. The right pane starts in the same directory as the left, but each pane has its own folder, cursor, scroll position, incremental search and marked entries. The active pane is indicated by a green marker in its header; `rzpk [v]` appears in the global title. Press `Tab` to focus the other pane; all existing navigation and selection shortcuts apply only to the active pane.

Type `:v` again to close the split and retain the left pane. On a terminal narrower than 42 columns, only the active pane is rendered until there is enough space, but both panes keep their state.

## Modes

The modes are independent and can be combined:

- `:v` toggles a vertical split with two independent directory views, indicated by **[v]** in muted Gruvbox blue.
- `:p` toggles a preview of the highlighted item, indicated by **[p]** in Gruvbox yellow. It shows bounded UTF-8 text previews, directory contents, and symlink targets; binary files show a placeholder.
- `:t` toggles a **lazy tree** (Gruvbox green). Each pane has its own expanded folders. Directories start collapsed, `Enter` expands/collapses the highlighted folder, and selecting `… more entries (Enter)` reveals the next page.
- `:v` together with `:p` enables a **separate preview in each pane**, updated by that pane's cursor and navigation.
- Modes share a single neutral-gray label, e.g. `rzpk [v, p, t]`: only the letters are colored (blue, yellow and green).
- `:n` returns to the normal single-list view, removing all modes and keeping the left pane's directory.

`Esc` **never disables a mode**. It cancels an active command or clears marks in the active pane. `:v` / `:p` / `:t` turn off their respective modes without affecting the others. `:n` is the explicit reset.

Previews adapt to terminal size: beside the list in wide panes, below it in narrower ones, and temporarily hidden in extremely small windows. Only the first 24 KiB / 120 lines of a file, or up to 60 directory entries, are read. No preview files are accessed during rendering.

## Tree behavior

Tree mode never recursively loads an entire project. It initially shows up to **24 entries per directory**, sorted with folders first, and a `… more entries (Enter)` row when necessary. Each Enter on that row loads another page; expanding an actual directory reads only its immediate entries. A global **360-row** safety limit and **12-level** maximum depth prevent runaway expansions. Symbolic links to directories are not recursively followed.

The tree works with the existing search, up/down navigation and marks. The active pane alone responds to input; switching between panes retains each one's tree state. `Backspace` still navigates to the parent of the current root. In very narrow windows, the tree is clipped horizontally rather than triggering unbounded layout work. No directory scanning happens in the drawing loop.

## Keys

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move the highlight without removing marks |
| `Shift+↑` / `Shift+↓` | Extend or shrink a contiguous selection |
| `Shift+J` / `Shift+K` | Extend or shrink a selection down/up (also works as `J` / `K`) |
| `Shift+Space` | Add or remove the highlighted file or directory from the selection |
| `Space` | Same toggle, for terminals that cannot distinguish Shift+Space |
| `Type letters` | Jump to filenames by prefix, then substring (case-insensitive) |
| `Enter` | Open the highlighted directory |
| `Backspace` | Go to the parent directory |
| `Esc` | Clear all marked files and folders; cancel command entry if active (never exits) |
| `Ctrl+C` | Quit immediately |
| `:q` then `Enter` | Quit through the command prompt |
| `:v` then `Enter` | Toggle a vertical split, initially showing the same directory in both panes |
| `:p` then `Enter` | Toggle a preview for every visible pane |
| `:t` then `Enter` | Toggle lazy tree in every pane |
| `Enter` (tree mode) | Expand/collapse a directory or load another page |
| `:n` then `Enter` | Return to normal mode (no split, preview or tree) |
| `Tab` | Switch the active pane while split view is open |

Selected files and folders show a green checkmark and a distinct background. The number of marked items appears in the title. Shift+movement selects the starting entry and the ones traversed; reversing direction shrinks the active range. Existing marks remain when navigating normally, searching or adding non-adjacent entries. Entering a different directory clears the selection (selection is local to each directory view). Shift-modified letters never trigger the filename search. Pressing Esc in normal navigation clears every mark (including active range selections) without changing the focused entry.

Typing is incremental: `D` jumps to a `d` / `D` entry, then `o` narrows to `Do…`. If no filename starts with the typed text, rzpk searches for it anywhere in the name: typing `vim` can jump to `neovim`. A match at the beginning of a filename always takes priority over a substring match. After 900 ms of inactivity the typed query resets. The `:` command prompt currently supports `:q`, `:v`, `:p`, `:t` and `:n`, followed by Enter; Esc cancels the prompt while preserving any marks. There are no file operations or tabs yet.
