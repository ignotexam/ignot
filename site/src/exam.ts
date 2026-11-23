// One exam's page: its seal, a check that runs here, its results, and every question with every reply.
import "./nav";
import { board } from "./board";
import { $, DOT, byId, count, countdown, esc, exams, local, modelName, now, short, usd, utc } from "./data";
import type { AnswerRecord, Exam } from "./types";
import { fetchAnswers, verifyHere } from "./verify";

const entry = byId(new URLSearchParams(location.search).get("id")) ?? exams[exams.length - 1]!;
const m = entry.manifest;

document.title = `${m.title} - ignot`;
$("#title").textContent = m.title;
const state = $("#state");
const say = () => {
  const left = entry.opens - now();
  state.textContent =
    left > 0
      ? `Sealed. It opens ${utc(entry.opens)} (${local(entry.opens)} your time), in ${countdown(left)}.`
      : `Opened ${utc(entry.opens)}. ${m.questionCount} questions${entry.results ? `, ${count.format(entry.results.leafCount)} graded answers` : ""}.`;
};
say();
if (entry.opens > now()) setInterval(say, 1000);

$("#facts").innerHTML = (
  [
    ["Exam", m.id],
    ["Opens at", `drand round ${count.format(m.openRound)}${DOT}${m.openAt}`],
    ["Sealed file", `sha256 ${m.ciphertextSha256}`],
    ["Paper", `sha256 ${m.plaintextSha256}`],
    ["Beacon", `drand quicknet ${short("0x" + m.drandChain)}`],
    ["Generator", `${m.generator.name} ${m.generator.version}${DOT}commit ${m.generator.commit.slice(0, 12)}`],
    ...(entry.results ? [["Results root", entry.results.resultsRoot]] : []),
  ] as [string, string][]
)
  .map(([name, value]) => `<dt>${name}</dt><dd>${esc(value)}</dd>`)
  .join("");

const verify = $<HTMLButtonElement>("#verify");
verify.addEventListener("click", async () => {
  verify.disabled = true;
  await verifyHere(entry, $("#checks"));
  verify.disabled = false;
});

if (entry.results) {
  $("#results").hidden = false;
  board(entry, $("#results"));
}

// ---- the paper itself, once it is open: each question, its key, and each model's reply on each track
if (entry.openedUrl) {
  $("#paper").hidden = false;
  const [exam, files] = await Promise.all([fetch(entry.openedUrl).then((r) => r.json() as Promise<Exam>), fetchAnswers(entry)]);
  const replies = new Map<string, AnswerRecord[]>();
  for (const r of files.flatMap((f) => f.records)) replies.set(r.questionId, [...(replies.get(r.questionId) ?? []), r]);

  $("#questions").innerHTML = exam.questions
    .map((q) => {
      const rs = replies.get(q.id) ?? [];
      return (
        `<li id="${esc(q.id)}"><details><summary><b>${esc(q.id)}</b><span>${esc(q.kind)}, level ${Number(q.level)}</span><span>${esc(q.prompt.split("\n")[0]!.slice(0, 90))}</span>` +
        `<span class="tally">${rs.length ? `${rs.filter((r) => r.correct).length}/${rs.length} right` : ""}</span></summary>` +
        `<div class="q-body"><pre>${esc(q.prompt)}</pre><p class="key">Answer key: ${esc(q.answer)}</p>` +
        `<div class="cells">${rs.map((r, i) => `<button type="button" class="${r.correct ? "" : "wrong"}" data-q="${esc(q.id)}" data-i="${i}" aria-pressed="false">${modelName(r.model)}${DOT}${r.track === "unlimited" ? "no limit" : "same budget"}</button>`).join("")}</div>` +
        `<div class="reply"></div></div></details></li>`
      );
    })
    .join("");

  const named = location.hash.length > 1 ? document.getElementById(location.hash.slice(1))?.querySelector("details") : null;
  if (named) {
    named.open = true; // a link from the answer sheet names its question
    named.scrollIntoView({ block: "start" });
  }

  $("#questions").addEventListener("click", (e) => {
    const cell = (e.target as HTMLElement).closest<HTMLButtonElement>(".cells button");
    if (!cell) return;
    const body = cell.closest(".q-body")!;
    for (const b of body.querySelectorAll(".cells button")) b.setAttribute("aria-pressed", String(b === cell));
    const r = replies.get(cell.dataset.q!)![Number(cell.dataset.i)]!;
    body.querySelector(".reply")!.innerHTML =
      `<p>${r.correct ? "right" : r.error ? `no answer: ${esc(r.error)}` : "wrong"}${DOT}extracted "${esc(r.extracted)}"${DOT}${usd(r.costNanoUsd)}${DOT}${count.format(r.completionTokens)} tokens${DOT}${(r.latencyMs / 1000).toFixed(1)} s${r.host ? `${DOT}served by ${esc(r.host)}` : ""}</p>` +
      `<pre>${esc(r.raw) || "(empty reply)"}</pre>`;
  });
}
