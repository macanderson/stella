//! Tree-sitter S-expression queries, one triple (symbols + imports + calls)
//! per language, as `const &str` **compile-time data** — never loaded from a
//! file at runtime (L-L2: built-in assets that resolve
//! relative to the binary's install path broke the moment the artifact was
//! bundled differently; embedding them as module data is the fix).
//!
//! Naming convention shared by every symbol query so
//! [`crate::parse`] can decode matches uniformly:
//! - `@name` captures the identifier whose text becomes the symbol name.
//! - the *kind capture* (`@fn` / `@method` / `@struct` / `@enum` / `@trait` /
//!   `@class` / `@interface`, plus the SQL kinds `@table` / `@column` /
//!   `@schema_enum` / `@view`) captures the whole definition node — its line
//!   span becomes the symbol span, and its capture name encodes the kind.
//!
//! Call queries (#335, B1) share one convention too: every pattern captures
//! exactly the **callee's name node** as `@callee` — the bare identifier being
//! called (`foo` in `foo()`, `helper` in `self.helper()`, `new` in
//! `Widget::new()`). These are *unresolved, name-based* edges: no receiver
//! types, no trait dispatch, no cross-file identity. That honesty is the
//! contract — `callees` reads them within a definition's span, `callers` is a
//! reverse name lookup labeled best-effort. Callee shapes a pattern does not
//! name (function-pointer calls through parens, computed callees, macro
//! invocations) are deliberately skipped rather than guessed at.
//!
//! Name fields are matched with the `(_)` wildcard rather than a concrete
//! node type (`identifier` vs `type_identifier` vs `property_identifier`
//! differ across and within these grammars); this makes the queries robust to
//! per-grammar naming quirks while still pinning the *statement* node type.
//!
//! Methods are intentionally double-captured (once by the general function
//! pattern, once by the enclosing-type pattern); [`crate::parse`] dedups by
//! the name node's byte range and lets the more specific `Method` kind win.

// Rust
/// Functions (incl. bodyless trait-method signatures, which are
/// `function_signature_item` not `function_item`), methods (impl bodies),
/// structs, enums, traits.
pub const RUST_SYMBOLS: &str = r#"
(function_item name: (_) @name) @fn
(function_signature_item name: (_) @name) @fn
(struct_item name: (_) @name) @struct
(enum_item name: (_) @name) @enum
(trait_item name: (_) @name) @trait
(impl_item body: (declaration_list (function_item name: (_) @name) @method))
"#;

/// `use` declarations and declaration-only `mod` items (`!body` excludes
/// inline `mod x { … }`, whose contents live in this same file). `use` paths
/// resolve through the module tree (`crate::rust_resolve`, #443) at the file
/// level; a `mod foo;` declaration is itself a dependency edge to `foo.rs` /
/// `foo/mod.rs`. Item-level identity and re-export chasing stay out of
/// scope; unresolvable paths keep the unresolved-edge shape, see
/// `crate::import::resolve`.
pub const RUST_IMPORTS: &str = r#"
(use_declaration argument: (_) @use)
(mod_item name: (_) @mod !body)
"#;

/// Call sites: plain calls, path calls (`Widget::new()`, capturing the last
/// segment), method calls (`x.helper()`), and turbofish forms
/// (`generic_function` wraps the callee when type arguments are spelled).
/// Macro invocations are not `call_expression`s and stay out on purpose.
pub const RUST_CALLS: &str = r#"
(call_expression function: (identifier) @callee)
(call_expression function: (scoped_identifier name: (_) @callee))
(call_expression function: (field_expression field: (_) @callee))
(call_expression function: (generic_function function: (identifier) @callee))
(call_expression function: (generic_function function: (scoped_identifier name: (_) @callee)))
"#;

// Python
/// Functions, methods (class bodies), classes.
pub const PYTHON_SYMBOLS: &str = r#"
(function_definition name: (_) @name) @fn
(class_definition name: (_) @name) @class
(class_definition body: (block (function_definition name: (_) @name) @method))
"#;

/// Both import statement forms; the relative-import decode (dots → package
/// directory) happens structurally in [`crate::parse`] against the captured
/// statement nodes.
pub const PYTHON_IMPORTS: &str = r#"
(import_statement) @import
(import_from_statement) @from_import
"#;

