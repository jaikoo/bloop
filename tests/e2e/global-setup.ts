import { execFileSync, spawn, ChildProcess } from 'child_process';
import { writeFileSync, mkdtempSync, existsSync } from 'fs';
import { tmpdir } from 'os';
import { join, resolve } from 'path';
import { createHash, randomBytes } from 'crypto';
import { seed, seedPostFlush } from './seed';

const STATE_FILE = join(__dirname, '.e2e-state.json');
const BLOOP_ROOT = resolve(__dirname, '../..');

interface E2EState {
  pid: number;
  port: number;
  tmpDb: string;
  baseUrl: string;
  hmacSecret: string;
  sessionToken: string;
}

async function waitForHealth(url: string, timeoutMs = 30_000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const r = await fetch(url);
      if (r.ok) return;
    } catch {}
    await new Promise(r => setTimeout(r, 300));
  }
  throw new Error(`Server did not become healthy within ${timeoutMs}ms`);
}

function findFreePort(): number {
  return 10000 + Math.floor(Math.random() * 50000);
}

/**
 * Insert a test user and session directly into SQLite.
 * Returns the plaintext session token (for the cookie).
 */
async function createTestSession(baseUrl: string, tmpDb: string): Promise<string> {
  // Generate a session token matching bloop's format:
  // 32 random bytes → base64url (no padding) → SHA-256 hash stored in DB
  const tokenBytes = randomBytes(32);
  const token = tokenBytes.toString('base64url'); // plaintext for cookie
  const tokenHash = createHash('sha256').update(token).digest('hex');

  const userId = 'e2e-test-user-id';
  const now = Math.floor(Date.now() / 1000);
  const expiresAt = now + 86400; // 24 hours

  // Use sqlite3 CLI to insert (available on macOS)
  execFileSync('sqlite3', [tmpDb,
    `INSERT INTO webauthn_users (id, username, display_name, created_at) VALUES ('${userId}', 'e2e-test', 'E2E Test User', ${now});`,
  ]);
  execFileSync('sqlite3', [tmpDb,
    `INSERT INTO sessions (token, user_id, created_at, expires_at) VALUES ('${tokenHash}', '${userId}', ${now}, ${expiresAt});`,
  ]);

  return token;
}

export default async function globalSetup() {
  // Build bloop
  const rawBin = process.env.BLOOP_BIN || join(BLOOP_ROOT, 'target/debug/bloop');
  const bloopBin = resolve(rawBin);
  if (!process.env.BLOOP_BIN) {
    console.log('Building bloop with llm-tracing...');
    execFileSync('cargo', ['build', '--features', 'llm-tracing'], {
      cwd: BLOOP_ROOT,
      stdio: 'inherit',
    });
  }

  if (!existsSync(bloopBin)) {
    throw new Error(`bloop binary not found at ${bloopBin}`);
  }

  // Create temp DB
  const tmpDir = mkdtempSync(join(tmpdir(), 'bloop-e2e-'));
  const tmpDb = join(tmpDir, 'bloop-e2e.db');

  const port = findFreePort();
  const hmacSecret = `e2e-test-secret-playwright-long-key-${Date.now()}`;

  console.log(`Starting bloop on port ${port}...`);

  const child: ChildProcess = spawn(bloopBin, [], {
    cwd: BLOOP_ROOT,
    env: {
      ...process.env,
      BLOOP__DATABASE__PATH: tmpDb,
      BLOOP__AUTH__HMAC_SECRET: hmacSecret,
      BLOOP__SERVER__PORT: String(port),
      BLOOP__LLM_TRACING__ENABLED: 'true',
      BLOOP__LLM_TRACING__DEFAULT_CONTENT_STORAGE: 'full',
      BLOOP__LLM_TRACING__FLUSH_INTERVAL_SECS: '1',
      BLOOP__LLM_TRACING__FLUSH_BATCH_SIZE: '50',
      BLOOP__PIPELINE__FLUSH_INTERVAL_SECS: '1',
      BLOOP__RETENTION__PRUNE_INTERVAL_SECS: '999999',
      RUST_LOG: 'bloop=warn',
    },
    stdio: 'pipe',
    detached: true,
  });

  let stderrOutput = '';
  child.stderr?.on('data', (data: Buffer) => {
    stderrOutput += data.toString();
    if (process.env.DEBUG) process.stderr.write(`[bloop] ${data}`);
  });
  child.on('exit', (code) => {
    if (code !== null && code !== 0) {
      console.error(`bloop exited with code ${code}\nstderr: ${stderrOutput}`);
    }
  });

  const baseUrl = `http://localhost:${port}`;

  // Wait for server to be ready
  await waitForHealth(`${baseUrl}/health`);
  console.log('Server ready!');

  // Create a test user + session in the DB (server already created tables)
  const sessionToken = await createTestSession(baseUrl, tmpDb);
  console.log('Test session created');

  // Seed test data using the legacy HMAC path
  await seed({ baseUrl, hmacSecret, sessionToken });

  // Wait for flush (flush_interval_secs=1, give it 3s to be safe)
  console.log('Waiting for data flush...');
  await new Promise(r => setTimeout(r, 3000));

  // Seed data that requires traces to exist in SQLite
  await seedPostFlush({ baseUrl, hmacSecret, sessionToken });

  // Write state for tests and teardown
  const state: E2EState = {
    pid: child.pid!,
    port,
    tmpDb,
    baseUrl,
    hmacSecret,
    sessionToken,
  };
  writeFileSync(STATE_FILE, JSON.stringify(state, null, 2));

  // Set env for Playwright
  process.env.BLOOP_TEST_URL = baseUrl;
}
