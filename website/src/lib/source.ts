import { docs } from "@/.source/server";
import { loader } from "fumadocs-core/source";

/**
 * The Fumadocs content source.
 *
 * This used to pass an `icon()` resolver that turned an `icon: <provider-id>`
 * frontmatter key into a vendor logomark beside the page in the sidebar. No
 * page in content/docs sets `icon:` — the feature had zero call sites and the
 * comment describing it was the only evidence it existed. It is removed rather
 * than left as a trap: re-adding it is `icon: (i) => i && createElement(...)`.
 */
export const source = loader({
  baseUrl: "/docs",
  source: docs.toFumadocsSource(),
});