/// Call sites: `foo()` and `obj.method()` (the attribute's final name).
pub const PYTHON_CALLS: &str = r#"
(call function: (identifier) @callee)
(call function: (attribute attribute: (_) @callee))
"#;

// JavaScript
/// Functions (incl. generators + arrow/function-expression consts), classes,
/// methods. JavaScript has no interfaces/enums, so the query is a strict
/// subset of the TypeScript one and must be kept separate — compiling the TS
/// query against the JS grammar would reference nonexistent node types.
pub const JS_SYMBOLS: &str = r#"
(function_declaration name: (_) @name) @fn
(generator_function_declaration name: (_) @name) @fn
(class_declaration name: (_) @name) @class
(method_definition name: (_) @name) @method
(variable_declarator name: (_) @name value: (arrow_function)) @fn
(variable_declarator name: (_) @name value: (function_expression)) @fn
"#;

/// `import`/`export … from`, dynamic `import(...)`, and `require(...)`. The
/// `require` pattern captures any identifier-callee call with a string
/// argument; [`crate::parse`] keeps only those whose callee text is
/// `require` (avoids query-predicate machinery).
pub const JS_IMPORTS: &str = r#"
(import_statement source: (string (string_fragment) @source))
(export_statement source: (string (string_fragment) @source))
(call_expression function: (import) arguments: (arguments (string (string_fragment) @source)))
(call_expression function: (identifier) @callee arguments: (arguments (string (string_fragment) @source)))
"#;

/// Call sites: `foo()` and `a.b.c()` (the member chain's final property).
/// `import(...)`'s callee is the `(import)` node, so dynamic imports never
/// register as calls; `require('x')` does — it *is* a call, and the import
/// edge it also produces is a separate fact.
pub const JS_CALLS: &str = r#"
(call_expression function: (identifier) @callee)
(call_expression function: (member_expression property: (_) @callee))
"#;

// TypeScript (also used for TSX)
/// Adds interfaces, enums, and abstract classes on top of the JS symbol set.
/// Shared verbatim by the `typescript` and `tsx` grammars (TSX is a superset
/// carrying the same declaration node types).
pub const TS_SYMBOLS: &str = r#"
(function_declaration name: (_) @name) @fn
(generator_function_declaration name: (_) @name) @fn
(class_declaration name: (_) @name) @class
(abstract_class_declaration name: (_) @name) @class
(interface_declaration name: (_) @name) @interface
(enum_declaration name: (_) @name) @enum
(method_definition name: (_) @name) @method
(variable_declarator name: (_) @name value: (arrow_function)) @fn
(variable_declarator name: (_) @name value: (function_expression)) @fn
"#;

/// Same import surface as JavaScript (the specifier node shapes are
/// identical); shared by `typescript` and `tsx`.
pub const TS_IMPORTS: &str = JS_IMPORTS;

/// Same call shapes as JavaScript; shared by `typescript` and `tsx`.
pub const TS_CALLS: &str = JS_CALLS;

// SQL
/// SQL DDL: tables (with their columns), views, and custom enum/composite
/// types. SQL has no import concept, so there is no `SQL_IMPORTS`.
///
/// `tree-sitter-sequel` node types:
/// - `create_table` wraps `object_reference` (table name) and
///   `column_definitions` (a list of `column_definition` children).
/// - `column_definition` has a `name` field.
/// - `create_type` is used for `CREATE TYPE foo AS ENUM (...)` and composite
///   types.
/// - `create_view` wraps a `column_definitions` optional + `select`.
/// - `object_reference` qualifies names via grammar fields
///   (`database:`/`schema:`/`name:`) — the object's own identifier is always
///   the `name:` field, so schema qualifiers must not become symbol names
///   (`CREATE TABLE public.users` indexes `users`, never `public` or
///   `public.users`).
pub const SQL_SYMBOLS: &str = r#"
(create_table
  (object_reference name: (_) @name)) @table
(column_definition name: (_) @name) @column
(create_type
  (object_reference name: (_) @name)) @schema_enum
(create_view
  (object_reference name: (_) @name)) @view
"#;

/// SQL has no imports — this empty string keeps the `LangPack` query-triple
/// shape uniform without introducing a conditional in `parse_file`.
pub const SQL_IMPORTS: &str = "";

/// SQL has no call graph either (function *invocations* in queries are not
/// the call edges this index models); empty for the same uniformity.
pub const SQL_CALLS: &str = "";

