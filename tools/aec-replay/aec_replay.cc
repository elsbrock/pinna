// Replays the Echo Dot's echo canceller on recorded audio, on your computer.
//
//   aec_replay CAPTURE.wav REF.wav OUT.wav [--lead MS] [--warm] [--no-transient] [--linear]
//
// CAPTURE is the beamformer output (dot_beam) and REF the echo reference
// (dot_ref), both 16 kHz mono float WAV as pw-record writes them. OUT is what
// dot_mic_aec would carry. The canceller is set up the way PipeWire 1.4.2's
// module-echo-cancel sets up webrtc-audio-processing 1.3 with Pinna's
// arguments (mkosi/mkosi.extra/etc/pipewire/pipewire.conf.d/10-echo-dot.conf):
// AEC3 on, high-pass on, noise suppression, gain control and voice detection
// off, transient suppression on (PipeWire's default, which Pinna doesn't
// change). It processes 10 ms blocks: the reference first, then the capture
// with a stream delay of 0, as PipeWire does with a 160-sample quantum.
//
// --lead MS   shifts the reference MS milliseconds earlier against the capture
//             (negative: later). The recordings start at different times, so
//             the offset the canceller saw on the device is not in the files.
// --warm      runs the whole take once to let the canceller adapt, then again
//             for the output, like a canceller that has been running for hours.
// --no-transient  turns transient suppression off.
// --linear    writes the linear echo canceller's output instead, before AEC3's
//             suppressor (which PipeWire doesn't expose).
//
// Build on your computer (needs a C++17 compiler, pkg-config and the
// webrtc-audio-processing 1.3 development files, with abseil):
//
//   c++ -std=c++17 -O2 -o build/aec-replay/aec_replay tools/aec-replay/aec_replay.cc \
//       $(pkg-config --cflags --libs webrtc-audio-processing-1)
//
// The recordings come from pw-record on the satellite; crates/pinna-wakeword's
// `bargereplay` example replays the beamformer, the wake word's stream and the
// detector on them.

#include <array>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <new>
#include <string>
#include <vector>

#include <api/audio/echo_canceller3_config.h>
#include <api/audio/echo_control.h>
#include <modules/audio_processing/include/audio_processing.h>

namespace webrtc {
// AEC3 itself. The library exports its constructor but the package doesn't
// install its header; it is only ever constructed in place below.
class EchoCanceller3 {
 public:
  EchoCanceller3(const EchoCanceller3Config& config, int sample_rate_hz, size_t num_render_channels,
                 size_t num_capture_channels);
};
}  // namespace webrtc

namespace {

// AEC3 with its default configuration plus the linear output, which the
// audio processing module only fills when AEC3 was built to export it.
class LinearAec3 : public webrtc::EchoControl {
 public:
  LinearAec3(int rate, int render, int capture) {
    webrtc::EchoCanceller3Config config;
    config.filter.export_linear_aec_output = true;
    storage_ = static_cast<unsigned char*>(std::aligned_alloc(64, 1 << 22));
    new (storage_) webrtc::EchoCanceller3(config, rate, render, capture);
    // EchoCanceller3 derives from EchoControl alone, so the base is at offset 0.
    aec_ = reinterpret_cast<webrtc::EchoControl*>(storage_);
  }
  ~LinearAec3() override {
    aec_->~EchoControl();
    std::free(storage_);
  }
  void AnalyzeRender(webrtc::AudioBuffer* b) override { aec_->AnalyzeRender(b); }
  void AnalyzeCapture(webrtc::AudioBuffer* b) override { aec_->AnalyzeCapture(b); }
  void ProcessCapture(webrtc::AudioBuffer* b, bool change) override { aec_->ProcessCapture(b, change); }
  void ProcessCapture(webrtc::AudioBuffer* b, webrtc::AudioBuffer* linear, bool change) override {
    aec_->ProcessCapture(b, linear, change);
  }
  Metrics GetMetrics() const override { return aec_->GetMetrics(); }
  void SetAudioBufferDelay(int ms) override { aec_->SetAudioBufferDelay(ms); }
  bool ActiveProcessing() const override { return aec_->ActiveProcessing(); }

