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
- `:t` toggles a **lazy tree** (Gruvbox green). The first two levels open automatically, with a budget of 6 auto-expanded branches per pane. `Enter` expands/collapses a highlighted folder, and `… more entries (Enter)` reveals additional entries.
- `:v` together with `:p` enables a **separate preview in each pane**, updated by that pane's cursor and navigation.
- Modes share a single neutral-gray label, e.g. `rzpk [v, p, t]`: only the letters are colored (blue, yellow and green).
- `:n` returns to the normal single-list view, removing all modes and keeping the left pane's directory.

`Esc` **never disables a mode**. It cancels an active command or clears marks in the active pane. `:v` / `:p` / `:t` turn off their respective modes without affecting the others. `:n` is the explicit reset.

Previews adapt to terminal size: beside the list in wide panes, below it in narrower ones, and temporarily hidden in extremely small windows. Only the first 24 KiB / 120 lines of a file, or up to 60 directory entries, are read. No preview files are accessed during rendering.

## Tree behavior

Tree mode **automatically expands the first two folder levels** when switched on, up to **6 automatically opened branches** per tree. That makes the hierarchy visible immediately without exploding into a huge recursive scan. Additional folders can be opened or closed with `Enter`.

Only **24 entries per directory** are loaded initially, with folders sorted first within each page and a `… more entries (Enter)` row when necessary. Press `Enter` on that row to reveal another page. A global **360-row** safety limit and **12-level** maximum depth prevent runaway expansion. Symbolic links to directories are not recursively followed.

The tree works with the existing search, up/down navigation and marks. The active pane alone responds to input; switching between panes retains each one's tree state. `Backspace` still navigates to the parent of the current root. In very narrow windows, the tree is clipped horizontally rather than triggering unbounded layout work. No directory scanning happens in the drawing loop.

## Copy and move

In split view (`:v`), the **active pane is the source** and the other pane's **current directory is the destination**, regardless of whether you are copying left-to-right or right-to-left. Mark items with Space / Shift+arrows, or leave everything unmarked to operate on the focused item.

- **Shift+C**: copy selected files and directories to the other pane
- **Shift+M**: move selected files and directories to the other pane

A confirmation dialog shows the source and destination. Press **Enter** to execute or **Esc** to cancel without losing marks. Existing targets never get silently overwritten: the operation is rejected before confirmation. Subdirectories and symlinks are copied without following symlinks, and selecting both a parent folder and a child transfers the parent only once.

The ordinary lowercase `c` and `m` still start or refine the incremental search. In tree mode, the destination is the *other pane's current root directory*, not a highlighted subtree. Copy and move operations run in a background worker. A permanent footer shows a live spinner, a progress bar for completed source items and transferred data. The file browser stays usable while an operation is in progress. Only one filesystem job can run at a time; quitting is deferred until it finishes. A failed multi-item operation can leave earlier completed items in place and reports an error.

## Footer and progress

rzpk reserves a single terminal row at the bottom for contextual help, success/error feedback, and background file jobs. During copy, move, archive creation, or extraction it displays an animated activity indicator, elapsed time, processed data and a progress bar when a meaningful total is available. The footer remains visible with `:v`, `:p`, and `:t` enabled, without covering directory entries. Navigation, selection and search still work during a job, but a second transfer or archive operation is rejected until the first completes.

## Built-in help

Press **`?`** anywhere during normal navigation to open the scrollable help window. `Esc` (or `?`) closes it without changing your selection or active modes. Scroll the help with `↑` / `↓` or `PageUp` / `PageDown`. Keyboard shortcuts and colon commands are both registered centrally in `src/commands.rs`, and the help is generated directly from those tables. Add future commands there so the executable and its help stay synchronized.

## Archives

- **Shift+A** creates a `.tar.gz` from the marked files/folders, or the focused entry when nothing is marked, in the active pane's current directory.
- **Shift+E** detects the focused archive format and extracts it into a **new, dedicated subfolder** named after the archive.
- `:archive` and `:extract` work too, without needing function keys.
- Supported extraction: `.zip`, `.tar`, `.tar.gz` / `.tgz`, `.tar.xz`, `.tar.bz2` (format detected by file signature wherever possible). Other types such as `.7z` are not yet supported.

Both actions require confirmation with **Enter**; **Esc** cancels. Existing target files/folders are never overwritten. Extraction rejects unsafe paths, symlinks, special filesystem entries, or archives exceeding 20,000 entries / 2 GiB uncompressed. Failures remove the newly created extraction folder. Archive operations run in the same background worker as transfers. The footer shows an animated spinner, processed data and an item progress bar when the count is known (packing selected items or extracting ZIP entries). For streaming TAR extraction, the total member count is unknown until completion, so rzpk deliberately shows activity and processed bytes instead of an invented percentage. The worker cannot be canceled after confirmation; finish the operation before quitting rzpk.

## Keys

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move the highlight without removing marks |
| `Shift+↑` / `Shift+↓` | Extend or shrink a contiguous selection |
| `Shift+J` / `Shift+K` | Extend or shrink a selection down/up (also works as `J` / `K`) |
| `Shift+Space` | Add or remove the highlighted file or directory from the selection |
| `Shift+C` | Copy marks (or focused item) to the other split pane, after confirmation |
| `Shift+A` | Create .tar.gz from marks (or the focused entry) |
| `Shift+E` | Extract selected archive into a new folder |
| `?` | Show the scrollable help popup; Esc closes it |
| `Shift+M` | Move marks (or focused item) to the other split pane, after confirmation |
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

Typing is incremental: `D` jumps to a `d` / `D` entry, then `o` narrows to `Do…`. If no filename starts with the typed text, rzpk searches for it anywhere in the name: typing `vim` can jump to `neovim`. A match at the beginning of a filename always takes priority over a substring match. After 900 ms of inactivity the typed query resets. The `:` command prompt currently supports `:q`, `:v`, `:p`, `:t`, `:n`, `:cp`, `:mv`, `:archive` and `:extract`, followed by Enter; Esc cancels the prompt while preserving any marks. There are no tabs yet.
