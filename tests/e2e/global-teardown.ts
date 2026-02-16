import { readFileSync, unlinkSync, existsSync, rmSync } from 'fs';
import { join, dirname } from 'path';

const STATE_FILE = join(__dirname, '.e2e-state.json');

export default async function globalTeardown() {
  if (!existsSync(STATE_FILE)) {
    console.log('No state file found, nothing to clean up.');
    return;
  }

  const state = JSON.parse(readFileSync(STATE_FILE, 'utf-8'));

  // Kill the server
  if (state.pid) {
    try {
      process.kill(state.pid, 'SIGTERM');
      await new Promise(r => setTimeout(r, 500));
      try { process.kill(state.pid, 'SIGKILL'); } catch {}
    } catch {
      // Process already exited
    }
    console.log(`Stopped bloop server (PID ${state.pid})`);
  }

  // Clean up temp DB files
  if (state.tmpDb) {
    for (const suffix of ['', '-wal', '-shm']) {
      const f = state.tmpDb + suffix;
      try { unlinkSync(f); } catch {}
    }
    try { rmSync(dirname(state.tmpDb), { recursive: true }); } catch {}
  }

  // Remove state file
  try { unlinkSync(STATE_FILE); } catch {}
  console.log('Cleanup complete.');
}
