/**
 * App bootstrap. Phase 1 scope: mount xterm.js, open one PTY in the Rust
 * backend, and pipe bytes both directions. Everything fancier (input editor,
 * blocks, sidebar, AI) layers on top in later phases — keep this file thin.
 */
import { setupTerminal } from "./terminal";

window.addEventListener("DOMContentLoaded", () => {
  const host = document.getElementById("terminal");
  if (!host) {
    // Fail loud during development; silent failure here would be invisible
    // because xterm.js never gets a chance to render anything.
    throw new Error("ArcTerm: #terminal mount point missing from index.html");
  }
  setupTerminal(host).catch((err) => {
    console.error("ArcTerm terminal init failed", err);
  });
});
