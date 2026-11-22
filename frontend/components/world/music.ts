"use client";

import { useEffect, useState } from "react";
import { EMPTY_BAG, errorBelongs, errorEffect, markPlayed, pickNext, playerErrorReason, readBag, skipsExhausted, type MusicBag } from "@/lib/music-bag";

export type Track = { id: string; title: string; by: string };

export const DEFAULT_TRACKS: Track[] = [
  { id: "0KrXs-FIh5w", title: "One Summer's Day", by: "Joe Hisaishi" },
  { id: "KEZmnPQions", title: "Ashitaka and San (Live in Vienna)", by: "Joe Hisaishi" },
  { id: "nN3sUAr4LNg", title: "The Path of the Wind (piano)", by: "Joe Hisaishi" },
  { id: "A5lN9lS5acM", title: "A Town with an Ocean View (piano)", by: "Joe Hisaishi" },
  { id: "lB4PRX737-0", title: "Merry-Go-Round of Life (piano)", by: "Joe Hisaishi" },
  { id: "hb5TsZcAiJQ", title: "Left to Bloom", by: "Lena Raine" },
  { id: "uPM--9Gar60", title: "Wending", by: "Lena Raine" },
  { id: "y-f8pEX1Q1M", title: "Infinite Amethyst", by: "Lena Raine" },
  { id: "TzcwvQ0MDCo", title: "Ancestry", by: "Lena Raine" },
  { id: "N8OHSXvneOE", title: "First Steps", by: "Lena Raine" },
  { id: "1rwAvUvvQzQ", title: "Resurrections", by: "Lena Raine" },
  { id: "FjG0ivBT4lQ", title: "Quiet and Falling", by: "Lena Raine" },
  { id: "QfhUSYq7jEs", title: "Reflection", by: "Lena Raine" },
  { id: "k79dMVVmvA8", title: "Comforting Memories", by: "Kumi Tanioka" },
  { id: "CG7bdkACNKM", title: "Floating Dream", by: "Kumi Tanioka" },
  { id: "Uu3dshFseaU", title: "Echo in the Wind", by: "Aaron Cherof" },
  { id: "SLS9tUa2GXI", title: "Featherfall", by: "Aaron Cherof" },
  { id: "1I7vAflFKS4", title: "Stardew Valley Overture", by: "ConcernedApe" },
  { id: "uqr6yV1AVpI", title: "Pelican Town", by: "ConcernedApe" },
  { id: "FO9WXi9gxQg", title: "Spring (It's a Big World Outside)", by: "ConcernedApe" },
  { id: "k0nqXeM-sns", title: "Summer (Nature's Crescendo)", by: "ConcernedApe" },
  { id: "C-84hnlrbys", title: "Summer (The Sun Can Bend an Orange Sky)", by: "ConcernedApe" },
  { id: "omVFjGHx0FQ", title: "Fall (The Smell of Mushroom)", by: "ConcernedApe" },
  { id: "AiX1dBfmftA", title: "Winter (Nocturne of Ice)", by: "ConcernedApe" },
  { id: "Tk6tA6OVqvk", title: "Cloud Country", by: "ConcernedApe" },
  { id: "R0-Q17WChH0", title: "Settling In", by: "ConcernedApe" },
  { id: "mhPSbmEXUgw", title: "Distant Banjo", by: "ConcernedApe" },
  { id: "cNogX2C8W6s", title: "7AM (Animal Crossing: New Horizons)", by: "Kazumi Totaka" },
  { id: "lqszVTQ5VZ8", title: "1PM (Animal Crossing: New Horizons)", by: "Kazumi Totaka" },
  { id: "4F59lA8c3lk", title: "5PM (Animal Crossing: New Horizons)", by: "Kazumi Totaka" },
  { id: "3jGmbgm3guU", title: "8PM (Animal Crossing: New Horizons)", by: "Kazumi Totaka" },
  { id: "V5Z4Fh8-9S8", title: "2AM (Animal Crossing: New Leaf)", by: "Kazumi Totaka" },
  { id: "o3SZee4YZX8", title: "Build That Wall", by: "Darren Korb" },
  { id: "U8M5IeaAkmQ", title: "Mother, I'm Here", by: "Darren Korb" },
  { id: "g60bPpXEq9Q", title: "Setting Sail, Coming Home", by: "Darren Korb" },
  { id: "qYcoJpqCha4", title: "Aruarian Dance", by: "Nujabes" },
  { id: "2wK27xW4OFI", title: "Reflection Eternal", by: "Nujabes" },
  { id: "yPl5BaZ4crs", title: "Spiral", by: "Nujabes" },
  { id: "u_sAwmfxUR8", title: "this girl", by: "nymano" },
  { id: "qH5hM1ILuFQ", title: "Fallen Down", by: "Toby Fox" },
  { id: "0UkRqMFMZic", title: "Home", by: "Toby Fox" },
  { id: "vC1GAZ62NPg", title: "Home (Music Box)", by: "Toby Fox" },
  { id: "QZ1Jp-Wg7b0", title: "Snowy", by: "Toby Fox" },
  { id: "rtb0N-JQ79k", title: "Memory", by: "Toby Fox" },
  { id: "NSlkW1fFkyo", title: "Dirtmouth", by: "Christopher Larkin" },
  { id: "fWquuWkHVP4", title: "Greenpath", by: "Christopher Larkin" },
  { id: "fZqZ7XH9K_Y", title: "See You at the Top", by: "Mark Sparling" },
  { id: "5dmWyIjPfAo", title: "Calling Out", by: "Gareth Coker" },
  { id: "t6vWDz5NzOY", title: "Light of Nibel", by: "Gareth Coker" },
  { id: "1OZDaRhHHyM", title: "Merry Christmas Mr. Lawrence", by: "Ryuichi Sakamoto" },
  { id: "VPgZFgOvJDE", title: "Secunda", by: "Jeremy Soule" },
  { id: "ZlbxNeOmjEE", title: "Mushishi no Theme", by: "Toshio Masuda" },
];

