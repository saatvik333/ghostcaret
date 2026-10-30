//! Reading the models endpoints. Every list comes from the provider.

use serde_json::Value;

/// A model the provider offers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ModelInfo {
    /// The name to send as `model`.
    pub id: String,
    /// A friendlier name when the provider gives one, else the id.
    pub name: String,
}

/// `(id, name)` for each entry of a list of model objects or plain strings.
pub(crate) fn from_list(list: Option<&Value>) -> Vec<(String, String)> {
    let Some(items) = list.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            if let Some(id) = item.as_str() {
                return Some((id.to_owned(), id.to_owned()));
            }
            let field = |k: &str| {
                item.get(k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            };
            let id = field("id")
                .or_else(|| field("model"))
                .or_else(|| field("name"))?;
            let name = field("display_name")
                .or_else(|| field("name"))
                .unwrap_or(id);
            Some((id.to_owned(), name.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_objects_and_strings_and_skips_the_rest() {
        let v = json!([{"id":"a","name":"Model A"}, "b", {"id":"c"}, {"object":"x"}, 7]);
        assert_eq!(
            from_list(Some(&v)),
            [
                ("a".into(), "Model A".into()),
                ("b".into(), "b".into()),
                ("c".into(), "c".into())
            ]
        );
        assert!(from_list(None).is_empty());
    }
}
