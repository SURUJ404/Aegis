import { useEffect, useState } from "react";
import EngineApp from "./EngineApp";
import Terminal from "./Terminal";

/**
 * Two products, one bundle. `/engine` keeps the liquidity-engine dashboard that
 * existing tests and links point at; every other path opens the Trading
 * Terminal (the quant REST API front end).
 */
function currentPath(): string {
  return location.pathname.replace(/\/+$/, "") || "/";
}

export default function App() {
  const [path, setPath] = useState(currentPath);

  useEffect(() => {
    const sync = () => setPath(currentPath());
    window.addEventListener("popstate", sync);
    return () => window.removeEventListener("popstate", sync);
  }, []);

  return path === "/engine" ? <EngineApp /> : <Terminal />;
}
