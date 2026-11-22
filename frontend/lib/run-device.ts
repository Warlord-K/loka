export function defaultDevice(at: { devices: string[]; home: string | null; appRunner: string | null }): string | null {
  if (at.appRunner && at.devices.includes(at.appRunner)) return at.appRunner;
  if (at.home && at.devices.includes(at.home)) return at.home;
  return at.devices[0] ?? null;
}

export function devicePickerShown(at: { devices: string[]; chosen: string | null; home: string | null }): boolean {
  return at.devices.length > 1 || (at.chosen !== null && at.chosen !== at.home);
}

export function devicesLoading(at: { needed: boolean; devices: readonly unknown[] | null; appRunner: unknown }): boolean {
  return at.needed && (at.devices === null || at.appRunner === null);
}
