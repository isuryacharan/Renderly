import "@renderly/ui/fonts.css";
import "@renderly/ui/styles.css";
import "./spike.css";
import { canCreateWebGL2, probeCapabilities } from "./probe";

const list = document.querySelector<HTMLUListElement>("#capabilities");
if (!list) throw new Error("Missing #capabilities");

for (const capability of probeCapabilities(globalThis)) {
  // An API that exists can still fail (blocklisted GPU, disabled hardware acceleration).
  const available =
    capability.name === "WebGL2" ? capability.available && canCreateWebGL2() : capability.available;

  const item = document.createElement("li");
  item.className = "capability";
  item.dataset["available"] = String(available);

  const name = document.createElement("span");
  name.className = "capability__name";
  name.textContent = capability.name;

  const state = document.createElement("span");
  state.className = "capability__state";
  state.textContent = available ? "true" : "false";

  const purpose = document.createElement("span");
  purpose.className = "capability__purpose";
  purpose.textContent = capability.purpose;

  item.append(name, state, purpose);
  list.append(item);
}
