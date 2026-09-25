// Included by the existing consolidation policy job; no separate CI lane.
import './consolidation-release.test.mjs';

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const read = relative => readFileSync(path.join(root, relative), 'utf8');

test('one executable selects existing owners without a subprocess shim', () => {
  const entry = read('products/loom/apps/loom/src-tauri/src/main.rs');
  assert.match(entry, /loom_app_lib::run\(\)/u);
  assert.match(entry, /mom_llama_app::run\(\)/u);
  assert.match(entry, /desktop_launch::resolve/u);
  assert.match(entry, /use desktop_launch::\{AppMode, LaunchAction\}/u);
  assert.doesNotMatch(entry, /Command::new|\.spawn\(/u);
  const manifest = read('products/mom/apps/mom-llama/src-tauri/Cargo.toml');
  assert.match(manifest, /autobins = false/u);
  assert.match(manifest, /path = "src\/main.rs"/u);
  assert.match(manifest, /path = "src\/launcher.rs"/u);
  assert.match(read('products/mom/apps/mom-llama/src-tauri/src/main.rs'), /pub fn run\(\)/u);
});
test('common roles retain the old serde module export and exact message metadata', () => {
  assert.match(read('products/mom/crates/mom-llama-runtime/src/conversation_store.rs'), /pub use workspace_document::MessageRole;/u);
  const core = read('crates/workspace-document/src/lib.rs');
  assert.match(core, /serde\(rename_all = "snake_case"\)/u);
  assert.match(core, /pub enum MessageRole/u);
  const adapter = read('products/mom/crates/mom-llama-runtime/src/document.rs');
  assert.match(adapter, /Result<Document<'_, &str, &Message>, DocumentError>/u);
  assert.match(adapter, /PartKind::Message\(message.role.clone\(\)\)/u);
  assert.doesNotMatch(adapter, /RuntimeStore|write_all|std::fs::write/u);
});
test('shared crates enter both primary and portable CI package selections', () => {
  const groups = JSON.parse(read('ci/package-groups.json'));
  for (const name of ['desktop-launch', 'workspace-document']) {
    assert(groups.primary.desktop.includes(name));
    assert(groups.secondary.portable.includes(name));
  }
});
test('cancel handler delegates terminal reconciliation without swallowing storage failure', () => {
  const source = read('products/fte/crates/fte-loopback/src/lib.rs');
  const start = source.indexOf('async fn cancel_response(');
  assert(start >= 0);
  const next = source.indexOf('\nasync fn ', start + 1);
  const handler = source.slice(start, next < 0 ? undefined : next);
  assert.match(handler, /response_cancel::reconcile/u);
  assert.doesNotMatch(handler, /\.ok\(\)\s*\.flatten\(\)|if cancelled > 0/u);
  const policy = read('products/fte/crates/fte-loopback/src/response_cancel.rs');
  for (const terminal of ['Completed', 'Incomplete', 'Cancelled', 'Failed']) {
    assert(policy.includes(`TerminalStatus::${terminal}`));
  }
  assert.match(policy, /CancelResolution::InProgress/u);
});
test('workspace pane defaults are quiet without removing explicit opt-in', () => {
  const source = read('products/loom/crates/tauri-plugin-loom/src/workspace_template.rs');
  assert.match(source, /visible: kind == PaneKind::Editor/u);
  assert.match(source, /optional_panes_need_explicit_visibility_even_after_kind_changes/u);
  assert.match(source, /visible=true/u);
});
test('documented route surface no longer invents inbound Gemini endpoints', () => {
  const readme = read('products/fte/README.md');
  assert.doesNotMatch(readme, /^- `POST \/v1beta/mu);
  assert.match(readme, /outbound provider adapter/u);
});
test('attachment matrix distinguishes bounded text and active vector data', () => {
  const matrix = read('crates/services/attachment/docs/FORMAT_SUPPORT.md');
  assert.doesNotMatch(matrix, /RTF is content-first detected, but remains opaque/u);
  assert.match(matrix, /attachment-native-document::rtf/u);
  assert.match(matrix, /not decoded raster media/u);
});
