use serde_json::Value;

pub(crate) fn decode_pace_payload(script: &str) -> Option<Value> {
    let marker = "self.__pace_f.push(";
    let start = script.find(marker)? + marker.len();
    let end = script.rfind(')')?;
    let pushed: Value = serde_json::from_str(script[start..end].trim()).ok()?;
    let encoded = pushed.as_array()?.get(1)?.as_str()?;
    let json_text = encoded.split_once(':').map_or(encoded, |(_, value)| value);
    serde_json::from_str(json_text).ok()
}
