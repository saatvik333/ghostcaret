import type { Chord, Key, LengthMode, Modifier, Trigger } from './types';

export function bytes(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(0)} MB`;
  return `${Math.max(1, Math.round(n / 1e3))} kB`;
}

export function percent(part: number, whole: number): string {
  return whole > 0 ? `${Math.round((part / whole) * 100)}%` : '–';
}

export function count(n: number): string {
  return n.toLocaleString();
}

export function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

export function dateTime(ms: number): string {
  return new Date(ms).toLocaleString([], { dateStyle: 'medium', timeStyle: 'medium' });
}

export function millis(us: number): string {
  return us >= 10_000 ? `${Math.round(us / 1000)} ms` : `${(us / 1000).toFixed(1)} ms`;
}

export const lengthLabels: Record<LengthMode, string> = {
  auto: 'Auto',
  word: 'Word',
  phrase: 'Phrase',
  sentence: 'Sentence',
  paragraph: 'Paragraph',
};

const mac = navigator.userAgent.includes('Mac');

const named: Record<string, string> = {
  tab: 'Tab', escape: 'Esc', enter: 'Enter', backspace: 'Backspace', space: 'Space',
  left: '←', right: '→', up: '↑', down: '↓', home: 'Home', end: 'End', page_up: 'Page Up', page_down: 'Page Down',
};

export function keyName(key: Key): string {
  if (typeof key === 'string') return named[key];
  if ('f' in key) return `F${key.f}`;
  return key.char === ' ' ? 'Space' : key.char;
}

export function chordName(chord: Chord): string {
  const parts: string[] = [];
  if (chord.mods.ctrl) parts.push(mac ? '⌃' : 'Ctrl');
  if (chord.mods.alt) parts.push(mac ? '⌥' : 'Alt');
  if (chord.mods.shift) parts.push(mac ? '⇧' : 'Shift');
  if (chord.mods.meta) parts.push(mac ? '⌘' : 'Super');
  parts.push(keyName(chord.key));
  return parts.join(mac ? '' : '+');
}

const modifierNames: Record<Modifier, string> = {
  left_alt: mac ? 'Left Option' : 'Left Alt',
  right_alt: mac ? 'Right Option' : 'Right Alt',
  left_ctrl: 'Left Ctrl',
  right_ctrl: 'Right Ctrl',
  left_shift: 'Left Shift',
  right_shift: 'Right Shift',
  left_meta: mac ? 'Left Command' : 'Left Super',
  right_meta: mac ? 'Right Command' : 'Right Super',
};

export function triggerName(t: Trigger): string {
  return 'chord' in t ? chordName(t.chord) : `Tap ${modifierNames[t.modifier_tap]}`;
}
