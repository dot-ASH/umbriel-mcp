import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const SERVER_TOOL_PREFIX = "mcp__umbriel__";
const APPLY_CONFIG_CHANGE = `${SERVER_TOOL_PREFIX}apply_config_change`;
const CONFIRM_LAUNCH = `${SERVER_TOOL_PREFIX}confirm_launch`;
const CONFIRMATION_CODE = /confirmation "([0-9a-f]+)"/;
const INSTRUCTIONS_START = "\n\nOnly after";

type Held = { kind: "config" | "launch"; preview: string };

const CONFIG_CHOICES = {
  keepBackup: "Apply it (keep a backup)",
  noBackup: "Apply it without a backup",
  wholeSession: "Apply it, and don't ask again this session (keep backups)",
  decline: "Don't apply it",
};

const LAUNCH_CHOICES = {
  start: "Start it",
  wholeSession: "Start it, and don't ask again this session",
  decline: "Don't start it",
};

function textOf(content: unknown): string {
  if (!Array.isArray(content)) return "";
  return content
    .filter((block) => block?.type === "text" && typeof block.text === "string")
    .map((block) => block.text as string)
    .join("\n");
}

function previewIn(text: string): string {
  const start = text.indexOf("\n\n");
  const end = text.lastIndexOf(INSTRUCTIONS_START);
  if (start < 0 || end <= start) return text;
  return text.slice(start + 2, end).trim();
}

function instructionsForTheModel(held: Held, code: string): string {
  const tool = held.kind === "config" ? "apply_config_change" : "confirm_launch";
  const what = held.kind === "config" ? "Nothing has been written yet." : "Nothing was started yet.";
  return (
    `${what} pi asks the user in its own dialog, so call ${tool} with confirmation "${code}" ` +
    `right away, without asking in the chat first and without setting backup or ` +
    `allow_rest_of_session: the dialog asks the user about those. If the dialog is declined, ` +
    `do not retry unless the user asks.`
  );
}

export default function umbrielApproval(pi: ExtensionAPI) {
  const held = new Map<string, Held>();

  pi.on("tool_result", async (event) => {
    if (!event.toolName.startsWith(SERVER_TOOL_PREFIX) || event.isError) return;
    const text = textOf(event.content);
    const code = text.match(CONFIRMATION_CODE)?.[1];
    if (!code || !text.includes(INSTRUCTIONS_START)) return;
    const entry: Held = {
      kind: text.includes("confirm_launch") ? "launch" : "config",
      preview: previewIn(text),
    };
    held.set(code, entry);
    return {
      content: [{ type: "text", text: `${entry.preview}\n\n${instructionsForTheModel(entry, code)}` }],
    };
  });

  pi.on("tool_call", async (event, ctx) => {
    if (event.toolName !== APPLY_CONFIG_CHANGE && event.toolName !== CONFIRM_LAUNCH) return;
    const input = event.input as {
      confirmation?: string;
      backup?: boolean;
      allow_rest_of_session?: boolean;
    };
    const launching = event.toolName === CONFIRM_LAUNCH;
    const nothingDone = launching ? "nothing was started" : "nothing was written";
    if (!ctx.hasUI) {
      return { block: true, reason: `pi has no dialog in this mode to ask the user, so ${nothingDone}.` };
    }
    const code = String(input.confirmation ?? "").trim();
    const preview =
      held.get(code)?.preview ??
      `(no preview was seen for confirmation code ${code}; it may be from an earlier session)`;

    if (launching) {
      const choice = await ctx.ui.select(
        `umbriel-mcp wants to start a program on your desktop:\n\n  ${preview}`,
        Object.values(LAUNCH_CHOICES),
      );
      if (choice !== LAUNCH_CHOICES.start && choice !== LAUNCH_CHOICES.wholeSession) {
        return { block: true, reason: `The user did not allow starting it, so ${nothingDone}.` };
      }
      input.allow_rest_of_session = choice === LAUNCH_CHOICES.wholeSession;
    } else {
      const choice = await ctx.ui.select(
        `umbriel-mcp wants to change your Umbriel config:\n\n${preview}`,
        Object.values(CONFIG_CHOICES),
      );
      if (choice === undefined || choice === CONFIG_CHOICES.decline) {
        return { block: true, reason: `The user did not approve the change, so ${nothingDone}.` };
      }
      input.backup = choice !== CONFIG_CHOICES.noBackup;
      input.allow_rest_of_session = choice === CONFIG_CHOICES.wholeSession;
    }
    held.delete(code);
  });
}
