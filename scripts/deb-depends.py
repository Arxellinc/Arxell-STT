#!/usr/bin/env python3
"""Generate Tauri .deb dependencies from the actual Linux release executable."""
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def main():
    binary, output = (Path(value).resolve() for value in sys.argv[1:])
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="arxell-deps-", dir=output.parent) as temporary:
        root = Path(temporary)
        debian = root / "debian"
        staged = debian / "arxell-speak/usr/bin" / binary.name
        staged.parent.mkdir(parents=True)
        shutil.copy2(binary, staged)
        (debian / "control").write_text(
            "Source: arxell-speak\nSection: utils\nPriority: optional\n"
            "Maintainer: Arxell\n\nPackage: arxell-speak\nArchitecture: any\n"
            "Description: Local dictation for Arxell\n",
            encoding="utf-8",
        )
        result = subprocess.run(
            ["dpkg-shlibdeps", "-O", f"-e{staged.relative_to(root)}"],
            cwd=root, check=True, text=True, stdout=subprocess.PIPE,
        )
    prefix = "shlibs:Depends="
    matches = [
        line[len(prefix):] for line in result.stdout.splitlines()
        if line.startswith(prefix)
    ]
    if len(matches) != 1 or not matches[0].strip():
        raise RuntimeError("dpkg-shlibdeps did not produce runtime dependencies")
    dependencies = [value.strip() for value in matches[0].split(",")]
    config = {"bundle": {"linux": {"deb": {"depends": dependencies}}}}
    output.write_text(json.dumps(config, indent=2) + "\n", encoding="utf-8")
    print(f"Debian runtime dependencies: {', '.join(dependencies)}")


if __name__ == "__main__":
    main()
