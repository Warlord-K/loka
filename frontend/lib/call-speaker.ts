export const BLOCK_AFTER_MS = 1500;

export type SpeakerRoute = "element" | "direct";

export type SpeakerContext = Pick<AudioContext, "state" | "destination" | "resume" | "createGain" | "addEventListener" | "removeEventListener"> & {
  createMediaStreamDestination?: () => MediaStreamAudioDestinationNode;
};

export type SpeakerElement = Pick<HTMLAudioElement, "srcObject" | "autoplay" | "paused" | "play" | "pause" | "addEventListener" | "removeEventListener">;

export type SpeakerEnv = {
  element: () => SpeakerElement | null;
  onGesture: (fn: () => void) => () => void;
  onDeviceChange: (fn: () => void) => () => void;
  later: (fn: () => void, ms: number) => () => void;
};

export function browserSpeakerEnv(): SpeakerEnv {
  return {
    element: () => (typeof Audio === "function" ? new Audio() : null),
    onGesture: (fn) => {
      window.addEventListener("pointerdown", fn, true);
      window.addEventListener("keydown", fn, true);
      return () => {
        window.removeEventListener("pointerdown", fn, true);
        window.removeEventListener("keydown", fn, true);
      };
    },
    onDeviceChange: (fn) => {
      const devices = typeof navigator === "undefined" ? undefined : navigator.mediaDevices;
      if (!devices?.addEventListener) return () => undefined;
      devices.addEventListener("devicechange", fn);
      return () => devices.removeEventListener("devicechange", fn);
    },
    later: (fn, ms) => {
      const id = window.setTimeout(fn, ms);
      return () => window.clearTimeout(id);
    },
  };
}

function errorName(e: unknown): string {
  const name = (e as { name?: unknown } | null)?.name;
  return typeof name === "string" && name ? name : "Error";
}

export class SoundNotice {
  private error: string | null = null;
  private blocked = false;
  private readonly notice: string;

  constructor(notice: string) {
    this.notice = notice;
  }

  setError(error: string | null): void {
    if (error !== this.notice) this.error = error;
  }

  setBlocked(blocked: boolean): void {
    this.blocked = blocked;
  }

  shown(): string | null {
    return this.blocked ? this.notice : this.error;
  }
}

export class AudioSlot<T> {
  private held: T | null = null;
  private readonly open: () => T;
  private readonly usable: (audio: T) => boolean;
  private readonly dispose: (audio: T) => void;

  constructor(open: () => T, usable: (audio: T) => boolean, dispose: (audio: T) => void) {
    this.open = open;
    this.usable = usable;
    this.dispose = dispose;
  }

  prime(): T {
    if (!this.held || !this.usable(this.held)) this.held = this.open();
    return this.held;
  }

  async use<R>(setup: (take: () => T) => Promise<R>): Promise<R> {
    let taken: T | null = null;
    const take = () => {
      taken = this.held && this.usable(this.held) ? this.held : this.open();
      this.held = null;
      return taken;
    };
    try {
      return await setup(take);
    } catch (e) {
      if (taken) this.dispose(taken);
      else this.drop();
      throw e;
    }
  }

  drop(): void {
    const held = this.held;
    this.held = null;
    if (held) this.dispose(held);
  }
}

const NEEDS_GESTURE = new Set(["NotAllowedError", "AbortError"]);

export class CallSpeaker {
  readonly input: GainNode;
  route: SpeakerRoute = "direct";
  private readonly watchers = new Set<(blocked: boolean) => void>();
  private element: SpeakerElement | null = null;
  private stream: MediaStream | null = null;
  private target: AudioNode;
  private blockedNow = false;
  private everBlocked = false;
  private lastError = "";
  private closed = false;
  private cancelWait: (() => void) | null = null;
  private stopGestures: (() => void) | null = null;
  private readonly stopDevices: () => void;
  private readonly ctx: SpeakerContext;
  private readonly env: SpeakerEnv;

