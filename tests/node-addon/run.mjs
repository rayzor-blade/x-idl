import { spawnSync } from 'node:child_process';
import { copyFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('./', import.meta.url));
const result = spawnSync('cargo', ['build', '--locked'], { cwd: root, stdio: 'inherit' });
if (result.status !== 0) {
  process.exit(result.status ?? 1);
}
const library = process.platform === 'darwin'
  ? 'libxidl_node_test.dylib'
  : process.platform === 'win32'
    ? 'xidl_node_test.dll'
    : 'libxidl_node_test.so';
await copyFile(new URL('target/debug/' + library, import.meta.url), new URL('test.node', import.meta.url));
await import('./test.mjs');
