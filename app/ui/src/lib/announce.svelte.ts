// One polite live region for results ("Saved", "Test passed", "Download
// finished"). Errors are shown beside their control as well.

export const live = $state({ message: '', seq: 0 });

export function announce(message: string) {
  // A changed counter re-renders the region even for a repeated message.
  live.message = message;
  live.seq += 1;
}
