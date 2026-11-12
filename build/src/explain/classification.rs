//! The classification of a `build_failed` line, as the explain view names it.

use serde_json::Value;

use ap_core::unowned_label::files_not_owned;

/// The label of a `build_failed` line's `classification`. A unit variant is
/// its name (`"InstallFailed"`). `FilesNotOwned` is an object, so it is read
/// here and worded with its paths; any other object gives no label.
pub(super) fn classification_label(v: &Value) -> Option<String> {
    if let Some(name) = v.as_str() {
        return Some(name.to_string());
    }
    let paths = v.get("FilesNotOwned")?.get("paths")?.as_array()?;
    let paths: Vec<String> = paths
        .iter()
        .filter_map(|p| p.as_str().map(str::to_string))
        .collect();
    Some(files_not_owned(&paths))
}

#[cfg(test)]
mod tests {
    use super::classification_label;
    use serde_json::json;

    #[test]
    fn a_unit_variant_is_its_name() {
        assert_eq!(
            classification_label(&json!("InstallFailed")).as_deref(),
            Some("InstallFailed")
        );
    }

    #[test]
    fn files_not_owned_is_worded_with_its_paths() {
        let v = json!({"FilesNotOwned": {"paths": ["/usr/share/a", "/usr/share/b"]}});
        assert_eq!(
            classification_label(&v).as_deref(),
            Some("files not owned: /usr/share/a, /usr/share/b")
        );
    }

    #[test]
    fn another_object_variant_still_gives_no_label() {
        let v = json!({"UnresolvableDeps": {"packages": ["bar-devel"]}});
        assert_eq!(classification_label(&v), None);
    }
}
