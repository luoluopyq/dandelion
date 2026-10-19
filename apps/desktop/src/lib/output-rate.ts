/** A display-only estimate. Provider tokenizers and hidden reasoning are unavailable in the bus. */
export function estimateOutputTokenUnits(text: string): number {
  let units = 0;
  for (const char of text) {
    if (/\p{Script=Han}/u.test(char)) units += 1;
    else if (/\s/u.test(char)) units += 0.1;
    else if (/[A-Za-z0-9]/u.test(char)) units += 0.25;
    else if (char.codePointAt(0)! > 0xffff) units += 1;
    else units += 0.5;
  }
  return units;
}

export function outputRate(tokens: number, startedAt: number, endedAt: number, samples: number): number | null {
  const seconds = (endedAt - startedAt) / 1000;
  if (samples < 2 || tokens < 1 || seconds < 0.5) return null;
  return tokens / seconds;
}
