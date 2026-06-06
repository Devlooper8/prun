//! Human-readable labels for ecosystem ids (curated, with a title-case fallback).

/// Human-readable label for an ecosystem id (curated, with a title-case fallback).
pub(crate) fn ecosystem_label(id: &str) -> String {
    let s = match id {
        "rust" => "Rust",
        "go" => "Go",
        "cpp" => "C/C++",
        "bazel" => "Bazel",
        "zig" => "Zig",
        "nim" => "Nim",
        "swift" => "Swift",
        "dotnet" => ".NET",
        "jvm" => "JVM",
        "node" => "Node.js",
        "python" => "Python",
        "php" => "PHP",
        "ruby" => "Ruby",
        "dart" => "Dart / Flutter",
        "beam" => "Erlang / Elixir",
        "haskell" => "Haskell",
        "crystal" => "Crystal",
        "gamedev" => "Game engines",
        "infra" => "Infra / IaC",
        "latex" => "LaTeX",
        "nix" => "Nix",
        "data" => "Data",
        "docs" => "Docs / SSG",
        "testing" => "Testing / E2E",
        "junk" => "OS / junk",
        "editor" => "Editor caches",
        other => return title_case(other),
    };
    s.to_string()
}

fn title_case(id: &str) -> String {
    if id.is_empty() {
        return "Other".to_string();
    }
    id.split(['-', '_'])
        .filter(|s| !s.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
