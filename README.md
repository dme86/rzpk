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

## Remembered directory focus

Each pane remembers the **last focused file or folder** in recently visited directories during the current rzpk session. For example, focus `README.md` inside `src/`, press `←` to leave and `→` to re-enter: `README.md` is focused again, along with the previous scroll position. This also works when returning via directory jumps, with tree/preview enabled, and independently in both split panes. Going up still focuses the folder you just left in the parent listing.

Only the cursor focus is remembered; multi-file marks remain directory-scoped and do not automatically reappear. If the previously focused file was removed, rzpk falls back to the first available entry. Focus history is session-only, with an upper bound of 128 directories per pane, and is not saved to `config.toml`.

## Directory jumps

Save up to **nine directory jumps** (slots 1–9) to switch instantly to frequently used paths such as `~/.config`, `/etc`, or project directories. Jumps work in normal, split, tree and preview modes and always change **only the active pane**.

- `:jset 1` saves the current pane's directory to slot 1 (replace `1` with any digit 1–9).
- **Ctrl+G, then 1** jumps to slot 1 on macOS, Linux, and SSH terminals without special terminal configuration. Press the keys in sequence, not simultaneously; the footer prompts for the digit. `Esc` cancels and the prompt expires after 4 seconds.
- **Option+1** (macOS) / **Alt+1** (Linux) jumps to slot 1 when the terminal sends Option as Alt.
- **Cmd+1** (macOS) also works after remapping Alacritty's default Cmd+digit tab-switch shortcuts to terminal input (see below).
- `:j1` is a terminal-independent alternative (similarly `:j2` through `:j9`).
- `:jdel 1` deletes slot 1.

Jumps are stored in the existing `~/.config/rzpk/config.toml` (or `$XDG_CONFIG_HOME/rzpk/config.toml`) and survive restarts. rzpk preserves existing config comments when saving from `:jset` or `:jdel`. You can edit jump paths directly:

```toml
[jumps]
1 = "~/.config"
2 = "/etc"
3 = "~/projects/my-very-long-project-name"
```

When at least one slot is defined, a compact Gruvbox jump strip appears at the top of each pane, such as `↗ 1 ~/.config  2 /etc  3 …/my-project`. It abbreviates home directories, shortens long path names and shows `+N` if not all slots fit. The slot matching the current pane is highlighted. Missing or inaccessible jump targets produce an error without changing the active directory.

### Optional macOS Alacritty setup for single-chord Cmd/Option+digits

The portable **Ctrl+G, then 1–9** shortcut already works without special configuration. For the convenience of one-step Cmd/Option shortcuts, Alacritty **uses Cmd+1–9 to switch tabs by default**, and its default `window.option_as_alt` is `"None"`. rzpk cannot see keys intercepted by the terminal, so these one-step shortcuts need a one-time Alacritty configuration:

1. Open `~/.config/alacritty/alacritty.toml`.
2. Under your existing `[window]` table, set `option_as_alt = "Both"`. This makes Option+1–9 send Alt+digits. To preserve your right Option key for special characters, use `"OnlyLeft"` instead.
3. Add the nine Cmd bindings from [docs/alacritty-jumps.toml](docs/alacritty-jumps.toml) to your existing `[[keyboard.bindings]]` list. These send the same Alt+digits codes as Option+digits. **Merge the snippet** rather than pasting duplicate `[window]` tables. The bindings are limited to the terminal's alternate screen (`mode = "Alt"`), so normal shell tab switching remains available; they may affect other fullscreen TUI apps.
4. Reopen or reload Alacritty's config, then retry inside rzpk.

To diagnose another terminal or keyboard layout, run **`:keys`** inside rzpk. Press a shortcut: the pop-up displays the key code, modifier flags and whether it would activate a Jump. **Esc** exits diagnostics. If no key is reported, the terminal intercepted it; if an unexpected character appears (instead of `Char('1')` with `ALT`), change the terminal's Option/Alt configuration. No key-test action modifies the current directory.

## Parent navigation and editing

