// The engine: the same Rust code the command line runs, compiled to WebAssembly and run in the reader's browser.
// Nothing a reader seals, opens or checks here is sent anywhere. This file only fetches what the engine needs
// from outside (a drand relay's answer, a transaction receipt); the engine trusts none of it and checks all of it.
import type { ExamEntry } from "./data";
import type { Report } from "./types";

type Engine = typeof import("./wasm/ignot_wasm.js");
let loading: Promise<Engine> | undefined;

/** The module is about 1.3 MB, so it is fetched the first time something needs it and never before. */
export function engine(): Promise<Engine> {
  return (loading ??= import("./wasm/ignot_wasm.js").then(async (module) => {
    await module.default();
    return module;
  }));
}

const patient = () => AbortSignal.timeout(10_000);

/**
 * What a relay answers for a round: the first answer whose signature the engine accepts. Every relay is asked at
 * once, so one that never answers costs nothing; none of them is trusted.
 */
export async function fetchBeacon(round: number): Promise<string> {
  const { relayUrls, beaconVerifies } = await engine();
  const ask = async (url: string) => {
    const response = await fetch(url, { signal: patient() });
    const answer = response.ok ? await response.text() : "";
    if (!beaconVerifies(round, answer)) throw new Error(`${url} gave no signature of the round`);
    return answer;
  };
  try {
    return await Promise.any(relayUrls(round).map(ask));
  } catch {
    throw new Error("no drand relay gave the round");
  }
}

/** A transaction's receipt from a node of the chain, as text; "null" when the node knows no such transaction. */
async function fetchReceipt(node: string, chainId: number, tx: string): Promise<string> {
  const call = async (method: string, params: unknown[]) => {
    const response = await fetch(node, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
      signal: patient(),
    });
    const answer = (await response.json()) as { result?: unknown; error?: unknown };
    if (!response.ok || answer.error || answer.result === undefined) throw new Error("the node refused");
    return answer.result;
  };
  if (Number(await call("eth_chainId", [])) !== chainId) throw new Error("the node serves another chain");
  return JSON.stringify(await call("eth_getTransactionReceipt", [tx]));
}

const text = (url: string) =>
  fetch(url).then((response) => {
    if (!response.ok) throw new Error(`${url} did not load`);
    return response.text();
  });
const bytes = (url: string) =>
  fetch(url).then(async (response) => {
    if (!response.ok) throw new Error(`${url} did not load`);
    return new Uint8Array(await response.arrayBuffer());
  });

/** An exam's sealed file, byte for byte. */
export const sealedFile = (entry: ExamEntry) => bytes(entry.sealedUrl);

/** Fetch an exam's files as they are published, ask drand and the chain, and let the engine check everything. */
export async function verifyExam(entry: ExamEntry): Promise<Report> {
  const { verify, publicNode } = await engine();
  const reference = entry.manifest.chain;
  const node = reference ? publicNode(reference.chainId) : undefined;
  const receipt = (tx: string | undefined) => (tx && node && reference ? fetchReceipt(node, reference.chainId, tx) : undefined);
  const [manifest, sealed, exam, answers, results, beacon, chain] = await Promise.all([
    text(entry.manifestUrl),
    sealedFile(entry),
    entry.openedUrl ? text(entry.openedUrl) : undefined,
    entry.answerUrls.length ? Promise.all(entry.answerUrls.map(text)) : undefined,
    entry.resultsUrl ? text(entry.resultsUrl) : undefined,
    // The relays are asked whatever the clock of this device says: it may be behind, and a signature that
    // exists settles that the round has passed. One that cannot be fetched is a matter for the report: before
    // the round the exam is sealed, past it that is a failed check, and neither is a failed page.
    fetchBeacon(entry.manifest.openRound).catch(() => undefined),
    // A node that cannot be reached leaves the chain unread, and the report says so.
    Promise.all([receipt(reference?.sealTx), receipt(reference?.publishTx)]).then(
      ([seal, publish]) => (node ? { seal, publish } : undefined),
      () => undefined,
    ),
  ]);
  return JSON.parse(verify(JSON.stringify({ manifest, nowMs: Date.now(), beacon, exam, answers, results, chain }), sealed)) as Report;
}
