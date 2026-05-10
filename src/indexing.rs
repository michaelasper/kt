use crate::{Chunk, Language};
use std::path::Path;
use tracing::warn;
use tree_sitter::{Node, Parser, Tree};

mod languages;

pub use languages::LanguageConfig;

pub fn parse_file(path: &Path, relative_path: &str, language: Language) -> Vec<Chunk> {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to read file {}: {e}", path.display());
            return Vec::new();
        }
    };

    let config = LanguageConfig::for_language(language);
    let mut parser = Parser::new();
    parser
        .set_language(&config.tree_sitter_language())
        .unwrap_or_else(|e| {
            panic!("Failed to set language for {:?}: {e}", language);
        });

    match parser.parse(&source, None) {
        Some(tree) => extract_chunks(&tree, &source, relative_path, language, &config),
        None => {
            warn!(
                "Tree-sitter parse failed for {}, falling back to line splitting",
                path.display()
            );
            fallback_line_chunks(&source, relative_path, language)
        }
    }
}

fn extract_chunks(
    tree: &Tree,
    source: &str,
    relative_path: &str,
    language: Language,
    config: &LanguageConfig,
) -> Vec<Chunk> {
    let root = tree.root_node();
    let mut chunks = Vec::new();
    let mut cursor = root.walk();

    collect_chunks(
        &mut cursor,
        source,
        relative_path,
        language,
        config,
        &mut chunks,
        None,
    );

    chunks
}

fn collect_chunks<'a>(
    cursor: &mut tree_sitter::TreeCursor<'a>,
    source: &'a str,
    relative_path: &str,
    language: Language,
    config: &LanguageConfig,
    chunks: &mut Vec<Chunk>,
    parent_context: Option<ParentContext>,
) {
    let node = cursor.node();
    let node_type = node.kind();

    if config.is_target_node(node_type) {
        if let Some(chunk) = build_chunk(node, source, relative_path, language, &parent_context) {
            chunks.push(chunk);
        }
    }

    let child_context = build_child_context(node, source, config);
    let ctx = child_context.or(parent_context);

    if cursor.goto_first_child() {
        loop {
            collect_chunks(
                cursor,
                source,
                relative_path,
                language,
                config,
                chunks,
                ctx.clone(),
            );
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

#[derive(Debug, Clone)]
struct ParentContext {
    header: String,
    node_type: String,
}

fn build_chunk(
    node: Node,
    source: &str,
    relative_path: &str,
    language: Language,
    parent_context: &Option<ParentContext>,
) -> Option<Chunk> {
    let content = node_text(node, source);
    if content.trim().is_empty() {
        return None;
    }

    let name = extract_name(node, source)?;
    let node_type = normalize_node_type(node.kind());
    let signature = extract_signature(node, source);
    let start_line = node.start_position().row;
    let end_line = node.end_position().row;

    let full_content = match parent_context {
        Some(ctx) => {
            if node_type == ctx.node_type {
                content.clone()
            } else {
                format!("{}\n{}", ctx.header, content)
            }
        }
        None => content.clone(),
    };

    let parent_ctx_str = parent_context.as_ref().map(|ctx| ctx.header.clone());

    let chunk_id = Chunk::generate_id(relative_path, &name);

    Some(Chunk {
        chunk_id,
        filepath: relative_path.to_string(),
        language,
        node_type,
        name,
        signature,
        content: full_content,
        parent_context: parent_ctx_str,
        start_line,
        end_line,
    })
}

fn build_child_context(node: Node, source: &str, config: &LanguageConfig) -> Option<ParentContext> {
    let node_type = node.kind();
    if !config.is_container_node(node_type) {
        return None;
    }

    let header = extract_container_header(node, source)?;
    Some(ParentContext {
        header,
        node_type: normalize_node_type(node_type),
    })
}

fn extract_container_header(node: Node, source: &str) -> Option<String> {
    let start = node.start_position().row;
    let end = node.end_position().row;

    if end - start <= 3 {
        return Some(node_text(node, source));
    }

    let lines: Vec<&str> = source.lines().take(start + 3).skip(start).collect();
    Some(lines.join("\n"))
}

fn extract_name(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" | "type_identifier" | "field_identifier" => {
                return Some(node_text(child, source));
            }
            "name" => {
                return Some(node_text(child, source));
            }
            _ => {}
        }
    }
    Some(format!("{}_{}", node.kind(), node.start_position().row))
}

