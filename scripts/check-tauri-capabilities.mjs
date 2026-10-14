#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import ts from 'typescript';
import { fileURLToPath } from 'node:url';

const CORE_MODULES = new Map([
  ['@tauri-apps/api/window', 'core:window'],
  ['@tauri-apps/api/menu', 'core:menu'],
  ['@tauri-apps/api/event', 'core:event']
]);

function sourceFiles(root) {
  const files = [];
  const visit = directory => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      if (entry.name === 'node_modules' || entry.name === 'dist' || entry.name === 'target') continue;
      const full = path.join(directory, entry.name);
      if (entry.isDirectory()) visit(full);
      else if (/\.(?:svelte|ts|tsx|js|mjs)$/.test(entry.name) && !/\.(?:test|spec)\./.test(entry.name)) files.push(full);
    }
  };
  visit(root);
  return files;
}

function scriptText(file) {
  const text = fs.readFileSync(file, 'utf8');
  if (!file.endsWith('.svelte')) return text;
  return [...text.matchAll(/<script(?:\s[^>]*)?>([\s\S]*?)<\/script>/g)].map(match => match[1]).join('\n');
}

function importedBindings(sourceFile) {
  const bindings = new Map();
  for (const statement of sourceFile.statements) {
    if (!ts.isImportDeclaration(statement) || !ts.isStringLiteral(statement.moduleSpecifier)) continue;
    const group = CORE_MODULES.get(statement.moduleSpecifier.text);
    const clause = statement.importClause;
    if (!group || !clause?.namedBindings || !ts.isNamedImports(clause.namedBindings)) continue;
    for (const element of clause.namedBindings.elements) {
      bindings.set(element.name.text, { imported: element.propertyName?.text ?? element.name.text, group });
    }
  }
  return bindings;
}

