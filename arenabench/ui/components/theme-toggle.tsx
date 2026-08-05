"use client";

import * as React from "react";
import { useTheme } from "next-themes";
import { Moon, Sun } from "lucide-react";
import { Button } from "@/components/ui/button";

export function ThemeToggle() {
  const { resolvedTheme, setTheme } = useTheme();
  const [mounted, setMounted] = React.useState(false);

  // The theme is only knowable on the client (it lives in localStorage), so
  // the first server-rendered paint shows a neutral placeholder instead of
  // guessing and flickering.
  React.useEffect(() => setMounted(true), []);

  const dark = resolvedTheme === "dark";
  return (
    <Button
      variant="ghost"
      size="icon"
      aria-label={mounted ? (dark ? "switch to light mode" : "switch to dark mode") : "toggle theme"}
      title={mounted ? (dark ? "light mode" : "dark mode") : undefined}
      onClick={() => setTheme(dark ? "light" : "dark")}
      className="border-transparent text-muted hover:text-foreground"
    >
      {mounted ? dark ? <Sun className="size-4" /> : <Moon className="size-4" /> : <span className="size-4" />}
    </Button>
  );
}
