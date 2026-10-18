#!/usr/bin/env python3
"""Build a Rust LaTeX format using the bundled, pinned TeX Live resources."""
import argparse
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def initialization_environment():
    """Keep initialization independent of the host's TeX installation."""
    assets = ROOT / "assets"
    fonts = str(assets / "fonts/assets")
    return dict(
        os.environ,
        TEXINPUTS=os.pathsep.join((".", str(assets / "latex-texmf/tex") + "//", str(assets / "tex"))),
        TEXMFCNF=str(assets),
        TFMFONTS=fonts,
        T1FONTS=fonts,
        ENCFONTS=fonts,
        TEXFONTMAPS=fonts,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/retex-rs2")
    parser.add_argument("--output", type=Path, default=ROOT / ".tools/formats/latex.rsfmt")
    parser.add_argument("--timeout", type=float, default=1200)
    args = parser.parse_args()
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    source = output.with_suffix(".init.tex")
    source.write_bytes(b"\\input latex.ltx\n")
    command = [str(args.binary.resolve()), "--exact-repro", "--ini", "--no-pdf",
               "--interaction", "nonstopmode", "--expansion-limit", "1000000000",
               "--dump-format", str(output), str(source)]
    with output.with_suffix(".stdout").open("wb") as stdout, output.with_suffix(".stderr").open("wb") as stderr:
        result = subprocess.run(command, cwd=output.parent, env=initialization_environment(),
                                stdout=stdout, stderr=stderr, timeout=args.timeout)
    if result.returncode or not output.is_file():
        raise SystemExit(f"Format initialization failed; see {output.with_suffix('.stdout')} and .stderr")
    print(f"Initialized format: {output} ({output.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
