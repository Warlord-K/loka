"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { CLI_OPTIONS, OPENCODE_OFF_NOTE, cliOptions, worldApi, type BackendModels, type WorldAgent } from "@/lib/world";
import { WCombobox, WSelect } from "./controls";
import { BTN_PRIMARY, CHUNKY, ICON_BTN, INPUT, PANEL, PIXEL, TITLE } from "./styles";
import { VoiceField } from "./voice-ui";
import { TagChips } from "./agent-manage";
import { CloseButton, WindowHeader } from "./settings-shell";
import { escapeTaken } from "@/lib/escape";
import { LookFields } from "./look-fields";
import type { Accessory } from "./types";
import type { NavigationRequest } from "@/lib/exact-world-navigation";

export function CreateAgentDialog({ onBeginNavigation, onCreated, onClose }: { onBeginNavigation: () => NavigationRequest; onCreated: (agent: WorldAgent, navigation: NavigationRequest) => void; onClose: () => void }) {
  const [backends, setBackends] = useState<Record<string, BackendModels>>({});
  const [name, setName] = useState("");
  const [persona, setPersona] = useState("");
  const [cli, setCli] = useState("claude");
  const [model, setModel] = useState("");
  const [species, setSpecies] = useState("");
  const [accessories, setAccessories] = useState<Accessory[]>([]);
  const [gear, setGear] = useState("");
  const [tags, setTags] = useState<string[]>([]);
  const [expertise, setExpertise] = useState("");
  const [busy, setBusy] = useState(false);
  const [created, setCreated] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const cliRef = useRef(cli);
  const cliTouched = useRef(false);
  const modelTouched = useRef(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !busy && !escapeTaken(e) && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onClose]);

  useEffect(() => {
    worldApi
      .backends()
      .then((next) => {
        setBackends(next);
        const selected = cliTouched.current
          ? cliRef.current
          : (CLI_OPTIONS.find((option) => next[option.value]?.default) ?? CLI_OPTIONS.find((option) => next[option.value]))?.value ?? cliRef.current;
        if (!cliTouched.current) {
          cliRef.current = selected;
          setCli(selected);
        }
        if (!modelTouched.current) {
          const defaultModel = next[selected]?.default_model ?? "";
          setModel(defaultModel);
        }
      })
      .catch(() => setError("Could not load the configured models."));
  }, []);

  const models = useMemo(() => {
    const backend = backends[cli];
    return Array.from(new Set([...(backend?.models ?? []), ...(backend?.default_model ? [backend.default_model] : [])]));
  }, [backends, cli]);

  const changeCli = (next: string) => {
    cliRef.current = next;
    cliTouched.current = true;
    const defaultModel = backends[next]?.default_model ?? "";
    modelTouched.current = false;
    setCli(next);
    setModel(defaultModel);
    setError(null);
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (busy || created || !name.trim() || !persona.trim() || !model.trim()) return;
    const navigation = onBeginNavigation();
    setBusy(true);
    setError(null);
    try {
      const agent = await worldApi.createAgent({
        name: name.trim(),
        persona: persona.trim(),
        cli,
        model: model.trim(),
        role: "worker",
        look: accessories.length ? { species: species.trim() || "human", accessories } : { species: species.trim() || "human" },
        gear: { quest: gear.trim() },
        visibility: "private",
        tags,
        expertise: expertise.trim() || undefined,
      });
      setCreated(true);
      setBusy(false);
      onCreated(agent, navigation);
    } catch {
      setError("Could not create that bot. Check the details and try again.");
      setBusy(false);
    }
  };

  return (
    <div role="dialog" aria-modal="true" aria-label="New agent" className={cn(PANEL, "pointer-events-auto flex h-full w-full flex-col overflow-hidden")}>
      <WindowHeader title="New agent" subtitle="Pick a job, a model and a look." onClose={busy ? undefined : onClose} />
      <form onSubmit={submit} className="min-h-0 flex-1 space-y-3 overflow-y-auto p-4 text-w-base sm:p-5">
        <label className="block">
          <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Name</span>
          <input autoFocus className={cn(INPUT, "w-full")} value={name} onChange={(e) => setName(e.target.value)} placeholder="Rook" />
        </label>
        <label className="block">
          <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Persona</span>
          <VoiceField multiline ariaLabel="Persona" value={persona} onChange={setPersona} placeholder="What they do and how they work" />
        </label>
        <label className="block">
          <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Expertise</span>
          <VoiceField ariaLabel="Expertise" value={expertise} onChange={setExpertise} placeholder="What they are good at, in a sentence" />
        </label>
        <div>
          <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Tags (help agents pick who does what)</span>
          <TagChips tags={tags} editable onChange={setTags} />
        </div>
        <div className="flex gap-2">
          <label className="block w-36">
            <span className="mb-1.5 block text-w-sm font-medium text-w-text2">CLI</span>
            <WSelect ariaLabel="CLI" className="w-full" value={cli} onChange={changeCli} options={cliOptions("opencode" in backends)} />
          </label>
          <label className="block flex-1">
            <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Model</span>
            <WCombobox
              ariaLabel="Model"
              value={model}
              onChange={(value) => {
                modelTouched.current = true;
                setModel(value);
              }}
              placeholder={backends[cli] ? "Pick or type a model" : "Configure this backend first"}
              options={models.map((value) => ({ value, label: value, hint: value === backends[cli]?.default_model ? "default" : backends[cli]?.labels?.[value] }))}
            />
          </label>
        </div>
        {Object.keys(backends).length > 0 && !("opencode" in backends) && <p className="-mt-2 text-w-xs text-w-muted">{OPENCODE_OFF_NOTE}</p>}
        <LookFields slug={name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-")} species={species} onSpecies={setSpecies} accessories={accessories} onAccessories={setAccessories} />
        <label className="block">
          <span className="mb-1.5 block text-w-sm font-medium text-w-text2">Quest gear</span>
          <VoiceField ariaLabel="Quest gear" value={gear} onChange={setGear} placeholder="sword and lantern" />
        </label>
        {error && <p role="alert" className="rounded-lg border-2 border-w-danger bg-w-rose p-2 text-w-danger">{error}</p>}
        <button type="submit" disabled={busy || created || !name.trim() || !persona.trim() || !model.trim()} className={BTN_PRIMARY}>
          {busy ? "Creating..." : created ? "Created" : "Create bot"}
        </button>
      </form>
    </div>
  );
}
