import type { Node as ProseMirrorNode } from 'prosemirror-model';
import type { NodeView, ViewMutationRecord } from 'prosemirror-view';
import { compileShaderPreview, normalizeFailure } from './ipc';

const MAX_SOURCE_BYTES = 16 * 1024;
const PREVIEW_SIZE = 256;
const VERTEX_SOURCE = `#version 300 es
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`;

type Compiler = (source: string) => Promise<{ fragment: string }>;

const pending = new Map<object, () => Promise<void>>();
let draining = false;

async function drain(): Promise<void> {
  if (draining) return;
  draining = true;
  try {
    while (pending.size) {
      const [key, job] = pending.entries().next().value!;
      pending.delete(key);
      await job();
      // Let context-loss cleanup settle before allocating the next surface.
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
    }
  } finally { draining = false; }
}

function enqueue(key: object, job: () => Promise<void>): boolean {
  if (!pending.has(key) && pending.size >= 64) return false;
  pending.set(key, job);
  void drain();
  return true;
}


function isShader(node: ProseMirrorNode): boolean {
  return node.type.name === 'code_block' && node.attrs.params?.trim().toLowerCase() === 'wgsl';
}

function compile(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader {
  const shader = gl.createShader(type);
  if (!shader) throw new Error('The preview shader could not be allocated.');
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    const message = gl.getShaderInfoLog(shader) || 'The preview shader could not compile.';
    gl.deleteShader(shader);
    throw new Error(message);
  }
  return shader;
}

function draw(gl: WebGL2RenderingContext, fragment: string): void {
  let vertex: WebGLShader | null = null;
  let pixel: WebGLShader | null = null;
  let program: WebGLProgram | null = null;
  let vertices: WebGLVertexArrayObject | null = null;
  try {
    vertex = compile(gl, gl.VERTEX_SHADER, VERTEX_SOURCE);
    pixel = compile(gl, gl.FRAGMENT_SHADER, fragment);
    program = gl.createProgram();
    vertices = gl.createVertexArray();
    if (!program || !vertices) throw new Error('The preview could not be allocated.');
    gl.attachShader(program, vertex);
    gl.attachShader(program, pixel);
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      throw new Error(gl.getProgramInfoLog(program) || 'The preview shader could not link.');
    }
    gl.viewport(0, 0, PREVIEW_SIZE, PREVIEW_SIZE);
    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.BLEND);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.useProgram(program);
    gl.bindVertexArray(vertices);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.flush();
  } finally {
    gl.bindVertexArray(null);
    gl.useProgram(null);
    if (vertices) gl.deleteVertexArray(vertices);
    if (program) gl.deleteProgram(program);
    if (vertex) gl.deleteShader(vertex);
    if (pixel) gl.deleteShader(pixel);
  }
}

function drawStaticPreview(canvas: HTMLCanvasElement, fragment: string): void {
  const surface = document.createElement('canvas');
  surface.width = surface.height = PREVIEW_SIZE;
  const gl = surface.getContext('webgl2', {
    alpha: true, antialias: false, depth: false, stencil: false, preserveDrawingBuffer: true
  });
  if (!gl || gl.isContextLost()) throw new Error('WebGL2 preview is unavailable.');
  try {
    draw(gl, fragment);
    if (gl.isContextLost()) throw new Error('The preview graphics context was lost.');
    const retained = canvas.getContext('2d');
    if (!retained) throw new Error('The preview image could not be retained.');
    retained.clearRect(0, 0, PREVIEW_SIZE, PREVIEW_SIZE);
    retained.drawImage(surface, 0, 0);
  } finally {
    gl.getExtension('WEBGL_lose_context')?.loseContext();
  }
}

/** Editable code remains ProseMirror-owned; the canvas is a disposable projection. */
export function shaderCodeBlockView(node: ProseMirrorNode, compiler: Compiler = compileShaderPreview): NodeView {
  const pre = document.createElement('pre');
  const code = document.createElement('code');
  pre.append(code);
  if (node.attrs.params) pre.dataset.params = node.attrs.params;
  if (!isShader(node)) {
    return {
      dom: pre, contentDOM: code,
      update(next) {
        if (next.type !== node.type || isShader(next)) return false;
        if (next.attrs.params) pre.dataset.params = next.attrs.params;
        else delete pre.dataset.params;
        return true;
      }
    };
  }

  const dom = document.createElement('div');
  dom.className = 'loom-shader-block';
  const canvas = document.createElement('canvas');
  canvas.width = canvas.height = PREVIEW_SIZE;
  canvas.className = 'loom-shader-preview';
  canvas.contentEditable = 'false';
  canvas.setAttribute('role', 'img');
  canvas.setAttribute('aria-label', 'Shader preview');
  canvas.hidden = true;
  const error = document.createElement('small');
  error.className = 'loom-shader-error';
  error.contentEditable = 'false';
  error.setAttribute('role', 'status');
  error.hidden = true;
  dom.append(pre, canvas, error);
  let source = node.textContent;
  let revision = 0;
  let destroyed = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const queueKey = {};

  function report(message: string): void {
    canvas.hidden = true;
    error.textContent = message;
    error.hidden = false;
  }

  async function render(captured: number): Promise<void> {
    try {
      if (source.length > MAX_SOURCE_BYTES || new TextEncoder().encode(source).length > MAX_SOURCE_BYTES) {
        throw new Error('Shader source exceeds 16 KiB.');
      }
      const result = await compiler(source);
      if (destroyed || captured !== revision) return;
      drawStaticPreview(canvas, result.fragment);
      error.hidden = true;
      canvas.hidden = false;
    } catch (failure) {
      if (!destroyed && captured === revision) report(normalizeFailure(failure).message);
    }
  }

  function schedule(): void {
    const captured = ++revision;
    pending.delete(queueKey);
    if (timer !== undefined) clearTimeout(timer);
    canvas.hidden = true;
    error.hidden = true;
    timer = setTimeout(() => {
      timer = undefined;
      if (!enqueue(queueKey, () => render(captured))) report('The preview queue is full; edit to try again.');
    }, 500);
  }

  schedule();

  return {
    dom, contentDOM: code,
    update(next) {
      if (!isShader(next)) return false;
      pre.dataset.params = next.attrs.params;
      if (next.textContent !== source) { source = next.textContent; schedule(); }
      return true;
    },
    ignoreMutation(mutation: ViewMutationRecord) {
      return mutation.type !== 'selection' && !code.contains(mutation.target);
    },
    destroy() {
      destroyed = true;
      revision += 1;
      if (timer !== undefined) clearTimeout(timer);
      pending.delete(queueKey);
      canvas.width = canvas.height = 0;
    }
  };
}