A pinned **`[..]`** entry appears at the top of each directory pane whenever a parent directory exists, even if the folder is empty or tree view is enabled. Press `↑` from the first real entry **or type `..`** to focus it, then `Enter` to return to the parent; `←` and `Backspace` always go back directly. Typing `..` uses the same incremental-search timeout as ordinary filenames; a single `.` can still match hidden files. The `[..]` entry is a **virtual navigation item**: it cannot be marked, searched, copied, moved, packed, or extracted, and the filesystem root does not show it.

`→` enters the highlighted folder in normal, split, or tree view. `Enter` on a folder enters it in normal mode, and in tree mode it expands/collapses the branch. **`Enter` on a regular file** starts your terminal editor using `$EDITOR` (falling back to `$VISUAL`, then `vi`). Editor commands with arguments, e.g. `EDITOR='nvim -u NONE'`, are supported without running a shell. rzpk temporarily releases the alternate screen/raw mode, waits for the editor to exit, restores the terminal, and refreshes the file listing. Search, marks, and navigation shortcuts are otherwise unchanged.

## SFTP remote connections

rzpk supports **SFTP over SSH** through the installed OpenSSH client on macOS and Linux. Add named profiles to the existing `~/.config/rzpk/config.toml`:

```toml
[connections.nas]
host = "nas"                 # SSH alias from ~/.ssh/config, or user@host
path = "/volume1/projects"   # initial remote folder

[connections.production]
host = "deploy@prod.example.org"
path = "/srv/apps"
```

Set up and verify SSH authentication before connecting (`ssh nas`). The SSH connection uses **strict known_hosts verification** and your existing SSH agent, keys, ProxyJump, port and identity rules from `~/.ssh/config`. rzpk does not store credentials or automatically trust unknown host keys. An SSH error appears in the footer.

Enter **`:connect nas`** to connect in the current pane, or to automatically open a right-hand SFTP pane when `:v` is not yet active. The connection is established in the background. Use `Tab` to change sides and **`Shift+S`** to rearrange them; the SFTP pane header identifies its connection. `:connections` lists configured profile names; `:disconnect` detaches the active SFTP pane and returns to your local working directory. `:refresh` reloads the active pane.

File navigation, `[..]`, incremental search and marks work similarly to local paths. **`Shift+C`** copies between local and remote; **`Shift+M`** moves (deleting the source only after a successful copy). Each operation requires confirmation and is processed in the **same FIFO queue** as local file operations. Recursive folders are supported up to 20,000 entries and depth 64; existing targets are never silently overwritten. Symlinks are intentionally not followed during remote transfers. Incomplete directories may remain on failure; individual failed file transfers clean up their partially written target.

The preview shows the first 24 KiB of remote text with Gruvbox syntax coloring and limited remote directory contents. Press **`Enter`** on a remote file to download a temporary working copy and open it in `$EDITOR`; after editing, changed content is queued for upload. To avoid accidental data loss, remote editing is limited to 8 MiB and checks size and modification metadata before saving. If a remote save fails, your edited temporary copy is preserved on disk and its path appears in the error. SFTP rename is not a true compare-and-swap; concurrent changes in the final instant remain possible.

**Current limitations:** Remote tree expansion, image rendering of remote files, remote-to-remote copying, direct archive operations on remote panes and FTP/FTPS are not yet supported. Directory listings and text previews currently wait for an SFTP response; the initial SSH connection and bulk file transfers run in the background. You can still navigate the local pane while remote file jobs run.

## Vertical split

Type `:v` and press Enter to open two side-by-side views. The right pane starts in the same directory as the left, but each pane has its own folder, cursor, scroll position, incremental search and marked entries. The active pane is indicated by a green marker in its header; `rzpk [v]` appears in the global title. Press `Tab` to focus the other pane; all existing navigation and selection shortcuts apply only to the active pane. Press **`Shift+S` (Swap)** or enter **`:swap`** to exchange the **left and right pane positions** without changing their contents, directory history, cursor, marks, scroll position, preview or tree state. The focused pane moves together with its contents; `Tab` continues to change only the focus. `Shift+S` outside split view shows a friendly hint to enable `:v` first.

Type `:v` again to close the split and retain the left pane. On a terminal narrower than 42 columns, only the active pane is rendered until there is enough space, but both panes keep their state.

## Modes

The modes are independent and can be combined:

