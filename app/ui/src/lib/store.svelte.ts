// The settings working copy and app-wide state. Controls edit `app.settings`
// directly and call `save()`; a rejected save keeps the user's values and puts
// the message beside the control the error names.
import { api, asError, events } from './api';
import { announce } from './announce.svelte';
import type { DownloadProgress, Overview, Settings, StatusView } from './types';

class AppStore {
  overview = $state<Overview | null>(null);
  settings = $state<Settings | null>(null);
  status = $state<StatusView | null>(null);
  /** Error text by settings key (`boldness`, `keys.take_all`, …). */
  errors = $state<Record<string, string>>({});
  /** Provider ids with a stored key. */
  keys = $state<string[]>([]);
  download = $state<DownloadProgress | null>(null);
  /** Unsaved drafts by name, so closing the window can ask first. */
  drafts = $state<Record<string, string>>({});

  #saving: Promise<void> = Promise.resolve();

  async load() {
    const o = await api.overview();
    this.overview = o;
    this.settings = o.settings;
    this.keys = o.keys;
    this.status = o.status ?? null;
    void events.status((s) => (this.status = s));
    void events.modelProgress((p) => (this.download = p));
    void events.modelDone(() => (this.download = null));
  }

  get desktop() {
    return this.overview?.status !== undefined;
  }

  /** Saves the working copy. Resolves true when it was accepted. */
  save(message = 'Saved'): Promise<boolean> {
    const run = this.#saving.then(async () => {
      if (!this.settings) return false;
      try {
        await api.saveSettings($state.snapshot(this.settings) as Settings);
        this.errors = {};
        if (this.overview) this.overview.settings_error = null;
        announce(message);
        return true;
      } catch (e) {
        const err = asError(e);
        this.errors = { [err.key ?? 'general']: err.message };
        announce(`Not saved: ${err.message}`);
        return false;
      }
    });
    this.#saving = run.then(() => undefined);
    return run;
  }

  /** Replaces the working copy after a command changed settings itself. */
  adopt(settings: Settings) {
    this.settings = settings;
    this.errors = {};
  }

  setDraft(name: string, label: string | null) {
    if (label === null) delete this.drafts[name];
    else this.drafts[name] = label;
  }
}

export const app = new AppStore();
