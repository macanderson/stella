import { TranscriptPage } from "@/components/arena/transcript-page";

// The jet-black reading grammar, scoped under `.tx`. Imported at the route
// rather than in `globals.css` so the scoreboard never pays for it — and so
// the one file that spells this page's layout sits beside the page it lays
// out. Every colour in it resolves to a `globals.css` token; see its header.
import "./transcript-surface.css";

export default function Page() {
  return <TranscriptPage />;
}
