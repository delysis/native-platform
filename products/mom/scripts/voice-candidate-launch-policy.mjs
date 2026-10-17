import { isAbsolute, join, resolve } from 'node:path';
import { existsSync, realpathSync, statSync } from 'node:fs';

export function resolveCandidateDataDirectory({ home, namespace, explicitPath = null }) {
  if (!/^[a-zA-Z0-9_-]+$/.test(namespace || '')) throw new Error('candidate data namespace is unsafe');
  const appDataRoot = join(home, '.local', 'share', 'llama-native-kit');
  const fallback = join(appDataRoot, 'mom-llama', namespace);
  const source = explicitPath == null ? 'compiled-candidate-namespace' : 'explicit-isolated-directory';
  if (explicitPath == null) return { path: fallback, source };
  if (typeof explicitPath !== 'string' || !explicitPath || !isAbsolute(explicitPath)) {
    throw new Error('explicit candidate data directory must be a nonempty absolute path');
  }
  const path = resolve(explicitPath);
  const protectedDirectories = [
    join(appDataRoot, 'mom-llama'),
    join(appDataRoot, 'mom-llama-development-mom1-v1'),
  ];
  if (protectedDirectories.includes(path)) {
    throw new Error('refusing the installed/default Mom data directory; provide a candidate-isolated directory');
  }
  const canonicalProtectedDirectories = protectedDirectories.map(directory =>
    existsSync(directory) ? realpathSync(directory) : resolve(directory));
  let canonicalPath;
  try {
    if (!statSync(path).isDirectory()) throw new Error('not a directory');
    canonicalPath = realpathSync(path);
  } catch {
    throw new Error('explicit candidate data directory must already exist as a directory');
  }
  if (canonicalProtectedDirectories.includes(canonicalPath)) {
    throw new Error('refusing the installed/default Mom data directory; provide a candidate-isolated directory');
  }
  return { path: canonicalPath, source };
}

export function runtimeReadyLogConfirmsPath(logText, expectedPath) {
  const marker = 'mom-llama runtime ready: ';
  return logText.split(/\r?\n/).some(line => line.startsWith(marker) && line.slice(marker.length) === expectedPath);
}
