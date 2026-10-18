// A compilation's wall clock is an input, including the timezone offset. Retain
// it for incremental typing in the workspace, not in engine checkpoints.
// Explicit Compile and opening a project capture a new input. Failed admissions
// must not replace the accepted document's clock.
export class CompilationClock {
  constructor(capture) {
    this.capture = capture;
    this.current = null;
  }
  forInput(data, retain) {
    if (
      retain &&
      this.current?.document === data.document &&
      (data.type !== "project" || data.resynchronize === true) &&
      data.refresh_clock !== true
    )
      return this.current;
    return Object.freeze({ document: data.document, ...this.capture() });
  }
  accept(clock) {
    this.current = clock;
  }
}
