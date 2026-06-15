//! Shared classification of external-command failures (`gh`, `git`, `acli`, `claude`) into
//! actionable, user-facing messages. The Tauri layer stringifies these straight into a toast,
//! so the wording is what the user reads — distinguish "not installed", "not authenticated",
//! and "network unreachable" from a generic failure rather than dumping raw stderr.

use std::io;

/// A command that couldn't even be spawned (the binary is missing, or the OS refused).
/// `ErrorKind::NotFound` is the common case: the CLI isn't installed / not on PATH.
pub fn spawn_error(tool: &str, e: &io::Error) -> anyhow::Error {
    if e.kind() == io::ErrorKind::NotFound {
        anyhow::anyhow!("{tool} is not installed or not on your PATH")
    } else {
        anyhow::anyhow!("could not run {tool}: {e}")
    }
}

/// Classify a command that ran but exited non-zero, from its stderr. Recognises auth and
/// network failures; otherwise passes the trimmed stderr through with the tool name.
pub fn classify(tool: &str, stderr: &str) -> anyhow::Error {
    let s = stderr.trim();
    let low = s.to_lowercase();
    if is_auth_failure(&low) {
        anyhow::anyhow!("{tool} is not authenticated — {}", auth_hint(tool))
    } else if is_network_failure(&low) {
        anyhow::anyhow!(
            "network error reaching {tool} — check your connection{}",
            first_line(s).map(|l| format!(" ({l})")).unwrap_or_default()
        )
    } else if s.is_empty() {
        anyhow::anyhow!("{tool} failed (no error output)")
    } else {
        anyhow::anyhow!("{tool} failed: {s}")
    }
}

pub fn is_auth_failure(low: &str) -> bool {
    const PATTERNS: [&str; 8] = [
        "not logged in",
        "not logged into",
        "authentication",
        "unauthorized",
        "auth login",
        "gh auth login",
        "401",
        "credentials",
    ];
    PATTERNS.iter().any(|p| low.contains(p))
}

pub fn is_network_failure(low: &str) -> bool {
    const PATTERNS: [&str; 10] = [
        "could not resolve host",
        "couldn't resolve host",
        "network is unreachable",
        "connection refused",
        "connection timed out",
        "could not connect",
        "failed to connect",
        "no such host",
        "temporary failure in name resolution",
        "dial tcp",
    ];
    PATTERNS.iter().any(|p| low.contains(p))
}

fn auth_hint(tool: &str) -> &'static str {
    match tool {
        "gh" => "run `gh auth login`",
        "acli" => "run `acli jira auth login`",
        _ => "check your credentials",
    }
}

fn first_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).find(|l| !l.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_not_found_says_not_installed() {
        let e = io::Error::new(io::ErrorKind::NotFound, "No such file or directory");
        let msg = spawn_error("gh", &e).to_string();
        assert!(msg.contains("gh"));
        assert!(msg.contains("not installed"));
    }

    #[test]
    fn classifies_auth_failures() {
        // gh's typical logged-out message, and a generic acli auth-expired one.
        let gh = classify("gh", "gh: To get started with GitHub CLI, please run: gh auth login").to_string();
        assert!(gh.contains("not authenticated"));
        assert!(gh.contains("gh auth login"));
        let acli = classify("acli", "Error: you are not logged in").to_string();
        assert!(acli.contains("not authenticated"));
        assert!(acli.contains("acli jira auth login"));
    }

    #[test]
    fn classifies_network_failures() {
        for stderr in [
            "fatal: unable to access 'https://github.com/x.git/': Could not resolve host: github.com",
            "dial tcp: lookup api.atlassian.com: no such host",
            "ssh: connect to host github.com port 22: Connection refused",
        ] {
            let msg = classify("git", stderr).to_string();
            assert!(msg.contains("network error"), "expected network classification for: {stderr}");
        }
    }

    #[test]
    fn falls_back_to_raw_stderr() {
        let msg = classify("git", "fatal: not a git repository").to_string();
        assert!(msg.contains("not a git repository"));
        // Empty stderr still produces a sensible message rather than a bare "failed:".
        assert!(classify("gh", "   ").to_string().contains("no error output"));
    }
}
