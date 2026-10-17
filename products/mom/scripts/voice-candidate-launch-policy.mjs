import { isAbsolute, join, resolve } from 'node:path';
import {
  constants, closeSync, existsSync, fstatSync, lstatSync, openSync, readFileSync, realpathSync, statSync,
} from 'node:fs';

export function parseStartOptions(optionArgs) {
  const values = { dataDirectory: null, storeKeyFile: null };
  const seen = new Set();
  for (let index = 0; index < optionArgs.length; index += 2) {
    const flag = optionArgs[index];
    const value = optionArgs[index + 1];
    if (flag !== '--data-dir' && flag !== '--store-key-file') {
      throw new Error('Unknown start option; expected --data-dir or --store-key-file');
    }
    if (seen.has(flag)) throw new Error(`Duplicate start option: ${flag}`);
    if (typeof value !== 'string' || !value || value.startsWith('--')) {
      throw new Error(`Missing value for ${flag}`);
    }
    seen.add(flag);
    if (flag === '--data-dir') values.dataDirectory = value;
    else values.storeKeyFile = value;
  }
  return values;
}

export function readPrivateStoreKeyFile(filePath) {
  let linkMetadata;
  try { linkMetadata = lstatSync(filePath); }
  catch { throw new Error('Store key file is unavailable'); }
  if (linkMetadata.isSymbolicLink() || !linkMetadata.isFile()) {
    throw new Error('Store key path must be a regular, non-symlink file');
  }
  let fileDescriptor;
  try { fileDescriptor = openSync(filePath, constants.O_RDONLY | constants.O_NOFOLLOW); }
  catch { throw new Error('Store key path must be a regular, non-symlink file'); }
  try {
    const metadata = fstatSync(fileDescriptor);
    if (!metadata.isFile()) throw new Error('Store key path must be a regular, non-symlink file');
    const permissions = metadata.mode & 0o777;
    if ((permissions & 0o077) !== 0 || (permissions & 0o100) !== 0 || (permissions & 0o400) === 0) {
      throw new Error('Store key file must be private to its owner (mode 0600 or stricter)');
    }
    if (metadata.size < 64 || metadata.size > 66) {
      throw new Error('Store key file must contain exactly 64 hexadecimal characters');
    }
    let contents;
    try { contents = readFileSync(fileDescriptor, 'utf8'); }
    catch { throw new Error('Store key file is unavailable'); }
    if (contents.endsWith('\n')) contents = contents.slice(0, -1);
    if (contents.endsWith('\r')) contents = contents.slice(0, -1);
    if (!/^[0-9a-fA-F]{64}$/.test(contents)) {
      throw new Error('Store key file must contain exactly 64 hexadecimal characters');
    }
    return contents.toLowerCase();
  } finally {
    closeSync(fileDescriptor);
  }
}

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
