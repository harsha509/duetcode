/// Per-million-token pricing (input, output) for known models.
/// Returns (input_price_per_1m, output_price_per_1m).
pub fn model_pricing(model: &str) -> Option<(f64, f64)> {
    let m = model.to_lowercase();

    // Claude models
    if m.contains("opus") {
        return Some((15.0, 75.0));
    }
    if m.contains("sonnet") {
        return Some((3.0, 15.0));
    }
    if m.contains("haiku") {
        return Some((0.25, 1.25));
    }

    // Gemini models
    if m.contains("gemini-2.5-pro") || m.contains("gemini-3") {
        return Some((1.25, 10.0));
    }
    if m.contains("gemini-2.5-flash") {
        return Some((0.15, 0.60));
    }
    if m.contains("gemini-2.0-flash") || m.contains("gemini-2.0-flash-lite") {
        return Some((0.10, 0.40));
    }
    if m.contains("gemini-1.5-pro") {
        return Some((1.25, 5.0));
    }
    if m.contains("gemini-1.5-flash") {
        return Some((0.075, 0.30));
    }

    None
}

pub fn compute_cost(model: &str, input_tokens: u64, output_tokens: u64) -> Option<f64> {
    model_pricing(model).map(|(input_price, output_price)| {
        (input_tokens as f64 * input_price / 1_000_000.0)
            + (output_tokens as f64 * output_price / 1_000_000.0)
    })
}