- `:v` toggles a vertical split with two independent directory views, indicated by **[v]** in muted Gruvbox blue.
- `:p` toggles a preview of the highlighted item, indicated by **[p]** in Gruvbox yellow. It shows bounded UTF-8 text previews, directory contents, symlink targets and raster images. Unsupported binary formats retain a placeholder.
- `:t` toggles a **lazy tree** (Gruvbox green). The first two levels open automatically, with a budget of 6 auto-expanded branches per pane. `Enter` expands/collapses a highlighted folder, and `… more entries (Enter)` reveals additional entries.
- `:v` together with `:p` enables a **separate preview in each pane**, updated by that pane's cursor and navigation.
- Modes share a single neutral-gray label, e.g. `rzpk [v,p,t]`: only the letters are colored (blue, yellow and green).
- `:n` returns to the normal single-list view, removing all modes and keeping the left pane's directory.

`Esc` **never disables a mode**. It cancels an active command or clears marks in the active pane. `:v` / `:p` / `:t` turn off their respective modes without affecting the others. `:n` is the explicit reset.

Previews adapt to terminal size: beside the list in wide panes, below it in narrower ones, and temporarily hidden in extremely small windows. Text previews read at most 24 KiB / 120 lines; directory previews list up to 60 entries. No file I/O occurs during rendering.

### Image previews

When `:p` is enabled, highlighting an image displays it **inside the preview pane**. Supported formats are PNG, JPEG, GIF (first frame), WebP and BMP. The preview title identifies image format and dimensions. Images are fitted to the available pane, retaining aspect ratio; resizing and terminal-protocol encoding run on a worker thread so the TUI can continue redrawing. Normal view, `:v` split view, `:t` tree view and terminal resizing are supported.