export type MusicPrefs = { on: boolean; volume: number; shuffle: boolean };

export type MusicState = {
  prefs: MusicPrefs;
  current: Track | null;
  playing: boolean;
  custom: boolean;
  waiting: boolean;
  note: string | null;
};

type YTPlayer = {
  loadVideoById: (id: string) => void;
  playVideo: () => void;
  pauseVideo: () => void;
  setVolume: (v: number) => void;
  getPlayerState: () => number;
  getVideoUrl?: () => string;
};

type YTNamespace = { Player: new (el: HTMLElement, opts: Record<string, unknown>) => YTPlayer };

declare global {
  interface Window {
    YT?: YTNamespace;
    onYouTubeIframeAPIReady?: () => void;
  }
}

const KEY = "world.music";
const BAG_KEY = "world.music.bag";
const PREFS_VERSION = 2;
const SKIP_DELAY_MS = 400;
const NONE_PLAYABLE = "None of the songs would play. Check your connection, then press play to try again.";
const DEFAULTS: MusicPrefs = { on: false, volume: 0.35, shuffle: true };

let prefs: MusicPrefs | null = null;
let host: HTMLElement | null = null;
let player: YTPlayer | null = null;
let ready = false;
let unlocked = false;
let holds = 0;
let playing = false;
let current: Track | null = null;
let custom: Track[] = [];
let bag: MusicBag | null = null;
let note: string | null = null;
let skipTimer: ReturnType<typeof setTimeout> | null = null;
const broken = new Set<string>();
let requested: string | null = null;
let stateSinceLoad = false;
const failedInARow = new Set<string>();
const listeners = new Set<(s: MusicState) => void>();

function loadPrefs(): MusicPrefs {
  if (prefs) return prefs;
  try {
    const raw = JSON.parse(window.localStorage.getItem(KEY) ?? "null") as (Partial<MusicPrefs> & { v?: number }) | null;
    prefs = {
      on: typeof raw?.on === "boolean" ? raw.on : DEFAULTS.on,
      volume: typeof raw?.volume === "number" && Number.isFinite(raw.volume) ? Math.min(1, Math.max(0, raw.volume)) : DEFAULTS.volume,
      shuffle: raw?.v === PREFS_VERSION && typeof raw.shuffle === "boolean" ? raw.shuffle : DEFAULTS.shuffle,
    };
  } catch {
    prefs = { ...DEFAULTS };
  }
  return prefs;
}

export function musicState(): MusicState {
  return { prefs: loadPrefs(), current, playing, custom: custom.length > 0, waiting: loadPrefs().on && !unlocked, note };
}

