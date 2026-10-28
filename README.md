# rzpk

A deliberately small, keyboard-first file browser written in Rust.

Opens in the **current working directory** and shows a single, colorized file list with Nerd Font icons. Directories appear before files.

## Run

```sh
cargo run --release
```

Install with `cargo install --path .`, then run `rzpk` from any directory. For the icons, use a [Nerd Font](https://www.nerdfonts.com/) in your terminal.

## Keys

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move the highlight |
| `Type letters` | Jump to the first filename starting with the typed prefix, ignoring case |
| `Enter` | Open the highlighted directory |
| `Backspace` | Go to the parent directory |
| `Esc` / `Ctrl+C` | Exit |

Typing is incremental: `D` jumps to a `d` / `D` entry, then `o` narrows to `Do…`. After 900 ms of inactivity the prefix resets. There are no file operations, previews, tabs or command modes yet.
