//! How an addon author can change the code or zip so a finding goes away, or at least shrinks.
//! Keyed by the stable rule ID and the snippet, so stored scans get hints without a rescan.

/// A hint for the finding of `rule` on `snippet`; `None` for rules that only inform.
pub fn hint(rule: &str, snippet: &str) -> Option<&'static str> {
    Some(match rule {
        "lua.forbidden_global" if snippet.contains("collectgarbage") => "Remove the collectgarbage call: the game collects garbage on its own.",
        "lua.forbidden_global" => "Remove the call. Addons may not reach files, the system or other code; use the game API instead.",
        "lua.forbidden_name_string" => "Do not spell the name of a forbidden function in a string; rename the key or remove the string.",
        "lua.dynamic_code" if reads_global_by_name(snippet) => {
            "To read a global by name, use getglobal(name) instead of loadstring(\"return \" .. name)."
        }
        "lua.dynamic_code" if snippet.contains("RunScript") => "Call the function directly instead of passing code to RunScript.",
        "lua.dynamic_code" => "Write the code as a normal function and call it, instead of running it from a string.",
        "lua.dynamic_global" if snippet.contains("_G") => "Use _G.Name or _G[\"Name\"] when the name is fixed; keep computed names to your own table.",
        "lua.dynamic_global" => "Pass the name as a fixed string, like setglobal(\"Name\", value), when it is known.",
        "lua.environment" => return None,
        "lua.global_metatable" => "Keep your data in your own table instead of changing how _G works.",
        "lua.chat_send" => "Send messages only when the player asks for it, from a button or a slash command.",
        "lua.api_override" => "Keep the original in a local and call it from your replacement, so the game and other addons still work.",
        "lua.obfuscation_char" | "lua.obfuscation_escape" => "Write the text as a plain string.",
        "lua.obfuscation_entropy" => "Ship data as a readable Lua table, not as an encoded string.",
        "lua.binary" => "Ship the .lua source code, not compiled bytecode.",
        "lua.long_line" => "Ship the code as written, not minified, and break up long lines.",
        "lua.parse_error" | "xml.parse_error" => "Fix the syntax error; code the scanner cannot read is not checked.",
        "lua.url" => "Remove the address unless the addon shows it to the player on purpose.",
        "xml.doctype" => "Remove the DOCTYPE line; game UI files do not need it.",
        "xml.missing_script_file" => "Add the file to the zip or remove the <Script> line.",
        "zip.executable" | "zip.nested_archive" | "zip.extension" => "Remove the file from the zip.",
        "zip.bomb" => "Zip only the addon folders, with normal compression.",
        "zip.deep_nesting" => "Flatten the folders of the addon.",
        "zip.path_traversal" | "zip.symlink" => "Zip the addon folders again with a normal zip tool.",
        "zip.unreadable" => "Zip the addon folders again, without a password.",
        "zip.foreign_folder" => "Remove the folder of the other addon from the zip.",
        "zip.undeclared_folder" => "Register the folder for this addon, or remove it from the zip.",
        "zip.missing_folder" => "Add the folder to the zip, or remove it from the registered folders.",
        "zip.toc_missing_file" => "Add the file to the zip or remove its line from the .toc.",
        _ => return None,
    })
}

/// `loadstring("return " .. name)`: code that only reads a global.
fn reads_global_by_name(snippet: &str) -> bool {
    let compact: String = snippet.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains("loadstring(\"return\"..") || compact.contains("loadstring('return'..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_code_hints_fit_the_snippet() {
        assert!(hint("lua.dynamic_code", r#"local chunk = loadstring("return " .. nm)"#).unwrap().contains("getglobal(name)"));
        assert!(hint("lua.dynamic_code", "RunScript(t)").unwrap().contains("directly"));
        assert!(hint("lua.dynamic_code", r#"loadstring("return AZS_GetTotems()")"#).unwrap().contains("normal function"));
    }

    #[test]
    fn info_rules_have_no_hint() {
        assert_eq!(hint("zip.hidden", ""), None);
        assert_eq!(hint("lua.environment", "getfenv(1)"), None);
        assert_eq!(hint("no.such_rule", ""), None);
    }
}
