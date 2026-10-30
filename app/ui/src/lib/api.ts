// Typed wrappers for the Rust commands in app/src/commands.rs.
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  Bindings, CmdError, DownloadDone, DownloadProgress, KeyProblem, ModelInfo, ModelsView, Overview,
  PauseFor, PauseScope, ProviderConfig, SeenApp, SentEntry, Settings, SetupStep, StatusView, StyleSample, Timing,
  Usage,
} from './types';

/** Normalises a rejected invoke into a CmdError. */
export function asError(e: unknown): CmdError {
  if (e && typeof e === 'object' && 'message' in e) return e as CmdError;
  return { message: String(e) };
}

export const api = {
  overview: () => invoke<Overview>('overview'),
  saveSettings: (settings: Settings) => invoke<void>('save_settings', { settings }),
  checkKeys: (keys: Bindings) => invoke<KeyProblem[]>('check_keys', { keys }),
  setApiKey: (provider: string, key: string) => invoke<void>('set_api_key', { provider, key }),
  removeProvider: (provider: string) => invoke<Settings>('remove_provider', { provider }),
  testProvider: (config: ProviderConfig, key: string | null) => invoke<void>('test_provider', { config, key }),
  providerModels: (config: ProviderConfig, key: string | null) =>
    invoke<ModelInfo[]>('provider_models', { config, key }),
  sentLog: () => invoke<SentEntry[]>('sent_log'),
  clearSentLog: () => invoke<void>('clear_sent_log'),
  usage: (days: number) => invoke<Usage>('usage', { days }),
  clearUsage: () => invoke<void>('clear_usage'),
  seenApps: () => invoke<SeenApp[]>('seen_apps'),
  setSetupStep: (step: SetupStep) => invoke<void>('set_setup_step', { step }),
  pause: (scope: PauseScope, length: PauseFor) => invoke<void>('pause', { scope, length }),
  styleSamples: () => invoke<StyleSample[]>('style_samples'),
  deleteStyle: (id: number | null, app: string | null) => invoke<void>('delete_style', { id, app }),
  resume: (app: string | null) => invoke<void>('resume', { app }),
  timings: () => invoke<Timing[]>('timings'),
  localModels: () => invoke<ModelsView>('local_models'),
  downloadModel: (id: string) => invoke<void>('download_model', { id }),
  cancelDownload: () => invoke<void>('cancel_download'),
  deleteModel: (id: string) => invoke<void>('delete_model', { id }),
  checkModelFile: (path: string) => invoke<number>('check_model_file', { path }),
};

export const events = {
  status: (f: (s: StatusView) => void): Promise<UnlistenFn> => listen<StatusView>('status', (e) => f(e.payload)),
  modelProgress: (f: (p: DownloadProgress) => void): Promise<UnlistenFn> =>
    listen<DownloadProgress>('model-progress', (e) => f(e.payload)),
  modelDone: (f: (d: DownloadDone) => void): Promise<UnlistenFn> =>
    listen<DownloadDone>('model-done', (e) => f(e.payload)),
};
