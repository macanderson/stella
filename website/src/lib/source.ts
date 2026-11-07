import { docs } from "@/.source/server";
import { loader } from "fumadocs-core/source";

/**
 * The Fumadocs content source.
 *
 * No `icon()` resolver is passed: no page in content/docs sets an
 * `icon: <provider-id>` frontmatter key, so a resolver turning one into a
 * vendor logomark in the sidebar would have zero call sites. Re-adding it is
 * `icon: (i) => i && createElement(...)`.
 */
export const source = loader({
  baseUrl: "/docs",
  source: docs.toFumadocsSource(),
});
