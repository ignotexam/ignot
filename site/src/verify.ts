// The verifier, run in the reader's browser: the same code the command line runs, on the same files.
import { esc, icon, type ExamEntry } from "./data";
import { verifyExam } from "./engine";
import type { AnswerFile } from "./types";

export const fetchAnswers = (entry: ExamEntry) => Promise.all(entry.answerUrls.map((url) => fetch(url).then((r) => r.json() as Promise<AnswerFile>)));

/** Fetch an exam's files, ask drand, and write every check into `list`. */
export async function verifyHere(entry: ExamEntry, list: HTMLElement) {
  list.innerHTML = `<li class="skip">Fetching the files and asking drand for the round...</li>`;
  try {
    const report = await verifyExam(entry);
    // A check that could not be made is not a check that passed, and the last line says how many there were.
    const skipped = report.checks.filter((c) => c.skipped).length;
    const verdict = !report.ok ? "A check failed." : skipped === 0 ? "Every check passes." : `No check failed. ${skipped === 1 ? "One could" : `${skipped} could`} not be made.`;
    list.innerHTML =
      report.checks
        .map((c, i) => `<li class="${c.skipped ? "skip" : c.ok ? "" : "bad"}" style="--i:${i}">${c.skipped ? "not checked: " : icon(c.ok)}${esc(c.name)}${c.detail ? `: ${esc(c.detail)}` : ""}</li>`)
        .join("") + `<li class="${report.ok ? "" : "bad"}" style="--i:${report.checks.length}"><b>${verdict}</b></li>`;
  } catch {
    list.innerHTML = `<li class="bad">${icon(false)}The files did not load. Check your connection and try again.</li>`;
  }
}
