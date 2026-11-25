import { ToastProvider } from "@renderly/ui/react";
import { Editor } from "./editor/Editor";
import { Home } from "./home/Home";
import type { Missing } from "./probe";
import { useRoute } from "./route";

export function App() {
  const route = useRoute();
  return (
    <ToastProvider>
      {route.name === "home" ? (
        <Home />
      ) : (
        <Editor key={`${route.place}/${route.folder}`} place={route.place} folder={route.folder} />
      )}
    </ToastProvider>
  );
}

export function Unsupported({ missing }: { missing: Missing[] }) {
  return (
    <main className="unsupported">
      <h1 className="text-heading">Renderly cannot run in this browser</h1>
      <p>It is missing:</p>
      <ul>
        {missing.map((m) => (
          <li key={m.feature}>
            <strong>{m.feature}</strong>, {m.why}
          </li>
        ))}
      </ul>
      <p>Open Renderly in a current version of Chrome or Edge on a desktop computer.</p>
    </main>
  );
}
