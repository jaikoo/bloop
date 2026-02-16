import { createHmac } from 'crypto';

export interface SeedConfig {
  baseUrl: string;
  hmacSecret: string;     // legacy HMAC secret for trace ingestion
  sessionToken: string;   // session cookie for query/mutation endpoints
}

function sign(secret: string, body: string): string {
  return createHmac('sha256', secret).update(body).digest('hex');
}

/** POST with HMAC auth (for /v1/traces ingest) */
async function hmacPost(config: SeedConfig, path: string, body: object) {
  const json = JSON.stringify(body);
  const sig = sign(config.hmacSecret, json);
  const resp = await fetch(`${config.baseUrl}${path}`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'X-Signature': sig,
      'X-Forwarded-For': '127.0.0.1',
    },
    body: json,
  });
  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(`POST ${path} failed (${resp.status}): ${text}`);
  }
  return resp.json();
}

/** Fetch with session cookie auth (for query/mutation endpoints) */
async function sessionFetch(config: SeedConfig, path: string, opts: RequestInit = {}) {
  const resp = await fetch(`${config.baseUrl}${path}`, {
    ...opts,
    headers: {
      ...opts.headers as Record<string, string>,
      'Cookie': `bloop_session=${config.sessionToken}`,
      'X-Forwarded-For': '127.0.0.1',
    },
  });
  return resp;
}

export async function seed(config: SeedConfig) {
  // ── Trace 1: Successful trace with nested spans ──
  await hmacPost(config, '/v1/traces', {
    id: 'trace-e2e-001',
    name: 'chat-completion-e2e',
    status: 'completed',
    session_id: 'session-e2e-1',
    user_id: 'e2e-user',
    spans: [
      {
        id: 'span-e2e-root',
        span_type: 'generation',
        name: 'root-generation',
        model: 'gpt-4o',
        provider: 'openai',
        input_tokens: 200,
        output_tokens: 100,
        cost: 0.005,
        latency_ms: 1500,
        time_to_first_token_ms: 200,
        status: 'ok',
        input: 'Explain quantum computing',
        output: 'Quantum computing uses qubits...',
      },
      {
        id: 'span-e2e-child1',
        parent_span_id: 'span-e2e-root',
        span_type: 'tool',
        name: 'search-tool',
        model: 'gpt-4o',
        provider: 'openai',
        input_tokens: 50,
        output_tokens: 30,
        cost: 0.001,
        latency_ms: 500,
        status: 'ok',
      },
      {
        id: 'span-e2e-child2',
        parent_span_id: 'span-e2e-root',
        span_type: 'retrieval',
        name: 'doc-retrieval',
        input_tokens: 20,
        output_tokens: 80,
        cost: 0.0005,
        latency_ms: 300,
        status: 'ok',
      },
    ],
  });

  // ── Trace 2: Error trace in same session ──
  await hmacPost(config, '/v1/traces', {
    id: 'trace-e2e-002',
    name: 'failed-generation',
    status: 'error',
    session_id: 'session-e2e-1',
    user_id: 'e2e-user',
    spans: [
      {
        id: 'span-e2e-err',
        span_type: 'generation',
        name: 'error-span',
        model: 'claude-3-opus',
        provider: 'anthropic',
        input_tokens: 150,
        output_tokens: 0,
        cost: 0.003,
        latency_ms: 5000,
        status: 'error',
        error_message: 'Rate limit exceeded',
      },
    ],
  });

  // ── Trace 3: Trace with prompt_name/prompt_version v1 ──
  await hmacPost(config, '/v1/traces', {
    id: 'trace-e2e-003',
    name: 'summarize-article',
    status: 'completed',
    prompt_name: 'summarizer',
    prompt_version: '1',
    spans: [
      {
        id: 'span-e2e-prompt1',
        span_type: 'generation',
        name: 'summarize-gen',
        model: 'gpt-4o',
        provider: 'openai',
        input_tokens: 500,
        output_tokens: 150,
        cost: 0.008,
        latency_ms: 900,
        status: 'ok',
        input: 'Summarize: The field of quantum computing...',
        output: 'Summary: Quantum computing leverages...',
      },
    ],
  });

  // ── Trace 4: Second version of same prompt ──
  await hmacPost(config, '/v1/traces', {
    id: 'trace-e2e-004',
    name: 'summarize-article-v2',
    status: 'completed',
    prompt_name: 'summarizer',
    prompt_version: '2',
    spans: [
      {
        id: 'span-e2e-prompt2',
        span_type: 'generation',
        name: 'summarize-gen-v2',
        model: 'gpt-4o',
        provider: 'openai',
        input_tokens: 400,
        output_tokens: 120,
        cost: 0.006,
        latency_ms: 700,
        status: 'ok',
      },
    ],
  });

  console.log('Seed data ingested: 4 traces, 6 spans');
}

/**
 * Seed data that depends on traces existing in SQLite (call after flush wait).
 * Uses session auth for mutation endpoints.
 */
export async function seedPostFlush(config: SeedConfig) {
  // ── Feedback on trace 1 ──
  const fbResp = await sessionFetch(config, '/v1/llm/traces/trace-e2e-001/feedback', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ user_id: 'e2e-tester', value: 1, comment: 'Good response' }),
  });
  if (!fbResp.ok) {
    console.warn(`Feedback seed failed: ${fbResp.status} ${await fbResp.text()}`);
  }

  // ── Scores on trace 1 ──
  for (const score of [
    { name: 'quality', value: 0.85 },
    { name: 'relevance', value: 0.72 },
  ]) {
    const scoreResp = await sessionFetch(config, '/v1/llm/traces/trace-e2e-001/scores', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(score),
    });
    if (!scoreResp.ok) {
      console.warn(`Score seed (${score.name}) failed: ${scoreResp.status} ${await scoreResp.text()}`);
    }
  }

  // ── Budget ──
  const budgetResp = await sessionFetch(config, '/v1/llm/budget', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ monthly_budget_micros: 50_000_000, alert_threshold_pct: 80 }),
  });
  if (!budgetResp.ok) {
    console.warn(`Budget seed failed: ${budgetResp.status} ${await budgetResp.text()}`);
  }

  console.log('Post-flush seed: 1 feedback, 2 scores, 1 budget');
}
