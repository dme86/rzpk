"""Exercise shell startup files in isolated homes; never edit the real home."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

SCRIPT = Path(__file__).with_name("setup-path.sh")

for shell in ("bash", "zsh", "fish"):
    executable = shutil.which(shell)
    if not executable:
        print(f"{shell}: not installed; skipping")
        continue
    with tempfile.TemporaryDirectory(prefix="rzpk-shell-") as tmp:
        root = Path(tmp)
        bin_dir = str(root / "space ' quote \\ and [glob]" / "bin")
        env = {
            **os.environ,
            "HOME": tmp,
            "SHELL": executable,
            "ZDOTDIR": str(root / "zsh"),
            "XDG_CONFIG_HOME": str(root / "xdg"),
        }
        # Existing content deliberately has no trailing newline.
        existing = {
            "bash": root / ".bash_login",
            "zsh": root / "zsh/.zshrc",
            "fish": root / "xdg/fish/conf.d/rzpk-path.fish",
        }[shell]
        existing.parent.mkdir(parents=True, exist_ok=True)
        existing.write_text("# existing user configuration")
        for _ in range(2):
            subprocess.run(
                ["sh", str(SCRIPT), bin_dir, executable], env=env,
                check=True, stdout=subprocess.DEVNULL,
            )
        assert existing.read_text().startswith("# existing user configuration\n")
        files = [file for file in root.rglob("*") if file.is_file()]
        assert len(files) == (2 if shell == "bash" else 1), files
        for file in files:
            assert file.read_text().count("# rzpk PATH:") == 1, file
            if shell == "fish":
                command = [executable, "-c", 'source "$argv[1]"; source "$argv[1]"; printf "%s\\n" $PATH', "--", str(file)]
            else:
                command = [executable, "-c", '. "$1"; . "$1"; printf "%s\\n" "$PATH"', "test", str(file)]
            result = subprocess.run(command, env=env, text=True, capture_output=True, check=True)
            paths = result.stdout.splitlines() if shell == "fish" else result.stdout.strip().split(":")
            assert paths.count(bin_dir) == 1, (shell, result.stdout, result.stderr)
        print(f"{shell}: existing config, quoting, repeated setup and sourcing OK")

with tempfile.TemporaryDirectory(prefix="rzpk-unsupported-") as tmp:
    env = {**os.environ, "HOME": tmp}
    result = subprocess.run(["sh", str(SCRIPT), "/opt/rzpk/bin", "/bin/unknown"], env=env, capture_output=True, text=True, check=True)
    assert "not supported" in result.stdout
    assert not list(Path(tmp).iterdir())
    for invalid in ("relative/bin", "/path:colon/bin", "/path\nnewline/bin"):
        result = subprocess.run(["sh", str(SCRIPT), invalid, "/bin/bash"], env=env, capture_output=True)
        assert result.returncode != 0
    assert not list(Path(tmp).iterdir())
    print("Unsupported shells and invalid paths: no startup files changed")