rzpk detects terminal graphics support when entering its alternate screen using [ratatui-image](https://github.com/ratatui/ratatui-image): Kitty, iTerm2 or Sixel graphics on terminals that support them, otherwise a **truecolor Unicode half-block** image that works in Alacritty and other ordinary terminals. No additional external viewer or executable is needed. Image files are checked before decoding: a preview is rejected if the file exceeds **20 MiB** or its dimensions exceed **12 million pixels**, avoiding accidental large allocations. Invalid, unsupported or oversized images display an explanatory message without stopping the browser. SVG is not currently rasterized.

### Gruvbox syntax colors

In `:p` preview mode, recognized source files are tokenized with **syntect** and rendered using a small, built-in **Gruvbox Dark** theme: keywords red, types yellow, strings green, functions aqua, numbers purple, comments muted gray, and parameters blue. Syntax is detected from the file extension or a recognized shebang; the preview title shows the detected language. Common source formats such as Rust, Python, Bash, JSON and Markdown use their bundled grammars. Files such as TOML, Nix, Terraform/HCL and `.env` use a deliberately lightweight config tokenizer if a full grammar is not available.

The syntax set and palette are initialized once, and tokens are highlighted **only when the selected file changes**, never on every draw. Unknown extensions remain plain text, while binary files and unreadable files retain their existing placeholders. Preview size limits are unchanged. The pure Rust `syntect` regex backend is used to avoid an Oniguruma/C build dependency on macOS and Linux.

## Tree behavior

Tree mode **automatically expands the first two folder levels** when switched on, up to **6 automatically opened branches** per tree. That makes the hierarchy visible immediately without exploding into a huge recursive scan. Additional folders can be opened or closed with `Enter`.

Only **24 entries per directory** are loaded initially, with folders sorted first within each page and a `… more entries (Enter)` row when necessary. Press `Enter` on that row to reveal another page. A global **360-row** safety limit and **12-level** maximum depth prevent runaway expansion. Symbolic links to directories are not recursively followed.

The tree works with the existing search, up/down navigation and marks. The active pane alone responds to input; switching between panes retains each one's tree state. `Backspace` still navigates to the parent of the current root. In very narrow windows, the tree is clipped horizontally rather than triggering unbounded layout work. No directory scanning happens in the drawing loop.

## Copy and move

In split view (`:v`), the **active pane is the source** and the other pane's **current directory is the destination**, regardless of whether you are copying left-to-right or right-to-left. Mark items with Space / Shift+arrows, or leave everything unmarked to operate on the focused item.

- **Shift+C**: copy selected files and directories to the other pane
- **Shift+M**: move selected files and directories to the other pane

A confirmation dialog shows the source and destination. Press **Enter** to execute or **Esc** to cancel without losing marks. Existing targets never get silently overwritten: the operation is rejected before confirmation. Subdirectories and symlinks are copied without following symlinks, and selecting both a parent folder and a child transfers the parent only once.

The ordinary lowercase `c` and `m` still start or refine the incremental search. In tree mode, the destination is the *other pane's current root directory*, not a highlighted subtree. Copy and move operations run in a background worker. The permanent footer shows a live spinner, a progress bar for completed items and transferred data, plus a separate **waiting queue** strip when jobs are pending. You can confirm up to 64 additional jobs while another runs; they execute **sequentially, FIFO**. Each job captures its source and destination paths when confirmed, so later pane navigation does not change its meaning. rzpk stays usable while the queue runs, but quitting is blocked until all jobs have finished or been removed. A failed job does not stop later independent jobs. Earlier completed items from a failed multi-item transfer may remain in place.

## Footer and progress

rzpk reserves a persistent terminal row at the bottom for contextual help, success/error feedback, and active file jobs. When jobs are waiting, a **second row** appears with the queue length, the next jobs and a `:jobs` hint. The active row shows an animated spinner, elapsed time, processed bytes and a progress bar when a meaningful total is available. This footer works with `:v`, `:p` and `:t` without covering directory entries.

Confirm copy, move, pack or extract actions as usual during another job to **enqueue** them. `:jobs` or **Shift+Q** opens the queue window: `↑`/`↓` selects a waiting job, `Delete` (or `Backspace`) removes it without touching the active worker, and `Esc` closes the window. There is a limit of 64 waiting jobs. Queued paths are captured at confirmation; if the filesystem changes before a job starts (for example, the target already exists), that job fails safely and the next job proceeds. There is no concurrent file I/O from multiple rzpk workers, no overwrite of existing targets and no mid-transfer cancellation of the active job.

## Built-in help

Press **`?`** anywhere during normal navigation to open the scrollable help window. `Esc` (or `?`) closes it without changing your selection or active modes. Scroll the help with `↑` / `↓` or `PageUp` / `PageDown`. Keyboard shortcuts and colon commands are both registered centrally in `src/commands.rs`, and the help is generated directly from those tables. Add future commands there so the executable and its help stay synchronized.

## Archives

- **Shift+A** creates a `.tar.gz` from the marked files/folders, or the focused entry when nothing is marked, in the active pane's current directory.
- **Shift+E** detects the focused archive format and extracts it into a **new, dedicated subfolder** named after the archive.
- `:archive` and `:extract` work too, without needing function keys.
- Supported extraction: `.zip`, `.tar`, `.tar.gz` / `.tgz`, `.tar.xz`, `.tar.bz2` (format detected by file signature wherever possible). Other types such as `.7z` are not yet supported.

Both actions require confirmation with **Enter**; **Esc** cancels. Existing target files/folders are never overwritten. Extraction rejects unsafe paths, symlinks, special filesystem entries, or archives exceeding 20,000 entries / 2 GiB uncompressed. Failures remove the newly created extraction folder. Archive operations run in the same background worker as transfers. The footer shows an animated spinner, processed data and an item progress bar when the count is known (packing selected items or extracting ZIP entries). For streaming TAR extraction, the total member count is unknown until completion, so rzpk deliberately shows activity and processed bytes instead of an invented percentage. Running work cannot currently be canceled, but waiting archive jobs can be removed from the queue before they begin.

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
| `Shift+Q` / `:jobs` | View the queued file operations; Delete removes a waiting job |
| `Shift+M` | Move marks (or focused item) to the other split pane, after confirmation |
| `Space` | Same toggle, for terminals that cannot distinguish Shift+Space |
| `Type letters` | Jump to filenames by prefix, then substring (case-insensitive) |
| `Enter` | Open the highlighted folder; open a regular file in `$EDITOR` (or `$VISUAL`, then `vi`) |
| `←` | Go to the parent directory (also in tree view) |
| `→` | Enter the highlighted directory (also in tree view) |
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
