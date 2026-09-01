import {
  actionFor,
  helpText,
  type Key,
  type KeyBinding,
} from "@roshbhatia/ts-utils";

export type TuiAction =
  | "next"
  | "previous"
  | "left"
  | "right"
  | "open"
  | "inspect"
  | "changes"
  | "tab-next"
  | "tab-previous"
  | "page-up"
  | "page-down"
  | "refresh"
  | "help"
  | "quit";

export const keyBindings: ReadonlyArray<KeyBinding<TuiAction>> = [
  {
    action: "next",
    description: "next",
    help: "j/down",
    keys: [{ name: "j" }, { name: "down" }],
  },
  {
    action: "previous",
    description: "previous",
    help: "k/up",
    keys: [{ name: "k" }, { name: "up" }],
  },
  {
    action: "left",
    description: "left",
    help: "h/left",
    keys: [{ name: "h" }, { name: "left" }],
  },
  {
    action: "right",
    description: "right",
    help: "l/right",
    keys: [{ name: "l" }, { name: "right" }],
  },
  {
    action: "open",
    description: "open",
    help: "enter",
    keys: [{ name: "return" }],
  },
  {
    action: "inspect",
    description: "inspect",
    help: "i",
    keys: [{ name: "i" }],
  },
  {
    action: "changes",
    description: "changes",
    help: "c",
    keys: [{ name: "c" }],
  },
  {
    action: "tab-next",
    description: "next tab",
    help: "tab",
    keys: [{ name: "tab" }],
  },
  {
    action: "tab-previous",
    description: "previous tab",
    help: "shift-tab",
    keys: [{ name: "tab", shift: true }],
  },
  {
    action: "page-up",
    description: "page up",
    help: "ctrl-u",
    keys: [{ ctrl: true, name: "u" }],
  },
  {
    action: "page-down",
    description: "page down",
    help: "ctrl-d",
    keys: [{ ctrl: true, name: "d" }],
  },
  {
    action: "refresh",
    description: "refresh",
    help: "r",
    keys: [{ name: "r" }],
  },
  { action: "help", description: "help", help: "?", keys: [{ name: "?" }] },
  {
    action: "quit",
    description: "quit",
    help: "q/esc",
    keys: [{ name: "q" }, { name: "escape" }, { ctrl: true, name: "c" }],
  },
];

export const tuiActionFor = (key: Key): TuiAction | undefined =>
  actionFor(keyBindings, key);

export const keyHelp = (
  actions: ReadonlyArray<TuiAction>,
  separator = "  ",
): string =>
  helpText(
    keyBindings.filter((binding) => actions.includes(binding.action)),
    separator,
  );

export const fullKeyHelp = (): ReadonlyArray<string> =>
  keyBindings.map(
    (binding) => `${binding.help.padEnd(12)} ${binding.description}`,
  );
