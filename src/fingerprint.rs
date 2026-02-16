use once_cell::sync::Lazy;
use regex::Regex;
use xxhash_rust::xxh3::xxh3_64;

static UUID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
        .unwrap()
});

static IP_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}").unwrap());

static NUMBER_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\d+").unwrap());

/// Normalize an error message for fingerprinting.
pub fn normalize_message(message: &str) -> String {
    let s = UUID_RE.replace_all(message, "<uuid>");
    let s = IP_RE.replace_all(&s, "<ip>");
    let s = NUMBER_RE.replace_all(&s, "<n>");
    s.to_lowercase().trim().to_string()
}

/// Extract the top non-framework stack frame.
/// Strips line numbers, keeps file + function.
pub fn extract_top_frame(stack: Option<&str>) -> String {
    let Some(stack) = stack else {
        return String::new();
    };

    // Common framework prefixes to skip
    let skip_prefixes = [
        "node_modules/",
        "UIKitCore",
        "CoreFoundation",
        "libdispatch",
        "Foundation",
        "SwiftUI",
        "java.lang.",
        "android.os.",
        "kotlin.",
        "com.apple.",
    ];

    for line in stack.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let is_framework = skip_prefixes.iter().any(|prefix| trimmed.contains(prefix));
        if !is_framework {
            // Strip line numbers (":123", " line 42", etc.)
            let cleaned = LINE_NUM_RE.replace_all(trimmed, "");
            return cleaned.trim().to_string();
        }
    }

    // Fallback to first non-empty line
    stack
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

static LINE_NUM_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r":\d+(?::\d+)?| line \d+").unwrap());

/// Compute the fingerprint for an event.
pub fn compute_fingerprint(
    source: &str,
    error_type: &str,
    route_or_procedure: Option<&str>,
    message: &str,
    stack: Option<&str>,
) -> String {
    let normalized = normalize_message(message);
    let top_frame = extract_top_frame(stack);
    let route = route_or_procedure.unwrap_or("");

    let input = format!("{source}:{error_type}:{route}:{normalized}:{top_frame}");
    let hash = xxh3_64(input.as_bytes());
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_message() {
        assert_eq!(
            normalize_message("Error at 192.168.1.1 for user abc123"),
            "error at <ip> for user abc<n>"
        );
        assert_eq!(
            normalize_message("Failed for 550e8400-e29b-41d4-a716-446655440000"),
            "failed for <uuid>"
        );
        assert_eq!(
            normalize_message("  Timeout after 5000ms  "),
            "timeout after <n>ms"
        );
    }

    #[test]
    fn test_extract_top_frame() {
        let stack = "  at MyApp.handleError (src/handler.ts:42:10)\n  at node_modules/express/lib/router.js:100:5";
        assert_eq!(
            extract_top_frame(Some(stack)),
            "at MyApp.handleError (src/handler.ts)"
        );
    }

    #[test]
    fn test_extract_top_frame_none() {
        assert_eq!(extract_top_frame(None), "");
    }

    #[test]
    fn test_fingerprint_deterministic() {
        let fp1 = compute_fingerprint(
            "api",
            "TypeError",
            Some("/users"),
            "Cannot read property 'id' of undefined",
            None,
        );
        let fp2 = compute_fingerprint(
            "api",
            "TypeError",
            Some("/users"),
            "Cannot read property 'id' of undefined",
            None,
        );
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 16);
    }

    #[test]
    fn test_fingerprint_normalizes_numbers() {
        let fp1 = compute_fingerprint("api", "TimeoutError", None, "Timeout after 5000ms", None);
        let fp2 = compute_fingerprint("api", "TimeoutError", None, "Timeout after 3000ms", None);
        assert_eq!(
            fp1, fp2,
            "different numbers should produce same fingerprint"
        );
    }
}