  constructor(ctx: SpeakerContext, env: SpeakerEnv = browserSpeakerEnv()) {
    this.ctx = ctx;
    this.env = env;
    this.input = ctx.createGain();
    const element = ctx.createMediaStreamDestination ? env.element() : null;
    if (element && ctx.createMediaStreamDestination) {
      const stream = ctx.createMediaStreamDestination();
      element.autoplay = true;
      element.srcObject = stream.stream;
      this.stream = stream.stream;
      element.addEventListener("pause", this.onChange);
      element.addEventListener("playing", this.onChange);
      this.element = element;
      this.target = stream;
      this.route = "element";
    } else {
      this.target = ctx.destination;
    }
    this.input.connect(this.target);
    ctx.addEventListener("statechange", this.onChange);
    this.stopDevices = env.onDeviceChange(this.onDevices);
  }

  get blocked(): boolean {
    return this.blockedNow;
  }

  subscribe(fn: (blocked: boolean) => void): () => void {
    this.watchers.add(fn);
    fn(this.blockedNow);
    return () => this.watchers.delete(fn);
  }

  private announce() {
    for (const fn of [...this.watchers]) fn(this.blockedNow);
  }

  healthy(): boolean {
    return !this.closed && this.ctx.state === "running" && (!this.element || !this.element.paused);
  }

  start(): Promise<boolean> {
    return this.kick();
  }

  hear(): Promise<boolean> {
    return this.kick();
  }

  check(): void {
    if (this.closed) return;
    if (this.healthy()) {
      this.clear();
      return;
    }
    void this.kick();
    if (this.blockedNow || this.cancelWait) return;
    this.cancelWait = this.env.later(() => {
      this.cancelWait = null;
      if (!this.closed && !this.healthy()) this.block();
    }, BLOCK_AFTER_MS);
  }

  stats(): { speaker_route: SpeakerRoute; speaker_error?: string; sound_blocked: boolean } {
    return { speaker_route: this.route, speaker_error: this.lastError || undefined, sound_blocked: this.everBlocked };
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.cancelWait?.();
    this.cancelWait = null;
    this.stopGestures?.();
    this.stopGestures = null;
    this.stopDevices();
    this.ctx.removeEventListener("statechange", this.onChange);
    this.watchers.clear();
    this.dropElement();
    this.input.disconnect();
  }

  private kick(): Promise<boolean> {
    if (this.closed) return Promise.resolve(false);
    const resumed = this.ctx.resume().catch(() => undefined);
    const played: Promise<string> = this.element ? this.element.play().then(() => "", errorName) : Promise.resolve("");
    return Promise.all([resumed, played]).then(([, failed]) => {
      if (this.closed) return false;
      if (failed) this.lastError = failed;
      if (failed && !NEEDS_GESTURE.has(failed)) this.useDirect();
      const ok = this.healthy();
      if (ok) this.clear();
      return ok;
    });
  }

  private useDirect() {
    if (!this.element) return;
    this.dropElement();
    this.input.disconnect();
    this.target = this.ctx.destination;
    this.input.connect(this.target);
    this.route = "direct";
  }

  private dropElement() {
    const element = this.element;
    if (!element) return;
    this.element = null;
    element.removeEventListener("pause", this.onChange);
    element.removeEventListener("playing", this.onChange);
    element.pause();
    element.srcObject = null;
  }

  private block() {
    if (this.blockedNow) return;
    this.blockedNow = true;
    this.everBlocked = true;
    this.stopGestures = this.env.onGesture(() => void this.kick());
    this.announce();
  }

  private clear() {
    this.cancelWait?.();
    this.cancelWait = null;
    if (!this.blockedNow) return;
    this.blockedNow = false;
    this.stopGestures?.();
    this.stopGestures = null;
    this.announce();
  }

  private readonly onChange = () => {
    if (this.closed) return;
    if (this.healthy()) this.clear();
    else this.check();
  };

  private readonly onDevices = () => {
    if (this.closed) return;
    void this.kick().then((ok) => {
      if (ok || this.closed) return;
      if (this.element && this.stream) {
        this.element.srcObject = null;
        this.element.srcObject = this.stream;
        void this.kick();
      }
      this.check();
    });
  };
}
