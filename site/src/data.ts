// Everything the pages know comes from the exam folders in this repository. The build lists them once (see
// vite.config.ts); the same list is served at /api/exams.json and the files themselves under /exams/.
import type { Manifest, Results, Track } from "./types";

declare const __EXAMS__: { exams: { manifest: Manifest; results: Results | null; files: string[] }[] };

// drand quicknet publishes one round every 3 seconds, counted from this second (the same numbers as the engine's;
// kept here so a page that only shows a clock does not load the engine). Every time on the site is derived from a
// round number, the way the verifier derives it.
const GENESIS = 1692803367;
const PERIOD = 3;
export const WEEK = 7 * 24 * 3600 * 1000;
export const roundTime = (round: number) => (GENESIS + (round - 1) * PERIOD) * 1000;
export const roundAt = (ms: number) => Math.floor((ms / 1000 - GENESIS) / PERIOD) + 1;
/** The first round published at or after a moment: the round a seal to that moment is locked to, as the engine counts it. */
export const roundAtOrAfter = (ms: number) => Math.ceil(Math.max(0, ms - GENESIS * 1000) / (PERIOD * 1000)) + 1;

// On a developer's machine a page can be viewed at another moment (?at=2026-11-24T13:59:50Z). Never in a build.
const skew = import.meta.env.DEV ? (Date.parse(new URLSearchParams(location.search).get("at") ?? "") || Date.now()) - Date.now() : 0;
export const now = () => Date.now() + skew;

export interface ExamEntry {
  manifest: Manifest;
  opens: number; // ms, from the round
  manifestUrl: string;
  sealedUrl: string;
  openedUrl?: string; // exam.json, present once the exam has been opened and committed
  results?: Results;
  resultsUrl?: string;
  answerUrls: string[];
}

export const exams: ExamEntry[] = __EXAMS__.exams
  .map(({ manifest, results, files }) => {
    // Each file is taken by its exact place in the exam's folder, the place the command line reads it from.
    const folder = `/exams/${manifest.id}/`;
    const file = (name: string) => files.find((f) => f === folder + name);
    return {
      manifest,
      opens: roundTime(manifest.openRound),
      manifestUrl: folder + "manifest.json",
      sealedUrl: folder + "exam.age",
      openedUrl: file("exam.json"),
      results: results ?? undefined,
      resultsUrl: file("results.json"),
      answerUrls: files.filter((f) => f.startsWith(folder + "answers/") && !f.slice(folder.length + 8).includes("/")),
    };
  })
  .sort((a, b) => a.opens - b.opens);

// The latest exam that has been opened and run, reduced to what the home page draws (see `sheet` in vite.config.ts).
export interface Run {
  model: string;
  track: Track;
  marks: string; // one letter a question: r right, w wrong, n no answer
  costs: number[]; // billionths of a dollar
}
declare const __SHEET__: {
  id: string;
  title: string;
  questions: { id: string; kind: string; level: number; prompt?: string; cut?: boolean; answer?: string }[];
  runs: Run[];
} | null;
export const sheet = __SHEET__;

export const byId = (id: string | null) => exams.find((e) => e.manifest.id === id);
/** The next exam to open, or the latest one that has. */
export const upcoming = () => exams.find((e) => e.opens > now()) ?? exams[exams.length - 1]!;
/** The latest opened exam with published results. An exam that is still sealed has none to show. */
export const latestResults = () => [...exams].reverse().find((e) => e.results && e.opens <= now());

// ---- words
const NAMES: Record<string, string> = {
  "anthropic/claude-fable-5.2": "Claude Fable 5.2",
  "anthropic/claude-haiku-5.5": "Claude Haiku 5.5",
  "deepseek/deepseek-v4.1-flash": "DeepSeek V4.1 Flash",
  "google/gemini-4-neon": "Gemini 4 Neon",
  "minimax/minimax-m3.1-flash": "MiniMax M3.1 Flash",
  "openai/gpt-6-luna": "GPT-6 Luna",
  "openai/gpt-6.1-terra": "GPT-6.1 Terra",
  "qwen/qwen3.8-flash": "Qwen3.8 Flash",
  "z-ai/glm-5.3-flash": "GLM-5.3 Flash",
};
const MAKERS: Record<string, string> = { anthropic: "Anthropic", deepseek: "DeepSeek", google: "Google", minimax: "MiniMax", openai: "OpenAI", qwen: "Alibaba", "z-ai": "Z.ai" };
/** Text from a file, made safe to write into a page. What the pages draw from the exam folders goes through it. */
export const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
/** A model's everyday name; an id we have no name for is shown as it is. */
export const modelName = (id: string) => (Object.hasOwn(NAMES, id) ? NAMES[id]! : esc(id.slice(id.indexOf("/") + 1)));
export const modelMaker = (id: string) => {
  const maker = id.slice(0, id.indexOf("/"));
  return Object.hasOwn(MAKERS, maker) ? MAKERS[maker]! : esc(maker);
};

export const DOT = ` ${String.fromCharCode(183)} `; // the middle dot between facts on one line
export const count = new Intl.NumberFormat("en-US");
export const pad = (n: number) => String(n).padStart(2, "0");
export const short = (hex: string) => `${hex.slice(0, 10)}...${hex.slice(-6)}`;
/** A price kept in billionths of a dollar. Small prices keep four decimals so $0.0022 and $0.0037 stay different. */
export const usd = (nano: number) => `$${(nano / 1e9).toFixed(nano < 1e9 ? 4 : 2)}`;

const part = (ms: number, options: Intl.DateTimeFormatOptions, timeZone?: string) => new Intl.DateTimeFormat("en-GB", { ...options, timeZone }).format(ms);
const HM: Intl.DateTimeFormatOptions = { hour: "2-digit", minute: "2-digit", hourCycle: "h23" };
/** "Tue 24 Nov, 14:00 UTC" */
export const utc = (ms: number) => `${part(ms, { weekday: "short" }, "UTC")} ${part(ms, { day: "numeric", month: "short" }, "UTC")}, ${part(ms, HM, "UTC")} UTC`;
/** "Tue 21:00" in the reader's own time zone */
export const local = (ms: number) => `${part(ms, { weekday: "short" })} ${part(ms, HM)}`;
/** "2d 02:39:27", or "02:39:27" in the last day (`withDays` keeps the day part even when it is 0) */
export function countdown(ms: number, withDays = false) {
  const s = Math.max(0, Math.floor(ms / 1000));
  const days = Math.floor(s / 86400);
  return `${days || withDays ? `${days}d ` : ""}${pad(Math.floor(s / 3600) % 24)}:${pad(Math.floor(s / 60) % 60)}:${pad(s % 60)}`;
}

export const $ = <T extends HTMLElement = HTMLElement>(selector: string, root: ParentNode = document) => root.querySelector<T>(selector)!;
/** A drawn tick or cross; the page uses no glyphs for icons. */
export const icon = (ok: boolean) =>
  `<svg class="mark" viewBox="0 0 13 13" aria-hidden="true"><path d="${ok ? "M1.5 7l3.3 3.3L11.5 3" : "M2.5 2.5l8 8M10.5 2.5l-8 8"}" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="square"/></svg>`;
