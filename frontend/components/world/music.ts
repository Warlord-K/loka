"use client";

import { useEffect, useState } from "react";

export type Track = { id: string; title: string; by: string };

export const DEFAULT_TRACKS: Track[] = [
  { id: "2mJXVrELK3Q", title: "Time Flows Ever Onward", by: "Evan Call" },
  { id: "eUDVUZZyA0M", title: "Experience", by: "Ludovico Einaudi" },
  { id: "Cp-j0EGmZlY", title: "Where the Blue-Moon Weed Grows", by: "Evan Call" },
  { id: "TK1Ij_-mank", title: "One Summer's Day", by: "Joe Hisaishi" },
  { id: "UYqBpt4vBuE", title: "For 1000 Years", by: "Evan Call" },
  { id: "WocHIfN5oCQ", title: "Across the Violet Sky", by: "Evan Call" },
  { id: "OvDPXZOACPQ", title: "Departures", by: "Evan Call" },
  { id: "t6vWDz5NzOY", title: "Light of Nibel", by: "Gareth Coker" },
  { id: "PuQ-kaLJKto", title: "One Last Adventure", by: "Evan Call" },
  { id: "sR2W2scFS4Y", title: "Nuvole Bianche", by: "Ludovico Einaudi" },
  { id: "aQ7U71iuG74", title: "Across the Northern Lands", by: "Evan Call" },
  { id: "2W5aNN6_DSg", title: "Never Coming Back", by: "Evan Call" },
  { id: "ZfZQRJs-Ewg", title: "Gone, but Not Forgotten", by: "Evan Call" },
  { id: "f7SS57LFPco", title: "Merry-Go-Round of Life", by: "Joe Hisaishi" },
  { id: "awyGTLf40mw", title: "A Sunrise Worth Seeing", by: "Evan Call" },
  { id: "XjKTdRK07O0", title: "The Voice in My Heart", by: "Evan Call" },
  { id: "UJ5uasDqMvo", title: "Song for the Beyond", by: "Evan Call" },
  { id: "VPgZFgOvJDE", title: "Secunda", by: "Jeremy Soule" },
  { id: "HKTjT9tR1nc", title: "A Doll's Beginning", by: "Evan Call" },
  { id: "ZlbxNeOmjEE", title: "Mushishi no Theme", by: "Toshio Masuda" },
  { id: "OZZoVC1KaK0", title: "Deep in the Dungeon", by: "Evan Call" },
  { id: "8QrpzphrQ7s", title: "Theme of Violet Evergarden", by: "Evan Call" },
  { id: "aBkTkxKDduc", title: "Sweden", by: "C418" },
  { id: "vPA6T0la6uI", title: "Day One", by: "Hans Zimmer" },
];

export type MusicPrefs = { on: boolean; volume: number; shuffle: boolean };

export type MusicState = {
  prefs: MusicPrefs;
  current: Track | null;
  playing: boolean;
  custom: boolean;
  waiting: boolean;
};

type YTPlayer = {
  loadVideoById: (id: string) => void;
  playVideo: () => void;
  pauseVideo: () => void;
  setVolume: (v: number) => void;
  getPlayerState: () => number;
};

type YTNamespace = { Player: new (el: HTMLElement, opts: Record<string, unknown>) => YTPlayer };

declare global {
  interface Window {
    YT?: YTNamespace;
    onYouTubeIframeAPIReady?: () => void;
  }
}

const KEY = "world.music";
const DEFAULTS: MusicPrefs = { on: false, volume: 0.35, shuffle: false };

let prefs: MusicPrefs | null = null;
let host: HTMLElement | null = null;
let player: YTPlayer | null = null;
let ready = false;
let unlocked = false;
let holds = 0;
let playing = false;
let current: Track | null = null;
let custom: Track[] = [];
let position = -1;
let skips = 0;
const listeners = new Set<(s: MusicState) => void>();

function loadPrefs(): MusicPrefs {
  if (prefs) return prefs;
  try {
    const raw = JSON.parse(window.localStorage.getItem(KEY) ?? "null") as Partial<MusicPrefs> | null;
    prefs = {
      on: typeof raw?.on === "boolean" ? raw.on : DEFAULTS.on,
      volume: typeof raw?.volume === "number" && Number.isFinite(raw.volume) ? Math.min(1, Math.max(0, raw.volume)) : DEFAULTS.volume,
      shuffle: typeof raw?.shuffle === "boolean" ? raw.shuffle : DEFAULTS.shuffle,
    };
  } catch {
    prefs = { ...DEFAULTS };
  }
  return prefs;
}

export function musicState(): MusicState {
  return { prefs: loadPrefs(), current, playing, custom: custom.length > 0, waiting: loadPrefs().on && !unlocked };
}

function emit() {
  const s = musicState();
  for (const l of listeners) l(s);
}

function savePrefs(next: MusicPrefs) {
  prefs = next;
  try {
    window.localStorage.setItem(KEY, JSON.stringify(next));
  } catch {}
}

function nextDefault(): Track {
  if (loadPrefs().shuffle && DEFAULT_TRACKS.length > 1) {
    let pick = position;
    while (pick === position) pick = Math.floor(Math.random() * DEFAULT_TRACKS.length);
    position = pick;
  } else {
    position = (position + 1) % DEFAULT_TRACKS.length;
  }
  return DEFAULT_TRACKS[position];
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
          if (e.data === 0) advance();
          if (e.data === 1) {
            playing = true;
            skips = 0;
            if (holds || !loadPrefs().on) player?.pauseVideo();
          }
          if (e.data === 2) playing = false;
          emit();
        },
        onError: () => {
          skips++;
          if (skips < 5) advance();
        },
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
  current = track;
  if (!unlocked || !host) {
    emit();
    return;
  }
  if (!player) create(track);
  else if (ready) {
    player.loadVideoById(track.id);
    player.setVolume(Math.round(loadPrefs().volume * 100));
  }
  emit();
}

function advance() {
  const next = custom.shift() ?? nextDefault();
  start(next);
}

export function attachMusicHost(el: HTMLElement | null) {
  host = el;
  if (!el) return;
  const unlock = () => {
    if (unlocked) return;
    unlocked = true;
    if (loadPrefs().on && !holds) start(current ?? nextDefault());
    else emit();
  };
  window.addEventListener("pointerdown", unlock, { once: true, capture: true });
  window.addEventListener("keydown", unlock, { once: true, capture: true });
}

export function setMusicOn(on: boolean) {
  savePrefs({ ...loadPrefs(), on });
  if (!on) {
    player?.pauseVideo();
    playing = false;
  } else if (!holds) {
    unlocked = true;
    if (player && ready && current) player.playVideo();
    else start(current ?? nextDefault());
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
  } else if (player && ready) player.playVideo();
  else start(current ?? nextDefault());
  emit();
}

export function skipMusic() {
  savePrefs({ ...loadPrefs(), on: true });
  unlocked = true;
  advance();
}

export function playTrack(track: Track) {
  savePrefs({ ...loadPrefs(), on: true });
  unlocked = true;
  const at = DEFAULT_TRACKS.findIndex((t) => t.id === track.id);
  if (at >= 0 && !custom.length) position = at;
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
  const [state, setState] = useState<MusicState>(() => (typeof window === "undefined" ? { prefs: DEFAULTS, current: null, playing: false, custom: false, waiting: false } : musicState()));
  useEffect(() => {
    setState(musicState());
    listeners.add(setState);
    return () => {
      listeners.delete(setState);
    };
  }, []);
  return state;
}
