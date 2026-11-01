// Prepare an isolated copy; optionally build and launch it. Never copy the cache.
import { existsSync, mkdirSync, copyFileSync, constants, openSync, closeSync } from 'node:fs';
import { dirname, resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { homedir } from 'node:os';
import { spawnSync, spawn } from 'node:child_process';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const profile = join(repo, '.local', 'test-profile');
const args = process.argv.slice(2);
if (args.some(arg => arg !== '--prepare-only')) throw new Error('Usage: node scripts/test-profile.mjs [--prepare-only]');
mkdirSync(profile, { recursive: true });
const source = join(process.env.APPDATA ?? join(homedir(), '.local', 'share'), 'flowsurface', 'saved-state.json');
const state = join(profile, 'saved-state.json');
if (!existsSync(state) && existsSync(source)) {
  copyFileSync(source, state, constants.COPYFILE_EXCL);
  console.log('Copied layout snapshot; source unchanged.');
}
console.log(`Test profile: ${profile}`);
if (!args.includes('--prepare-only')) {
  const cargo = join(homedir(), '.cargo', 'bin', process.platform === 'win32' ? 'cargo.exe' : 'cargo');
  const logPath = join(repo, 'test-profile-build.log');
  const log = openSync(logPath, 'w');
  let built;
  try {
    console.log(`Building; output: ${logPath}`);
    built = spawnSync(cargo, ['build', '--release', '--locked'], { cwd: repo, stdio: ['ignore', log, log], windowsHide: true });
  } finally { closeSync(log); }
  if (built.error || built.status !== 0) throw new Error(`Build failed. See ${logPath}`, { cause: built.error });
  const executable = join(repo, 'target', 'release', process.platform === 'win32' ? 'flowsurface.exe' : 'flowsurface');
  const child = spawn(executable, ['--profile-dir', profile], {
    cwd: repo, detached: true, stdio: 'ignore',
    // Also isolate older binaries that know only the existing environment option.
    env: { ...process.env, FLOWSURFACE_DATA_PATH: profile, FLOWSURFACE_LOG_DIR: join(profile, 'logs') },
  });
  child.on('error', error => { console.error(error.message); process.exitCode = 1; });
  child.unref();
  console.log(`Launched test terminal, PID ${child.pid}. Working exe not replaced.`);
}
