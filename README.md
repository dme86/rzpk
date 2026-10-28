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

## Keys

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move the highlight |
| `Type letters` | Jump to filenames by prefix, then substring (case-insensitive) |
| `Enter` | Open the highlighted directory |
| `Backspace` | Go to the parent directory |
| `Esc` / `Ctrl+C` | Exit |

Typing is incremental: `D` jumps to a `d` / `D` entry, then `o` narrows to `Do…`. If no filename starts with the typed text, rzpk searches for it anywhere in the name: typing `vim` can jump to `neovim`. A match at the beginning of a filename always takes priority over a substring match. After 900 ms of inactivity the typed query resets. There are no file operations, previews, tabs or command modes yet.
