# Writes the reference features and voiceprint that pinna-speaker's tests compare against, from the
# first 1.5 s of pinna-wakeword's alexa-what-time.wav:
#   fbank-alexa-what-time.f32       kaldi-native-fbank's features, [150][80] little-endian f32, with
#                                   the options sherpa-onnx uses for speaker embedding models
#   voiceprint-alexa-what-time.json sherpa-onnx's embedding with CAM++ advanced, L2-normalised
#
# Run from the repository root (it fetches its packages once):
#   uv run --no-project --python 3.12 --with kaldi-native-fbank==1.23.0 --with sherpa-onnx==2.0.1 \
#     --with numpy python -I crates/pinna-speaker/tests/fixtures/make_reference.py <model.onnx>
import json
import pathlib
import sys
import wave

import kaldi_native_fbank as knf
import numpy as np
import sherpa_onnx

here = pathlib.Path(__file__).resolve().parent
root = here.parents[3]
wav = root / "crates/pinna-wakeword/tests/fixtures/alexa-what-time.wav"
model = sys.argv[1]

with wave.open(str(wav)) as w:
    assert (w.getnchannels(), w.getsampwidth(), w.getframerate()) == (1, 2, 16000)
    pcm = np.frombuffer(w.readframes(24000), dtype="<i2")
assert len(pcm) == 24000
samples = (pcm.astype(np.float32) / 32768.0).tolist()

opts = knf.FbankOptions()
opts.frame_opts.dither = 0
opts.frame_opts.snip_edges = False
opts.frame_opts.samp_freq = 16000
opts.mel_opts.num_bins = 80
opts.mel_opts.high_freq = -400
fbank = knf.OnlineFbank(opts)
fbank.accept_waveform(16000, samples)
fbank.input_finished()
feats = np.array([fbank.get_frame(i) for i in range(fbank.num_frames_ready)], dtype="<f4")
assert feats.shape == (150, 80), feats.shape
feats.tofile(here / "fbank-alexa-what-time.f32")

config = sherpa_onnx.SpeakerEmbeddingExtractorConfig(model=model, num_threads=1)
extractor = sherpa_onnx.SpeakerEmbeddingExtractor(config)
stream = extractor.create_stream()
stream.accept_waveform(sample_rate=16000, waveform=samples)
stream.input_finished()
emb = np.array(extractor.compute(stream), dtype=np.float64)
emb /= np.linalg.norm(emb)
(here / "voiceprint-alexa-what-time.json").write_text(json.dumps([float(v) for v in emb]) + "\n")
print(f"features {feats.shape}, voiceprint {emb.shape}")
