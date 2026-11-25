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


def generate(work: Path, compatible: bytes | None) -> Path:
    work.mkdir()
    device_tree = work / "compatible"
    if compatible is not None:
        device_tree.write_bytes(compatible)

    # Map the image's absolute paths to the test fixture and the host generator.
    script = wrapper.read_text()
    paths = {
        "/sys/firmware/devicetree/base/compatible": device_tree,
        "/etc/fstab.mt8519": image / "etc/fstab.mt8519",
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
    ubi = generate(work / "ubi", b"mediatek,cannoli\0mediatek,mt8519\0")
    mount = (ubi / "var.mount").read_text()
    assert "What=ubi0:userdata\n" in mount, mount
    assert "Type=ubifs\n" in mount, mount
    assert "ReadWriteOnly=yes\n" in mount, mount
    assert not (ubi / "var.mount.requires").exists()

    for name, compatible in (("gpt", b"mediatek,mt8163\0"), ("vm", None)):
        gpt = generate(work / name, compatible)
        mount = (gpt / "var.mount").read_text()
        assert "What=/dev/disk/by-partlabel/userdata\n" in mount, mount
        assert "Type=ext4\n" in mount, mount
        assert list((gpt / "var.mount.requires").glob("systemd-makefs@*.service"))

print("MT8519 uses persistent UBIFS, and GPT boards and VMs keep ext4")