function emit() {
  const s = musicState();
  for (const l of listeners) l(s);
}

function savePrefs(next: MusicPrefs) {
  prefs = next;
  try {
    window.localStorage.setItem(KEY, JSON.stringify({ ...next, v: PREFS_VERSION }));
  } catch {}
}

function loadBag(): MusicBag {
  if (bag) return bag;
  try {
    bag = readBag(JSON.parse(window.localStorage.getItem(BAG_KEY) ?? "null"));
  } catch {
    bag = EMPTY_BAG;
  }
  return bag;
}

function saveBag(next: MusicBag) {
  bag = next;
  try {
    window.localStorage.setItem(BAG_KEY, JSON.stringify(next));
  } catch {}
}

const DEFAULT_IDS = DEFAULT_TRACKS.map((t) => t.id);

function playable(): string[] {
  return DEFAULT_IDS.filter((id) => !broken.has(id));
}

function pickNextId(): string | null {
  const next = pickNext(loadBag(), DEFAULT_IDS, broken, loadPrefs().shuffle, Math.random);
  if (next.id) saveBag(next.bag);
  return next.id;
}

function retryAfterGivingUp() {
  if (!note) return;
  note = null;
  broken.clear();
  failedInARow.clear();
}

function nextDefault(): Track | null {
  const id = pickNextId();
  return DEFAULT_TRACKS.find((t) => t.id === id) ?? null;
}

function giveUp() {
  player?.pauseVideo();
  playing = false;
  note = NONE_PLAYABLE;
  console.warn("Loka music: stopped because no song in the queue or the playlist would play.");
  emit();
}

function loadedId(): string | null {
  try {
    const url = player?.getVideoUrl?.();
    return url ? new URL(url).searchParams.get("v") : null;
  } catch {
    return null;
  }
}

function skipBroken(code: number) {
  const failed = current;
  if (!errorBelongs({ requested, current: failed?.id ?? null, urlId: loadedId(), stateSinceLoad })) return;
  if (failed) {
    if (errorEffect(code) === "broken") broken.add(failed.id);
    failedInARow.add(failed.id);
    console.warn(`Loka music: skipped "${failed.title}" (${failed.id}): ${playerErrorReason(code)}.`);
  }
  playing = false;
  if (skipTimer) clearTimeout(skipTimer);
  skipTimer = setTimeout(() => {
    skipTimer = null;
    if (!custom.length && skipsExhausted(failedInARow, playable())) giveUp();
    else advance();
  }, SKIP_DELAY_MS);
}

function loadScript() {
  if (window.YT?.Player || document.getElementById("yt-iframe-api")) return;
  const s = document.createElement("script");
  s.id = "yt-iframe-api";
  s.src = "https://www.youtube.com/iframe_api";
  s.async = true;
  document.head.appendChild(s);
}

function create(track: Track) {
  if (!host || player) return;
  const frame = document.createElement("iframe");
  frame.setAttribute("credentialless", "");
  frame.allow = "autoplay; encrypted-media";
  frame.title = "Loka music";
  frame.className = "aspect-video h-auto w-full";
  frame.src = `https://www.youtube.com/embed/${track.id}?enablejsapi=1&autoplay=1&playsinline=1&rel=0&origin=${encodeURIComponent(window.location.origin)}`;
  host.replaceChildren(frame);
  requested = track.id;
  stateSinceLoad = false;
  const build = () => {
    if (!window.YT?.Player || player) return;
    player = new window.YT.Player(frame, {
      events: {
        onReady: () => {
          ready = true;
          player?.setVolume(Math.round(loadPrefs().volume * 100));
          if (holds || !loadPrefs().on) player?.pauseVideo();
          else player?.playVideo();
        },
        onStateChange: (e: { data: number }) => {
          stateSinceLoad = true;
          if (e.data === 0) advance();
          if (e.data === 1) {
            failedInARow.clear();
            playing = true;
            note = null;
            if (holds || !loadPrefs().on) player?.pauseVideo();
          }
          if (e.data === 2) playing = false;
          emit();
        },
        onError: (e: { data: number }) => skipBroken(e.data),
      },
    });
  };
  const prior = window.onYouTubeIframeAPIReady;
  window.onYouTubeIframeAPIReady = () => {
    prior?.();
    build();
  };
  loadScript();
  build();
}

