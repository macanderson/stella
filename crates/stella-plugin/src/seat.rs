//! The seat key — one plugin's role, in a name no other plugin can write.
//!
//! A user gives a model to a seat in `[seats]`. The key is always
//! `<plugin>/<role>` (`doc:roleless-core` §8.4). The plugin half is the `name`
//! field of the manifest. The role half is one `[roles.<name>]` key.
//!
//! The host joins the two. A plugin sends its own bare role name and nothing
//! more. So no plugin can write a key that another plugin owns.
//!
//! Nothing splits a key to route a turn. The lookup compares whole strings. A
//! screen may split one to show where a seat came from, so the manifest turns
//! down a `/` in either half.

/// The character between the plugin half and the role half of a seat key.
///
/// A `/`, because the values beside these keys are `provider/slug` model
/// strings and read the same way (`doc:roleless-core` §8.4 weighs the two
/// rejected spellings).
pub const SEAT_SEPARATOR: char = '/';

/// The seat key for `role` of `plugin`.
///
/// `plugin` is a [`PluginManifest::name`](crate::PluginManifest::name), which
/// is what a plugin is installed under and what the roster looks it up by.
/// `role` is one `[roles.<name>]` key, as the plugin spelled it.
///
/// The one place this key is built. Two ends that each join their own halves
/// are two ends free to drift: a settings pane offering `vera/verifier` while
/// dispatch asks for `verifier` is a settings line no lookup can ever find.
#[must_use]
pub fn seat_key(plugin: &str, role: &str) -> String {
    format!("{plugin}{SEAT_SEPARATOR}{role}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_the_plugin_then_the_role() {
        assert_eq!(seat_key("vera", "verifier"), "vera/verifier");
    }

    /// Two plugins with the same bare role name get two keys, which is the
    /// whole reason the key carries a plugin at all.
    #[test]
    fn the_same_role_under_two_plugins_is_two_keys() {
        assert_ne!(
            seat_key("stella-plan", "planner"),
            seat_key("acme-plan", "planner")
        );
    }
}