fn extract_signature(node: Node, source: &str) -> String {
    let kind = node.kind();
    let text = node_text(node, source);

    if matches!(
        kind,
        "struct_item"
            | "struct_declaration"
            | "enum_item"
            | "enum_declaration"
            | "trait_item"
            | "interface_declaration"
            | "type_alias"
            | "type_alias_declaration"
    ) {
        let first_line = text.lines().next().unwrap_or("").trim();
        if let Some(pos) = first_line.find('{') {
            return first_line[..pos].trim().to_string();
        }
        return first_line.to_string();
    }

    if matches!(
        kind,
        "function_item"
            | "function_declaration"
            | "method_declaration"
            | "constructor_declaration"
    ) {
        let first_line = text.lines().next().unwrap_or("").trim();
        if let Some(stripped) = first_line.strip_suffix('{') {
            return stripped.trim().to_string();
        }
        if first_line.ends_with("->") || first_line.ends_with('=') {
            let lines: Vec<&str> = text.lines().take(3).collect();
            for line in &lines {
                let l = line.trim();
                if let Some(stripped) = l.strip_suffix('{') {
                    return stripped.trim().to_string();
                }
            }
        }
        return first_line.to_string();
    }

    let first_line = text.lines().next().unwrap_or("").trim();
    if first_line.len() > 200 {
        first_line[..200].to_string()
    } else {
        first_line.to_string()
    }
}

fn node_text(node: Node, source: &str) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    source[start..end].to_string()
}

fn normalize_node_type(kind: &str) -> String {
    match kind {
        "function_item" | "function_declaration" | "method_declaration" => "function".to_string(),
        "struct_item" | "struct_declaration" | "type_declaration" => "struct".to_string(),
        "enum_item" | "enum_declaration" => "enum".to_string(),
        "impl_item" => "impl".to_string(),
        "trait_item" => "trait".to_string(),
        "interface_declaration" => "interface".to_string(),
        "class_declaration" => "class".to_string(),
        "constructor_declaration" => "constructor".to_string(),
        "type_alias" | "type_alias_declaration" => "type_alias".to_string(),
        "const_item" | "const_declaration" => "const".to_string(),
        _ => kind.to_string(),
    }
}

fn fallback_line_chunks(source: &str, relative_path: &str, language: Language) -> Vec<Chunk> {
    let lines: Vec<&str> = source.lines().collect();
    let chunk_size = 30;
    let mut chunks = Vec::new();

    for (i, chunk_lines) in lines.chunks(chunk_size).enumerate() {
        let start_line = i * chunk_size;
        let end_line = start_line + chunk_lines.len().saturating_sub(1);
        let content = chunk_lines.join("\n");
        let name = format!(
            "lines_{}_{}",
            start_line,
            end_line
        );
        let chunk_id = Chunk::generate_id(relative_path, &name);

        chunks.push(Chunk {
            chunk_id,
            filepath: relative_path.to_string(),
            language,
            node_type: "text_block".to_string(),
            name: name.clone(),
            signature: format!("lines {}-{}", start_line, end_line),
            content,
            parent_context: None,
            start_line,
            end_line,
        });
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rust_function() {
        let source = r#"
fn hello_world() -> String {
    "hello".to_string()
}
"#;
        let config = LanguageConfig::for_language(Language::Rust);
        let mut parser = Parser::new();
        parser.set_language(&config.tree_sitter_language()).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let chunks = extract_chunks(&tree, source, "test.rs", Language::Rust, &config);
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].node_type, "function");
        assert_eq!(chunks[0].name, "hello_world");
    }

    #[test]
    fn test_parse_rust_impl_with_parent_context() {
        let source = r#"
struct Foo {
    x: i32,
}

impl Foo {
    pub fn get_x(&self) -> i32 {
        self.x
    }
}
"#;
        let config = LanguageConfig::for_language(Language::Rust);
        let mut parser = Parser::new();
        parser.set_language(&config.tree_sitter_language()).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let chunks = extract_chunks(&tree, source, "test.rs", Language::Rust, &config);
        assert!(chunks.len() >= 2);

        let impl_fn = chunks
            .iter()
            .find(|c| c.name == "get_x")
            .expect("should find get_x");
        assert!(impl_fn.content.contains("impl"));
        assert_eq!(impl_fn.node_type, "function");
        assert!(chunks.iter().all(|c| c.node_type != "impl"));
    }

    #[test]
    fn test_fallback_line_chunks() {
        let source = "line1\nline2\nline3\nline4\n";
        let chunks = fallback_line_chunks(source, "test.rs", Language::Rust);
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].node_type, "text_block");
    }
}
