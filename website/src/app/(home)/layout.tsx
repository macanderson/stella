import type { ReactNode } from "react";
import { HomeChrome } from "@/components/home-chrome";

export default function Layout({ children }: { children: ReactNode }) {
  return <HomeChrome>{children}</HomeChrome>;
}
