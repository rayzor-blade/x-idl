import { spawnSync } from 'node:child_process';
import { copyFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
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
// A file URL's pathname is not a portable filesystem path. Exercise both
// Windows drive letters and URL-escaped characters in the worker fixture.
const fixture = await mkdtemp(join(tmpdir(), 'x-idl fixture # '));
let status;
try {
  await Promise.all(['test.mjs', 'generated.ts', 'test.node'].map(name =>
    copyFile(new URL(name, import.meta.url), join(fixture, name))));
  const test = spawnSync(process.execPath, [join(fixture, 'test.mjs')], { stdio: 'inherit' });
  if (test.error) throw test.error;
  status = test.status ?? 1;
} finally {
  // The child has exited, so Windows can release and remove the native DLL.
  await rm(fixture, { recursive: true, force: true });
}
process.exitCode = status;