function start(track: Track) {
  if (skipTimer) {
    clearTimeout(skipTimer);
    skipTimer = null;
  }
  current = track;
  if (!unlocked || !host) {
    emit();
    return;
  }
  if (!player) create(track);
  else if (ready) {
    requested = track.id;
    stateSinceLoad = false;
    player.loadVideoById(track.id);
    player.setVolume(Math.round(loadPrefs().volume * 100));
  }
  emit();
}

function advance() {
  const next = custom.shift() ?? nextDefault();
  if (next) start(next);
  else giveUp();
}

function resume() {
  const next = current ?? nextDefault();
  if (next) start(next);
  else giveUp();
}

export function attachMusicHost(el: HTMLElement | null) {
  host = el;
  if (!el) return;
  const unlock = () => {
    if (unlocked) return;
    unlocked = true;
    if (loadPrefs().on && !holds) resume();
    else emit();
  };
  window.addEventListener("pointerdown", unlock, { once: true, capture: true });
  window.addEventListener("keydown", unlock, { once: true, capture: true });
}

export function setMusicOn(on: boolean) {
  savePrefs({ ...loadPrefs(), on });
  if (!on) {
    if (skipTimer) clearTimeout(skipTimer);
    skipTimer = null;
    player?.pauseVideo();
    playing = false;
  } else if (!holds) {
    unlocked = true;
    if (note) {
      retryAfterGivingUp();
      advance();
    } else if (player && ready && current) player.playVideo();
    else resume();
  }
  emit();
}

export function setMusicVolume(volume: number) {
  savePrefs({ ...loadPrefs(), volume: Math.min(1, Math.max(0, volume)) });
  player?.setVolume(Math.round(loadPrefs().volume * 100));
  emit();
}

export function setMusicShuffle(shuffle: boolean) {
  savePrefs({ ...loadPrefs(), shuffle });
  emit();
}

export function toggleMusicPause() {
  if (!loadPrefs().on) return setMusicOn(true);
  if (playing) {
    player?.pauseVideo();
    playing = false;
  } else if (note) {
    retryAfterGivingUp();
    advance();
  } else if (player && ready) player.playVideo();
  else resume();
  emit();
}

export function skipMusic() {
  savePrefs({ ...loadPrefs(), on: true });
  unlocked = true;
  failedInARow.clear();
  retryAfterGivingUp();
  advance();
}

export function playTrack(track: Track) {
  savePrefs({ ...loadPrefs(), on: true });
  unlocked = true;
  failedInARow.clear();
  retryAfterGivingUp();
  broken.delete(track.id);
  if (DEFAULT_IDS.includes(track.id)) saveBag(markPlayed(loadBag(), playable(), track.id, Math.random));
  start(track);
}

export function musicCommand(action: string, tracks: Track[]) {
  if (action === "stop") return setMusicOn(false);
  if (!loadPrefs().on) {
    if (action === "default") custom = [];
    else if (action === "queue") custom = [...custom, ...tracks];
    else if (tracks.length) {
      custom = tracks.slice(1);
      current = tracks[0];
    }
    return emit();
  }
  retryAfterGivingUp();
  if (action === "default") {
    custom = [];
    savePrefs({ ...loadPrefs(), on: true });
    unlocked = true;
    return advance();
  }
  if (!tracks.length) return;
  if (action === "queue") {
    custom = [...custom, ...tracks];
    if (!loadPrefs().on || !current) {
      savePrefs({ ...loadPrefs(), on: true });
      unlocked = true;
      advance();
    } else emit();
    return;
  }
  custom = tracks.slice(1);
  savePrefs({ ...loadPrefs(), on: true });
  unlocked = true;
  start(tracks[0]);
}

export function holdMusic(): () => void {
  holds++;
  player?.pauseVideo();
  let released = false;
  return () => {
    if (released) return;
    released = true;
    holds = Math.max(0, holds - 1);
    if (!holds && loadPrefs().on && player && ready && current) player.playVideo();
  };
}

export function useMusic(): MusicState {
  const [state, setState] = useState<MusicState>(() => (typeof window === "undefined" ? { prefs: DEFAULTS, current: null, playing: false, custom: false, waiting: false, note: null } : musicState()));
  useEffect(() => {
    setState(musicState());
    listeners.add(setState);
    return () => {
      listeners.delete(setState);
    };
  }, []);
  return state;
}
