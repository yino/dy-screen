export type TextDiffKind = "unchanged" | "removed" | "added";

export interface TextDiffPart {
  kind: TextDiffKind;
  text: string;
}

function appendPart(parts: TextDiffPart[], kind: TextDiffKind, value: string): void {
  const previous = parts[parts.length - 1];
  if (previous?.kind === kind) {
    previous.text += value;
  } else {
    parts.push({ kind, text: value });
  }
}

export function buildTextDiff(before: string, after: string): TextDiffPart[] {
  const left = [...before];
  const right = [...after];
  const lengths = Array.from(
    { length: left.length + 1 },
    () => new Uint16Array(right.length + 1),
  );

  for (let leftIndex = left.length - 1; leftIndex >= 0; leftIndex -= 1) {
    for (let rightIndex = right.length - 1; rightIndex >= 0; rightIndex -= 1) {
      lengths[leftIndex][rightIndex] = left[leftIndex] === right[rightIndex]
        ? lengths[leftIndex + 1][rightIndex + 1] + 1
        : Math.max(lengths[leftIndex + 1][rightIndex], lengths[leftIndex][rightIndex + 1]);
    }
  }

  const parts: TextDiffPart[] = [];
  let leftIndex = 0;
  let rightIndex = 0;
  while (leftIndex < left.length || rightIndex < right.length) {
    if (leftIndex < left.length && rightIndex < right.length && left[leftIndex] === right[rightIndex]) {
      appendPart(parts, "unchanged", left[leftIndex]);
      leftIndex += 1;
      rightIndex += 1;
    } else if (rightIndex < right.length && (leftIndex === left.length || lengths[leftIndex][rightIndex + 1] >= lengths[leftIndex + 1][rightIndex])) {
      appendPart(parts, "added", right[rightIndex]);
      rightIndex += 1;
    } else {
      appendPart(parts, "removed", left[leftIndex]);
      leftIndex += 1;
    }
  }
  return parts;
}
