use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Parse an `/etc/os-release`-style KEY=VALUE file. Strips matching outer
/// double or single quotes from the value. Comments and blank lines ignored.
pub fn parse<P: AsRef<Path>>(path: P) -> Result<HashMap<String, String>> {
    let path = path.as_ref();
    let content =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        let v = strip_quotes(v);
        map.insert(k.trim().to_string(), v.to_string());
    }
    Ok(map)
}

fn strip_quotes(s: &str) -> &str {
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parses_quoted_unquoted_and_comments() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "# header").unwrap();
        writeln!(f, "ID=ubuntu").unwrap();
        writeln!(f, "VERSION_CODENAME=\"noble\"").unwrap();
        writeln!(f, "PRETTY_NAME='Ubuntu 24.04 LTS'").unwrap();
        writeln!(f).unwrap();
        let m = parse(f.path()).unwrap();
        assert_eq!(m.get("ID").unwrap(), "ubuntu");
        assert_eq!(m.get("VERSION_CODENAME").unwrap(), "noble");
        assert_eq!(m.get("PRETTY_NAME").unwrap(), "Ubuntu 24.04 LTS");
    }
}
