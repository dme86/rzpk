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
| `Tab` | Switch the active pane while split view is open |

Selected files and folders show a green checkmark and a distinct background. The number of marked items appears in the title. Shift+movement selects the starting entry and the ones traversed; reversing direction shrinks the active range. Existing marks remain when navigating normally, searching or adding non-adjacent entries. Entering a different directory clears the selection (selection is local to each directory view). Shift-modified letters never trigger the filename search. Pressing Esc in normal navigation clears every mark (including active range selections) without changing the focused entry.

Typing is incremental: `D` jumps to a `d` / `D` entry, then `o` narrows to `Do…`. If no filename starts with the typed text, rzpk searches for it anywhere in the name: typing `vim` can jump to `neovim`. A match at the beginning of a filename always takes priority over a substring match. After 900 ms of inactivity the typed query resets. The `:` command prompt currently supports `:q` and `:v`, followed by Enter; Esc cancels the prompt while preserving any marks. There are no file operations, previews, or tabs yet.
