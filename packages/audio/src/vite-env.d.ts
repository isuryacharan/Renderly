// Vite bundles a module imported with `?worker&url` as a standalone script and returns its URL.
// The AudioWorklet processor is loaded that way, since `audioWorklet.addModule` takes a URL.
declare module "*?worker&url" {
  const url: string;
  export default url;
}
