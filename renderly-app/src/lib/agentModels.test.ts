import { describe, expect, it } from "vitest";
import {
  DEFAULT_LAUNCH_PREFS,
  chooseEffort,
  chooseModel,
  modelOption,
  parseLaunchPrefs,
  resolveLaunchOptions,
  type AgentModels,
  type ModelOption,
} from "./agentChat";

const model = (id: string, efforts: string[] = []): ModelOption => ({
  id,
  label: id,
  description: null,
  efforts,
  defaultEffort: null,
  contextLength: null,
  vision: false,
  thinking: efforts.length > 0,
});

/** A list as the backend returns it for Codex: its own default, its own effort levels. */
const codex: AgentModels = {
  agentId: "codex",
  status: "ready",
  models: [model("gpt-6-terra", ["low", "medium"]), model("gpt-6-luna")],
  defaultModel: "gpt-6-terra",
  message: null,
};

const notRunning: AgentModels = {
  agentId: "ollama",
  status: "not_running",
  models: [],
  defaultModel: null,
  message: "Ollama is not running at http://127.0.0.1:11434. Start Ollama, then refresh.",
};

describe("chooseModel", () => {
  it("uses a saved model the agent still offers, with no notice", () => {
    expect(chooseModel(codex, "gpt-6-luna", "Codex")).toEqual({ model: "gpt-6-luna", notice: null });
  });

  it("uses the agent's default when nothing is saved", () => {
    expect(chooseModel(codex, undefined, "Codex")).toEqual({ model: "gpt-6-terra", notice: null });
  });

  it("falls back to the default and names the model it replaces when the saved one is gone", () => {
    const choice = chooseModel(codex, "gpt-retired", "Codex");
    expect(choice.model).toBe("gpt-6-terra");
    expect(choice.notice).toBe('"gpt-retired" is no longer offered by Codex. Using gpt-6-terra.');
  });

  it("falls back to the first model when the reported default is not in the list", () => {
    const list = { ...codex, defaultModel: "missing" };
    expect(chooseModel(list, undefined, "Codex").model).toBe("gpt-6-terra");
  });

  it("keeps the saved value while the list is not loaded, and offers none when it is not ready", () => {
    expect(chooseModel(null, "x", "Codex")).toEqual({ model: "x", notice: null });
    expect(chooseModel(notRunning, undefined, "Ollama")).toEqual({ model: null, notice: null });
  });
});

describe("effort", () => {
  it("is offered only for a model that lists levels, and a saved level must be one of them", () => {
    const terra = modelOption(codex, "gpt-6-terra");
    expect(chooseEffort(terra, "medium")).toBe("medium");
    expect(chooseEffort(terra, "max")).toBeNull();
    expect(chooseEffort(modelOption(codex, "gpt-6-luna"), "medium")).toBeNull();
  });
});

describe("resolveLaunchOptions", () => {
  it("builds the start options from the list and the saved preferences, per agent", () => {
    const prefs = { ...DEFAULT_LAUNCH_PREFS, models: { codex: "gpt-6-terra" }, efforts: { codex: "low" }, nativeTools: true };
    const { options, notice } = resolveLaunchOptions("codex", "Codex", codex, prefs);
    expect(options).toEqual({ model: "gpt-6-terra", effort: "low", nativeTools: true, ollamaHost: null });
    expect(notice).toBeNull();
  });

  it("sends the Ollama address only for Ollama", () => {
    const prefs = { ...DEFAULT_LAUNCH_PREFS, ollamaHost: "http://10.0.0.5:11434" };
    expect(resolveLaunchOptions("ollama", "Ollama", null, prefs).options.ollamaHost).toBe("http://10.0.0.5:11434");
    expect(resolveLaunchOptions("codex", "Codex", codex, prefs).options.ollamaHost).toBeNull();
  });

  it("drops a saved effort the new model does not offer", () => {
    const prefs = { ...DEFAULT_LAUNCH_PREFS, models: { codex: "gpt-6-luna" }, efforts: { codex: "medium" } };
    expect(resolveLaunchOptions("codex", "Codex", codex, prefs).options.effort).toBeNull();
  });
});

describe("parseLaunchPrefs", () => {
  it("reads the per-agent choices and ignores agents it does not know", () => {
    const raw = JSON.stringify({
      nativeTools: true,
      ollamaHost: "http://127.0.0.1:11434",
      models: { codex: "gpt-6-terra", "unknown-agent": "x", claude: 3 },
      efforts: { ollama: "high" },
    });
    expect(parseLaunchPrefs(raw)).toEqual({
      nativeTools: true,
      ollamaHost: "http://127.0.0.1:11434",
      models: { codex: "gpt-6-terra" },
      efforts: { ollama: "high" },
    });
  });

  it("returns the defaults for nothing saved or unreadable JSON", () => {
    expect(parseLaunchPrefs(null)).toEqual(DEFAULT_LAUNCH_PREFS);
    expect(parseLaunchPrefs("{not json")).toEqual(DEFAULT_LAUNCH_PREFS);
  });
});
