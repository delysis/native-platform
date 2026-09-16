/** Presentation only. Retained output and subsequent prompts keep the original bytes. */
export interface ChatText {
  thoughts: { text: string; complete: boolean }[];
  answer: string;
}

const reasoningMarkers = [
  ['<think>', '</think>'],
  ['<<<reasoning_content_start>>>', '<<<reasoning_content_end>>>'],
  ['[Start thinking]', '[End thinking]'],
  ['<|channel>thought\n', '<channel|>'],
  ['<|channel>analysis\n', '<channel|>']
] as const;

export function presentChatText(raw: string): ChatText {
  const thoughts: ChatText['thoughts'] = [];
  let answer = raw;
  // Only a leading, explicit protocol block is reasoning. Words such as
  // "thought" and examples embedded in prose or code remain ordinary text.
  for (;;) {
    const trimmed = answer.trimStart();
    const marker = reasoningMarkers.find(([start]) => trimmed.startsWith(start));
    if (!marker) break;
    const [start, end] = marker;
    const close = trimmed.indexOf(end, start.length);
    if (close < 0) {
      thoughts.push({ text: trimmed.slice(start.length), complete: false });
      return { thoughts, answer: '' };
    }
    thoughts.push({ text: trimmed.slice(start.length, close), complete: true });
    answer = trimmed.slice(close + end.length);
  }
  return { thoughts, answer: thoughts.length ? answer.trimStart() : answer };
}
