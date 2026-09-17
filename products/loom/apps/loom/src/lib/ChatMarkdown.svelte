<script lang="ts">
  import { DOMSerializer } from 'prosemirror-model';
  import { defaultMarkdownParser, schema } from 'prosemirror-markdown';

  export let text: string;
  const serializer = new DOMSerializer({
    ...DOMSerializer.nodesFromSchema(schema),
    // A generated image URL must not fetch remote content just by appearing.
    image: node => ['span', { class: 'image-description' }, node.attrs.alt || 'Image']
  }, DOMSerializer.marksFromSchema(schema));

  function render(node: HTMLElement, value: string) {
    function update(source: string) {
      const fragment = serializer.serializeFragment(defaultMarkdownParser.parse(source).content);
      for (const link of fragment.querySelectorAll('a')) {
        const href = link.getAttribute('href') ?? '';
        if (/^https?:\/\//i.test(href)) {
          link.setAttribute('target', '_blank');
          link.setAttribute('rel', 'noopener noreferrer');
        } else if (!/^loom-(?:material|evidence|attachment):/.test(href)) {
          link.replaceWith(...link.childNodes);
        }
      }
      node.replaceChildren(fragment);
    }
    update(value);
    return { update };
  }
</script>

<div class="chat-markdown" use:render={text}></div>

<style>
  .chat-markdown { min-width:0; overflow-wrap:anywhere; line-height:1.55; }
  .chat-markdown :global(> :first-child) { margin-top:0; }
  .chat-markdown :global(> :last-child) { margin-bottom:0; }
  .chat-markdown :global(p) { margin:.6em 0; white-space:pre-wrap; }
  .chat-markdown :global(h1), .chat-markdown :global(h2), .chat-markdown :global(h3), .chat-markdown :global(h4), .chat-markdown :global(h5), .chat-markdown :global(h6) { font-size:1.05em; line-height:1.4; margin:1em 0 .4em; }
  .chat-markdown :global(ul), .chat-markdown :global(ol) { padding-left:1.4em; margin:.5em 0; }
  .chat-markdown :global(li > p) { margin:.25em 0; }
  .chat-markdown :global(pre) { overflow:auto; padding:10px 12px; border:1px solid var(--line-soft); border-radius:7px; background:var(--paper-deep); white-space:pre; font-size:.88em; }
  .chat-markdown :global(code) { font-family:ui-monospace, SFMono-Regular, Menlo, monospace; font-size:.9em; }
  .chat-markdown :global(:not(pre) > code) { padding:1px 4px; border-radius:4px; background:var(--paper-deep); }
  .chat-markdown :global(blockquote) { margin:.6em 0; padding-left:12px; border-left:2px solid var(--line); color:var(--muted); }
  .chat-markdown :global(a) { color:inherit; text-decoration-color:var(--muted); text-underline-offset:3px; }
  .chat-markdown :global(hr) { border:0; border-top:1px solid var(--line-soft); margin:1em 0; }
  .chat-markdown :global(.image-description) { color:var(--muted); font-style:italic; }
</style>
