// Mirrors of the Rust types the commands exchange, in their serde JSON form.
// Sources: crates/engine/src/{settings,io,complete}.rs and app/src/*.rs.

export type LengthMode = 'word' | 'phrase' | 'sentence' | 'paragraph' | 'auto';
export type FixedLength = Exclude<LengthMode, 'auto'>;

export type NamedKey =
  | 'tab' | 'escape' | 'enter' | 'backspace' | 'space'
  | 'left' | 'right' | 'up' | 'down' | 'home' | 'end' | 'page_up' | 'page_down';
export type Key = NamedKey | { f: number } | { char: string };

export interface Mods {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
}

export interface Chord {
  key: Key;
  mods: Mods;
}

export type Modifier =
  | 'left_alt' | 'right_alt' | 'left_ctrl' | 'right_ctrl'
  | 'left_shift' | 'right_shift' | 'left_meta' | 'right_meta';

export type Trigger = { chord: Chord } | { modifier_tap: Modifier };

export interface Bindings {
  take_word: Chord | null;
  take_all: Chord | null;
  show_more: Chord | null;
  next_alternative: Chord | null;
  prev_alternative: Chord | null;
  dismiss: Chord | null;
  flow_draft: Trigger | null;
}
export type Slot = keyof Bindings;

export type AppCategory = 'chat' | 'email' | 'document' | 'search' | 'terminal' | 'code' | 'other';

export interface Ceilings {
  chat: FixedLength;
  email: FixedLength;
  document: FixedLength;
  search: FixedLength;
  terminal: FixedLength;
  other: FixedLength;
}

export type ModelChoice =
  | { kind: 'none' }
  | { kind: 'local'; model: string }
  | { kind: 'provider'; provider: string };

export type ProviderKind =
  | { type: 'open_ai_compatible'; base_url: string }
  | { type: 'anthropic' }
  | { type: 'gemini' };

export interface ProviderConfig {
  id: string;
  label: string;
  kind: ProviderKind;
  model: string;
}

export interface ContextSettings {
  conversation: boolean;
  screen_ocr: boolean;
  clipboard: boolean;
  style_memory: boolean;
  about_me: string;
}

export interface CustomAction {
  name: string;
  instruction: string;
}

export interface FlowDraftSettings {
  enabled: boolean;
  auto_offer: boolean;
  custom_actions: CustomAction[];
}

export interface AppRule {
  name?: string | null;
  enabled?: boolean | null;
  length?: LengthMode | null;
  ceiling?: LengthMode | null;
  category?: AppCategory | null;
}

export interface Appearance {
  style: 'faint' | 'high_contrast';
  announce: boolean;
}

export interface Settings {
  version: number;
  enabled: boolean;
  length: LengthMode;
  boldness: number;
  ceilings: Ceilings;
  paragraph_words: number;
  learn_length: boolean;
  pause_ms: number;
  keys: Bindings;
  autocomplete_model: ModelChoice;
  draft_model: ModelChoice;
  providers: ProviderConfig[];
  context: ContextSettings;
  flow_draft: FlowDraftSettings;
  apps: Record<string, AppRule>;
  daily_request_cap: number | null;
  low_power: boolean;
  unload_after_minutes: number;
  appearance: Appearance;
  update_check: boolean;
}

export type SetupStep = 'welcome' | 'permissions' | 'model' | 'practice' | 'done';

export type StatusKind =
  | 'starting' | 'active' | 'paused' | 'off_in_app' | 'no_model' | 'cloud_unavailable'
  | 'permission_needed' | 'secure_input' | 'unavailable' | 'loading' | 'downloading' | 'error';

export interface PauseInfo {
  app: string | null;
  name: string | null;
  until_ms: number | null;
}

export interface StatusView {
  kind: StatusKind;
  text: string;
  pauses: PauseInfo[];
  app: { id: string; name: string } | null;
  local_model_loaded: boolean;
}

export interface Overview {
  version: string;
  os: string;
  settings: Settings;
  settings_error: string | null;
  keys: string[];
  setup: SetupStep;
  ram_bytes: number | null;
  local_available: boolean;
  /** Present on desktop only. */
  status?: StatusView;
}

export interface CmdError {
  message: string;
  key?: string;
}

export interface KeyProblem {
  slot: Slot;
  message: string;
}

export interface ModelInfo {
  id: string;
  name: string;
}

export interface SentEntry {
  at_ms: number;
  provider: string;
  model: string;
  body: string;
  truncated: boolean;
}

export interface Counts {
  shown: number;
  accepted: number;
  accepted_whole: number;
  dismissed: number;
  chars: number;
  keystrokes_saved: number;
  drafts: number;
}
export interface AppUsage extends Counts {
  app: string;
  name: string;
}
export interface DayUsage extends Counts {
  day: string;
}
export interface Usage {
  apps: AppUsage[];
  days: DayUsage[];
}

export interface SeenApp {
  id: string;
  name: string;
  last_seen: string;
}

export interface StyleSample {
  id: number;
  app: string;
  at_ms: number;
  text: string;
}

export interface LocalModel {
  id: string;
  name: string;
  license: string;
  size: number;
  min_ram_gb: number;
  tier: 'small' | 'standard' | 'comparison';
  installed: boolean;
  recommended: boolean;
  in_use: boolean;
}

export interface ModelsView {
  models: LocalModel[];
  ram_bytes: number | null;
  downloading: string | null;
  dir: string;
}

export interface DownloadProgress {
  id: string;
  downloaded: number;
  total: number;
}

export interface DownloadDone {
  id: string;
  error: string | null;
  cancelled: boolean;
}

export interface Timing {
  id: number;
  at_ms: number;
  wait_us: number;
  build_us: number;
  queue_us: number;
  decode_us: number;
  render_us: number;
  total_us: number;
  shown: boolean;
  cancelled: boolean;
}

export type PauseFor = { kind: 'minutes'; minutes: number } | { kind: 'until_resumed' };
export type PauseScope = { kind: 'all' } | { kind: 'app'; id: string; name: string };
