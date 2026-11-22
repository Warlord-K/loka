export type CallState = { agentId: string | null; minimised: boolean };

export type CallAction = { type: "start"; agentId: string } | { type: "minimise" } | { type: "restore" } | { type: "hang_up" };

export const NO_CALL: CallState = { agentId: null, minimised: false };

export function callReducer(state: CallState, action: CallAction): CallState {
  switch (action.type) {
    case "start":
      return { agentId: action.agentId, minimised: false };
    case "minimise":
      return state.agentId && !state.minimised ? { ...state, minimised: true } : state;
    case "restore":
      return state.agentId && state.minimised ? { ...state, minimised: false } : state;
    case "hang_up":
      return state.agentId ? NO_CALL : state;
  }
}

export function callClock(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  const minutes = Math.floor(whole / 60);
  const rest = String(whole % 60).padStart(2, "0");
  return minutes >= 60 ? `${Math.floor(minutes / 60)}:${String(minutes % 60).padStart(2, "0")}:${rest}` : `${minutes}:${rest}`;
}
