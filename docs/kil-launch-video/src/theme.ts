export const C = {
  bg: '#0b0d10',
  bgSoft: '#101317',
  panel: '#14171b',
  sidebar: '#16181d',
  term: '#111316',
  text: '#e3e6eb',
  muted: '#7d8490',
  accent: '#6ca8ff',
  accentDim: '#2f5f9e',
  failure: '#f06c6c',
  success: '#98c379',
  warn: '#e5c07b',
  divider: '#262930',
  chrome: '#f2f3f5',
};

export const SANS =
  '-apple-system, BlinkMacSystemFont, "SF Pro Display", "Helvetica Neue", sans-serif';
export const MONO = '"SF Mono", Menlo, "DejaVu Sans Mono", monospace';

export const ease = (
  frame: number,
  start: number,
  dur: number,
  easing: (t: number) => number = (t) => 1 - Math.pow(1 - t, 3),
) => {
  const t = Math.min(1, Math.max(0, (frame - start) / dur));
  return easing(t);
};

export const fade = (frame: number, start: number, dur: number) =>
  ease(frame, start, dur);
