import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const root = new URL('../../', import.meta.url);
const read = path => readFileSync(new URL(path, root), 'utf8');

// This is a documentation/source-shape guard, not a router execution test.
// Refuse an unfamiliar registration expression instead of silently ignoring it.
function loopbackRoutes(source) {
  const start = source.indexOf('fn router(');
  assert.ok(start >= 0, 'router function must exist');
  const end = source.indexOf('.layer(', start);
  assert.ok(end > start, 'bounded router registration block must exist');
  const block = source.slice(start, end);
  const routes = [];
  const pattern = /\.route\(\s*"([^"]+)"\s*,\s*((?:get|post|put|patch|delete)\(\w+\)(?:\.(?:get|post|put|patch|delete)\(\w+\))*)\s*,?\s*\)/g;
  const registrations = [...block.matchAll(pattern)];
  assert.equal(registrations.length, [...block.matchAll(/\.route\(/g)].length,
    'update the documentation guard for the new route expression');
  for (const [, path, expression] of registrations) {
    if (!path.startsWith('/v1/')) continue;
    for (const [, method] of expression.matchAll(/\b(get|post|put|patch|delete)\(/g)) {
      routes.push(`${method.toUpperCase()} ${path}`);
    }
  }
  return routes.sort();
}

test('documented FTE inbound methods and paths equal registered v1 routes', () => {
  const source = read('products/fte/crates/fte-loopback/src/lib.rs');
  const readme = read('products/fte/README.md');
  const section = readme.split('## API surface\n')[1]?.split('### Legacy text completions')[0];
  assert.ok(section, 'the API surface must remain identifiable');
  const documented = [...section.matchAll(/^- `(GET|POST|DELETE|PUT|PATCH) ([^`]+)`$/gm)]
    .map(([, method, path]) => `${method} ${path}`).sort();
  assert.deepEqual(documented, loopbackRoutes(source));
  assert.match(section, /outbound provider adapter/);
});

test('RTF documentation agrees with the bounded RichText dispatch', () => {
  const source = read('crates/services/attachment/crates/attachment-native-document/src/lib.rs');
  const matrix = read('crates/services/attachment/docs/FORMAT_SUPPORT.md');
  assert.match(source, /DetectedFormat::RichText\s*=>\s*state\.render_result\(source,\s*rtf::canonicalize/);
  assert.match(matrix, /\| Rich text \| RTF \| content-first detected, bounded plain-text canonicalization \|/);
  assert.doesNotMatch(matrix, /RTF is content-first detected, but remains opaque/);
});

test('SVG documentation does not upgrade opaque XML to decoded raster evidence', () => {
  const source = read('crates/services/attachment/crates/attachment-native-document/src/lib.rs');
  const svg = source.split('DetectedFormat::Svg =>')[1]?.split('DetectedFormat::JupyterNotebook')[0];
  assert.ok(svg);
  assert.match(svg, /emit_opaque_with_warnings/);
  const row = read('crates/services/attachment/docs/FORMAT_SUPPORT.md')
    .split('\n').find(line => line.startsWith('| Vector image | SVG |'));
  assert.ok(row);
  assert.match(row, /not decoded raster media/);
  assert.doesNotMatch(row, /direct when target allows/);
});

test('Native documentation names the monorepo and direct Mom dependency', () => {
  const readme = read('crates/native/README.md');
  const map = read('crates/native/docs/MODULE_BOUNDARIES.md');
  assert.match(readme, /delysis\/native-platform/);
  assert.match(map, /Mom's local chat\/consult route calls Native directly/);
  assert.match(map, /crates\/services\/speech/);
  assert.doesNotMatch(map, /llama-native-kit ──> free-token-energy ──> mom-llama/);
});
