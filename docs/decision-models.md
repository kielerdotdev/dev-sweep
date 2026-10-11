# Decision models for cleanup

Research checked on 2026-10-11. No paid provider evaluation or cleanup of real user files was performed for this comparison.

## Why GPT-4.1 mini was the wrong default

The original integration used a generic chat model to return JSON advice. It reviewed at most eight manually selected items and could not build a draft selection. Changing the model name alone would not fix it: decision models use a different request and response contract.

OpenRouter exposes `POST /api/alpha/decisions`, using the same OpenRouter API key and billing. Requests contain `model`, factual `state`, and typed `questions`. Choice returns one option and its probability distribution; Noul evaluates independent predicates; Score evaluates an ordered rubric. These models do not write explanations or shell commands. `typesafe/jev-router` is a chat model router, not the classifier needed here.

Sources: [Decisions API](https://openrouter.ai/docs/api/api-reference/alphadecisions/submit-a-decisions-request), [Jev integration](https://openrouter.ai/docs/guides/community/jev).

## Model choice

The live [decision-model catalog](https://openrouter.ai/api/v1/models?output_modalities=decisions) lists 19 entries, including aliases and free variants. The default chat-model catalog omits these models. Prices below are input-token prices checked during this research, not guaranteed future prices.

| Model | Input price per million tokens | Fit and limits |
| --- | --- | --- |
| Cloudflare Clef | $0.24 | Provisional default for metadata classification; vendor reports strong out-of-domain rejection. |
| Clef Flash | $0.038 | Faster and cheaper; vendor results show materially weaker rejection on some benchmarks. |
| TypeSafe Jev 1.13 | $0.042 | Useful alternative; typed decisions and substantially lower listed price. |
| Nace DREX 1.5 | $0.04 | Available competitor; no cleanup-specific evidence found. |
| Microsoft Decision 1 | $0.042 | Available competitor; no cleanup-specific evidence found. |
| Perplexity Decider 1.1 27B | $0.02 | Available competitor; no cleanup-specific evidence found. |

Cloudflare's published comparison reports median inference times of 209.3 ms for Clef, 38.8 ms for Flash, and 524.1 ms for Jev across its test suite. Clef scores 97.43 on CLINC150 plus out-of-scope detection versus Flash's 66.77 and Jev's 89.27; Jev wins When2Call, 80.97 versus Clef's 72.37. These are vendor benchmarks, not measurements of this program, and they do not establish a winner for filesystem cleanup. Live OpenRouter provider latency is a different measurement and should not be compared as if it were the same test.

Sources: [Cloudflare announcement](https://blog.cloudflare.com/clef-decision-models/), [benchmark changelog](https://developers.cloudflare.com/changelog/post/2026-10-01-clef-workers-ai/), [Clef listing](https://openrouter.ai/cloudflare/clef), [Jev listing](https://openrouter.ai/typesafe/jev-1.13).

Clef is a provisional default because this task benefits from conservative rejection of unfamiliar cases. That is an inference from vendor results, not a verified cleanup accuracy claim. Jev is a sensible alternative to evaluate alongside it. The app records a returned resolved model name when available. Catalog aliases can change; a model upgrade needs another evaluation.

## Evidence and context limits

OpenRouter documents that the Cloudflare decision provider reads roughly the first 2,000 text state tokens and silently truncates the rest, despite a much larger advertised context window. Dumping an entire inventory into one request risks dropping the evidence that should prevent deletion.

The app sends one compact item per request, removes the potentially enormous list of bundled artifact paths, and retains its count, cleanup scope, action, age evidence, blockers, consequences, and uncertainty notices. Evidence longer than 1,800 serialized UTF-8 bytes is rejected locally. This conservative byte guard is not an exact tokenizer measurement. Nothing is shortened silently. Four workers bound simultaneous requests; this is concurrency, not a single giant batch prompt.

Source: [OpenRouter multimodal decisions and text limit](https://openrouter.ai/docs/guides/community/multimodal-decisions).

Rust checks the facts. Models classify the remaining uncertainty. Git cleanliness and reachability, filesystem identity, path exclusions, activity, bytes, and overlapping selections do not become model judgments. Paths and names are untrusted data. Global npm, Bun, and pnpm data is protected before a request is considered.

## Confidence is not deletion safety

A three-way choice fits the workflow: propose regenerable data, investigate, or retain. A single “safe to delete” boolean hides unknown recovery requirements and user intent. The model has not inspected source files, backups, or every process.

TypeSafe documents Choice confidence as a normalized maximum probability, `(p_max - 1/n) / (1 - 1/n)`. A confidence of 95% is not evidence that deletion is safe 95% of the time. A universal numeric approval threshold would be invented. This implementation uses no such threshold: model agreement produces a draft only, with explicit user adoption and separate deletion approval. Tied choices and malformed answers are not proposed. Additional providers can reject a draft item; agreement is not proof that their failures are independent.

Source: [TypeSafe confidence definitions](https://docs.typesafe.ai/confidence).

An independent USC preprint reports that option labels can override written criteria in tested open implementations, sometimes with misleading confidence. It does not test hosted Clef or Jev, so its findings cannot be assigned directly to those services. It does justify adversarial label and pathname tests before relying on any classifier.

Source: [Decision model prompt sensitivity preprint](https://arxiv.org/abs/2610.02586).

## Approval workflow

1. Scan locally; choose a category or search if needed.
2. Press `p`; inspect metadata and approve sending it.
3. Review the draft. Failed, conflicting, blocked, and persistent-store cases remain outside it.
4. Enter adopts the proposal; Space removes exceptions. Esc cancels without changing existing selections.
5. Press `r`, review exact actions, and type the required confirmation.
6. Recheck local guards immediately before every action.

The daily task remains report-only and makes no model requests. Proposal generation never starts automatically. Worktree proposals include their generated children once; child paths are not separately selected. Native package stores and Docker volumes still require manual review.

## What should be evaluated next

Build a labeled set from representative metadata before claiming one model is best. Include old clean worktrees, dirty or unpushed commits, locks, unknown ignored files, environment files, keep markers, recent activity, missing timestamps, symlinks, global Node data, stopped-container references, persistent databases and volumes, parent/child overlap, hostile pathnames, and evidence near the context limit. Include invalid responses and provider failures in integration tests.

Measure dangerous proposal false positives first, then proposal coverage, recovery classification, latency, cost, and calibration. Repeat with changed labels and model versions. User approval remains required even after evaluation. Clef's RL program would need a real labeled workload; fine-tuning before collecting that evidence is premature.