 private:
  unsigned char* storage_;
  webrtc::EchoControl* aec_;
};

class LinearAec3Factory : public webrtc::EchoControlFactory {
 public:
  std::unique_ptr<webrtc::EchoControl> Create(int rate, int render, int capture) override {
    return std::make_unique<LinearAec3>(rate, render, capture);
  }
};

constexpr int kRate = 16000;
constexpr int kBlock = kRate / 100;

[[noreturn]] void die(const std::string& msg) {
  std::fprintf(stderr, "aec_replay: %s\n", msg.c_str());
  std::exit(1);
}

uint32_t u32(const unsigned char* p) { return p[0] | p[1] << 8 | p[2] << 16 | uint32_t(p[3]) << 24; }
uint16_t u16(const unsigned char* p) { return p[0] | p[1] << 8; }

// Reads a mono 16 kHz WAV of 32-bit floats (format 3, or WAVE_FORMAT_EXTENSIBLE).
std::vector<float> read_wav(const char* path) {
  FILE* f = std::fopen(path, "rb");
  if (!f) die(std::string("cannot open ") + path);
  std::vector<unsigned char> b;
  unsigned char buf[65536];
  size_t n;
  while ((n = std::fread(buf, 1, sizeof buf, f)) > 0) b.insert(b.end(), buf, buf + n);
  std::fclose(f);
  if (b.size() < 12 || std::memcmp(b.data(), "RIFF", 4) || std::memcmp(b.data() + 8, "WAVE", 4))
    die(std::string(path) + " is not a WAV file");
  int channels = 0, bits = 0, format = 0;
  uint32_t rate = 0;
  for (size_t i = 12; i + 8 <= b.size();) {
    uint32_t len = u32(&b[i + 4]);
    const unsigned char* d = &b[i + 8];
    if (!std::memcmp(&b[i], "fmt ", 4)) {
      format = u16(d);
      channels = u16(d + 2);
      rate = u32(d + 4);
      bits = u16(d + 14);
      if (format == 0xFFFE && len >= 26) format = u16(d + 24);
    } else if (!std::memcmp(&b[i], "data", 4)) {
      if (format != 3 || bits != 32 || channels != 1 || rate != kRate)
        die(std::string(path) + ": need 16 kHz mono 32-bit float");
      // pw-record leaves the length at its maximum when it is stopped; take what is there.
      size_t avail = std::min<size_t>(len, b.size() - (i + 8)) / 4;
      std::vector<float> out(avail);
      std::memcpy(out.data(), d, avail * 4);
      return out;
    }
    i += 8 + len + (len & 1);
  }
  die(std::string(path) + ": no data chunk");
}

void put32(FILE* f, uint32_t v) { unsigned char b[4] = {uint8_t(v), uint8_t(v >> 8), uint8_t(v >> 16), uint8_t(v >> 24)}; std::fwrite(b, 1, 4, f); }
void put16(FILE* f, uint16_t v) { unsigned char b[2] = {uint8_t(v), uint8_t(v >> 8)}; std::fwrite(b, 1, 2, f); }

void write_wav(const char* path, const std::vector<float>& s) {
  FILE* f = std::fopen(path, "wb");
  if (!f) die(std::string("cannot create ") + path);
  uint32_t bytes = s.size() * 4;
  std::fwrite("RIFF", 1, 4, f); put32(f, 36 + bytes); std::fwrite("WAVE", 1, 4, f);
  std::fwrite("fmt ", 1, 4, f); put32(f, 16); put16(f, 3); put16(f, 1);
  put32(f, kRate); put32(f, kRate * 4); put16(f, 4); put16(f, 32);
  std::fwrite("data", 1, 4, f); put32(f, bytes);
  std::fwrite(s.data(), 4, s.size(), f);
  if (std::fclose(f)) die(std::string("cannot write ") + path);
}

std::unique_ptr<webrtc::AudioProcessing> make_apm(bool transient, bool linear) {
  webrtc::AudioProcessing::Config config;
  config.echo_canceller.enabled = true;
  config.echo_canceller.export_linear_aec_output = linear;
  config.pipeline.multi_channel_capture = false;
  config.pipeline.multi_channel_render = false;
  config.gain_controller1.enabled = false;
  config.gain_controller1.mode = webrtc::AudioProcessing::Config::GainController1::Mode::kAdaptiveDigital;
  config.gain_controller1.analog_level_minimum = 0;
  config.gain_controller1.analog_level_maximum = 255;
  config.gain_controller2.enabled = false;
  config.high_pass_filter.enabled = true;
  config.noise_suppression.enabled = false;
  config.noise_suppression.level = webrtc::AudioProcessing::Config::NoiseSuppression::kHigh;
  config.transient_suppression.enabled = transient;
  config.voice_detection.enabled = false;

  webrtc::AudioProcessingBuilder builder;
  if (linear) builder.SetEchoControlFactory(std::make_unique<LinearAec3Factory>());
  auto apm = std::unique_ptr<webrtc::AudioProcessing>(builder.Create());
  apm->ApplyConfig(config);
  webrtc::StreamConfig mono(kRate, 1, false);
  webrtc::ProcessingConfig pconfig = {{mono, mono, mono, mono}};
  if (apm->Initialize(pconfig) != webrtc::AudioProcessing::kNoError) die("cannot initialise the audio processing module");
  return apm;
}

void run(webrtc::AudioProcessing& apm, const std::vector<float>& cap, const std::vector<float>& ref,
         long lead, bool linear, std::vector<float>& out) {
  webrtc::StreamConfig mono(kRate, 1, false);
  float play[kBlock], rec[kBlock], res[kBlock];
  out.assign(cap.size() / kBlock * kBlock, 0.0f);
  for (size_t at = 0; at + kBlock <= cap.size(); at += kBlock) {
    for (int i = 0; i < kBlock; i++) {
      long r = long(at) + i + lead;
      play[i] = r >= 0 && r < long(ref.size()) ? ref[r] : 0.0f;
      rec[i] = cap[at + i];
    }
    float* p = play;
    float* c = rec;
    float* o = res;
    if (apm.ProcessReverseStream(&p, mono, mono, &p) != webrtc::AudioProcessing::kNoError) die("reverse stream failed");
    apm.set_stream_delay_ms(0);
    if (apm.ProcessStream(&c, mono, mono, &o) != webrtc::AudioProcessing::kNoError) die("stream failed");
    if (linear) {
      std::array<float, kBlock> lin;
      if (!apm.GetLinearAecOutput(rtc::ArrayView<std::array<float, kBlock>>(&lin, 1))) die("no linear output");
      // The linear output is on the 16-bit scale; the capture side is not.
      for (int i = 0; i < kBlock; i++) out[at + i] = lin[i] / 32768.0f;
    } else {
      std::memcpy(&out[at], res, sizeof res);
    }
  }
}

}  // namespace

int main(int argc, char** argv) {
  std::vector<const char*> files;
  double lead_ms = 0;
  bool warm = false, transient = true, linear = false;
  for (int i = 1; i < argc; i++) {
    std::string a = argv[i];
    if (a == "--lead" && i + 1 < argc) lead_ms = std::atof(argv[++i]);
    else if (a == "--warm") warm = true;
    else if (a == "--no-transient") transient = false;
    else if (a == "--linear") linear = true;
    else if (!a.empty() && a[0] == '-') die("unknown option " + a);
    else files.push_back(argv[i]);
  }
  if (files.size() != 3) die("usage: aec_replay CAPTURE.wav REF.wav OUT.wav [--lead MS] [--warm] [--no-transient] [--linear]");
  auto cap = read_wav(files[0]);
  auto ref = read_wav(files[1]);
  long lead = std::lround(lead_ms * kRate / 1000);
  auto apm = make_apm(transient, linear);
  std::vector<float> out;
  if (warm) run(*apm, cap, ref, lead, linear, out);
  run(*apm, cap, ref, lead, linear, out);
  write_wav(files[2], out);
  return 0;
}
