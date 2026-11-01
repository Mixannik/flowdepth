// Wait for the selected test process to exit normally, snapshot, then rebuild/relaunch.
import { openSync, closeSync, writeFileSync, unlinkSync, existsSync, mkdirSync, copyFileSync } from 'node:fs';
import { resolve, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { setTimeout } from 'node:timers/promises';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const pid = Number(process.argv[2]);
if (!Number.isSafeInteger(pid) || pid <= 0 || process.argv.length !== 3) throw new Error('Usage: node scripts/rebuild-test-when-closed.mjs <test-process-pid>');
const local = join(repo, '.local');
mkdirSync(local, { recursive: true });
const lock = join(local, 'test-rebuild-waiter.lock');
const fd = openSync(lock, 'wx'); // A second waiting updater must not start.
try {
  writeFileSync(fd, String(process.pid));
  const deadline = Date.now() + 12 * 60 * 60 * 1000;
  const running = () => {
    try { process.kill(pid, 0); return true; }
    catch (error) { if (error.code === 'ESRCH') return false; throw error; }
  };
  console.log(`Waiting for test process ${pid} to close normally; no stop signal will be sent.`);
  while (running()) {
    if (Date.now() > deadline) throw new Error('Timed out waiting for the test terminal');
    await setTimeout(2000);
  }
  await setTimeout(2000);
  const stamp = new Date().toISOString().replace(/[:.]/g, '-');
  const snapshot = join(local, 'snapshots', stamp);
  mkdirSync(snapshot, { recursive: true });
  const state = join(local, 'test-profile', 'saved-state.json');
  const exe = join(repo, 'target', 'release', process.platform === 'win32' ? 'flowsurface.exe' : 'flowsurface');
  if (existsSync(state)) copyFileSync(state, join(snapshot, 'saved-state.json'));
  if (existsSync(exe)) copyFileSync(exe, join(snapshot, process.platform === 'win32' ? 'flowsurface.exe' : 'flowsurface'));
  console.log(`Snapshot: ${snapshot}`);
  const result = spawnSync(process.execPath, [join(repo, 'scripts', 'test-profile.mjs')], {
    cwd: repo, stdio: 'inherit', windowsHide: true,
    env: { ...process.env, FLOWSURFACE_LOG_LEVEL: 'info' },
  });
  if (result.error || result.status !== 0) throw new Error('Test rebuild/relaunch failed', { cause: result.error });
  console.log('Test rebuild/relaunch completed.');
} finally {
  closeSync(fd);
  unlinkSync(lock);
}
