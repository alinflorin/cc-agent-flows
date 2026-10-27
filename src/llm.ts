export const MODELS = [
  { id: "claude-fable-5-1", label: "Fable 5.1" },
  { id: "claude-opus-5-5", label: "Opus 5.5" },
  { id: "claude-sonnet-5-5", label: "Sonnet 5.5" },
  { id: "claude-haiku-5-5", label: "Haiku 5.5" },
] as const;
export const THINKING_LEVELS = ["low", "medium", "high", "xhigh", "max"] as const;

export const DEFAULT_MODEL = "claude-sonnet-5-5";
export const DEFAULT_THINKING = "medium";
