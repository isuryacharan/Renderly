// Two routes, kept in the URL hash so a reload reopens the same screen:
//   #/                          home
//   #/project/<place>/<folder>  editor

import { useEffect, useState } from "react";
import type { Place } from "./places";

export type Route = { name: "home" } | { name: "editor"; place: Place; folder: string };

export function parseRoute(hash: string): Route {
  const match = /^#\/project\/(browser|disk)\/([^/]+)$/.exec(hash);
  if (match?.[1] && match[2])
    return { name: "editor", place: match[1] as Place, folder: decodeURIComponent(match[2]) };
  return { name: "home" };
}

export function href(route: Route): string {
  return route.name === "home"
    ? "#/"
    : `#/project/${route.place}/${encodeURIComponent(route.folder)}`;
}

export function navigate(route: Route): void {
  location.hash = href(route);
}

export function useRoute(): Route {
  const [route, setRoute] = useState(() => parseRoute(location.hash));
  useEffect(() => {
    const update = () => setRoute(parseRoute(location.hash));
    addEventListener("hashchange", update);
    return () => removeEventListener("hashchange", update);
  }, []);
  return route;
}
