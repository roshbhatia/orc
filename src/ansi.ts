import {
  RGBA,
  StyledText,
  TextAttributes,
  type TextChunk,
} from "@opentui/core";

interface AnsiState {
  attributes: number;
  background: RGBA | undefined;
  foreground: RGBA | undefined;
}

const escapeSequenceEnd = (
  input: string,
  start: number,
): number | undefined => {
  const introducer = input[start + 1];
  if (introducer === undefined) return undefined;
  if (introducer === "[") {
    for (let index = start + 2; index < input.length; index++) {
      const code = input.charCodeAt(index);
      if (code >= 0x40 && code <= 0x7e) return index + 1;
    }
    return undefined;
  }
  if (["]", "P", "X", "^", "_"].includes(introducer)) {
    for (let index = start + 2; index < input.length; index++) {
      if (introducer === "]" && input.charCodeAt(index) === 0x07)
        return index + 1;
      if (input[index] === "\u001b" && input[index + 1] === "\\")
        return index + 2;
    }
    return undefined;
  }
  return start + 2;
};

export const sanitizeTerminalText = (input: string): string => {
  let output = "";
  for (let index = 0; index < input.length; ) {
    const code = input.charCodeAt(index);
    if (code === 0x1b) {
      const end = escapeSequenceEnd(input, index);
      if (end === undefined) break;
      const sequence = input.slice(index, end);
      const parameters = sequence.slice(2, -1);
      if (
        sequence.startsWith("\u001b[") &&
        sequence.endsWith("m") &&
        /^[0-9;:]*$/.test(parameters)
      )
        output += sequence;
      index = end;
      continue;
    }
    if (
      (code < 0x20 && code !== 0x09 && code !== 0x0a && code !== 0x0d) ||
      code === 0x7f ||
      (code >= 0x80 && code <= 0x9f)
    ) {
      index += 1;
      continue;
    }
    output += input[index];
    index += 1;
  }
  return output;
};

const colorIndex = (code: number, background: boolean): number | undefined => {
  const base = background ? 40 : 30;
  const bright = background ? 100 : 90;
  if (code >= base && code <= base + 7) return code - base;
  if (code >= bright && code <= bright + 7) return code - bright + 8;
  return undefined;
};

const setAttribute = (
  attributes: number,
  attribute: number,
  enabled: boolean,
): number => (enabled ? attributes | attribute : attributes & ~attribute);

const applySgr = (state: AnsiState, values: ReadonlyArray<number>): void => {
  const codes = values.length === 0 ? [0] : values;
  for (let index = 0; index < codes.length; index++) {
    const code = codes[index] ?? 0;
    if (code === 0) {
      state.attributes = TextAttributes.NONE;
      state.background = undefined;
      state.foreground = undefined;
      continue;
    }
    if (code === 1)
      state.attributes = setAttribute(
        state.attributes,
        TextAttributes.BOLD,
        true,
      );
    else if (code === 2)
      state.attributes = setAttribute(
        state.attributes,
        TextAttributes.DIM,
        true,
      );
    else if (code === 4)
      state.attributes = setAttribute(
        state.attributes,
        TextAttributes.UNDERLINE,
        true,
      );
    else if (code === 22) {
      state.attributes = setAttribute(
        state.attributes,
        TextAttributes.BOLD,
        false,
      );
      state.attributes = setAttribute(
        state.attributes,
        TextAttributes.DIM,
        false,
      );
    } else if (code === 24)
      state.attributes = setAttribute(
        state.attributes,
        TextAttributes.UNDERLINE,
        false,
      );
    else if (code === 39) state.foreground = undefined;
    else if (code === 49) state.background = undefined;
    else if (code === 38 || code === 48) {
      const target = code === 38 ? "foreground" : "background";
      const mode = codes[index + 1];
      if (mode === 5 && codes[index + 2] !== undefined) {
        state[target] = RGBA.fromIndex(codes[index + 2] ?? 0);
        index += 2;
      } else if (
        mode === 2 &&
        codes[index + 2] !== undefined &&
        codes[index + 3] !== undefined &&
        codes[index + 4] !== undefined
      ) {
        state[target] = RGBA.fromInts(
          codes[index + 2] ?? 0,
          codes[index + 3] ?? 0,
          codes[index + 4] ?? 0,
        );
        index += 4;
      }
    } else {
      const foreground = colorIndex(code, false);
      const background = colorIndex(code, true);
      if (foreground !== undefined)
        state.foreground = RGBA.fromIndex(foreground);
      if (background !== undefined)
        state.background = RGBA.fromIndex(background);
    }
  }
};

export const ansiToStyledText = (input: string): StyledText => {
  const sanitized = sanitizeTerminalText(input);
  const chunks: TextChunk[] = [];
  const state: AnsiState = {
    attributes: TextAttributes.NONE,
    background: undefined,
    foreground: undefined,
  };
  const escapeCharacter = String.fromCharCode(27);
  const expression = new RegExp(`${escapeCharacter}\\[([0-9;]*)m`, "g");
  let start = 0;
  for (const match of sanitized.matchAll(expression)) {
    const index = match.index;
    if (index > start) {
      chunks.push({
        __isChunk: true,
        attributes: state.attributes,
        ...(state.background ? { bg: state.background } : {}),
        ...(state.foreground ? { fg: state.foreground } : {}),
        text: sanitized.slice(start, index),
      });
    }
    applySgr(
      state,
      (match[1] ?? "")
        .split(";")
        .filter((value) => value.length > 0)
        .map(Number),
    );
    start = index + match[0].length;
  }
  if (start < sanitized.length || chunks.length === 0) {
    chunks.push({
      __isChunk: true,
      attributes: state.attributes,
      ...(state.background ? { bg: state.background } : {}),
      ...(state.foreground ? { fg: state.foreground } : {}),
      text: sanitized.slice(start),
    });
  }
  return new StyledText(chunks);
};
