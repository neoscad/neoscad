// `viewer.onFrame(callback)`: a requestAnimationFrame loop for a page that
// moves scenes (`viewer.addScene`), which runs only while the canvas is on
// screen and the tab is visible. A hero animation left running in a
// background tab, or scrolled far out of view, would keep the GPU and the
// main thread busy for nothing, and on a phone drain the battery.
//
// The callback gets the loop's own clock: milliseconds of running time,
// which stands still while the loop is paused (by the page or by being off
// screen), so an animation driven by it resumes where it stopped instead of
// jumping ahead by however long the tab was hidden. A long gap between two
// frames (a busy main thread, a debugger) counts as at most `MAX_STEP_MS`
// for the same reason.

export const MAX_STEP_MS = 100;

export class FrameLoop {
  /// `env` (tests replace it): requestAnimationFrame,
  /// cancelAnimationFrame, document, IntersectionObserver.
  constructor(element, callback, env = globalThis) {
    if (typeof callback !== "function") throw new TypeError("NeoSCAD: onFrame needs a function");
    this.callback = callback;
    this.env = env;
    this.elapsed = 0;
    this.last = null;
    this.handle = 0;
    this.userPaused = false;
    this.visible = true;
    this.stopped = false;
    this.tick = (now) => this.frame(now);
    this.onVisibility = () => this.update();
    env.document?.addEventListener?.("visibilitychange", this.onVisibility);
    if (element && typeof env.IntersectionObserver === "function") {
      this.observer = new env.IntersectionObserver((entries) => {
        const e = entries[entries.length - 1];
        if (!e) return;
        this.visible = e.isIntersecting;
        this.update();
      });
      this.observer.observe(element);
    }
    this.update();
  }

  /// Whether frames are being delivered now.
  get running() {
    return !this.stopped && !this.userPaused && this.visible && !this.env.document?.hidden;
  }

  /// Whether the page paused the loop (not whether it is off screen).
  get paused() {
    return this.userPaused;
  }

  /// Milliseconds of running time so far.
  get time() {
    return this.elapsed;
  }

  pause() {
    this.userPaused = true;
    this.update();
  }

  play() {
    this.userPaused = false;
    this.update();
  }

  /// End the loop for good.
  stop() {
    if (this.stopped) return;
    this.stopped = true;
    this.update();
    this.env.document?.removeEventListener?.("visibilitychange", this.onVisibility);
    this.observer?.disconnect();
    this.observer = null;
  }

  update() {
    if (this.running) {
      if (!this.handle) this.handle = this.env.requestAnimationFrame(this.tick);
    } else {
      if (this.handle) this.env.cancelAnimationFrame(this.handle);
      this.handle = 0;
      // The next frame after a pause starts the clock again from there.
      this.last = null;
    }
  }

  frame(now) {
    this.handle = 0;
    if (!this.running) return;
    const step = this.last === null ? 0 : Math.min(Math.max(now - this.last, 0), MAX_STEP_MS);
    this.last = now;
    this.elapsed += step;
    // Ask for the next frame first, so a callback that throws does not
    // end the animation; the error still reaches the console.
    this.handle = this.env.requestAnimationFrame(this.tick);
    this.callback(this.elapsed, step);
  }
}
