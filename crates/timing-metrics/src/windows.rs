//! Split structured windows without truncating metrics or exceeding logger limits.
use serde_json::{Map, Value};

#[derive(Clone)]
enum Segment {
    Key(String),
    Index(usize),
}

fn leaves(value: &Value, path: &mut Vec<Segment>, output: &mut Vec<(Vec<Segment>, Value)>) {
    let atomic = value.as_object().is_some_and(|v| {
        v.contains_key("calls") && v.contains_key("total_us") && v.contains_key("max_us")
            || v.contains_key("stage") && v.contains_key("timing_us")
            || v.contains_key("channel")
    });
    if atomic
        || value.as_object().is_some_and(Map::is_empty)
        || value.as_array().is_some_and(Vec::is_empty)
    {
        output.push((path.clone(), value.clone()));
        return;
    }
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                path.push(Segment::Key(key.clone()));
                leaves(child, path, output);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                path.push(Segment::Index(index));
                leaves(child, path, output);
                path.pop();
            }
        }
        _ => output.push((path.clone(), value.clone())),
    }
}
fn insert(value: &mut Value, path: &[Segment], leaf: Value) {
    let Some((head, tail)) = path.split_first() else {
        *value = leaf;
        return;
    };
    match head {
        Segment::Key(key) => {
            if !value.is_object() {
                *value = Value::Object(Map::new());
            }
            insert(
                value
                    .as_object_mut()
                    .expect("created object")
                    .entry(key)
                    .or_insert(Value::Null),
                tail,
                leaf,
            );
        }
        Segment::Index(index) => {
            if !value.is_array() {
                *value = Value::Array(Vec::new());
            }
            let items = value.as_array_mut().expect("created array");
            items.resize(items.len().max(index + 1), Value::Null);
            insert(&mut items[*index], tail, leaf);
        }
    }
}
/// Identity fields repeat; every measurement and non-identity counter occurs once.
/// The schema has no caller-controlled paths or unbounded payload values.
pub fn window_parts(payload: &Value) -> Vec<Value> {
    const BUDGET: usize = 6800; // Runtime's 7500-byte limit leaves metadata headroom.
    if payload.to_string().len() <= BUDGET {
        return vec![payload.clone()];
    }
    let Some(fields) = payload.as_object() else {
        return vec![serde_json::json!({"event":"diagnostic_unsupported_shape"})];
    };
    let mut identity = Map::new();
    let mut observations = Vec::new();
    for (key, value) in fields {
        if [
            "event",
            "stream",
            "encoder_session",
            "module_type",
            "context_index",
            "principal",
            "window_us",
            "window_ms",
            "scope",
            "timing_scope",
            "monotonic_end_us",
            "os_thread_id",
            "resources",
        ]
        .contains(&key.as_str())
        {
            identity.insert(key.clone(), value.clone());
        } else {
            leaves(
                value,
                &mut vec![Segment::Key(key.clone())],
                &mut observations,
            );
        }
    }
    identity.insert(
        "diagnostic_window_monotonic_us".into(),
        serde_json::json!(super::monotonic_time_us()),
    );
    let base = Value::Object(identity);
    let mut current = base.clone();
    let mut count = 0;
    let mut parts = Vec::new();
    for (path, value) in observations {
        let mut trial = current.clone();
        insert(&mut trial, &path, value.clone());
        if trial.to_string().len() > BUDGET && count != 0 {
            parts.push(current);
            current = base.clone();
            count = 0;
        }
        insert(&mut current, &path, value);
        count += 1;
    }
    if count != 0 {
        parts.push(current);
    }
    let total = parts.len();
    for (index, part) in parts.iter_mut().enumerate() {
        part["diagnostic_part"] = Value::from(index + 1);
        part["diagnostic_parts"] = Value::from(total);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_large_window_keeps_each_metric_once_with_the_same_identity() {
        let mut metrics = Map::new();
        for index in 0..160 {
            metrics.insert(
                format!("stage{index}"),
                serde_json::json!({"calls":1,"total_us":12345,"max_us":12345}),
            );
        }
        let payload = serde_json::json!({"event":"test_window","stream":7,"timing_us":metrics});
        let parts = window_parts(&payload);
        assert!(parts.len() > 1);
        assert!(
            parts
                .iter()
                .all(|p| p.to_string().len() < 7500 && p["stream"] == 7)
        );
        assert_eq!(
            parts
                .iter()
                .map(|p| p["timing_us"].as_object().unwrap().len())
                .sum::<usize>(),
            160
        );
    }
}