function collectSurfaceCalls(file, text) {
  const sourceFile = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const imports = importedBindings(sourceFile);
  const windowFactories = new Set([...imports].filter(([, value]) => value.group === 'core:window' && value.imported === 'getCurrentWindow').map(([name]) => name));
  const menuClasses = new Set([...imports].filter(([, value]) => value.group === 'core:menu' && value.imported === 'Menu').map(([name]) => name));
  const eventFunctions = new Map([...imports].filter(([, value]) => value.group === 'core:event'));
  const windowVariables = new Set();
  const menuVariables = new Set();
  const calls = [];

  const rememberAssignment = node => {
    if (!ts.isVariableDeclaration(node) || !ts.isIdentifier(node.name)) return;
    const initializer = node.initializer && ts.isAwaitExpression(node.initializer) ? node.initializer.expression : node.initializer;
    if (initializer && ts.isCallExpression(initializer)) {
      if (ts.isIdentifier(initializer.expression) && windowFactories.has(initializer.expression.text)) windowVariables.add(node.name.text);
      if (ts.isPropertyAccessExpression(initializer.expression) && ts.isIdentifier(initializer.expression.expression) &&
          menuClasses.has(initializer.expression.expression.text) && initializer.expression.name.text === 'new') menuVariables.add(node.name.text);
    }
    if (/(?:^|[<|&\s])Menu(?:[>|\s]|$)/.test(node.type?.getText(sourceFile) ?? '')) menuVariables.add(node.name.text);
  };
  const walkBindings = node => { rememberAssignment(node); ts.forEachChild(node, walkBindings); };
  walkBindings(sourceFile);

  const add = (group, method, node) => calls.push({ group, method, file, line: sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1 });
  const visit = node => {
    if (ts.isCallExpression(node)) {
      if (ts.isIdentifier(node.expression)) {
        const direct = eventFunctions.get(node.expression.text);
        if (direct) add(direct.group, direct.imported, node.expression);
      } else if (ts.isPropertyAccessExpression(node.expression)) {
        const receiver = node.expression.expression;
        const method = node.expression.name.text;
        if (ts.isCallExpression(receiver) && ts.isIdentifier(receiver.expression) && windowFactories.has(receiver.expression.text)) {
          add('core:window', method, node.expression.name);
        } else if (ts.isIdentifier(receiver) && windowVariables.has(receiver.text)) {
          add('core:window', method, node.expression.name);
        } else if (ts.isIdentifier(receiver) && menuClasses.has(receiver.text) && method === 'new') {
          add('core:menu', method, node.expression.name);
        } else if (ts.isIdentifier(receiver) && menuVariables.has(receiver.text)) {
          add(method === 'close' ? 'core:resources' : 'core:menu', method, node.expression.name);
        }
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sourceFile);
  return calls;
}

function apiMethodCommands(apiFile, className) {
  const sourceFile = ts.createSourceFile(apiFile, fs.readFileSync(apiFile, 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
  const commands = new Map();
  const commandIn = body => {
    let command;
    const visit = node => {
      if (command || !ts.isCallExpression(node) || !ts.isIdentifier(node.expression) || node.expression.text !== 'invoke') {
        if (!command) ts.forEachChild(node, visit);
        return;
      }
      const first = node.arguments[0];
      if (first && ts.isStringLiteral(first)) command = first.text;
    };
    visit(body);
    return command;
  };
  const visit = node => {
    if (ts.isClassDeclaration(node) && node.name?.text === className) {
      for (const member of node.members) {
        if (!member.body || !member.name) continue;
        const name = ts.isIdentifier(member.name) || ts.isStringLiteral(member.name) ? member.name.text : undefined;
        const command = commandIn(member.body);
        if (name && command) commands.set(name, command);
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sourceFile);
  return commands;
}

function allowedCommands(manifests, configuredPermissions) {
  const allowed = new Set();
  const seen = new Set();
  const expand = identifier => {
    if (seen.has(identifier)) return;
    seen.add(identifier);
    const split = identifier.lastIndexOf(':');
    if (split < 0) throw new Error(`invalid capability permission ${identifier}`);
    const plugin = identifier.slice(0, split);
    const name = identifier.slice(split + 1);
    const manifest = manifests[plugin];
    if (!manifest) throw new Error(`capability references unknown plugin ${plugin}`);
    const entry = name === 'default' ? manifest.default_permission : manifest.permission_sets?.[name] ?? manifest.permissions?.[name];
    if (!entry) throw new Error(`capability references unknown permission ${identifier}`);
    for (const nested of entry.permissions ?? []) expand(nested.includes(':') ? nested : `${plugin}:${nested}`);
    for (const command of entry.commands?.allow ?? []) allowed.add(`${plugin}|${command}`);
  };
  for (const permission of configuredPermissions) expand(typeof permission === 'string' ? permission : permission.identifier);
  return allowed;
}

export function auditTauriCapabilities({ appDir, capability: capabilityOverride, apiRoot }) {
  apiRoot ??= path.dirname(fs.realpathSync(path.join(appDir, 'node_modules/@tauri-apps/api/package.json')));
  const capabilityPath = path.join(appDir, 'src-tauri/capabilities/default.json');
  const manifestPath = path.join(appDir, 'src-tauri/gen/schemas/acl-manifests.json');
  const capability = capabilityOverride ?? JSON.parse(fs.readFileSync(capabilityPath, 'utf8'));
  const manifests = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
  const allowed = allowedCommands(manifests, capability.permissions);
  const mappings = new Map([
    ['core:window', apiMethodCommands(path.join(apiRoot, 'window.js'), 'Window')],
    ['core:menu', apiMethodCommands(path.join(apiRoot, 'menu/menu.js'), 'Menu')]
  ]);
  const eventMethods = new Set(['listen', 'onDragDropEvent', 'onFocusChanged']);
  const required = [];
  const unknown = [];
  for (const file of sourceFiles(path.join(appDir, 'src'))) {
    for (const call of collectSurfaceCalls(file, scriptText(file))) {
      let commands = [];
      if (call.group === 'core:event' && call.method === 'listen') commands = ['core:event|listen', 'core:event|unlisten'];
      else if (call.group === 'core:window' && eventMethods.has(call.method)) commands = ['core:event|listen', 'core:event|unlisten'];
      else if (call.group === 'core:resources' && call.method === 'close') commands = ['core:resources|close'];
      else if (call.group === 'core:menu' && call.method === 'new') commands = ['core:menu|new'];
      else {
        const raw = mappings.get(call.group)?.get(call.method);
        if (raw) commands = [raw.replace('plugin:', 'core:')];
        else unknown.push(call);
      }
      for (const command of commands) required.push({ ...call, command });
    }
  }
  const missing = required.filter(item => !allowed.has(item.command));
  return { allowed, required, missing, unknown };
}

function format(item, root) {
  return `${path.relative(root, item.file)}:${item.line}: ${item.group}.${item.method}${item.command ? ` requires ${item.command}` : ' is not classified'}`;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const root = process.cwd();
  const appDir = path.resolve(process.argv[2] ?? 'products/loom/apps/loom');
  const result = auditTauriCapabilities({ appDir });
  if (result.unknown.length || result.missing.length) {
    for (const item of result.unknown) console.error(`unclassified Tauri API call: ${format(item, root)}`);
    for (const item of result.missing) console.error(`missing Tauri capability: ${format(item, root)}`);
    process.exit(1);
  }
  const unique = new Set(result.required.map(item => item.command));
  console.log(`Tauri capability contract covers ${unique.size} native commands across ${result.required.length} call sites.`);
}
