import { describe, expect, it } from 'vitest';
import { presentChatText } from './chatPresentation';

describe('chat presentation preserves raw content while recognizing explicit reasoning', () => {
  it.each([
    ['<think>plan</think>**Answer**', 'plan', '**Answer**'],
    ['<|channel>thought\nplan<channel|>Answer', 'plan', 'Answer'],
    ['<|channel>analysis\nplan<channel|>Answer', 'plan', 'Answer'],
    ['<<<reasoning_content_start>>>plan<<<reasoning_content_end>>>Answer', 'plan', 'Answer']
  ])('separates a complete protocol block: %s', (raw, text, answer) => {
    expect(presentChatText(raw)).toEqual({ thoughts: [{ text, complete: true }], answer });
  });
  it('retains unfinished thinking and does not invent an answer', () => {
    expect(presentChatText('<think>Still considering')).toEqual({ thoughts: [{ text: 'Still considering', complete: false }], answer: '' });
  });
  it.each(['A thought about life.', '```xml\n<think>example</think>\n```', 'Explain `<think>` here.', '<thinking>literal</thinking>', 'thought\nCould be prose.'])('does not reinterpret ordinary prose or code: %s', raw => {
    expect(presentChatText(raw)).toEqual({ thoughts: [], answer: raw });
  });
  it('keeps repeated leading reasoning blocks separate and preserves Unicode', () => {
    expect(presentChatText('<think>é 🌙</think>\n<think>二</think>\nVoilà')).toEqual({ thoughts: [{ text: 'é 🌙', complete: true }, { text: '二', complete: true }], answer: 'Voilà' });
  });
});
