//! Validation-only Wasm library; it does not add a BrowserJob protocol operation.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn evaluate(
    pattern: &[u8],
    text: &[u8],
    count: u32,
    ignore_case: bool,
) -> Result<String, wasm_bindgen::JsValue> {
    let result = retex_native_regex::matches(pattern, text, count as usize, ignore_case)
        .map_err(|e| wasm_bindgen::JsValue::from_str(&e))?;
    Ok(serde_json::json!({"matched":result.matched,"spans":result.spans,"compile_error":result.compile_error}).to_string())
}
