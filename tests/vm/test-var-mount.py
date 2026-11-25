#!/usr/bin/env python3
"""Check the /var units generated for UBI and GPT images."""

import os
from pathlib import Path
from shlex import quote
import subprocess
from tempfile import TemporaryDirectory


image = Path(__file__).resolve().parents[2] / "mkosi/mkosi.extra"
generator_name = "systemd-fstab-generator"
wrapper = image / "etc/systemd/system-generators" / generator_name
assert os.access(wrapper, os.X_OK), f"Generator is missing or not executable: {wrapper}"
generator_dir = subprocess.check_output(
    ["systemd-path", "systemd-system-generator"], text=True
).strip()
generator = Path(generator_dir) / generator_name


def generate(work: Path, storage: str | None) -> Path:
    work.mkdir()
    bootctl = work / "pinna-bootctl"
    if storage is None:
        query = "exit 1"
    else:
        query = f'[ "$1" = storage-kind ] || exit 1\necho {quote(storage)}'
    bootctl.write_text(f"#!/bin/sh\n{query}\n")
    bootctl.chmod(0o755)

    # Map the image's absolute paths to the test fixture and the host generator.
    script = wrapper.read_text()
    paths = {
        "/usr/bin/pinna-bootctl": bootctl,
        "/etc/fstab.ubi": image / "etc/fstab.ubi",
        f"/usr/lib/systemd/system-generators/{generator_name}": generator,
    }
    for image_path, fixture_path in paths.items():
        script = script.replace(image_path, quote(str(fixture_path)))
    normal, early, late = (work / path for path in ("normal", "early", "late"))
    for directory in (normal, early, late):
        directory.mkdir()
    env = {**os.environ, "SYSTEMD_FSTAB": str(image / "etc/fstab")}
    subprocess.run(
        ["sh", "-c", script, generator_name, str(normal), str(early), str(late)],
        env=env,
        check=True,
    )
    assert (normal / "local-fs.target.requires/var.mount").is_symlink()
    return normal


with TemporaryDirectory(prefix="pinna-var-mount-") as temporary:
    work = Path(temporary)
    ubi = generate(work / "ubi", "ubi")
    mount = (ubi / "var.mount").read_text()
    assert "What=ubi0:userdata\n" in mount, mount
    assert "Type=ubifs\n" in mount, mount
    assert "ReadWriteOnly=yes\n" in mount, mount
    assert not (ubi / "var.mount.requires").exists()

    for name, storage in (("gpt", "gpt"), ("unavailable", None)):
        gpt = generate(work / name, storage)
        mount = (gpt / "var.mount").read_text()
        assert "What=/dev/disk/by-partlabel/userdata\n" in mount, mount
        assert "Type=ext4\n" in mount, mount
        assert list((gpt / "var.mount.requires").glob("systemd-makefs@*.service"))

print("MT8519 uses persistent UBIFS, and GPT boards and VMs keep ext4")
