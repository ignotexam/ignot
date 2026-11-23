// The shapes of the files in an exam's folder, as the engine writes them. The pages only read these to show
// them; every check on them is made by the engine.
export type Track = "same-budget" | "unlimited";

export interface Generator {
  name: string;
  version: string;
  commit: string;
}

export interface Manifest {
  format: number;
  name: string;
  id: string;
  examId: string;
  title: string;
  openAt: string;
  openRound: number;
  drandChain: string;
  ciphertextSha256: string;
  plaintextSha256: string;
  questionCount: number;
  generator: Generator;
  chain?: { chainId: number; contract: string; sealTx?: string; publishTx?: string };
}

export interface Question {
  id: string;
  kind: string;
  level: number;
  prompt: string;
  answer: string;
  grade: string;
}

export interface Exam {
  format: number;
  id: string;
  title: string;
  createdAt: string;
  generator: Generator;
  seed: string;
  questions: Question[];
}

/** Money is counted in billionths of a US dollar, so that no sum rounds. */
export interface AnswerRecord {
  model: string;
  track: Track;
  questionId: string;
  raw: string;
  extracted: string;
  correct: boolean;
  costNanoUsd: number;
  promptTokens: number;
  completionTokens: number;
  maxTokens: number;
  latencyMs: number;
  finishReason: string | null;
  host?: string; // which of the model's hosts served the reply
  error?: string;
}

export interface AnswerFile {
  format: number;
  examId: string;
  model: string;
  records: AnswerRecord[];
}

export interface ModelScore {
  model: string;
  correct: number;
  total: number;
  costNanoUsd: number;
  costPerCorrectNanoUsd: number | null;
}

export interface Results {
  format: number;
  examId: string;
  budgetNanoUsd: number;
  tracks: { track: Track; models: ModelScore[] }[];
  leafCount: number;
  resultsRoot: string;
}

export interface Check {
  name: string;
  ok: boolean;
  skipped?: boolean;
  detail?: string;
}

export interface Report {
  ok: boolean;
  state: "sealed" | "open";
  checks: Check[];
  exam?: Exam;
}
