#!/usr/bin/env python3
"""Reference openWakeWord pipeline on onnxruntime, mirroring LibreEcho's
wake_engine_onnx.cpp block for block. Writes per-block scores, mel and
embedding tensors for a WAV to JSON so the tract implementation can be
compared against it.

    uv run --with onnxruntime --with numpy --with soundfile \
        ref_scores.py MODEL_DIR KEYWORD.onnx IN.wav OUT.json
"""
import json
import sys

import numpy as np
import onnxruntime as ort
import soundfile as sf

BLOCK, OVERLAP, MEL_FRAMES, MEL_BINS, FEAT_FRAMES, FEAT_BINS = 1280, 480, 76, 32, 16, 96


class Engine:
    def __init__(self, model_dir, keyword):
        opts = ort.SessionOptions()
        opts.intra_op_num_threads = 1
        opts.inter_op_num_threads = 1

        def load(name):
            return ort.InferenceSession(
                f"{model_dir}/{name}", opts, providers=["CPUExecutionProvider"]
            )

        self.mel, self.emb, self.cls = (
            load("melspectrogram.onnx"),
            load("embedding_model.onnx"),
            load(keyword),
        )
        self.mel_hist = np.ones((MEL_FRAMES, MEL_BINS), np.float32)
        self.feat_hist = np.zeros((FEAT_FRAMES, FEAT_BINS), np.float32)
        self.overlap = np.zeros(0, np.int16)
        self.feat_count = 0
        self.pred_count = 0
        for _ in range(FEAT_FRAMES):  # warm-up on silence, startup suppression off
            self.block(np.zeros(BLOCK, np.int16), False)
        self.mel_hist[:] = 1.0
        self.overlap = np.zeros(0, np.int16)
        self.pred_count = 0

    def block(self, blk, suppress):
        audio = np.concatenate([self.overlap, blk]).astype(np.float32)[None, :]
        mel = self.mel.run(["output"], {"input": audio})[0].reshape(-1, MEL_BINS)
        mel = mel / np.float32(10.0) + np.float32(2.0)
        n = mel.shape[0]
        if n >= MEL_FRAMES:
            self.mel_hist = mel[n - MEL_FRAMES :].copy()
        else:
            self.mel_hist = np.concatenate([self.mel_hist[n:], mel]).astype(np.float32)
        emb = self.emb.run(
            ["conv2d_19"], {"input_1": self.mel_hist.reshape(1, MEL_FRAMES, MEL_BINS, 1)}
        )[0].reshape(FEAT_BINS)
        self.feat_hist = np.concatenate([self.feat_hist[1:], emb[None]]).astype(np.float32)
        self.feat_count = min(self.feat_count + 1, FEAT_FRAMES)
        score = 0.0
        if self.feat_count >= FEAT_FRAMES:
            score = float(
                self.cls.run(["13"], {"onnx::Flatten_0": self.feat_hist[None]})[0].reshape(-1)[0]
            )
        self.overlap = blk[BLOCK - OVERLAP :].copy()
        if suppress:
            if self.pred_count < 5:
                score = 0.0
            self.pred_count += 1
        return max(0.0, min(1.0, score)), mel, emb


def main():
    model_dir, keyword, wav, out = sys.argv[1:5]
    data, rate = sf.read(wav, dtype="int16")
    assert rate == 16000 and data.ndim == 1, "need 16 kHz mono"
    eng = Engine(model_dir, keyword)
    blocks = []
    for i in range(len(data) // BLOCK):
        score, mel, emb = eng.block(data[i * BLOCK : (i + 1) * BLOCK], True)
        blocks.append(
            {"score": score, "mel": mel.reshape(-1).tolist(), "embedding": emb.tolist()}
        )
    with open(out, "w") as f:
        json.dump({"wav": wav, "blocks": blocks}, f)
    print(f"{wav}: {len(blocks)} blocks, max score {max(b['score'] for b in blocks):.4f}")


if __name__ == "__main__":
    main()
