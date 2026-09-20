"use client";

import { useCallback, useEffect, useRef } from "react";
import { MusicNotes, MusicNotesSimple, Pause, Play, Shuffle, SkipForward, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import type { WorldEvent } from "@/lib/world";
import { useWorldEvent } from "./loka-client";
import { attachMusicHost, DEFAULT_TRACKS, musicCommand, playTrack, setMusicOn, setMusicShuffle, setMusicVolume, skipMusic, toggleMusicPause, useMusic, type Track } from "./music";
import { CHUNKY, HUD_ICON, HUD_SEG, PANEL } from "./styles";

export function MusicButton({ open, onToggle, grouped = false }: { open: boolean; onToggle: () => void; grouped?: boolean }) {
  const music = useMusic();
  const live = music.prefs.on && music.playing;
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-pressed={open}
      aria-label="Music"
      title={music.current && music.prefs.on ? `Music: ${music.current.title} (${music.current.by})` : "Music is off"}
      className={cn(grouped ? HUD_SEG : HUD_ICON, open && "bg-w-field")}
    >
      {live ? <MusicNotes size={20} weight="fill" className="text-w-teal" /> : <MusicNotesSimple size={20} weight="bold" className={music.prefs.on ? undefined : "text-w-muted"} />}
    </button>
  );
}

function isTrack(v: unknown): v is Track {
  const t = v as Track;
  return Boolean(t) && typeof t.id === "string" && /^[A-Za-z0-9_-]{11}$/.test(t.id) && typeof t.title === "string";
}

export function MusicDock({ open, meId, onClose }: { open: boolean; meId: string | null; onClose: () => void }) {
  const music = useMusic();
  const hostRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    attachMusicHost(hostRef.current);
    return () => attachMusicHost(null);
  }, []);
  useWorldEvent(
    useCallback(
      (ev: WorldEvent) => {
        if (ev.type !== "music.play" || !meId || ev.payload.owner_user_id !== meId) return;
        const tracks = Array.isArray(ev.payload.tracks) ? ev.payload.tracks.filter(isTrack).map((t) => ({ id: t.id, title: t.title, by: typeof t.by === "string" ? t.by : "" })) : [];
        musicCommand(String(ev.payload.action ?? "play"), tracks);
      },
      [meId],
    ),
  );
  const now = music.current;
  return (
    <div
      className={cn(
        open ? "pointer-events-auto max-h-[calc(100vh-10rem)] w-[22rem] max-w-[calc(100vw-1.5rem)] overflow-y-auto" : "pointer-events-none fixed bottom-0 left-0 h-px w-px overflow-hidden opacity-0",
      )}
      aria-hidden={!open}
    >
      <div className={cn(PANEL, "space-y-3 p-3")}>
        <div className="flex items-center gap-2">
          <MusicNotes size={16} weight="bold" />
          <span className="flex-1 text-w-sm font-semibold text-w-strong">Music</span>
          <button
            type="button"
            role="switch"
            aria-checked={music.prefs.on}
            onClick={() => setMusicOn(!music.prefs.on)}
            className={cn(CHUNKY, "px-2.5 py-0.5 text-w-xs", music.prefs.on ? "bg-w-teal text-w-onscrim hover:bg-w-teal2" : "bg-w-paper hover:bg-w-sand")}
          >
            {music.prefs.on ? "On" : "Off"}
          </button>
          <button type="button" onClick={onClose} aria-label="Close music" className="rounded-md p-1 text-w-muted hover:bg-w-field hover:text-w-text">
            <X size={14} weight="bold" />
          </button>
        </div>
        <div ref={hostRef} className="overflow-hidden rounded-md border-2 border-w-ink bg-black" />
        <div className="min-w-0">
          <p className="truncate text-w-base font-semibold">{now ? now.title : "Nothing playing yet"}</p>
          <p className="truncate text-w-xs text-w-muted">
            {now ? now.by : music.waiting ? "Starts when you click or press a key" : "Turn music on to start the playlist"}
            {now && music.custom && " · picked by your agent"}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button type="button" onClick={toggleMusicPause} aria-label={music.playing ? "Pause" : "Play"} className={cn(CHUNKY, "grid h-8 w-8 place-items-center bg-w-card hover:bg-w-sand")}>
            {music.playing ? <Pause size={15} weight="fill" /> : <Play size={15} weight="fill" />}
          </button>
          <button type="button" onClick={skipMusic} aria-label="Next song" className={cn(CHUNKY, "grid h-8 w-8 place-items-center bg-w-card hover:bg-w-sand")}>
            <SkipForward size={15} weight="fill" />
          </button>
          <button
            type="button"
            aria-pressed={music.prefs.shuffle}
            onClick={() => setMusicShuffle(!music.prefs.shuffle)}
            aria-label="Shuffle the default playlist"
            className={cn(CHUNKY, "grid h-8 w-8 place-items-center", music.prefs.shuffle ? "bg-w-invert text-w-oninvert" : "bg-w-card hover:bg-w-sand")}
          >
            <Shuffle size={15} weight="bold" />
          </button>
          <input
            type="range"
            min={0}
            max={100}
            value={Math.round(music.prefs.volume * 100)}
            onChange={(e) => setMusicVolume(Number(e.target.value) / 100)}
            aria-label="Music volume"
            className="min-w-0 flex-1 accent-w-teal"
          />
        </div>
        {music.custom && (
          <button type="button" onClick={() => musicCommand("default", [])} className="text-w-xs text-w-link underline">
            Back to the default playlist
          </button>
        )}
        <details className="text-w-sm">
          <summary className="cursor-pointer text-w-muted">Playlist ({DEFAULT_TRACKS.length} songs)</summary>
          <ul className="mt-1.5 max-h-48 space-y-0.5 overflow-y-auto pr-1">
            {DEFAULT_TRACKS.map((t) => (
              <li key={t.id}>
                <button
                  type="button"
                  onClick={() => playTrack(t)}
                  className={cn("w-full truncate rounded-md px-1.5 py-0.5 text-left hover:bg-w-field", now?.id === t.id && "bg-w-teal/20 font-semibold")}
                >
                  {t.title} <span className="text-w-muted">· {t.by}</span>
                </button>
              </li>
            ))}
          </ul>
        </details>
        <p className="text-w-xs text-w-muted">Ask any of your agents to play something, like &quot;play some Ghibli&quot; or a YouTube link. Music plays through YouTube.</p>
      </div>
    </div>
  );
}
