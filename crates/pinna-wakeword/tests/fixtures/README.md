# Test fixtures

16 kHz mono 16-bit speech/music recordings copied from the previous firmware (MIT, see `THIRD-PARTY-NOTICES.md`):
`tests/voice-e2e/fixtures/` in the previous firmware's UI tree (`alexa-twice`, `alexa-what-time`,
`no-wake-word`, `music-clean`, `music-+06`, `music--12`, `music-bedonly`).

`music-clean.wav` is `alexa-what-time.wav` after a 2 s lead-in with no music
bed, so it contains one wake word; the other `music-*` files mix a music bed
into it at the SNR in the file name. `music-bedonly.wav` is the bed alone.
See the previous firmware's `music-snr.json` for the generation parameters.
