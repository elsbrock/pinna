// Whether this dashboard runs the recording (ADR 0030): the Overview names a
// recording that another dashboard runs, and doesn't name this one's own.
class Recorder {
  /** The Record audio page has a recording running, or is stopping one. */
  here = $state(false);
}

export const recorder = new Recorder();