// Go
/// Functions, methods (receiver functions), and named types. Go's grammar is
/// regular enough that these three patterns cover the declaration surface.
pub const GO_SYMBOLS: &str = r#"
(function_declaration name: (_) @name) @fn
(method_declaration name: (_) @name) @method
(type_declaration (type_spec name: (_) @name type: (struct_type))) @struct
(type_declaration (type_spec name: (_) @name type: (interface_type))) @interface
"#;

/// Both the grouped `import ( … )` block and the single-line form; the
/// quoted path inside each spec is the edge target.
pub const GO_IMPORTS: &str = r#"
(import_declaration) @import
"#;

/// Call sites: `foo()` and `pkg.Fn()` / `recv.Method()` (the selector's
/// field). A package qualifier and a receiver are indistinguishable without
/// resolution — both record the final name, which is the honest grain here.
pub const GO_CALLS: &str = r#"
(call_expression function: (identifier) @callee)
(call_expression function: (selector_expression field: (_) @callee))
"#;

// Java
/// Classes, interfaces, enums, records, and methods. Java carries no free
/// functions, so every callable is a method on a type.
pub const JAVA_SYMBOLS: &str = r#"
(class_declaration name: (_) @name) @class
(interface_declaration name: (_) @name) @interface
(enum_declaration name: (_) @name) @enum
(record_declaration name: (_) @name) @class
(method_declaration name: (_) @name) @method
"#;

/// `import a.b.C;` — a package path, which resolves to a file only under the
/// conventional package-as-directory layout.
pub const JAVA_IMPORTS: &str = r#"
(import_declaration) @import
"#;

/// Call sites: every method invocation names its method directly in the
/// grammar's `name:` field. Constructor calls (`new Foo()`) are typed
/// object creation, not the name-based call edge this table models.
pub const JAVA_CALLS: &str = r#"
(method_invocation name: (_) @callee)
"#;

// C
/// Function definitions, plus the struct/union/enum and typedef surface.
/// The declarator nesting is why functions need their own pattern rather
/// than a bare name capture: the identifier sits under `function_declarator`.
pub const C_SYMBOLS: &str = r#"
(function_definition
  declarator: (function_declarator declarator: (identifier) @name)) @fn
(struct_specifier name: (type_identifier) @name) @struct
(union_specifier name: (type_identifier) @name) @struct
(enum_specifier name: (type_identifier) @name) @enum
(type_definition declarator: (type_identifier) @name) @struct
"#;

/// `#include` in both forms. Only the quoted form reliably names a file in
/// the tree; angle-bracket includes point at system headers and are captured
/// but will not resolve to a workspace path.
pub const C_IMPORTS: &str = r#"
(preproc_include) @import
"#;

/// Call sites: `foo()` and struct-member calls through `.`/`->`
/// (`ops->handler()`). A call through a parenthesized pointer expression
/// (`(*fp)()`) has no name node and is skipped rather than guessed.
pub const C_CALLS: &str = r#"
(call_expression function: (identifier) @callee)
(call_expression function: (field_expression field: (_) @callee))
"#;

// PHP
/// Classes, interfaces, traits, enums, free functions, and methods.
pub const PHP_SYMBOLS: &str = r#"
(class_declaration name: (_) @name) @class
(interface_declaration name: (_) @name) @interface
(trait_declaration name: (_) @name) @trait
(enum_declaration name: (_) @name) @enum
(function_definition name: (_) @name) @fn
(method_declaration name: (_) @name) @method
"#;

/// Both mechanisms, which resolve differently: `require`/`include` name a
/// literal path and resolve like C's quoted include, while `use` names a
/// NAMESPACE — mapping that to a file needs composer.json's PSR-4 autoload
/// map, so those edges are captured but stay unresolved until that lands.
pub const PHP_IMPORTS: &str = r#"
(namespace_use_declaration) @import
(expression_statement (include_expression)) @include
(expression_statement (require_expression)) @require
"#;

/// Call sites: free functions (`foo()`), instance methods (`$x->m()`), and
/// static methods (`Cls::m()`). Namespace-qualified free calls keep only
/// their final name via the decode's last-identifier rule.
pub const PHP_CALLS: &str = r#"
(function_call_expression function: (name) @callee)
(member_call_expression name: (_) @callee)
(scoped_call_expression name: (_) @callee)
"#;
