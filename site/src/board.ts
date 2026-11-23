// The results table, shared by the home page and an exam's own page.
import { modelMaker, modelName, usd, type ExamEntry } from "./data";
import type { Track } from "./types";

/** `budget` is in billionths of a dollar, as the results file has it. */
export const TRACK_NOTES: Record<Track, (budget: number) => string> = {
  "same-budget": (budget) => `Every model gets $${budget / 1e9} for each question. A right answer that cost more does not count.`,
  unlimited: () => "Every model thinks at high effort, with up to 32,000 tokens for each question.",
};

/** Most right first; between equals, the cheaper right answer. */
const ranked = (entry: ExamEntry, track: Track) =>
  [...(entry.results!.tracks.find((t) => t.track === track)?.models ?? [])].sort(
    (a, b) => b.correct - a.correct || (a.costPerCorrectNanoUsd ?? Infinity) - (b.costPerCorrectNanoUsd ?? Infinity),
  );
const slug = (model: string) => model.replace(/[^a-z0-9]/gi, "-");

/** The same scores as a skyline: each model a monolith as tall as its right answers, the sphere over the cheapest
    right answer. A full score stands in the mark's own proportions. The table under it carries the numbers. */
export function skyline(entry: ExamEntry, track: Track): string {
  const models = ranked(entry, track);
  const cheapest = Math.min(...models.map((m) => m.costPerCorrectNanoUsd ?? Infinity));
  return models
    .map(
      (m, i) =>
        `<div class="tower${m.costPerCorrectNanoUsd === cheapest ? " best" : ""}" style="--s:${(m.correct / m.total).toFixed(3)};--i:${i};view-transition-name:tower-${slug(m.model)}">` +
        `<div class="lift"><i class="orb"></i><i class="slab"></i></div>` +
        `<p><b>${m.correct}</b><small>/${m.total}</small><span>${modelName(m.model)}</span><em>${m.costPerCorrectNanoUsd === null ? "none right" : usd(m.costPerCorrectNanoUsd)}</em></p></div>`,
    )
    .join("");
}

/** The cheapest right answer of all gets the sphere. */
export function boardTable(entry: ExamEntry, track: Track): string {
  const models = ranked(entry, track);
  if (!models.length) return `<p class="empty">No model has sat this track yet.</p>`;
  const cheapest = Math.min(...models.map((m) => m.costPerCorrectNanoUsd ?? Infinity));
  return (
    `<table><thead><tr><th></th><th>Model</th><th class="num">Right</th><th class="num">Spent</th><th class="num">For one right answer</th></tr></thead><tbody>` +
    models
      .map(
        (m, i) =>
          `<tr style="view-transition-name:row-${slug(m.model)}"><td class="rank">${i + 1}</td>` +
          `<td class="model"><b>${modelName(m.model)}</b><span>${modelMaker(m.model)}</span><i class="bar"><i style="width:${(m.correct / m.total) * 100}%"></i></i></td>` +
          `<td class="num"><data value="${m.correct}">${m.correct}</data><span>/${m.total}</span></td>` +
          `<td class="num">${usd(m.costNanoUsd)}</td>` +
          `<td class="num per${m.costPerCorrectNanoUsd === cheapest ? " best" : ""}">${m.costPerCorrectNanoUsd === null ? "none right" : usd(m.costPerCorrectNanoUsd)}</td></tr>`,
      )
      .join("") +
    `</tbody></table>`
  );
}

/** Wire a pair of track buttons to a table and its note. */
export function board(entry: ExamEntry, root: ParentNode) {
  const buttons = [...root.querySelectorAll<HTMLButtonElement>(".tracks button")];
  const show = (track: Track) => {
    for (const b of buttons) b.setAttribute("aria-pressed", String(b.dataset.track === track));
    root.querySelector(".track-note")!.textContent = TRACK_NOTES[track](entry.results!.budgetNanoUsd);
    root.querySelector(".table")!.innerHTML = boardTable(entry, track);
    const sky = root.querySelector(".skyline");
    if (sky) sky.innerHTML = skyline(entry, track);
  };
  // where the browser can, rows slide to their new rank and the pressed track slides across, instead of jumping
  const swap = (track: Track) => {
    if (!document.startViewTransition) return show(track);
    document.documentElement.classList.add("swap"); // a change inside the page, not a new page (sections.css)
    document.startViewTransition(() => show(track)).finished.finally(() => document.documentElement.classList.remove("swap"));
  };
  for (const b of buttons) b.addEventListener("click", () => swap(b.dataset.track as Track));
  show("same-budget");
}
