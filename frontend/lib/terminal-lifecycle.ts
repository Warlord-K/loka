export type NativeCloseResult = { id: string; status: "closed" | "protected" | "failed"; error?: string };
export type NativeAttachLease = { id: string; owner: object; done: () => void };

export class NativeShellLifecycle {
  private readonly holders = new Map<string, object>();
  private readonly claims = new Map<string, Set<NativeAttachLease>>();
  private readonly attaching = new Map<string, Promise<void>>();
  private readonly closing = new Map<string, Promise<NativeCloseResult>>();

  occupied(id: string, owner?: object): boolean {
    const holder = this.holders.get(id);
    if (holder && holder !== owner) return true;
    return [...(this.claims.get(id) ?? [])].some((claim) => claim.owner !== owner);
  }

  holder(id: string): object | null {
    return this.holders.get(id) ?? null;
  }

  async claim(id: string, owner: object): Promise<NativeAttachLease> {
    const before = this.attaching.get(id) ?? Promise.resolve();
    let finish = () => {};
    const finished = new Promise<void>((resolve) => {
      finish = resolve;
    });
    const turn = before.catch(() => {}).then(() => finished);
    this.attaching.set(id, turn);
    let done = false;
    const lease: NativeAttachLease = {
      id,
      owner,
      done: () => {
        if (done) return;
        done = true;
        this.releaseClaim(lease);
        finish();
        void turn.then(() => {
          if (this.attaching.get(id) === turn) this.attaching.delete(id);
        });
      },
    };
    const claims = this.claims.get(id) ?? new Set<NativeAttachLease>();
    claims.add(lease);
    this.claims.set(id, claims);
    await before.catch(() => {});
    const closing = this.closing.get(id);
    if (closing) await closing;
    return lease;
  }

  confirm(id: string, owner: object, lease?: NativeAttachLease): object | null {
    const previous = this.holders.get(id) ?? null;
    if (lease) this.releaseClaim(lease);
    this.holders.set(id, owner);
    return previous;
  }

  release(id: string, owner: object): void {
    if (this.holders.get(id) === owner) this.holders.delete(id);
  }

  contended(id: string, lease: NativeAttachLease | null): boolean {
    return [...(this.claims.get(id) ?? [])].some((claim) => claim !== lease);
  }

  async close(id: string, owner: object | null, closer: (id: string) => Promise<unknown>): Promise<NativeCloseResult> {
    if (this.occupied(id, owner ?? undefined)) return { id, status: "protected" };
    const active = this.closing.get(id);
    if (active) return active;
    const closing = closer(id)
      .then((): NativeCloseResult => ({ id, status: "closed" }))
      .catch((error): NativeCloseResult => ({ id, status: "failed", error: error instanceof Error ? error.message : "could not close the shell" }))
      .finally(() => {
        if (this.closing.get(id) === closing) this.closing.delete(id);
      });
    this.closing.set(id, closing);
    return closing;
  }

  private releaseClaim(lease: NativeAttachLease): void {
    const claims = this.claims.get(lease.id);
    if (!claims) return;
    claims.delete(lease);
    if (!claims.size) this.claims.delete(lease.id);
  }
}

export function terminalStartsAllowed(devicesPanel: boolean, front: boolean, closing: boolean): boolean {
  return !devicesPanel || (front && !closing);
}
