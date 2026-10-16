"""Render the approved usage-meter icon and publish its canonical exports."""

import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ICON_DIR = ROOT / "src" / "icons"
REDESIGN_DIR = ICON_DIR / "redesign"
GITHUB_ICON = ROOT / ".github" / "quota-usage-monitor-icon.png"


def get_powershell() -> str:
    for command in ("pwsh", "powershell"):
        executable = shutil.which(command)
        if executable:
            return executable
    raise RuntimeError("PowerShell is required to run src/icons/redesign/render.ps1")


def main() -> None:
    renderer = REDESIGN_DIR / "render.ps1"
    subprocess.run(
        [get_powershell(), "-NoLogo", "-NoProfile", "-File", str(renderer)],
        cwd=REDESIGN_DIR,
        check=True,
    )

    for size in (16, 32, 48, 256):
        (ICON_DIR / f"{size}x{size}.png").write_bytes(
            (REDESIGN_DIR / f"quota-usage-monitor-{size}.png").read_bytes()
        )
    (ICON_DIR / "icon.ico").write_bytes(
        (REDESIGN_DIR / "quota-usage-monitor.ico").read_bytes()
    )
    GITHUB_ICON.parent.mkdir(parents=True, exist_ok=True)
    GITHUB_ICON.write_bytes((REDESIGN_DIR / "quota-usage-monitor-256.png").read_bytes())

    svg_source = (REDESIGN_DIR / "quota-usage-monitor.svg").read_text(encoding="utf-8")
    dimensions = 'width="1024" height="1024"'
    if svg_source.count(dimensions) != 1:
        raise RuntimeError("Unexpected root dimensions in the approved meter SVG")
    for size in (16, 32, 48, 256):
        svg = svg_source.replace(
            dimensions, f'width="{size}" height="{size}"', 1
        )
        (ICON_DIR / f"{size}.svg").write_text(svg, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
