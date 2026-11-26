#!/usr/bin/env python3
"""Check that the MT8519 AFE configures its back ends with their own parameters (patch 0236).

Amazon's driver read dai->rate, dai->channels and dai->sample_bits: a back
end's parameters after the machine driver's fixup. 6.12 names them
dai->symmetric_*. A DPCM back end shares its front end's runtime, so reading
substream->runtime gave eTDM2, fixed at 32 bits for the MAX98396, DL2's
16-bit samples.
"""
from pathlib import Path
import re
import sys


def main():
    linux = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux").resolve()
    afe = (linux / "sound/soc/mediatek/mt8519/mt8519-afe-pcm.c").read_text()

    # Amazon's own reads of another substream's runtime (be_substream,
    # paired_substream) stay; a DAI callback's own substream must not.
    own = re.findall(r"(?<![A-Za-z_])substream->runtime->(rate|channels|sample_bits)\b", afe)
    if own:
        raise SystemExit(f"Back ends read the front end's runtime: {sorted(set(own))}")

    for function in ("mt8519_afe_prepare_etdm_out", "mt8519_afe_prepare_etdm_in"):
        body = afe[afe.index(f"static int {function}("):]
        body = body[:body.index("\n}\n")]
        for field in ("rate", "channels", "sample_bits"):
            if f"dai->symmetric_{field}" not in body:
                raise SystemExit(f"{function} doesn't read dai->symmetric_{field}")
    print("ok: the eTDM ports take their own rate, channels and sample width")


if __name__ == "__main__":
    main()
