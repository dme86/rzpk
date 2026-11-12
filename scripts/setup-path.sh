#!/bin/sh
# Called explicitly by make install / setup-path, never by rzpk itself.
set -eu
bin_dir=${1:?Usage: setup-path.sh ABSOLUTE_BIN_DIR [SHELL]}
login_shell=${2:-${SHELL:-}}
case "$bin_dir" in
    /*) ;;
    *) printf '%s\n' 'PATH setup requires an absolute PREFIX.' >&2; exit 1 ;;
esac
case "$bin_dir" in
    *:*) printf '%s\n' 'PATH setup does not support colons in PREFIX.' >&2; exit 1 ;;
    *'
'*) printf '%s\n' 'PATH setup does not support newlines in PREFIX.' >&2; exit 1 ;;
esac

# Quote literal paths for sh/zsh/bash; fish additionally treats backslashes specially.
quoted=$(printf '%s' "$bin_dir" | sed "s/'/'\\\\''/g")
fish_quoted=$(printf '%s' "$bin_dir" | sed "s/\\\\/\\\\\\\\/g; s/'/\\\\'/g")
marker="# rzpk PATH: $bin_dir"
append_path() {
    file=$1
    line=$2
    mkdir -p "$(dirname "$file")"
    if [ -f "$file" ] && grep -Fqx -- "$marker" "$file"; then
        printf 'PATH already configured in %s\n' "$file"
        return
    fi
    # Start on a new line even if the existing file has no trailing newline.
    printf '\n%s\n%s\n' "$marker" "$line" >> "$file"
    printf 'Added PATH setup to %s\n' "$file"
}

case "${login_shell##*/}" in
    zsh)
        line="case \":\$PATH:\" in *:'$quoted':*) ;; *) export PATH='$quoted':\"\$PATH\" ;; esac"
        append_path "${ZDOTDIR:-$HOME}/.zshrc" "$line"
        ;;
    bash)
        line="case \":\$PATH:\" in *:'$quoted':*) ;; *) export PATH='$quoted':\"\$PATH\" ;; esac"
        append_path "$HOME/.bashrc" "$line"
        # Bash login shells read only the first of these existing files.
        if [ -f "$HOME/.bash_profile" ]; then profile=$HOME/.bash_profile
        elif [ -f "$HOME/.bash_login" ]; then profile=$HOME/.bash_login
        elif [ -f "$HOME/.profile" ]; then profile=$HOME/.profile
        else profile=$HOME/.bash_profile
        fi
        append_path "$profile" "$line"
        ;;
    fish)
        line="if not contains -- '$fish_quoted' \$PATH; set -gx PATH '$fish_quoted' \$PATH; end"
        append_path "${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/rzpk-path.fish" "$line"
        ;;
    *)
        printf 'Shell "%s" is not supported for automatic PATH setup.\n' "$login_shell"
        printf "Add this directory to your shell's PATH: %s\n" "$bin_dir"
        exit 0
        ;;
esac
printf '%s\n' 'Open a new terminal to use rzpk. The current shell is unchanged.'
